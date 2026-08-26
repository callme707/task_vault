use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::deepseek::{self, AiError};
use crate::models::{InboxItem, Subtask, Task, TaskDraft};
use crate::settings::Settings;
use crate::AppState;

#[tauri::command]
pub async fn capture_inbox(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    source: Option<String>,
) -> Result<InboxItem, String> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err("内容不能为空".to_string());
    }

    let item = state
        .db
        .insert_inbox(raw, source.as_deref().unwrap_or("hotkey"))?;

    let settings = state.settings.read().map_err(|e| e.to_string())?.clone();
    if settings.auto_parse {
        if settings.effective_api_key().is_some() {
            let handle = app.clone();
            let db = state.db.clone();
            let http = state.http.clone();
            let model = settings.model.clone();
            let base_url = settings.base_url.clone();
            let api_key = settings
                .effective_api_key()
                .ok_or_else(|| "no key".to_string())?;
            tauri::async_runtime::spawn(async move {
                let _ =
                    parse_inbox_with_retry(&db, &http, &base_url, &model, &api_key, item.id).await;
                let _ = handle.emit("inbox-updated", item.id);
            });
        } else {
            state
                .db
                .set_ai_result(item.id, "unconfigured", None, None)?;
            let _ = app.emit("inbox-updated", item.id);
        }
    } else {
        state.db.set_ai_result(item.id, "skipped", None, None)?;
        let _ = app.emit("inbox-updated", item.id);
    }

    Ok(state.db.get_inbox(item.id)?)
}

#[tauri::command]
pub fn list_inbox(state: State<'_, AppState>) -> Result<Vec<InboxItem>, String> {
    state.db.list_inbox(false)
}

#[tauri::command]
pub fn list_tasks(state: State<'_, AppState>) -> Result<Vec<Task>, String> {
    state.db.list_tasks()
}

#[tauri::command]
pub fn parse_inbox_item(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let item = state.db.get_inbox(id)?;
    let settings = state.settings.read().map_err(|e| e.to_string())?.clone();
    let Some(api_key) = settings.effective_api_key() else {
        state.db.set_ai_result(id, "unconfigured", None, None)?;
        let _ = app.emit("inbox-updated", id);
        return Err("未配置 DeepSeek API Key，请先在设置中填写".to_string());
    };

    let db = state.db.clone();
    let http = state.http.clone();
    let model = settings.model.clone();
    let base_url = settings.base_url.clone();
    tauri::async_runtime::spawn(async move {
        let _ = parse_inbox_with_retry(&db, &http, &base_url, &model, &api_key, item.id).await;
        let _ = app.emit("inbox-updated", item.id);
    });
    Ok(())
}

#[tauri::command]
pub fn process_pending_inbox(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let ids = state.db.pending_ai_ids()?;
    if ids.is_empty() {
        return Ok(0);
    }

    let settings = state.settings.read().map_err(|e| e.to_string())?.clone();
    if settings.effective_api_key().is_none() {
        return Err("未配置 DeepSeek API Key，请先在设置中填写".to_string());
    }

    for id in &ids {
        let db = state.db.clone();
        let http = state.http.clone();
        let model = settings.model.clone();
        let base_url = settings.base_url.clone();
        let api_key = settings.effective_api_key().unwrap_or_default();
        let handle = app.clone();
        let item_id = *id;
        tauri::async_runtime::spawn(async move {
            let _ = parse_inbox_with_retry(&db, &http, &base_url, &model, &api_key, item_id).await;
            let _ = handle.emit("inbox-updated", item_id);
        });
    }
    Ok(ids.len())
}

