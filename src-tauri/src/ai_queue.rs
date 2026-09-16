//! Durable request claims and a bounded in-process executor.
use std::path::Path;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::deepseek::{self, AiError, ImageInput};
use crate::models::AiParsed;
use crate::AppState;

#[derive(Clone)]
pub struct AiQueue {
    permits: Arc<Semaphore>,
}

impl AiQueue {
    pub fn new() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(2)),
        }
    }

    async fn run<T>(&self, work: impl std::future::Future<Output = T>) -> Result<T, String> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| "AI 队列已关闭，请重新启动")?;
        Ok(work.await)
    }

    pub fn enqueue(&self, app: &AppHandle, id: i64, request: bool) -> Result<bool, String> {
        let state = app.state::<AppState>();
        let settings = state.settings.read().map_err(|_| "设置读取失败")?.clone();
        if !settings.api_ready() {
            return Err("请先在设置中配置 AI 服务与密钥".into());
        }
        if request && !state.db.request_ai(id)? {
            return Ok(false);
        }
        let Some(generation) = state.db.claim_ai(id)? else {
            return Ok(false);
        };
        let db = state.db.clone();
        let http = state.http.clone();
        let model = settings.clone();
        let dir = state
            .settings_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("attachments");
        let app = app.clone();
        let queue = self.clone();
        let _ = app.emit("inbox-updated", id);
        tauri::async_runtime::spawn(async move {
            let result = queue
                .run(run_claimed_with(
                    &db,
                    &dir,
                    id,
                    generation,
                    &model,
                    move |text, image| {
                        let http = http.clone();
                        let settings = settings.clone();
                        async move {
                            deepseek::parse_inbox_text(&http, &settings, &text, image.as_ref())
                                .await
                        }
                    },
                ))
                .await
                .and_then(|result| result);
            if let Err(message) = result {
                tracing::error!("AI 状态持久化失败: {message}");
                let _ = app.emit(
                    "operation-error",
                    serde_json::json!({"id":id,"message":message}),
                );
            }
            let _ = app.emit("inbox-updated", id);
        });
        Ok(true)
    }
}

pub fn resume_pending(app: &AppHandle) -> Result<usize, String> {
    let state = app.state::<AppState>();
    if !state
        .settings
        .read()
        .map_err(|_| "设置读取失败")?
        .api_ready()
    {
        return Ok(0);
    }
    let mut count = 0;
    for id in state.db.queued_ai_ids()? {
        count += usize::from(state.ai_queue.enqueue(app, id, false)?);
    }
    Ok(count)
}

async fn run_claimed_with<F, Fut>(
    db: &Db,
    dir: &Path,
    id: i64,
    generation: i64,
    settings: &crate::settings::Settings,
    mut parse: F,
) -> Result<(), String>
where
    F: FnMut(String, Option<ImageInput>) -> Fut,
    Fut: std::future::Future<Output = Result<AiParsed, AiError>>,
{
    let item = db.get_inbox(id)?;
    if item.deleted_at.is_some()
        || item.ai_generation != generation
        || item.ai_status != "processing"
    {
        return Ok(());
    }
    let model = settings.model_for(item.image_path.is_some());
    let image = match item
        .image_path
        .as_deref()
        .map(|name| {
            crate::attachments::read(dir, name)
                .and_then(|bytes| deepseek::encode_image(&bytes).map_err(|e| e.to_string()))
        })
        .transpose()
    {
        Ok(image) => image,
        Err(error) => {
            db.complete_ai(id, generation, "failed", Some(&error), Some(model))?;
            return Ok(());
        }
    };
    for attempt in 0..3 {
        match parse(item.raw_text.clone(), image.clone()).await {
            Ok(parsed) => {
                let json = serde_json::to_string(&parsed).map_err(|e| e.to_string())?;
                let status = if parsed.partial { "partial" } else { "done" };
                db.complete_ai(id, generation, status, Some(&json), Some(model))?;
                return Ok(());
            }
            Err(error) => {
                if attempt == 2 || !error.retryable() {
                    db.complete_ai(
                        id,
                        generation,
                        "failed",
                        Some(&error.to_string()),
                        Some(model),
                    )?;
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_secs(1 + attempt)).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn db() -> (Db, PathBuf) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "taskvault_ai_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (
            Db::open(&dir.join("inbox.db"), &dir.join("tasks.db")).unwrap(),
            dir,
        )
    }

    #[tokio::test]
    async fn missing_attachment_is_a_durable_failure_without_network() {
        let (db, dir) = db();
        let item = db.insert_inbox("", "image", Some("missing.tvimg")).unwrap();
        assert!(db.request_ai(item.id).unwrap());
        let generation = db.claim_ai(item.id).unwrap().unwrap();
        run_claimed_with(
            &db,
            &dir,
            item.id,
            generation,
            &crate::settings::Settings::default(),
            |_, _| async { panic!("missing attachment must never reach provider") },
        )
        .await
        .unwrap();
        let saved = db.get_inbox(item.id).unwrap();
        assert_eq!(saved.ai_status, "failed");
        assert!(saved.ai_result.unwrap().contains("附件"));
        drop(db);
        let db = Db::open(&dir.join("inbox.db"), &dir.join("tasks.db")).unwrap();
        assert_eq!(db.get_inbox(item.id).unwrap().ai_status, "failed");
    }

    #[tokio::test]
    async fn claims_are_unique_and_partial_results_are_not_complete() {
        let (db, dir) = db();
        let item = db.insert_inbox("整理会议记录", "hotkey", None).unwrap();
        assert!(db.request_ai(item.id).unwrap());
        let generation = db.claim_ai(item.id).unwrap().unwrap();
        assert!(!db.request_ai(item.id).unwrap());
        assert!(db.claim_ai(item.id).unwrap().is_none());
        run_claimed_with(
            &db,
            &dir,
            item.id,
            generation,
            &crate::settings::Settings::default(),
            |_, _| async {
                Ok(AiParsed {
                    title: "会议记录".into(),
                    partial: true,
                    ..Default::default()
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(db.get_inbox(item.id).unwrap().ai_status, "partial");
    }

    #[tokio::test]
    async fn permanent_provider_failure_is_not_retried() {
        let (db, dir) = db();
        let item = db.insert_inbox("整理记录", "hotkey", None).unwrap();
        db.request_ai(item.id).unwrap();
        let generation = db.claim_ai(item.id).unwrap().unwrap();
        let calls = AtomicUsize::new(0);
        run_claimed_with(
            &db,
            &dir,
            item.id,
            generation,
            &crate::settings::Settings::default(),
            |_, _| {
                calls.fetch_add(1, Ordering::Relaxed);
                async { Err(AiError::Http(401)) }
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(db.get_inbox(item.id).unwrap().ai_status, "failed");
    }

    #[tokio::test]
    async fn concurrency_budget_allows_only_two_requests() {
        let queue = AiQueue::new();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..6 {
            let queue = queue.clone();
            let (active, peak, completed) = (active.clone(), peak.clone(), completed.clone());
            workers.push(tokio::spawn(async move {
                queue
                    .run(async {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        completed.fetch_add(1, Ordering::SeqCst);
                    })
                    .await
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.await.unwrap();
        }
        assert_eq!(completed.load(Ordering::SeqCst), 6);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }
}
