use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Local;
use rusqlite::{params, params_from_iter, Connection};

use crate::models::{InboxItem, Subtask, Task, TaskDraft};

const INBOX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS inbox (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    raw_text     TEXT NOT NULL,
    source       TEXT NOT NULL DEFAULT 'hotkey',
    created_at   TEXT NOT NULL,
    ai_status    TEXT NOT NULL DEFAULT 'pending',
    ai_result    TEXT,
    ai_model     TEXT,
    is_promoted  INTEGER NOT NULL DEFAULT 0,
    deleted_at   TEXT,
    image_path   TEXT
);

CREATE INDEX IF NOT EXISTS idx_inbox_created ON inbox(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_inbox_status ON inbox(ai_status);
"#;

const TASK_SCHEMA: &str = r#"
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
"#;

const SUBTASK_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS subtasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    is_done    INTEGER NOT NULL DEFAULT 0,
    position   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_subtasks_task ON subtasks(task_id, position);
"#;

#[derive(Clone)]
pub struct Db {
    inbox: Arc<Mutex<Connection>>,
    tasks: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(inbox_path: &Path, tasks_path: &Path) -> Result<Self, String> {
        let inbox = Connection::open(inbox_path).map_err(|e| e.to_string())?;
        let tasks = Connection::open(tasks_path).map_err(|e| e.to_string())?;

        for conn in [&inbox, &tasks] {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(|e| e.to_string())?;
            conn.pragma_update(None, "foreign_keys", "ON")
                .map_err(|e| e.to_string())?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
        }

        inbox
            .execute_batch(INBOX_SCHEMA)
            .map_err(|e| e.to_string())?;
        tasks
            .execute_batch(TASK_SCHEMA)
            .map_err(|e| e.to_string())?;
        tasks
            .execute_batch(SUBTASK_SCHEMA)
            .map_err(|e| e.to_string())?;
        ensure_column(&tasks, "tasks", "is_archived", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&tasks, "tasks", "deleted_at", "TEXT")?;
        ensure_column(&inbox, "inbox", "image_path", "TEXT")?;

        Ok(Self {
            inbox: Arc::new(Mutex::new(inbox)),
            tasks: Arc::new(Mutex::new(tasks)),
        })
    }

    pub fn insert_inbox(
        &self,
        raw_text: &str,
        source: &str,
        image_path: Option<&str>,
    ) -> Result<InboxItem, String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        let now = Local::now().to_rfc3339();
        conn.execute(
            "INSERT INTO inbox(raw_text, source, created_at, ai_status, image_path) VALUES (?1, ?2, ?3, 'pending', ?4)",
            params![raw_text, source, now, image_path],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_inbox(id)
    }

    pub fn get_inbox(&self, id: i64) -> Result<InboxItem, String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        row_to_inbox(&conn, id).map_err(|e| e.to_string())
    }

    pub fn list_inbox(&self, include_deleted: bool) -> Result<Vec<InboxItem>, String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        let sql = if include_deleted {
            "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model, is_promoted, deleted_at, image_path FROM inbox ORDER BY id DESC"
        } else {
            "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model, is_promoted, deleted_at, image_path FROM inbox WHERE deleted_at IS NULL ORDER BY id DESC"
        };
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], inbox_from_row)
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn pending_ai_ids(&self) -> Result<Vec<i64>, String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare("SELECT id FROM inbox WHERE ai_status IN ('pending', 'failed') AND deleted_at IS NULL ORDER BY id ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn set_ai_processing(&self, id: i64) -> Result<(), String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE inbox SET ai_status = 'processing' WHERE id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_ai_result(
        &self,
        id: i64,
        status: &str,
        result_json: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE inbox SET ai_status = ?2, ai_result = COALESCE(?3, ai_result), ai_model = COALESCE(?4, ai_model) WHERE id = ?1",
            params![id, status, result_json, model],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn soft_delete_inbox(&self, id: i64) -> Result<(), String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        let now = Local::now().to_rfc3339();
        conn.execute(
            "UPDATE inbox SET deleted_at = ?2 WHERE id = ?1",
            params![id, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn mark_promoted(&self, id: i64) -> Result<(), String> {
        let conn = self
            .inbox
            .lock()
            .map_err(|_| "inbox db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE inbox SET is_promoted = 1 WHERE id = ?1",
            params![id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn create_task(
        &self,
        draft: &TaskDraft,
        source_inbox_id: Option<i64>,
        source_snapshot: Option<&str>,
    ) -> Result<Task, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let now = Local::now().to_rfc3339();
        let tags = serde_json::to_string(&draft.tags).unwrap_or_else(|_| "[]".to_string());
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
        insert_subtasks(&conn, id, &draft.subtasks)?;
        drop(conn);
        self.get_task(id)
    }

    pub fn get_task(&self, id: i64) -> Result<Task, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let mut task = task_by_id(&conn, id).map_err(|e| e.to_string())?;
        drop(conn);
        let mut map = self.subtasks_for_tasks(&[task.id])?;
        task.subtasks = map.remove(&task.id).unwrap_or_default();
        Ok(task)
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, String> {
        let mut tasks = self.query_tasks("deleted_at IS NULL AND is_archived = 0")?;
        self.attach_subtasks(&mut tasks)?;
        Ok(tasks)
    }

    pub fn list_archived_tasks(&self) -> Result<Vec<Task>, String> {
        let mut tasks = self.query_tasks("deleted_at IS NULL AND is_archived = 1")?;
        self.attach_subtasks(&mut tasks)?;
        Ok(tasks)
    }

    fn attach_subtasks(&self, tasks: &mut [Task]) -> Result<(), String> {
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        let map = self.subtasks_for_tasks(&ids)?;
        for t in tasks.iter_mut() {
            t.subtasks = map.get(&t.id).cloned().unwrap_or_default();
        }
        Ok(())
    }

    /// 一次查询取出多个任务的子任务，避免列表页 N+1。
    pub fn subtasks_for_tasks(
        &self,
        task_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<Subtask>>, String> {
        let mut out: HashMap<i64, Vec<Subtask>> = HashMap::new();
        if task_ids.is_empty() {
            return Ok(out);
        }
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let placeholders = task_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, task_id, text, is_done, position FROM subtasks
             WHERE task_id IN ({placeholders}) ORDER BY position, id"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params_from_iter(task_ids.iter()), subtask_from_row)
            .map_err(|e| e.to_string())?;
        for r in rows {
            let st = r.map_err(|e| e.to_string())?;
            out.entry(st.task_id).or_default().push(st);
        }
        Ok(out)
    }

    pub fn get_subtask(&self, id: i64) -> Result<Subtask, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        conn.query_row(
            "SELECT id, task_id, text, is_done, position FROM subtasks WHERE id = ?1",
            params![id],
            subtask_from_row,
        )
        .map_err(|e| e.to_string())
    }

    pub fn add_subtask(&self, task_id: i64, text: &str) -> Result<Subtask, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("子任务内容不能为空".to_string());
        }
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let position: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM subtasks WHERE task_id = ?1",
                params![task_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO subtasks(task_id, text, position, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![task_id, text, position, Local::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.get_subtask(id)
    }

    pub fn set_subtask_done(&self, id: i64, done: bool) -> Result<Subtask, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let n = conn
            .execute(
                "UPDATE subtasks SET is_done = ?2 WHERE id = ?1",
                params![id, if done { 1 } else { 0 }],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("子任务不存在".to_string());
        }
        drop(conn);
        self.get_subtask(id)
    }

    pub fn delete_subtask(&self, id: i64) -> Result<(), String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM subtasks WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("子任务不存在".to_string());
        }
        Ok(())
    }

    fn query_tasks(&self, filter: &str) -> Result<Vec<Task>, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
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
        for r in rows {
            out.push(r.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn set_task_priority(&self, id: i64, priority: &str) -> Result<Task, String> {
        if !matches!(priority, "P0" | "P1" | "P2" | "P3") {
            return Err("优先级只能是 P0/P1/P2/P3".to_string());
        }
        let now = Local::now().to_rfc3339();
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE tasks SET priority = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, priority, now],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_task(id)
    }

    pub fn set_task_archived(&self, id: i64, archived: bool) -> Result<Task, String> {
        let now = Local::now().to_rfc3339();
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE tasks SET is_archived = ?2, updated_at = ?3 WHERE id = ?1 AND deleted_at IS NULL",
            params![id, if archived { 1 } else { 0 }, now],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_task(id)
    }

    pub fn delete_task(&self, id: i64) -> Result<(), String> {
        let now = Local::now().to_rfc3339();
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let n = conn
            .execute(
                "UPDATE tasks SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
                params![id, now],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("任务不存在或已删除".to_string());
        }
        Ok(())
    }

    pub fn update_task(&self, task: &Task) -> Result<Task, String> {
        let conn = self
            .tasks
            .lock()
            .map_err(|_| "tasks db lock poisoned".to_string())?;
        let now = Local::now().to_rfc3339();
        let tags = serde_json::to_string(&task.tags).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE tasks SET title=?2, description=?3, kind=?4, status=?5, priority=?6,
                    due_at=?7, estimate_minutes=?8, actual_minutes=?9, progress=?10,
                    planned_start=?11, planned_end=?12, tags=?13, updated_at=?14
             WHERE id=?1",
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
                now
            ],
        )
        .map_err(|e| e.to_string())?;
        drop(conn);
        self.get_task(task.id)
    }
}

fn row_to_inbox(conn: &Connection, id: i64) -> rusqlite::Result<InboxItem> {
    conn.query_row(
        "SELECT id, raw_text, source, created_at, ai_status, ai_result, ai_model, is_promoted, deleted_at, image_path FROM inbox WHERE id=?1",
        params![id],
        inbox_from_row,
    )
}

fn inbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    Ok(InboxItem {
        id: row.get(0)?,
        raw_text: row.get(1)?,
        source: row.get(2)?,
        created_at: row.get(3)?,
        ai_status: row.get(4)?,
        ai_result: row.get(5)?,
        ai_model: row.get(6)?,
        is_promoted: row.get::<_, i64>(7)? != 0,
        deleted_at: row.get(8)?,
        image_path: row.get(9)?,
    })
}

