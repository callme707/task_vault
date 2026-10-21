use std::path::Path;

use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::attachments;
use crate::deepseek::encode_image;
use crate::models::{InboxItem, Subtask, Task, TaskDraft};
use crate::settings::{SettingsUpdate, SettingsView};
use crate::AppState;

// Tauri 2 runs plain (non-async) commands on the main thread. Commands that
// lock the database, write the encrypted snapshot or decode images use
// `#[tauri::command(async)]` so that work cannot freeze the window. Settings,
// hotkey and window commands stay synchronous on purpose.

fn attachments_dir(state: &State<'_, AppState>) -> std::path::PathBuf {
    state
        .settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("attachments")
}

fn trigger_parse(
    app: &AppHandle,
    state: &State<'_, AppState>,
    id: i64,
    persona: Option<String>,
) {
    let result = (|| -> Result<(), String> {
        let settings = state.settings.read().map_err(|_| "设置读取失败")?.clone();
        if !settings.auto_parse {
            state.db.set_ai_result(id, "skipped", None, None)?;
        } else if !settings.api_ready() {
            state.db.set_ai_result(id, "unconfigured", None, None)?;
        } else {
            state.ai_queue.enqueue_with_persona(app, id, true, persona)?;
        }
        Ok(())
    })();
    if let Err(message) = result {
        let _ = app.emit("operation-error", serde_json::json!({"id":id,"message":format!("记录已保存，启动 AI 解析失败：{message}")}));
    }
    let _ = app.emit("inbox-updated", id);
}

#[tauri::command]
pub async fn capture_inbox_image(
    app: AppHandle,
    state: State<'_, AppState>,
    text: Option<String>,
    image_base64: String,
    persona: Option<String>,
) -> Result<InboxItem, String> {
    // Validate before writing either attachment or record.
    let bytes = attachments::decode_capture(&image_base64)?;
    let name = attachments::save(&attachments_dir(&state), &bytes)?;
    let raw = text.unwrap_or_default().trim().to_string();
    let item = state.db.insert_inbox(&raw, "image", Some(&name))?;
    trigger_parse(&app, &state, item.id, persona);
    state.db.get_inbox(item.id)
}

#[tauri::command(async)]
pub fn read_inbox_image(state: State<'_, AppState>, id: i64) -> Result<Option<String>, String> {
    let item = state.db.get_inbox(id)?;
    if item.deleted_at.is_some() {
        return Err("记录已删除".into());
    }
    let Some(name) = item.image_path else {
        return Ok(None);
    };
    let bytes = attachments::read(&attachments_dir(&state), &name)?;
    let img = encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(format!("data:{};base64,{}", img.mime, img.data)))
}

#[tauri::command(async)]
pub fn read_image_file_base64(path: String) -> Result<String, String> {
    let extension = Path::new(&path)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
        return Err("仅支持 PNG/JPEG/WebP/GIF 图片".into());
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "无法读取图片文件")?;
    if !metadata.is_file() || metadata.len() > crate::deepseek::MAX_IMAGE_BYTES as u64 {
        return Err("请选择不超过 4 MB 的图片文件".into());
    }
    let bytes = std::fs::read(path).map_err(|_| "无法读取图片文件")?;
    let img = encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(format!("data:{};base64,{}", img.mime, img.data))
}

#[tauri::command]
pub async fn capture_inbox(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
    source: Option<String>,
    persona: Option<String>,
) -> Result<InboxItem, String> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err("内容不能为空".into());
    }
    let item = state
        .db
        .insert_inbox(raw, source.as_deref().unwrap_or("hotkey"), None)?;
    trigger_parse(&app, &state, item.id, persona);
    state.db.get_inbox(item.id)
}

#[tauri::command(async)]
pub fn list_inbox(state: State<'_, AppState>) -> Result<Vec<InboxItem>, String> {
    state.db.list_inbox(false)
}

#[tauri::command(async)]
pub fn list_tasks(state: State<'_, AppState>) -> Result<Vec<Task>, String> {
    state.db.list_tasks()
}

#[tauri::command(async)]
pub fn parse_inbox_item(
    app: AppHandle,
    state: State<'_, AppState>,
    id: i64,
    persona: Option<String>,
) -> Result<(), String> {
    state
        .ai_queue
        .enqueue_with_persona(&app, id, true, persona)?;
    Ok(())
}

#[tauri::command(async)]
pub fn process_pending_inbox(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let mut started = 0;
    for id in state.db.pending_ai_ids()? {
        started += usize::from(state.ai_queue.enqueue(&app, id, true)?);
    }
    Ok(started)
}

#[tauri::command(async)]
pub fn promote_inbox(
    app: AppHandle,
    state: State<'_, AppState>,
    inbox_id: i64,
    draft: TaskDraft,
    item_key: String,
) -> Result<Task, String> {
    let task = state.db.promote_inbox(inbox_id, &draft, &item_key)?;
    let _ = app.emit("inbox-updated", inbox_id);
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command(async)]
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

#[tauri::command(async)]
pub fn update_task(app: AppHandle, state: State<'_, AppState>, task: Task) -> Result<Task, String> {
    let updated = state.db.update_task(&task)?;
    let _ = app.emit("tasks-updated", ());
    Ok(updated)
}

#[tauri::command(async)]
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

#[tauri::command(async)]
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

#[tauri::command(async)]
pub fn delete_subtask(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.delete_subtask(id)?;
    let _ = app.emit("tasks-updated", ());
    Ok(())
}

#[tauri::command(async)]
pub fn list_archived_tasks(state: State<'_, AppState>) -> Result<Vec<Task>, String> {
    state.db.list_archived_tasks()
}

#[tauri::command(async)]
pub fn archive_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<Task, String> {
    let task = state.db.set_task_archived(id, true)?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command(async)]
pub fn unarchive_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<Task, String> {
    let task = state.db.set_task_archived(id, false)?;
    let _ = app.emit("tasks-updated", ());
    Ok(task)
}

#[tauri::command(async)]
pub fn delete_task(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.delete_task(id)?;
    let _ = app.emit("tasks-updated", ());
    Ok(())
}

#[tauri::command(async)]
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

#[tauri::command(async)]
pub fn delete_inbox(app: AppHandle, state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.db.soft_delete_inbox(id)?;
    let _ = app.emit("inbox-updated", id);
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<SettingsView, String> {
    state
        .settings
        .read()
        .map(|s| s.view())
        .map_err(|_| "设置读取失败".into())
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: SettingsUpdate,
) -> Result<SettingsView, String> {
    let saved = {
        let mut guard = state.settings.write().map_err(|_| "设置读取失败")?;
        let next = settings.merge(&guard)?;
        next.save(&state.settings_path)?;
        *guard = next.clone();
        next
    };
    crate::register_hotkeys(&app, &saved);
    if saved.autostart {
        app.autolaunch()
            .enable()
            .map_err(|e| format!("设置已保存，但启用开机自启失败：{e}"))?;
    } else if let Err(e) = app.autolaunch().disable() {
        tracing::warn!("关闭开机自启失败（可能本就未启用）: {e}");
    }
    crate::ai_queue::resume_pending(&app)?;
    Ok(saved.view())
}

#[tauri::command]
pub fn hide_window(app: AppHandle, label: String) -> Result<(), String> {
    let win = app
        .get_webview_window(&label)
        .ok_or_else(|| format!("窗口不存在: {label}"))?;
    win.hide().map_err(|e| e.to_string())
}
