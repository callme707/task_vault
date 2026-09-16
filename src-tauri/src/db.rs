use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::Local;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

use crate::models::{AiParsed, InboxItem, Subtask, Task, TaskDraft};

const SNAPSHOT_NAME: &str = "tasks.vault";
const EMPTY_LEGACY_ARCHIVE_MARKER: &[u8] = b"TaskVault empty legacy file v1";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS inbox (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    raw_text       TEXT NOT NULL,
    source         TEXT NOT NULL DEFAULT 'hotkey',
    created_at     TEXT NOT NULL,
    ai_status      TEXT NOT NULL DEFAULT 'pending',
    ai_result      TEXT,
    ai_model       TEXT,
    is_promoted    INTEGER NOT NULL DEFAULT 0,
    deleted_at     TEXT,
    image_path     TEXT,
    promoted_keys  TEXT NOT NULL DEFAULT '[]',
    ai_generation  INTEGER NOT NULL DEFAULT 0,
    ai_requested   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_inbox_created ON inbox(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_inbox_status ON inbox(ai_status);
CREATE INDEX IF NOT EXISTS idx_inbox_requested ON inbox(ai_status, ai_requested, deleted_at);

CREATE TABLE IF NOT EXISTS tasks (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    title            TEXT NOT NULL,
    description      TEXT NOT NULL DEFAULT '',
    kind             TEXT NOT NULL DEFAULT 'task',
    status           TEXT NOT NULL DEFAULT 'todo',
    priority         TEXT NOT NULL DEFAULT 'P2',
    due_at           TEXT,
    estimate_minutes INTEGER,
    actual_minutes   INTEGER NOT NULL DEFAULT 0,
    progress         INTEGER NOT NULL DEFAULT 0,
    planned_start    TEXT,
    planned_end      TEXT,
    tags             TEXT NOT NULL DEFAULT '[]',
    source_inbox_id  INTEGER,
    source_snapshot  TEXT,
    is_archived      INTEGER NOT NULL DEFAULT 0,
    deleted_at       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE INDEX IF NOT EXISTS idx_tasks_due ON tasks(due_at);

CREATE TABLE IF NOT EXISTS subtasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    is_done    INTEGER NOT NULL DEFAULT 0,
    position   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_subtasks_task ON subtasks(task_id, position);

-- A separate relation preserves item-level idempotency while keeping the
-- existing tasks table/source metadata compatible with older databases.
CREATE TABLE IF NOT EXISTS inbox_promotions (
    inbox_id   INTEGER NOT NULL REFERENCES inbox(id) ON DELETE CASCADE,
    item_key   TEXT NOT NULL,
    task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (inbox_id, item_key)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_inbox_promotions_task
    ON inbox_promotions(task_id);
"#;

#[derive(Clone)]
pub struct Db {
    state: Arc<Mutex<Connection>>,
    snapshot_path: Arc<PathBuf>,
}

impl Db {
    /// Open the protected TaskVault store.
    ///
    /// `inbox_path` and `tasks_path` remain the legacy-compatible entry
    /// points used by the application. New state is kept in one in-memory
    /// SQLite database and persisted as an encrypted `tasks.vault` snapshot.
    pub fn open(inbox_path: &Path, tasks_path: &Path) -> Result<Self, String> {
        let snapshot_path = snapshot_path_for(tasks_path);
        if let Some(parent) = snapshot_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|e| format!("创建数据库目录失败: {e}"))?;
        }

        if snapshot_path.exists() {
            let mut conn = load_snapshot(&snapshot_path)?;
            let schema_changed = ensure_loaded_schema(&mut conn)?;
            let db = Self {
                state: Arc::new(Mutex::new(conn)),
                snapshot_path: Arc::new(snapshot_path),
            };

            if schema_changed {
                db.persist_current()?;
            }
            // A crash can leave a worker's lease in processing. Recover it
            // before exposing the queue to the rest of the application.
            db.recover_processing()?;
            // If a previous migration wrote the snapshot and crashed before
            // removing the old files, finish that cleanup on the next open.
            cleanup_legacy_files(inbox_path, tasks_path, &db.snapshot_path)?;
            return Ok(db);
        }

        let legacy_files = legacy_files(inbox_path, tasks_path, &snapshot_path);
        reject_orphan_sidecars(&legacy_files)?;
        let has_legacy = legacy_files.iter().any(|p| p.exists());

        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        configure_connection(&conn)?;
        initialize_schema(&conn)?;

        // Keep old connections open through archiving. The exclusive locks
        // close the copy-to-archive race: a writer cannot change a legacy
        // file after it was copied but before its archive is fixed.
        {
            let legacy_inbox = open_legacy_if_present(inbox_path, &snapshot_path)?;
            let legacy_tasks = open_legacy_if_present(tasks_path, &snapshot_path)?;
            if let Some(ref old) = legacy_inbox {
                validate_legacy(old, "inbox")?;
                checkpoint_legacy(old, "inbox")?;
            }
            if let Some(ref old) = legacy_tasks {
                validate_legacy(old, "tasks")?;
                checkpoint_legacy(old, "tasks")?;
            }

            if let Some(ref old) = legacy_inbox {
                copy_legacy_inbox(old, &conn)?;
                copy_legacy_sequences(old, &conn)?;
            }
            if let Some(ref old) = legacy_tasks {
                copy_legacy_tasks(old, &conn)?;
                copy_legacy_subtasks(old, &conn)?;
                copy_legacy_promotions(old, &conn)?;
                copy_legacy_sequences(old, &conn)?;
            }
            archive_legacy_files(&legacy_files)?;
        }

        validate_database(&conn, "consolidated database")?;
        let db = Self {
            state: Arc::new(Mutex::new(conn)),
            snapshot_path: Arc::new(snapshot_path),
        };

        // Legacy bytes were archived while their exclusive locks were held.
        // The encrypted snapshot is the canonical commit point; old
        // plaintext files are removed only after it succeeds.
        db.persist_current()?;
        // A legacy worker may have been interrupted while processing. Recover
        // it on the first protected open as well as on later restarts.
        db.recover_processing()?;
        if has_legacy {
            cleanup_legacy_files(inbox_path, tasks_path, &db.snapshot_path)?;
        }
        Ok(db)
    }

    pub fn insert_inbox(
        &self,
        raw_text: &str,
        source: &str,
        image_path: Option<&str>,
    ) -> Result<InboxItem, String> {
        let id = self.mutate(|conn| {
            let now = Local::now().to_rfc3339();
            conn.execute(
                "INSERT INTO inbox(raw_text, source, created_at, ai_status, image_path) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![raw_text, source, now, image_path],
            )
            .map_err(|e| e.to_string())?;
            Ok(conn.last_insert_rowid())
        })?;
        self.get_inbox(id)
    }

    pub fn get_inbox(&self, id: i64) -> Result<InboxItem, String> {
        let conn = self.lock_state()?;
        row_to_inbox(&conn, id).map_err(|e| e.to_string())
    }

    pub fn list_inbox(&self, include_deleted: bool) -> Result<Vec<InboxItem>, String> {
        let conn = self.lock_state()?;
        let sql = if include_deleted {
            "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model, is_promoted, deleted_at, image_path, promoted_keys, ai_generation, ai_requested FROM inbox ORDER BY id DESC"
        } else {
            "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model, is_promoted, deleted_at, image_path, promoted_keys, ai_generation, ai_requested FROM inbox WHERE deleted_at IS NULL ORDER BY id DESC"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], inbox_from_row)
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Broad manual retry list. Explicitly queued work is also included;
    /// active, completed and partial work is left to the caller's policy.
    pub fn pending_ai_ids(&self) -> Result<Vec<i64>, String> {
        self.ai_ids_for("ai_status IN ('pending', 'failed', 'skipped', 'unconfigured')")
    }

    /// Work explicitly requested by the user or automatic capture.
    pub fn queued_ai_ids(&self) -> Result<Vec<i64>, String> {
        let conn = self.lock_state()?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM inbox
                 WHERE ai_status = 'pending' AND ai_requested = 1 AND deleted_at IS NULL
                 ORDER BY id ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Alias used by queue recovery callers.
    pub fn unfinished_ai_ids(&self) -> Result<Vec<i64>, String> {
        self.queued_ai_ids()
    }

    /// Queue an AI request once. Repeated requests while it is queued or
    /// processing are no-ops and return false.
    pub fn request_ai(&self, id: i64) -> Result<bool, String> {
        self.mutate_dirty(|conn| {
            let row: Option<(String, Option<String>, bool)> = conn
                .query_row(
                    "SELECT ai_status, deleted_at, ai_requested != 0 FROM inbox WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((status, deleted_at, requested)) = row else {
                return Err("收件箱记录不存在".to_string());
            };
            if deleted_at.is_some() || status == "processing" || (status == "pending" && requested)
            {
                return Ok((false, false));
            }

            let changed = conn
                .execute(
                    "UPDATE inbox SET ai_status = 'pending', ai_requested = 1,
                            ai_result = NULL, ai_model = NULL WHERE id = ?1",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
            Ok((true, changed != 0))
        })
    }

    /// Claim one queued request and return its new generation fence.
    pub fn claim_ai(&self, id: i64) -> Result<Option<i64>, String> {
        self.mutate_dirty(|conn| {
            let changed = conn
                .execute(
                    "UPDATE inbox SET ai_status = 'processing',
                            ai_generation = ai_generation + 1,
                            ai_result = NULL, ai_model = NULL
                     WHERE id = ?1 AND ai_status = 'pending' AND ai_requested = 1
                       AND deleted_at IS NULL",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Ok((None, false));
            }
            let generation = conn
                .query_row(
                    "SELECT ai_generation FROM inbox WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| e.to_string())?;
            Ok((Some(generation), true))
        })
    }

    /// Complete only the currently active processing generation.
    pub fn complete_ai(
        &self,
        id: i64,
        generation: i64,
        status: &str,
        result_json: Option<&str>,
        model: Option<&str>,
    ) -> Result<bool, String> {
        validate_ai_status(status)?;
        self.mutate_dirty(|conn| {
            let changed = conn
                .execute(
                    "UPDATE inbox SET ai_status = ?3, ai_result = ?4, ai_model = ?5,
                            ai_requested = 0
                     WHERE id = ?1 AND ai_generation = ?2 AND ai_status = 'processing'
                       AND deleted_at IS NULL",
                    params![id, generation, status, result_json, model],
                )
                .map_err(|e| e.to_string())?;
            Ok((changed != 0, changed != 0))
        })
    }

    /// Compatibility wrapper for older callers. New workers should use
    /// request_ai/claim_ai/complete_ai so stale responses cannot win.
    pub fn set_ai_processing(&self, id: i64) -> Result<(), String> {
        let _ = self.request_ai(id)?;
        if self.claim_ai(id)?.is_some() {
            return Ok(());
        }
        let item = self.get_inbox(id)?;
        if item.ai_status == "processing" {
            Ok(())
        } else {
            Err("AI 记录无法进入 processing".to_string())
        }
    }

    /// Compatibility wrapper for legacy command paths.
    pub fn set_ai_result(
        &self,
        id: i64,
        status: &str,
        result_json: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), String> {
        validate_ai_status(status)?;
        self.mutate_dirty(|conn| {
            let changed = conn
                .execute(
                    "UPDATE inbox SET ai_status = ?2,
                            ai_result = COALESCE(?3, ai_result),
                            ai_model = COALESCE(?4, ai_model), ai_requested = 0
                     WHERE id = ?1",
                    params![id, status, result_json, model],
                )
                .map_err(|e| e.to_string())?;
            Ok(((), changed != 0))
        })
    }

    /// Convert stranded worker leases back to explicitly requested pending
    /// work. Incrementing the generation invalidates late completions.
    pub fn recover_processing(&self) -> Result<usize, String> {
        self.mutate_dirty(|conn| {
            let changed = conn
                .execute(
                    "UPDATE inbox SET ai_status = 'pending', ai_requested = 1,
                            ai_generation = ai_generation + 1
                     WHERE ai_status = 'processing'",
                    [],
                )
                .map_err(|e| e.to_string())?;
            Ok((changed, changed != 0))
        })
    }

    pub fn soft_delete_inbox(&self, id: i64) -> Result<(), String> {
        self.mutate_dirty(|conn| {
            let now = Local::now().to_rfc3339();
            let changed = conn
                .execute(
                    "UPDATE inbox SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                    params![id, now],
                )
                .map_err(|e| e.to_string())?;
            Ok(((), changed != 0))
        })
    }

    /// Legacy promotion marker. New promotion callers should use
    /// promote_inbox so task, subtasks and item marker share one transaction.
    pub fn mark_promoted(&self, id: i64) -> Result<(), String> {
        self.mutate_dirty(|conn| {
            let changed = conn
                .execute(
                    "UPDATE inbox SET is_promoted = 1 WHERE id = ?1 AND is_promoted = 0",
                    params![id],
                )
                .map_err(|e| e.to_string())?;
            Ok(((), changed != 0))
        })
    }

    pub fn create_task(
        &self,
        draft: &TaskDraft,
        source_inbox_id: Option<i64>,
        source_snapshot: Option<&str>,
    ) -> Result<Task, String> {
        validate_draft(draft)?;
        let id = self.mutate(|conn| insert_task(conn, draft, source_inbox_id, source_snapshot))?;
        self.get_task(id)
    }

    /// Atomically promote one AI item (or the manual/text item) and make a
    /// replay of the same `(inbox_id, item_key)` return the original task.
    pub fn promote_inbox(
        &self,
        inbox_id: i64,
        draft: &TaskDraft,
        item_key: &str,
    ) -> Result<Task, String> {
        validate_draft(draft)?;
        let item_key = item_key.trim();
        if item_key.is_empty() {
            return Err("转正项标识不能为空".to_string());
        }

        let task_id = self.mutate_dirty(|conn| {
            let row: Option<(String, Option<String>, Option<String>, String)> = conn
                .query_row(
                    "SELECT raw_text, deleted_at, ai_result, promoted_keys
                     FROM inbox WHERE id = ?1",
                    params![inbox_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let Some((raw_text, deleted_at, ai_result, promoted_json)) = row else {
                return Err("收件箱记录不存在".to_string());
            };
            if deleted_at.is_some() {
                return Err("该记录已删除".to_string());
            }

            let valid_keys = item_keys_for(ai_result.as_deref());
            if !valid_keys.iter().any(|key| key == item_key) {
                return Err("转正项标识已失效，请刷新后重试".to_string());
            }

            let mut existing: Option<i64> = conn
                .query_row(
                    "SELECT task_id FROM inbox_promotions
                     WHERE inbox_id = ?1 AND item_key = ?2",
                    params![inbox_id, item_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            // A pre-AI manual promotion used the legacy implicit key. Treat
            // it as the same single text item when the AI result later gives
            // that record the explicit `text` key (and vice versa).
            if existing.is_none() && matches!(item_key, "manual" | "text") {
                existing = conn
                    .query_row(
                        "SELECT task_id FROM inbox_promotions
                         WHERE inbox_id = ?1 AND item_key IN ('manual', 'text')
                         ORDER BY item_key LIMIT 1",
                        params![inbox_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
            }

            let mut promoted_keys = parse_string_list(&promoted_json);
            let mut dirty = false;
            if !promoted_keys.iter().any(|key| key == item_key) {
                promoted_keys.push(item_key.to_string());
                let json = serde_json::to_string(&promoted_keys).map_err(|e| e.to_string())?;
                conn.execute(
                    "UPDATE inbox SET is_promoted = 1, promoted_keys = ?2 WHERE id = ?1",
                    params![inbox_id, json],
                )
                .map_err(|e| e.to_string())?;
                dirty = true;
            }

            if let Some(task_id) = existing {
                return Ok((task_id, dirty));
            }

            let task_id = insert_task(conn, draft, Some(inbox_id), Some(&raw_text))?;
            conn.execute(
                "INSERT INTO inbox_promotions(inbox_id, item_key, task_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![inbox_id, item_key, task_id, Local::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
            Ok((task_id, true))
        })?;
        self.get_task(task_id)
    }

    pub fn get_task(&self, id: i64) -> Result<Task, String> {
        let conn = self.lock_state()?;
        let mut task = task_by_id(&conn, id).map_err(|e| e.to_string())?;
        attach_subtasks_conn(&conn, std::slice::from_mut(&mut task))?;
        Ok(task)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, String> {
        let conn = self.lock_state()?;
        let mut tasks = query_tasks_conn(&conn, "deleted_at IS NULL AND is_archived = 0")?;
        attach_subtasks_conn(&conn, &mut tasks)?;
        Ok(tasks)
    }

    pub fn list_archived_tasks(&self) -> Result<Vec<Task>, String> {
        let conn = self.lock_state()?;
        let mut tasks = query_tasks_conn(&conn, "deleted_at IS NULL AND is_archived = 1")?;
        attach_subtasks_conn(&conn, &mut tasks)?;
        Ok(tasks)
    }

    /// One query fetches all requested task subtasks, avoiding list-page N+1.
    pub fn subtasks_for_tasks(
        &self,
        task_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<Subtask>>, String> {
        let conn = self.lock_state()?;
        subtasks_for_tasks_conn(&conn, task_ids)
    }

    pub fn get_subtask(&self, id: i64) -> Result<Subtask, String> {
        let conn = self.lock_state()?;
        conn.query_row(
            "SELECT id, task_id, text, is_done, position FROM subtasks WHERE id = ?1",
            params![id],
            subtask_from_row,
        )
        .map_err(|e| e.to_string())
    }

    pub fn add_subtask(&self, task_id: i64, text: &str) -> Result<Subtask, String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("子任务内容不能为空".to_string());
        }
        let id = self.mutate(|conn| {
            let active: Option<i64> = conn
                .query_row(
                    "SELECT id FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
                    params![task_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if active.is_none() {
                return Err("任务不存在或已删除".to_string());
            }
            let position: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(position), -1) + 1 FROM subtasks WHERE task_id = ?1",
                    params![task_id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO subtasks(task_id, text, position, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task_id, text, position, Local::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
            Ok(conn.last_insert_rowid())
        })?;
        self.get_subtask(id)
    }

    pub fn set_subtask_done(&self, id: i64, done: bool) -> Result<Subtask, String> {
        self.mutate(|conn| {
            let changed = conn
                .execute(
                    "UPDATE subtasks SET is_done = ?2 WHERE id = ?1",
                    params![id, if done { 1 } else { 0 }],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("子任务不存在".to_string());
            }
            Ok(())
        })?;
        self.get_subtask(id)
    }

    pub fn delete_subtask(&self, id: i64) -> Result<(), String> {
        self.mutate(|conn| {
            let changed = conn
                .execute("DELETE FROM subtasks WHERE id = ?1", params![id])
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("子任务不存在".to_string());
            }
            Ok(())
        })
    }

    pub fn set_task_priority(&self, id: i64, priority: &str) -> Result<Task, String> {
        if !matches!(priority, "P0" | "P1" | "P2" | "P3") {
            return Err("优先级只能是 P0/P1/P2/P3".to_string());
        }
        self.mutate(|conn| {
            let changed = conn
                .execute(
                    "UPDATE tasks SET priority = ?2, updated_at = ?3
                     WHERE id = ?1 AND deleted_at IS NULL",
                    params![id, priority, Local::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("任务不存在或已删除".to_string());
            }
            Ok(())
        })?;
        self.get_task(id)
    }

    pub fn set_task_archived(&self, id: i64, archived: bool) -> Result<Task, String> {
        self.mutate(|conn| {
            let changed = conn
                .execute(
                    "UPDATE tasks SET is_archived = ?2, updated_at = ?3
                     WHERE id = ?1 AND deleted_at IS NULL",
                    params![id, if archived { 1 } else { 0 }, Local::now().to_rfc3339()],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("任务不存在或已删除".to_string());
            }
            Ok(())
        })?;
        self.get_task(id)
    }

    pub fn delete_task(&self, id: i64) -> Result<(), String> {
        self.mutate(|conn| {
            let now = Local::now().to_rfc3339();
            let changed = conn
                .execute(
                    "UPDATE tasks SET deleted_at = ?2, updated_at = ?2
                     WHERE id = ?1 AND deleted_at IS NULL",
                    params![id, now],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("任务不存在或已删除".to_string());
            }
            Ok(())
        })
    }

    pub fn update_task(&self, task: &Task) -> Result<Task, String> {
        validate_task(task)?;
        self.mutate(|conn| {
            let tags = serde_json::to_string(&task.tags).map_err(|e| e.to_string())?;
            let changed = conn
                .execute(
                    "UPDATE tasks SET title = ?2, description = ?3, kind = ?4,
                            status = ?5, priority = ?6, due_at = ?7,
                            estimate_minutes = ?8, actual_minutes = ?9, progress = ?10,
                            planned_start = ?11, planned_end = ?12, tags = ?13,
                            updated_at = ?14
                     WHERE id = ?1 AND deleted_at IS NULL",
                    params![
                        task.id,
                        task.title,
                        task.description,
                        task.kind,
                        task.status,
                        task.priority,
                        task.due_at,
                        task.estimate_minutes,
                        task.actual_minutes,
                        task.progress,
                        task.planned_start,
                        task.planned_end,
                        tags,
                        Local::now().to_rfc3339(),
                    ],
                )
                .map_err(|e| e.to_string())?;
            if changed == 0 {
                return Err("任务不存在或已删除".to_string());
            }
            Ok(())
        })?;
        self.get_task(task.id)
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.state.lock().map_err(|_| "数据库锁已损坏".to_string())
    }

    fn persist_current(&self) -> Result<(), String> {
        let conn = self.lock_state()?;
        persist_connection(&conn, &self.snapshot_path)
    }

    fn mutate<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, String>,
    {
        self.mutate_dirty(|conn| operation(conn).map(|value| (value, true)))
    }

    fn mutate_dirty<T, F>(&self, operation: F) -> Result<T, String>
    where
        F: FnOnce(&Transaction<'_>) -> Result<(T, bool), String>,
    {
        let mut conn = self.lock_state()?;
        let before = serialize_connection(&conn)?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let (value, dirty) = match operation(&tx) {
            Ok(value) => value,
            Err(error) => {
                drop(tx);
                return Err(error);
            }
        };
        if let Err(commit_error) = tx.commit() {
            let restore_error = restore_connection(&mut conn, &before);
            return match restore_error {
                Ok(()) => Err(format!("数据库事务提交失败，变更已回滚: {commit_error}")),
                Err(error) => Err(format!(
                    "数据库事务提交失败，且内存回滚失败: {commit_error}; {error}"
                )),
            };
        }
        if !dirty {
            return Ok(value);
        }

        if let Err(persist_error) = persist_connection(&conn, &self.snapshot_path) {
            let restore_error = restore_connection(&mut conn, &before);
            return match restore_error {
                Ok(()) => Err(format!("数据库快照写入失败，变更已回滚: {persist_error}")),
                Err(error) => Err(format!(
                    "数据库快照写入失败，且内存回滚失败: {persist_error}; {error}"
                )),
            };
        }
        Ok(value)
    }

    fn ai_ids_for(&self, predicate: &str) -> Result<Vec<i64>, String> {
        let conn = self.lock_state()?;
        let sql = format!(
            "SELECT id FROM inbox WHERE {predicate} AND deleted_at IS NULL ORDER BY id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }
}

fn snapshot_path_for(tasks_path: &Path) -> PathBuf {
    if tasks_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(SNAPSHOT_NAME))
    {
        tasks_path.to_path_buf()
    } else {
        tasks_path.with_file_name(SNAPSHOT_NAME)
    }
}

fn configure_connection(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn initialize_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    // These calls make initialization robust if SCHEMA is later reduced for
    // a compatibility build.
    ensure_column(conn, "inbox", "image_path", "TEXT")?;
    ensure_column(conn, "inbox", "promoted_keys", "TEXT NOT NULL DEFAULT '[]'")?;
    ensure_column(conn, "inbox", "ai_generation", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "inbox", "ai_requested", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "tasks", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "tasks", "deleted_at", "TEXT")?;
    Ok(())
}

fn ensure_loaded_schema(conn: &mut Connection) -> Result<bool, String> {
    for table in ["inbox", "tasks", "subtasks"] {
        if !table_exists(conn, table)? {
            return Err(format!("数据库快照缺少核心表: {table}"));
        }
    }
    let mut changed = false;
    changed |= ensure_column(conn, "inbox", "image_path", "TEXT")?;
    changed |= ensure_column(conn, "inbox", "is_promoted", "INTEGER NOT NULL DEFAULT 0")?;
    changed |= ensure_column(conn, "inbox", "promoted_keys", "TEXT NOT NULL DEFAULT '[]'")?;
    changed |= ensure_column(conn, "inbox", "ai_generation", "INTEGER NOT NULL DEFAULT 0")?;
    changed |= ensure_column(conn, "inbox", "ai_requested", "INTEGER NOT NULL DEFAULT 0")?;
    changed |= ensure_column(conn, "tasks", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;
    changed |= ensure_column(conn, "tasks", "deleted_at", "TEXT")?;
    if !table_exists(conn, "inbox_promotions")? {
        conn.execute_batch(
            "CREATE TABLE inbox_promotions (
                 inbox_id INTEGER NOT NULL REFERENCES inbox(id) ON DELETE CASCADE,
                 item_key TEXT NOT NULL,
                 task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                 created_at TEXT NOT NULL,
                 PRIMARY KEY (inbox_id, item_key)
             );
             CREATE UNIQUE INDEX idx_inbox_promotions_task ON inbox_promotions(task_id);",
        )
        .map_err(|e| e.to_string())?;
        changed = true;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_inbox_requested
             ON inbox(ai_status, ai_requested, deleted_at);",
    )
    .map_err(|e| e.to_string())?;
    Ok(changed)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_row| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(|e| e.to_string())
}

fn table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row.map_err(|e| e.to_string())?);
    }
    Ok(columns)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<bool, String> {
    let columns = table_columns(conn, table)?;
    if columns.contains(column) {
        return Ok(false);
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"))
        .map_err(|e| e.to_string())?;
    Ok(true)
}

fn serialize_connection(conn: &Connection) -> Result<Vec<u8>, String> {
    conn.serialize(rusqlite::MAIN_DB)
        .map(|data| data.to_vec())
        .map_err(|e| e.to_string())
}

fn restore_connection(conn: &mut Connection, raw: &[u8]) -> Result<(), String> {
    conn.deserialize_read_exact(rusqlite::MAIN_DB, raw, raw.len(), false)
        .map_err(|e| e.to_string())?;
    configure_connection(conn)
}

fn load_snapshot(path: &Path) -> Result<Connection, String> {
    let protected = fs::read(path).map_err(|e| format!("读取数据库快照失败: {e}"))?;
    if protected.is_empty() {
        return Err("数据库快照为空".to_string());
    }
    let raw =
        crate::storage::unprotect(&protected).map_err(|e| format!("解密数据库快照失败: {e}"))?;
    if raw.is_empty() {
        return Err("数据库快照解密后为空".to_string());
    }
    let mut conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
    conn.deserialize_read_exact(rusqlite::MAIN_DB, raw.as_slice(), raw.len(), false)
        .map_err(|e| format!("加载数据库快照失败: {e}"))?;
    configure_connection(&conn)?;
    validate_database(&conn, "数据库快照")?;
    Ok(conn)
}

fn persist_connection(conn: &Connection, path: &Path) -> Result<(), String> {
    let raw = serialize_connection(conn)?;
    let protected =
        crate::storage::protect(&raw).map_err(|e| format!("保护数据库快照失败: {e}"))?;
    // Validate the envelope before the atomic replacement. `atomic_write` is
    // the sole commit point; after it succeeds there must be no fallible
    // verification or best-effort rollback that could split disk and memory.
    let roundtrip = crate::storage::unprotect(&protected)
        .map_err(|e| format!("校验数据库快照加密往返失败: {e}"))?;
    if roundtrip != raw {
        return Err("数据库快照加密往返校验失败：内容不一致".to_string());
    }
    crate::storage::atomic_write(path, &protected).map_err(|e| format!("写入数据库快照失败: {e}"))
}

fn validate_database(conn: &Connection, label: &str) -> Result<(), String> {
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| format!("{label}完整性检查失败: {e}"))?;
    if !check.eq_ignore_ascii_case("ok") {
        return Err(format!("{label}完整性检查未通过: {check}"));
    }
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| format!("{label}外键检查失败: {e}"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("{label}外键检查失败: {e}"))?;
    if rows
        .next()
        .map_err(|e| format!("{label}外键检查失败: {e}"))?
        .is_some()
    {
        return Err(format!("{label}存在外键不一致"));
    }
    Ok(())
}

fn open_legacy_if_present(path: &Path, snapshot_path: &Path) -> Result<Option<Connection>, String> {
    if path == snapshot_path || !path.exists() {
        return Ok(None);
    }
    let conn =
        Connection::open(path).map_err(|e| format!("打开旧数据库失败 {}: {e}", path.display()))?;
    // Hold an exclusive SQLite lock for the read/checkpoint window. This
    // makes a concurrent writer fail closed instead of letting migration copy
    // a moving database.
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
        .map_err(|e| format!("锁定旧数据库失败 {}: {e}", path.display()))?;
    configure_connection(&conn)?;
    Ok(Some(conn))
}

fn validate_legacy(conn: &Connection, label: &str) -> Result<(), String> {
    validate_database(conn, &format!("旧{label}数据库"))
}

fn checkpoint_legacy(conn: &Connection, label: &str) -> Result<(), String> {
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .map_err(|e| format!("旧{label}数据库 WAL checkpoint 失败: {e}"))?;
    if busy != 0 {
        return Err(format!(
            "旧{label}数据库仍被其他进程占用，无法完成 WAL checkpoint"
        ));
    }
    Ok(())
}

fn legacy_files(inbox_path: &Path, tasks_path: &Path, snapshot_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for base in [inbox_path, tasks_path] {
        if base == snapshot_path {
            continue;
        }
        for path in [
            base.to_path_buf(),
            sidecar_path(base, "-wal"),
            sidecar_path(base, "-shm"),
        ] {
            if !paths.iter().any(|existing| existing == &path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn reject_orphan_sidecars(paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        if !path.exists() {
            continue;
        }
        let path_string = path.to_string_lossy();
        if !path_string.ends_with("-wal") && !path_string.ends_with("-shm") {
            continue;
        }
        let suffix = if path_string.ends_with("-wal") {
            "-wal"
        } else {
            "-shm"
        };
        let base = path_string.trim_end_matches(suffix);
        if !Path::new(base).exists() {
            return Err(format!(
                "发现没有主文件的旧数据库 sidecar: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn sidecar_path(base: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", base.display(), suffix))
}

fn archive_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.vault", path.display()))
}

fn cleanup_legacy_files(
    inbox_path: &Path,
    tasks_path: &Path,
    snapshot_path: &Path,
) -> Result<(), String> {
    let paths = legacy_files(inbox_path, tasks_path, snapshot_path);
    for path in paths {
        remove_archived_legacy_file(&path)?;
    }
    Ok(())
}

fn archive_legacy_files(paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        archive_legacy_file(path)?;
    }
    Ok(())
}

fn archive_legacy_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let raw =
        fs::read(path).map_err(|e| format!("读取旧数据库文件失败 {}: {e}", path.display()))?;
    let archive = archive_path(path);
    if archive.exists() {
        let protected = fs::read(&archive)
            .map_err(|e| format!("读取旧数据库加密归档失败 {}: {e}", archive.display()))?;
        verify_legacy_archive(&protected, &raw, &archive, path)?;
    } else {
        let protected = protect_legacy_bytes(&raw)
            .map_err(|e| format!("保护旧数据库归档失败 {}: {e}", path.display()))?;
        crate::storage::atomic_write(&archive, &protected)
            .map_err(|e| format!("写入旧数据库归档失败 {}: {e}", archive.display()))?;
        let verify = fs::read(&archive)
            .map_err(|e| format!("读取旧数据库归档校验失败 {}: {e}", archive.display()))?;
        verify_legacy_archive(&verify, &raw, &archive, path)?;
    }
    Ok(())
}

fn remove_archived_legacy_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    // Keep a read/delete-sharing handle from comparison through removal. It
    // blocks a concurrent writer and lets Windows remove the file safely.
    #[cfg(windows)]
    let mut legacy_file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .open(path)
        .map_err(|e| format!("打开旧数据库文件失败 {}: {e}", path.display()))?;
    #[cfg(windows)]
    let raw = {
        let mut raw = Vec::new();
        legacy_file
            .read_to_end(&mut raw)
            .map_err(|e| format!("读取旧数据库文件失败 {}: {e}", path.display()))?;
        raw
    };
    #[cfg(not(windows))]
    let raw =
        fs::read(path).map_err(|e| format!("读取旧数据库文件失败 {}: {e}", path.display()))?;
    let archive = archive_path(path);
    if !archive.exists() {
        return Err(format!(
            "旧数据库文件没有既有加密归档，拒绝删除: {}",
            path.display()
        ));
    }
    let protected = fs::read(&archive)
        .map_err(|e| format!("读取旧数据库加密归档失败 {}: {e}", archive.display()))?;
    verify_legacy_archive(&protected, &raw, &archive, path)?;

    fs::remove_file(path).map_err(|e| format!("删除旧数据库明文失败 {}: {e}", path.display()))?;
    if path.exists() {
        return Err(format!("旧数据库明文仍存在: {}", path.display()));
    }
    Ok(())
}

fn protect_legacy_bytes(raw: &[u8]) -> Result<Vec<u8>, String> {
    // DPAPI rejects a zero-length input. An empty WAL/SHM sidecar still gets
    // an authenticated archive so cleanup can require a matching snapshot.
    let payload = if raw.is_empty() {
        EMPTY_LEGACY_ARCHIVE_MARKER
    } else {
        raw
    };
    crate::storage::protect(payload)
}

fn verify_legacy_archive(
    protected: &[u8],
    raw: &[u8],
    archive: &Path,
    source: &Path,
) -> Result<(), String> {
    let archived = crate::storage::unprotect(protected)
        .map_err(|e| format!("解密旧数据库归档失败 {}: {e}", archive.display()))?;
    let matches =
        archived == raw || (raw.is_empty() && archived.as_slice() == EMPTY_LEGACY_ARCHIVE_MARKER);
    if !matches {
        return Err(format!(
            "旧数据库归档与当前文件不一致: {}",
            source.display()
        ));
    }
    Ok(())
}

fn legacy_expr(columns: &HashSet<String>, column: &str, fallback: &str) -> String {
    if columns.contains(column) {
        format!("COALESCE({column}, {fallback})")
    } else {
        fallback.to_string()
    }
}

fn legacy_nullable_expr(columns: &HashSet<String>, column: &str) -> String {
    if columns.contains(column) {
        column.to_string()
    } else {
        "NULL".to_string()
    }
}

fn copy_legacy_inbox(source: &Connection, target: &Connection) -> Result<(), String> {
    if !table_exists(source, "inbox")? {
        return Ok(());
    }
    let columns = table_columns(source, "inbox")?;
    let sql = format!(
        "SELECT id,
                {}, {}, {}, {},
                {}, {}, {}, {}, {},
                {}, {}, {}
         FROM inbox ORDER BY id",
        legacy_expr(&columns, "raw_text", "''"),
        legacy_expr(&columns, "source", "'hotkey'"),
        legacy_expr(&columns, "created_at", "''"),
        legacy_expr(&columns, "ai_status", "'pending'"),
        legacy_nullable_expr(&columns, "ai_result"),
        legacy_nullable_expr(&columns, "ai_model"),
        legacy_expr(&columns, "is_promoted", "0"),
        legacy_nullable_expr(&columns, "deleted_at"),
        legacy_nullable_expr(&columns, "image_path"),
        legacy_expr(&columns, "promoted_keys", "'[]'"),
        legacy_expr(&columns, "ai_generation", "0"),
        legacy_expr(&columns, "ai_requested", "0"),
    );
    let mut stmt = source.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (
            id,
            raw_text,
            source_name,
            created_at,
            status,
            result,
            model,
            promoted,
            deleted,
            image,
            promoted_keys,
            generation,
            requested,
        ) = row.map_err(|e| e.to_string())?;
        target
            .execute(
                "INSERT OR REPLACE INTO inbox(
                    id, raw_text, source, created_at, ai_status, ai_result, ai_model,
                    is_promoted, deleted_at, image_path, promoted_keys, ai_generation, ai_requested
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id,
                    raw_text,
                    source_name,
                    created_at,
                    status,
                    result,
                    model,
                    promoted,
                    deleted,
                    image,
                    promoted_keys,
                    generation,
                    requested,
                ],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn copy_legacy_tasks(source: &Connection, target: &Connection) -> Result<(), String> {
    if !table_exists(source, "tasks")? {
        return Ok(());
    }
    let columns = table_columns(source, "tasks")?;
    let sql = format!(
        "SELECT id, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}
         FROM tasks ORDER BY id",
        legacy_expr(&columns, "title", "''"),
        legacy_expr(&columns, "description", "''"),
        legacy_expr(&columns, "kind", "'task'"),
        legacy_expr(&columns, "status", "'todo'"),
        legacy_expr(&columns, "priority", "'P2'"),
        legacy_nullable_expr(&columns, "due_at"),
        legacy_nullable_expr(&columns, "estimate_minutes"),
        legacy_expr(&columns, "actual_minutes", "0"),
        legacy_expr(&columns, "progress", "0"),
        legacy_nullable_expr(&columns, "planned_start"),
        legacy_nullable_expr(&columns, "planned_end"),
        legacy_expr(&columns, "tags", "'[]'"),
        legacy_nullable_expr(&columns, "source_inbox_id"),
        legacy_nullable_expr(&columns, "source_snapshot"),
        legacy_expr(&columns, "is_archived", "0"),
        legacy_nullable_expr(&columns, "deleted_at"),
        legacy_expr(&columns, "created_at", "''"),
        legacy_expr(&columns, "updated_at", "''"),
    );
    let mut stmt = source.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, i64>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, String>(17)?,
                row.get::<_, String>(18)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (
            id,
            title,
            description,
            kind,
            status,
            priority,
            due_at,
            estimate_minutes,
            actual_minutes,
            progress,
            planned_start,
            planned_end,
            tags,
            source_inbox_id,
            source_snapshot,
            is_archived,
            deleted_at,
            created_at,
            updated_at,
        ) = row.map_err(|e| e.to_string())?;
        target
            .execute(
                "INSERT OR REPLACE INTO tasks(
                    id, title, description, kind, status, priority, due_at, estimate_minutes,
                    actual_minutes, progress, planned_start, planned_end, tags,
                    source_inbox_id, source_snapshot, is_archived, deleted_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                params![
                    id,
                    title,
                    description,
                    kind,
                    status,
                    priority,
                    due_at,
                    estimate_minutes,
                    actual_minutes,
                    progress,
                    planned_start,
                    planned_end,
                    tags,
                    source_inbox_id,
                    source_snapshot,
                    is_archived,
                    deleted_at,
                    created_at,
                    updated_at,
                ],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn copy_legacy_subtasks(source: &Connection, target: &Connection) -> Result<(), String> {
    if !table_exists(source, "subtasks")? {
        return Ok(());
    }
    let columns = table_columns(source, "subtasks")?;
    let sql = format!(
        "SELECT id, {}, {}, {}, {}, {} FROM subtasks ORDER BY id",
        legacy_expr(&columns, "task_id", "0"),
        legacy_expr(&columns, "text", "''"),
        legacy_expr(&columns, "is_done", "0"),
        legacy_expr(&columns, "position", "0"),
        legacy_expr(&columns, "created_at", "''"),
    );
    let mut stmt = source.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (id, task_id, text, is_done, position, created_at) = row.map_err(|e| e.to_string())?;
        target
            .execute(
                "INSERT OR REPLACE INTO subtasks(id, task_id, text, is_done, position, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, task_id, text, is_done, position, created_at],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn copy_legacy_sequences(source: &Connection, target: &Connection) -> Result<(), String> {
    if !table_exists(source, "sqlite_sequence")? {
        return Ok(());
    }
    let mut stmt = source
        .prepare(
            "SELECT name, seq FROM sqlite_sequence
             WHERE name IN ('inbox', 'tasks', 'subtasks')",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (name, sequence) = row.map_err(|e| e.to_string())?;
        let current: Option<i64> = target
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if current.map_or(true, |current| sequence > current) {
            if current.is_some() {
                target
                    .execute(
                        "UPDATE sqlite_sequence SET seq = ?2 WHERE name = ?1",
                        params![name, sequence],
                    )
                    .map_err(|e| e.to_string())?;
            } else {
                target
                    .execute(
                        "INSERT INTO sqlite_sequence(name, seq) VALUES (?1, ?2)",
                        params![name, sequence],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn copy_legacy_promotions(source: &Connection, target: &Connection) -> Result<(), String> {
    if !table_exists(source, "inbox_promotions")? {
        return Ok(());
    }
    let columns = table_columns(source, "inbox_promotions")?;
    if !(columns.contains("inbox_id")
        && columns.contains("item_key")
        && columns.contains("task_id"))
    {
        return Ok(());
    }
    let created = legacy_expr(&columns, "created_at", "''");
    let mut stmt = source
        .prepare(&format!(
            "SELECT inbox_id, item_key, task_id, {created} FROM inbox_promotions"
        ))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (inbox_id, item_key, task_id, created_at) = row.map_err(|e| e.to_string())?;
        target
            .execute(
                "INSERT OR IGNORE INTO inbox_promotions(inbox_id, item_key, task_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![inbox_id, item_key, task_id, created_at],
            )
            .map_err(|e| e.to_string())?;
        let existing: String = target
            .query_row(
                "SELECT promoted_keys FROM inbox WHERE id = ?1",
                params![inbox_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let mut keys = parse_string_list(&existing);
        if !keys.iter().any(|key| key == &item_key) {
            keys.push(item_key.clone());
            let json = serde_json::to_string(&keys).map_err(|e| e.to_string())?;
            target
                .execute(
                    "UPDATE inbox SET is_promoted = 1, promoted_keys = ?2 WHERE id = ?1",
                    params![inbox_id, json],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn insert_task(
    conn: &Connection,
    draft: &TaskDraft,
    source_inbox_id: Option<i64>,
    source_snapshot: Option<&str>,
) -> Result<i64, String> {
    let now = Local::now().to_rfc3339();
    let tags = serde_json::to_string(&draft.tags).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO tasks(
            title, description, kind, status, priority, due_at, estimate_minutes,
            planned_start, planned_end, tags, source_inbox_id, source_snapshot, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
        params![
            draft.title,
            draft.description,
            draft.kind,
            draft.status,
            draft.priority,
            draft.due_at,
            draft.estimate_minutes,
            draft.planned_start,
            draft.planned_end,
            tags,
            source_inbox_id,
            source_snapshot,
            now,
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();
    insert_subtasks(conn, id, &draft.subtasks)?;
    Ok(id)
}

fn insert_subtasks(conn: &Connection, task_id: i64, texts: &[String]) -> Result<(), String> {
    for (position, text) in texts.iter().enumerate() {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT INTO subtasks(task_id, text, position, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, text, position as i64, Local::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn task_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Task> {
    conn.query_row(
        "SELECT id, title, description, kind, status, priority, due_at, estimate_minutes,
                actual_minutes, progress, planned_start, planned_end, tags,
                source_inbox_id, source_snapshot, is_archived, deleted_at, created_at, updated_at
         FROM tasks WHERE id = ?1",
        params![id],
        task_from_row,
    )
}

fn query_tasks_conn(conn: &Connection, filter: &str) -> Result<Vec<Task>, String> {
    let sql = format!(
        "SELECT id, title, description, kind, status, priority, due_at, estimate_minutes,
                actual_minutes, progress, planned_start, planned_end, tags,
                source_inbox_id, source_snapshot, is_archived, deleted_at, created_at, updated_at
         FROM tasks WHERE {filter} ORDER BY
            CASE status WHEN 'todo' THEN 0 WHEN 'in_progress' THEN 1 WHEN 'blocked' THEN 2 ELSE 3 END,
            CASE priority WHEN 'P0' THEN 0 WHEN 'P1' THEN 1 WHEN 'P2' THEN 2 ELSE 3 END,
            COALESCE(due_at, '9999') ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], task_from_row)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn attach_subtasks_conn(conn: &Connection, tasks: &mut [Task]) -> Result<(), String> {
    let ids: Vec<i64> = tasks.iter().map(|task| task.id).collect();
    let map = subtasks_for_tasks_conn(conn, &ids)?;
    for task in tasks {
        task.subtasks = map.get(&task.id).cloned().unwrap_or_default();
    }
    Ok(())
}

fn subtasks_for_tasks_conn(
    conn: &Connection,
    task_ids: &[i64],
) -> Result<HashMap<i64, Vec<Subtask>>, String> {
    let mut out = HashMap::new();
    if task_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = task_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, task_id, text, is_done, position FROM subtasks
         WHERE task_id IN ({placeholders}) ORDER BY position, id"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(task_ids.iter()), subtask_from_row)
        .map_err(|e| e.to_string())?;
    for row in rows {
        let subtask = row.map_err(|e| e.to_string())?;
        out.entry(subtask.task_id)
            .or_insert_with(Vec::new)
            .push(subtask);
    }
    Ok(out)
}

fn row_to_inbox(conn: &Connection, id: i64) -> rusqlite::Result<InboxItem> {
    conn.query_row(
        "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model,
                is_promoted, deleted_at, image_path, promoted_keys, ai_generation, ai_requested
         FROM inbox WHERE id = ?1",
        params![id],
        inbox_from_row,
    )
}

fn inbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    let ai_result: Option<String> = row.get(5)?;
    let promoted_keys_raw: String = row.get(10)?;
    let promoted_keys = parse_string_list(&promoted_keys_raw);
    Ok(InboxItem {
        id: row.get(0)?,
        raw_text: row.get(1)?,
        source: row.get(2)?,
        created_at: row.get(3)?,
        ai_status: row.get(4)?,
        ai_result: ai_result.clone(),
        ai_model: row.get(6)?,
        is_promoted: row.get::<_, i64>(7)? != 0 || !promoted_keys.is_empty(),
        deleted_at: row.get(8)?,
        image_path: row.get(9)?,
        promoted_keys,
        item_keys: item_keys_for(ai_result.as_deref()),
        ai_generation: row.get(11)?,
        ai_requested: row.get::<_, i64>(12)? != 0,
    })
}

fn subtask_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subtask> {
    Ok(Subtask {
        id: row.get(0)?,
        task_id: row.get(1)?,
        text: row.get(2)?,
        is_done: row.get::<_, i64>(3)? != 0,
        position: row.get(4)?,
    })
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let tags_raw: String = row.get(12)?;
    let tags = serde_json::from_str(&tags_raw).unwrap_or_default();
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        kind: row.get(3)?,
        status: row.get(4)?,
        priority: row.get(5)?,
        due_at: row.get(6)?,
        estimate_minutes: row.get(7)?,
        actual_minutes: row.get(8)?,
        progress: row.get(9)?,
        planned_start: row.get(10)?,
        planned_end: row.get(11)?,
        tags,
        source_inbox_id: row.get(13)?,
        source_snapshot: row.get(14)?,
        is_archived: row.get::<_, i64>(15)? != 0,
        deleted_at: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        subtasks: Vec::new(),
    })
}

fn parse_string_list(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    serde_json::from_str::<Vec<String>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

/// Return stable keys for the current AI payload. Serde's fixed struct field
/// order is the canonical JSON representation for an item, so edits to an
/// unrelated item's order do not make an existing item look new.
fn item_keys_for(ai_result: Option<&str>) -> Vec<String> {
    let Some(raw) = ai_result else {
        return vec!["manual".to_string()];
    };
    let Ok(parsed) = serde_json::from_str::<AiParsed>(raw) else {
        return vec!["manual".to_string()];
    };
    if parsed.items.is_empty() {
        return vec!["text".to_string()];
    }
    parsed.items.iter().map(item_key).collect()
}

fn item_key(item: &AiParsed) -> String {
    // Only fields that become part of the editable task participate in the
    // key. Confidence, summary, missing-info and truncation metadata can
    // change on a later parse without representing a new user-visible item.
    #[derive(serde::Serialize)]
    struct ItemKeyPayload<'a> {
        kind: &'a str,
        title: &'a str,
        description: &'a str,
        priority: &'a str,
        deadline: Option<&'a str>,
        estimate_minutes: Option<i64>,
        tags: &'a [String],
        subtasks: &'a [String],
    }
    let canonical = serde_json::to_vec(&ItemKeyPayload {
        kind: &item.kind,
        title: &item.title,
        description: &item.description,
        priority: &item.priority,
        deadline: item.deadline.as_deref(),
        estimate_minutes: item.estimate_minutes,
        tags: &item.tags,
        subtasks: &item.subtasks,
    })
    .unwrap_or_default();
    let digest = Sha256::digest(canonical);
    format!("{digest:x}")
}

fn validate_ai_status(status: &str) -> Result<(), String> {
    if matches!(
        status,
        "pending" | "processing" | "done" | "partial" | "failed" | "skipped" | "unconfigured"
    ) {
        Ok(())
    } else {
        Err(format!("未知 AI 状态: {status}"))
    }
}

fn validate_draft(draft: &TaskDraft) -> Result<(), String> {
    if draft.title.trim().is_empty() {
        return Err("任务标题不能为空".to_string());
    }
    validate_task_values(
        &draft.kind,
        &draft.status,
        &draft.priority,
        draft.due_at.as_deref(),
        draft.estimate_minutes,
        0,
        0,
        draft.planned_start.as_deref(),
        draft.planned_end.as_deref(),
    )
}

fn validate_task(task: &Task) -> Result<(), String> {
    if task.title.trim().is_empty() {
        return Err("任务标题不能为空".to_string());
    }
    validate_task_values(
        &task.kind,
        &task.status,
        &task.priority,
        task.due_at.as_deref(),
        task.estimate_minutes,
        task.actual_minutes,
        task.progress,
        task.planned_start.as_deref(),
        task.planned_end.as_deref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_task_values(
    kind: &str,
    status: &str,
    priority: &str,
    _due_at: Option<&str>,
    estimate_minutes: Option<i64>,
    actual_minutes: i64,
    progress: i64,
    planned_start: Option<&str>,
    planned_end: Option<&str>,
) -> Result<(), String> {
    if !matches!(kind, "task" | "requirement" | "idea" | "note") {
        return Err("任务类型只能是 task/requirement/idea/note".to_string());
    }
    if !matches!(status, "todo" | "in_progress" | "blocked" | "done") {
        return Err("任务状态只能是 todo/in_progress/blocked/done".to_string());
    }
    if !matches!(priority, "P0" | "P1" | "P2" | "P3") {
        return Err("优先级只能是 P0/P1/P2/P3".to_string());
    }
    if estimate_minutes.is_some_and(|minutes| minutes < 0) {
        return Err("预估工时不能为负数".to_string());
    }
    if actual_minutes < 0 {
        return Err("实际工时不能为负数".to_string());
    }
    if !(0..=100).contains(&progress) {
        return Err("进度必须在 0 到 100 之间".to_string());
    }
    if let (Some(start), Some(end)) = (planned_start, planned_end) {
        if start > end {
            return Err("计划开始时间不能晚于结束时间".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskDraft;
    #[cfg(windows)]
    use std::os::windows::fs::OpenOptionsExt;

    fn cleanup_test_dir(path: &Path) {
        if !path.exists() {
            return;
        }
        let temp_root = std::env::temp_dir()
            .canonicalize()
            .expect("canonicalize test temp root");
        let candidate = path.canonicalize().expect("canonicalize test directory");
        let safe_name = candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("taskvault_db_test_"));
        assert!(
            candidate != temp_root && candidate.starts_with(&temp_root) && safe_name,
            "refusing to remove unexpected test path: {}",
            candidate.display()
        );
        fs::remove_dir_all(candidate).expect("remove test directory");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "taskvault_db_test_{}_{}_{}",
            name,
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        cleanup_test_dir(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn draft(title: &str, subtasks: &[&str]) -> TaskDraft {
        TaskDraft {
            title: title.to_string(),
            description: "description".to_string(),
            kind: "task".to_string(),
            status: "todo".to_string(),
            priority: "P1".to_string(),
            due_at: None,
            estimate_minutes: Some(10),
            planned_start: None,
            planned_end: None,
            tags: vec!["tag".to_string()],
            subtasks: subtasks.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn inbox_and_task_flow_works() {
        let dir = temp_dir("flow");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        let item = db
            .insert_inbox("做一个 Rust 任务记录工具", "hotkey", None)
            .unwrap();
        assert_eq!(item.ai_status, "pending");
        assert_eq!(item.item_keys, vec!["manual"]);

        let img_item = db
            .insert_inbox("", "image", Some("20260829_1.png"))
            .unwrap();
        assert_eq!(img_item.image_path.as_deref(), Some("20260829_1.png"));

        db.set_ai_processing(item.id).unwrap();
        db.set_ai_result(
            item.id,
            "done",
            Some(r#"{"is_actionable":true}"#),
            Some("deepseek-v4-flash"),
        )
        .unwrap();
        assert_eq!(db.get_inbox(item.id).unwrap().ai_status, "done");
        assert_eq!(db.get_inbox(item.id).unwrap().item_keys, vec!["text"]);

        let task = db
            .promote_inbox(
                item.id,
                &draft("任务记录工具", &["确认筛选字段", "修改导出接口"]),
                "text",
            )
            .unwrap();
        assert_eq!(task.source_inbox_id, Some(item.id));
        assert_eq!(task.subtasks.len(), 2);
        assert_eq!(db.list_tasks().unwrap().len(), 1);
        let replay = db
            .promote_inbox(item.id, &draft("changed title", &["different"]), "text")
            .unwrap();
        assert_eq!(replay.id, task.id);
        assert_eq!(db.list_tasks().unwrap().len(), 1);
        assert_eq!(db.get_inbox(item.id).unwrap().promoted_keys, vec!["text"]);

        let done = db.set_subtask_done(task.subtasks[0].id, true).unwrap();
        assert!(done.is_done);
        let added = db.add_subtask(task.id, " 前端联调 ").unwrap();
        assert_eq!(added.text, "前端联调");
        assert_eq!(added.position, 2);
        assert!(db.add_subtask(task.id, "   ").is_err());
        db.delete_subtask(added.id).unwrap();
        let listed = db.list_tasks().unwrap();
        assert_eq!(listed[0].subtasks.len(), 2);
        assert!(listed[0].subtasks[0].is_done);

        assert_eq!(db.set_task_priority(task.id, "P0").unwrap().priority, "P0");
        assert!(db.set_task_priority(task.id, "P9").is_err());
        assert!(db.set_task_archived(task.id, true).unwrap().is_archived);
        assert_eq!(db.list_tasks().unwrap().len(), 0);
        assert_eq!(db.list_archived_tasks().unwrap().len(), 1);
        assert!(!db.set_task_archived(task.id, false).unwrap().is_archived);
        db.delete_task(task.id).unwrap();
        assert_eq!(db.list_tasks().unwrap().len(), 0);

        db.soft_delete_inbox(item.id).unwrap();
        assert_eq!(db.list_inbox(false).unwrap().len(), 1);
        assert_eq!(db.list_inbox(true).unwrap().len(), 2);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn request_claim_complete_and_recovery_are_fenced() {
        let dir = temp_dir("ai");
        let db = Db::open(&dir.join("inbox.db"), &dir.join("tasks.db")).unwrap();
        let item = db.insert_inbox("x", "hotkey", None).unwrap();
        assert!(db.request_ai(item.id).unwrap());
        assert!(!db.request_ai(item.id).unwrap());
        assert_eq!(db.queued_ai_ids().unwrap(), vec![item.id]);
        let generation = db.claim_ai(item.id).unwrap().unwrap();
        assert!(!db.request_ai(item.id).unwrap());
        assert!(!db
            .complete_ai(item.id, generation - 1, "done", Some("old"), None)
            .unwrap());
        assert!(db
            .complete_ai(item.id, generation, "done", Some("new"), None)
            .unwrap());
        assert!(!db
            .complete_ai(item.id, generation, "done", Some("again"), None)
            .unwrap());

        assert!(db.request_ai(item.id).unwrap());
        let second_generation = db.claim_ai(item.id).unwrap().unwrap();
        assert!(second_generation > generation);
        assert_eq!(db.recover_processing().unwrap(), 1);
        assert_eq!(db.get_inbox(item.id).unwrap().ai_status, "pending");
        assert_eq!(db.queued_ai_ids().unwrap(), vec![item.id]);
        assert!(!db
            .complete_ai(item.id, second_generation, "done", Some("stale"), None)
            .unwrap());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn encrypted_snapshot_reopens_and_legacy_plaintext_is_removed() {
        let dir = temp_dir("migration");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        {
            let inbox = Connection::open(&inbox_p).unwrap();
            inbox
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                 CREATE TABLE inbox (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, raw_text TEXT NOT NULL,
                    source TEXT NOT NULL, created_at TEXT NOT NULL,
                    ai_status TEXT NOT NULL, ai_result TEXT, ai_model TEXT,
                    is_promoted INTEGER NOT NULL DEFAULT 0, deleted_at TEXT,
                    image_path TEXT
                 );
                 INSERT INTO inbox(raw_text,source,created_at,ai_status)
                 VALUES ('secret legacy task','hotkey','2026-01-01T00:00:00Z','processing');",
                )
                .unwrap();
            let tasks = Connection::open(&tasks_p).unwrap();
            tasks
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                 CREATE TABLE tasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL,
                    status TEXT NOT NULL, priority TEXT NOT NULL, due_at TEXT,
                    estimate_minutes INTEGER, actual_minutes INTEGER NOT NULL DEFAULT 0,
                    progress INTEGER NOT NULL DEFAULT 0, planned_start TEXT,
                    planned_end TEXT, tags TEXT NOT NULL DEFAULT '[]',
                    source_inbox_id INTEGER, source_snapshot TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE subtasks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL,
                    text TEXT NOT NULL, is_done INTEGER NOT NULL DEFAULT 0,
                    position INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL
                 );",
                )
                .unwrap();
        }
        let raw = b"secret legacy task".to_vec();
        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        let vault = dir.join(SNAPSHOT_NAME);
        assert!(vault.exists());
        let protected = fs::read(&vault).unwrap();
        assert!(!protected.windows(raw.len()).any(|window| window == raw));
        assert!(!inbox_p.exists());
        assert!(!tasks_p.exists());
        assert!(dir.join("inbox.db.vault").exists());
        assert!(dir.join("tasks.db.vault").exists());
        let migrated = db.list_inbox(false).unwrap();
        assert_eq!(migrated[0].ai_status, "pending");
        assert!(migrated[0].ai_requested);
        assert_eq!(migrated[0].ai_generation, 1);
        drop(db);

        let reopened = Db::open(&inbox_p, &tasks_p).unwrap();
        assert_eq!(
            reopened.list_inbox(false).unwrap()[0].raw_text,
            "secret legacy task"
        );
        cleanup_test_dir(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn persistence_failure_rolls_back_task_and_subtasks() {
        let dir = temp_dir("persist_failure");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        let item = db.insert_inbox("atomic promotion", "hotkey", None).unwrap();

        // Keep the old snapshot open without delete sharing. MoveFileExW must
        // then fail, while the existing bytes remain available for restart.
        let vault = dir.join(SNAPSHOT_NAME);
        let vault_before = fs::read(&vault).unwrap();
        let vault_hold = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(3)
            .open(&vault)
            .unwrap();

        let error = db
            .promote_inbox(
                item.id,
                &draft("atomic task", &["first", "second"]),
                "manual",
            )
            .expect_err("promotion must fail when its snapshot cannot be replaced");
        assert!(error.contains("写入数据库快照失败"));
        drop(vault_hold);
        assert!(db.list_tasks().unwrap().is_empty());
        let conn = db.state.lock().unwrap();
        let subtask_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM subtasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(subtask_count, 0);
        drop(conn);
        let item_after = db.get_inbox(item.id).unwrap();
        assert!(!item_after.is_promoted);
        assert!(item_after.promoted_keys.is_empty());
        assert_eq!(fs::read(&vault).unwrap(), vault_before);

        drop(db);
        let reopened = Db::open(&inbox_p, &tasks_p).unwrap();
        assert!(reopened.list_tasks().unwrap().is_empty());
        let reopened_item = reopened.get_inbox(item.id).unwrap();
        assert!(!reopened_item.is_promoted);
        assert!(reopened_item.promoted_keys.is_empty());
        cleanup_test_dir(&dir);
    }

    #[test]
    fn legacy_migration_preserves_autoincrement_high_water_marks() {
        let dir = temp_dir("sequence");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        {
            let inbox = Connection::open(&inbox_p).unwrap();
            inbox.execute_batch(SCHEMA).unwrap();
            inbox
                .execute(
                    "INSERT INTO inbox(raw_text, source, created_at) VALUES (?1, 'test', '2026-01-01')",
                    params!["kept inbox"],
                )
                .unwrap();
            inbox
                .execute(
                    "INSERT INTO inbox(raw_text, source, created_at) VALUES (?1, 'test', '2026-01-01')",
                    params!["deleted inbox"],
                )
                .unwrap();
            let deleted_id = inbox.last_insert_rowid();
            inbox
                .execute("DELETE FROM inbox WHERE id = ?1", params![deleted_id])
                .unwrap();

            let tasks = Connection::open(&tasks_p).unwrap();
            tasks.execute_batch(SCHEMA).unwrap();
            tasks
                .execute(
                    "INSERT INTO tasks(title, created_at, updated_at) VALUES ('kept task', '2026-01-01', '2026-01-01')",
                    [],
                )
                .unwrap();
            let kept_task_id = tasks.last_insert_rowid();
            tasks
                .execute(
                    "INSERT INTO tasks(title, created_at, updated_at) VALUES ('deleted task', '2026-01-01', '2026-01-01')",
                    [],
                )
                .unwrap();
            let deleted_task_id = tasks.last_insert_rowid();
            tasks
                .execute("DELETE FROM tasks WHERE id = ?1", params![deleted_task_id])
                .unwrap();
            tasks
                .execute(
                    "INSERT INTO subtasks(task_id, text, created_at) VALUES (?1, 'kept subtask', '2026-01-01')",
                    params![kept_task_id],
                )
                .unwrap();
            tasks
                .execute(
                    "INSERT INTO subtasks(task_id, text, created_at) VALUES (?1, 'deleted subtask', '2026-01-01')",
                    params![kept_task_id],
                )
                .unwrap();
            let deleted_subtask_id = tasks.last_insert_rowid();
            tasks
                .execute(
                    "DELETE FROM subtasks WHERE id = ?1",
                    params![deleted_subtask_id],
                )
                .unwrap();
        }

        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        let new_inbox = db.insert_inbox("new inbox", "test", None).unwrap();
        assert_eq!(new_inbox.id, 3);
        let new_task = db.create_task(&draft("new task", &[]), None, None).unwrap();
        assert_eq!(new_task.id, 3);
        let new_subtask = db.add_subtask(1, "new subtask").unwrap();
        assert_eq!(new_subtask.id, 3);
        cleanup_test_dir(&dir);
    }

    #[test]
    fn corrupt_vault_fails_closed_without_importing_legacy_plaintext() {
        let dir = temp_dir("corrupt_vault");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        let vault = dir.join(SNAPSHOT_NAME);
        {
            let inbox = Connection::open(&inbox_p).unwrap();
            inbox
                .execute_batch(
                    "CREATE TABLE inbox (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        raw_text TEXT NOT NULL,
                        source TEXT NOT NULL,
                        created_at TEXT NOT NULL,
                        ai_status TEXT NOT NULL,
                        ai_result TEXT,
                        ai_model TEXT,
                        is_promoted INTEGER NOT NULL DEFAULT 0,
                        deleted_at TEXT,
                        image_path TEXT
                    );
                    INSERT INTO inbox(raw_text, source, created_at, ai_status)
                    VALUES ('legacy plaintext', 'test', '2026-01-01', 'pending');",
                )
                .unwrap();
        }
        let legacy_bytes = fs::read(&inbox_p).unwrap();
        fs::write(&vault, b"corrupt vault bytes").unwrap();

        assert!(Db::open(&inbox_p, &tasks_p).is_err());
        assert!(inbox_p.exists());
        assert_eq!(fs::read(&inbox_p).unwrap(), legacy_bytes);
        assert!(!archive_path(&inbox_p).exists());
        assert_eq!(fs::read(&vault).unwrap(), b"corrupt vault bytes");
        cleanup_test_dir(&dir);
    }

    #[test]
    fn existing_vault_refuses_to_archive_new_legacy_file() {
        let dir = temp_dir("new_legacy");
        let inbox_p = dir.join("inbox.db");
        let tasks_p = dir.join("tasks.db");
        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        drop(db);

        fs::write(&inbox_p, b"legacy file written after migration").unwrap();
        let vault = dir.join(SNAPSHOT_NAME);
        let vault_before = fs::read(&vault).unwrap();
        let error = match Db::open(&inbox_p, &tasks_p) {
            Ok(_) => panic!("a valid vault must not adopt an unarchived legacy file"),
            Err(error) => error,
        };
        assert!(error.contains("没有既有加密归档"));
        assert!(inbox_p.exists());
        assert!(!archive_path(&inbox_p).exists());
        assert_eq!(fs::read(&vault).unwrap(), vault_before);
        cleanup_test_dir(&dir);
    }
}