fn task_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Task> {
    conn.query_row(
        "SELECT id, title, description, kind, status, priority, due_at, estimate_minutes,
                actual_minutes, progress, planned_start, planned_end, tags,
                source_inbox_id, source_snapshot, is_archived, deleted_at, created_at, updated_at
         FROM tasks WHERE id=?1",
        params![id],
        task_from_row,
    )
}

fn insert_subtasks(conn: &Connection, task_id: i64, texts: &[String]) -> Result<(), String> {
    for (i, text) in texts.iter().enumerate() {
        let t = text.trim();
        if t.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT INTO subtasks(task_id, text, position, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![task_id, t, i as i64, Local::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
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

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<(), String> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    for col in cols {
        if col.map_err(|e| e.to_string())? == column {
            return Ok(());
        }
    }
    conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskDraft;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("taskvault_test_{}_{}.db", name, std::process::id()))
    }

    #[test]
    fn inbox_and_task_flow_works() {
        let inbox_p = temp_path("inbox");
        let tasks_p = temp_path("tasks");
        let _ = std::fs::remove_file(&inbox_p);
        let _ = std::fs::remove_file(&tasks_p);

        let db = Db::open(&inbox_p, &tasks_p).unwrap();
        let item = db
            .insert_inbox("做一个 Rust 任务记录工具", "hotkey", None)
            .unwrap();
        assert_eq!(item.ai_status, "pending");
        assert!(item.image_path.is_none());

        // 带图片附件的记录
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

        let draft = TaskDraft {
            title: "任务记录工具".into(),
            description: "记录任务并由 AI 整理".into(),
            kind: "task".into(),
            status: "todo".into(),
            priority: "P1".into(),
            due_at: None,
            estimate_minutes: Some(120),
            planned_start: None,
            planned_end: None,
            tags: vec!["Rust".into()],
            subtasks: vec!["确认筛选字段".into(), "修改导出接口".into()],
        };
        let task = db
            .create_task(&draft, Some(item.id), Some(&item.raw_text))
            .unwrap();
        assert_eq!(task.source_inbox_id, Some(item.id));
        assert_eq!(task.subtasks.len(), 2);

        db.mark_promoted(item.id).unwrap();
        assert!(db.get_inbox(item.id).unwrap().is_promoted);
        assert_eq!(db.list_tasks().unwrap().len(), 1);
        // 收件箱里有文字记录 + 图片记录两条
        assert_eq!(db.list_inbox(false).unwrap().len(), 2);

        // 子任务：转正带入 → 勾选 / 追加 / 删除
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
        assert!(!listed[0].subtasks[1].is_done);

        // 优先级调整
        let updated = db.set_task_priority(task.id, "P0").unwrap();
        assert_eq!(updated.priority, "P0");
        assert!(db.set_task_priority(task.id, "P9").is_err());

        // 归档 / 恢复
        assert!(db.set_task_archived(task.id, true).unwrap().is_archived);
        assert_eq!(db.list_tasks().unwrap().len(), 0);
        assert_eq!(db.list_archived_tasks().unwrap().len(), 1);
        assert!(!db.set_task_archived(task.id, false).unwrap().is_archived);
        assert_eq!(db.list_tasks().unwrap().len(), 1);

        // 软删除
        db.delete_task(task.id).unwrap();
        assert_eq!(db.list_tasks().unwrap().len(), 0);
        assert_eq!(db.list_archived_tasks().unwrap().len(), 0);

        db.soft_delete_inbox(item.id).unwrap();
        assert_eq!(db.list_inbox(false).unwrap().len(), 1);
        assert_eq!(db.list_inbox(true).unwrap().len(), 2);

        let _ = std::fs::remove_file(&inbox_p);
        let _ = std::fs::remove_file(&tasks_p);
        let _ = std::fs::remove_file(inbox_p.with_extension("db-wal"));
        let _ = std::fs::remove_file(tasks_p.with_extension("db-wal"));
    }
}