async fn parse_inbox_with_retry(
    db: &crate::db::Db,
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
    api_key: &str,
    id: i64,
) -> Result<(), AiError> {
    let raw = match db.get_inbox(id) {
        Ok(item) => item.raw_text,
        Err(_) => return Err(AiError::Api("记录不存在".to_string())),
    };
    let settings = Settings {
        api_key: api_key.to_string(),
        base_url: base_url.to_string(),
        model: model.to_string(),
        auto_parse: true,
        ..Default::default()
    };

    db.set_ai_processing(id).ok();

    let mut last_err = AiError::Api("unknown".to_string());
    for attempt in 0..3 {
        match deepseek::parse_inbox_text(http, &settings, &raw).await {
            Ok(parsed) => {
                let json = serde_json::to_string(&parsed).unwrap_or_else(|_| "{}".to_string());
                let _ = db.set_ai_result(id, "done", Some(&json), Some(model));
                return Ok(());
            }
            Err(e) => {
                last_err = e;
                if attempt < 2 {
                    tokio::time::sleep(std::time::Duration::from_secs(1 + attempt)).await;
                }
            }
        }
    }

    let msg = last_err.to_string();
    let _ = db.set_ai_result(id, "failed", Some(&msg), Some(model));
    Err(last_err)
}

#[tauri::command]
pub fn promote_inbox(
    app: AppHandle,
    state: State<'_, AppState>,
    inbox_id: i64,
    draft: TaskDraft,
) -> Result<Task, String> {
    let item = state.db.get_inbox(inbox_id)?;
    if item.deleted_at.is_some() {
        return Err("该记录已删除".to_string());
    }
    if draft.title.trim().is_empty() {
        return Err("任务标题不能为空".to_string());
    }

    let task = state
        .db
        .create_task(&draft, Some(inbox_id), Some(&item.raw_text))?;
    state.db.mark_promoted(inbox_id)?;

    let _ = app.emit("inbox-updated", inbox_id);
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command]
pub fn create_task(
    app: AppHandle,
    state: State<'_, AppState>,
    draft: TaskDraft,
) -> Result<Task, String> {
    if draft.title.trim().is_empty() {
        return Err("任务标题不能为空".to_string());
    }
    let task = state.db.create_task(&draft, None, None)?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command]
pub fn update_task(app: AppHandle, state: State<'_, AppState>, task: Task) -> Result<Task, String> {
    let updated = state.db.update_task(&task)?;
    let _ = app.emit("tasks-updated", ());
    Ok(updated)
}

#[tauri::command]
pub fn add_subtask(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: i64,
    text: String,
) -> Result<Subtask, String> {
    let st = state.db.add_subtask(task_id, &text)?;
    let _ = app.emit("tasks-updated", ());
    Ok(st)
}

#[tauri::command]
pub fn set_subtask_done(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    is_done: bool,
) -> Result<Subtask, String> {
    let st = state.db.set_subtask_done(id, is_done)?;
    let _ = app.emit("tasks-updated", ());
    Ok(st)
}

#[tauri::command]
pub fn delete_subtask(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.delete_subtask(id)?;
    let _ = app.emit("tasks-updated", ());
    Ok(())
}

#[tauri::command]
pub fn list_archived_tasks(state: State<'_, AppState>) -> Result<Vec<Task>, String> {
    state.db.list_archived_tasks()
}

#[tauri::command]
pub fn archive_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<Task, String> {
    let task = state.db.set_task_archived(id, true)?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command]
pub fn unarchive_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<Task, String> {
    let task = state.db.set_task_archived(id, false)?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command]
pub fn delete_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.delete_task(id)?;
    let _ = app.emit("tasks-updated", ());
    Ok(())
}

#[tauri::command]
pub fn set_task_priority(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    priority: String,
) -> Result<Task, String> {
    let task = state.db.set_task_priority(id, priority.trim())?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command]
pub fn delete_inbox(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.soft_delete_inbox(id)?;
    let _ = app.emit("inbox-updated", id);
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    state
        .settings
        .read()
        .map(|s| s.clone())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<Settings, String> {
    {
        let mut guard = state.settings.write().map_err(|e| e.to_string())?;
        *guard = settings.clone();
    }
    settings.save(&state.settings_path)?;
    crate::register_hotkeys(&app, &settings);

    if settings.autostart {
        app.autolaunch().enable().map_err(|e| e.to_string())?;
    } else {
        app.autolaunch().disable().map_err(|e| e.to_string())?;
    }
    Ok(settings)
}

#[tauri::command]
pub fn hide_window(app: AppHandle, label: String) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.hide();
        Ok(())
    } else {
        Err(format!("窗口不存在: {label}"))
    }
}
