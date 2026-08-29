use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chrono::Local;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;

use crate::deepseek::{self, encode_image, AiError};
use crate::models::{InboxItem, Subtask, Task, TaskDraft};
use crate::settings::Settings;
use crate::AppState;

/// 附件目录：app_data/attachments
fn attachments_dir(state: &State<'_, AppState>) -> std::path::PathBuf {
    state
        .settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("attachments")
}

/// 录入后的 AI 解析调度：按设置决定自动解析 / 未配置 / 跳过，异步执行不阻塞录入。
fn trigger_parse(app: &AppHandle, state: &State<'_, AppState>, id: i64) {
    let settings = match state.settings.read() {
        Ok(s) => s.clone(),
        Err(_) => return,
    };
    if settings.auto_parse {
        if settings.effective_api_key().is_some() {
            let handle = app.clone();
            let db = state.db.clone();
            let http = state.http.clone();
            let attachments_dir = attachments_dir(state);
            tauri::async_runtime::spawn(async move {
                let _ = parse_inbox_with_retry(&db, &http, &settings, &attachments_dir, id).await;
                let _ = handle.emit("inbox-updated", id);
            });
        } else {
            let _ = state.db.set_ai_result(id, "unconfigured", None, None);
            let _ = app.emit("inbox-updated", id);
        }
    } else {
        let _ = state.db.set_ai_result(id, "skipped", None, None);
        let _ = app.emit("inbox-updated", id);
    }
}

fn save_attachment(
    dir: &Path,
    bytes: &[u8],
) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let ext = match deepseek::sniff_image_mime(bytes) {
        Some("image/png") => "png",
        Some("image/jpeg") => "jpg",
        Some("image/webp") => "webp",
        Some("image/gif") => "gif",
        _ => return Err("不支持的图片格式（仅支持 PNG/JPEG/WebP/GIF）".to_string()),
    };
    let name = format!("img_{}.{}", Local::now().format("%Y%m%d_%H%M%S%3f"), ext);
    std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string())?;
    Ok(name)
}

/// 剥离 data URL 前缀，返回纯 base64
fn strip_data_url(data: &str) -> Result<&str, String> {
    match data.find(";base64,") {
        Some(i) => Ok(&data[i + 8..]),
        None => {
            if data.contains("base64") || data.starts_with('/') || data.starts_with('i') {
                Ok(data)
            } else {
                Err("图片数据格式不正确".to_string())
            }
        }
    }
}

#[tauri::command]
pub async fn capture_inbox_image(
    app: AppHandle,
    state: State<'_, AppState>,
    text: Option<String>,
    image_base64: String,
) -> Result<InboxItem, String> {
    let raw_b64 = strip_data_url(&image_base64)?;
    let bytes = B64.decode(raw_b64.trim())
        .map_err(|e| format!("图片解码失败: {e}"))?;
    let name = save_attachment(&attachments_dir(&state), &bytes)?;

    let raw = text.unwrap_or_default().trim().to_string();
    let item = state.db.insert_inbox(&raw, "image", Some(&name))?;
    trigger_parse(&app, &state, item.id);
    Ok(state.db.get_inbox(item.id)?)
}

/// 详情页展示图片：返回 data URL（省去 asset 协议/CSP 配置）
#[tauri::command]
pub fn read_inbox_image(
    state: State<'_, AppState>,
    id: i64,
) -> Result<Option<String>, String> {
    let item = state.db.get_inbox(id)?;
    let Some(rel) = item.image_path else {
        return Ok(None);
    };
    let bytes = std::fs::read(attachments_dir(&state).join(rel))
        .map_err(|e| format!("读取图片失败: {e}"))?;
    let img = encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(Some(format!("data:{};base64,{}", img.mime, img.data)))
}

/// 快速窗拖拽图片文件时取预览用：读取磁盘图片转 data URL（仅限图片扩展名）
#[tauri::command]
pub fn read_image_file_base64(path: String) -> Result<String, String> {
    let ext_ok = ["png", "jpg", "jpeg", "webp", "gif"]
        .iter()
        .any(|e| path.rsplit('.').next().is_some_and(|x| x.eq_ignore_ascii_case(e)));
    if !ext_ok {
        return Err("仅支持 PNG/JPEG/WebP/GIF 图片".to_string());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取文件失败: {e}"))?;
    let img = encode_image(&bytes).map_err(|e| e.to_string())?;
    Ok(format!("data:{};base64,{}", img.mime, img.data))
}

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
        .insert_inbox(raw, source.as_deref().unwrap_or("hotkey"), None)?;
    trigger_parse(&app, &state, item.id);
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
    let Some(_api_key) = settings.effective_api_key() else {
        state.db.set_ai_result(id, "unconfigured", None, None)?;
        let _ = app.emit("inbox-updated", id);
        return Err("未配置 API Key，请先在设置中填写".to_string());
    };

    let db = state.db.clone();
    let http = state.http.clone();
    let task_settings = settings.clone();
    let attachments_dir = attachments_dir(&state);
    tauri::async_runtime::spawn(async move {
        let _ = parse_inbox_with_retry(&db, &http, &task_settings, &attachments_dir, item.id).await;
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
        return Err("未配置 API Key，请先在设置中填写".to_string());
    }

    for id in &ids {
        let db = state.db.clone();
        let http = state.http.clone();
        let task_settings = settings.clone();
        let attachments_dir = attachments_dir(&state);
        let handle = app.clone();
        let item_id = *id;
        tauri::async_runtime::spawn(async move {
            let _ =
                parse_inbox_with_retry(&db, &http, &task_settings, &attachments_dir, item_id).await;
            let _ = handle.emit("inbox-updated", item_id);
        });
    }
    Ok(ids.len())
}

async fn parse_inbox_with_retry(
    db: &crate::db::Db,
    http: &reqwest::Client,
    settings: &Settings,
    attachments_dir: &Path,
    id: i64,
) -> Result<(), AiError> {
    let item = match db.get_inbox(id) {
        Ok(item) => item,
        Err(_) => return Err(AiError::Api("记录不存在".to_string())),
    };
    // 带图片的记录走视觉模型；附件丢失直接失败并说明原因
    let image = match &item.image_path {
        Some(rel) => Some(encode_image(
            &std::fs::read(attachments_dir.join(rel))
                .map_err(|e| AiError::Api(format!("图片附件读取失败: {e}")))?,
        )?),
        None => None,
    };

    db.set_ai_processing(id).ok();

    let model_used = settings.model_for(image.is_some()).to_string();
    let mut last_err = AiError::Api("unknown".to_string());
    for attempt in 0..3 {
        match deepseek::parse_inbox_text(http, settings, &item.raw_text, image.as_ref()).await {
            Ok(parsed) => {
                // 图片解析可能一次返回多个待办：首条留在原记录，其余拆成新记录（共享同一张图）
                let store = |a: &crate::models::AiParsed| {
                    serde_json::to_string(&crate::models::AiParsed {
                        items: Vec::new(),
                        ..a.clone()
                    })
                    .unwrap_or_else(|_| "{}".to_string())
                };
                if parsed.items.is_empty() {
                    let _ = db.set_ai_result(id, "done", Some(&store(&parsed)), Some(&model_used));
                } else {
                    let _ = db.set_ai_result(id, "done", Some(&store(&parsed.items[0])), Some(&model_used));
                    for extra in parsed.items.iter().skip(1) {
                        let raw = format!("[图片待办] {}", extra.title);
                        if let Ok(new_item) =
                            db.insert_inbox(&raw, "image-split", item.image_path.as_deref())
                        {
                            let _ = db.set_ai_result(
                                new_item.id,
                                "done",
                                Some(&store(extra)),
                                Some(&model_used),
                            );
                        }
                    }
                }
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
    let _ = db.set_ai_result(id, "failed", Some(&msg), Some(&model_used));
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
        app.autolaunch()
            .enable()
            // 走到这里时设置已落盘，报错也要说明“保存成功，只是自启没开起来”
            .map_err(|e| format!("设置已保存，但启用开机自启失败：{e}"))?;
    } else if let Err(e) = app.autolaunch().disable() {
        // Windows 下自启项本就不存在时 disable 会报 os error 2，此时已是关闭状态，不算失败
        tracing::warn!("关闭开机自启失败（可能本就未启用）: {e}");
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
