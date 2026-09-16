mod ai_queue;
mod attachments;
mod commands;
mod db;
mod deepseek;
mod models;
mod settings;
mod storage;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use db::Db;
use settings::Settings;

pub struct AppState {
    pub db: Db,
    pub settings_path: PathBuf,
    pub settings: Arc<RwLock<Settings>>,
    pub http: reqwest::Client,
    pub ai_queue: ai_queue::AiQueue,
}

fn toggle_window(app: &AppHandle, label: &str) {
    if let Some(win) = app.get_webview_window(label) {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            if label == "quick" {
                if let Ok(cursor) = win.cursor_position() {
                    let x = (cursor.x as i32 - 300).max(0);
                    let y = (cursor.y as i32 - 40).max(0);
                    let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
                }
            }
            let _ = win.show();
            let _ = win.set_focus();
            if label == "quick" {
                let js = "setTimeout(() => { const el = document.getElementById('task-input'); if (el) { el.focus(); el.select(); } }, 30)";
                let _ = win.eval(js);
            }
        }
    }
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn make_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_main_item = MenuItem::with_id(app, "show_main", "打开主窗口", true, None::<&str>)?;
    let quick_item = MenuItem::with_id(app, "quick_capture", "快速记录", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_main_item, &quick_item, &quit_item])?;

    let rgba = include_bytes!("../icons/icon_32.rgba").to_vec();
    let icon = tauri::image::Image::new_owned(rgba, 32, 32);

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("任务记录")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show_main" => show_main(app),
            "quick_capture" => toggle_window(app, "quick"),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

pub fn register_hotkeys(app: &AppHandle, settings: &Settings) {
    use tauri_plugin_global_shortcut::ShortcutState;
    let handle = app.clone();
    let quick = settings.quick_hotkey.clone();
    let mainh = settings.main_hotkey.clone();

    if let Err(e) = app.global_shortcut().unregister_all() {
        tracing::error!("注销旧快捷键失败: {e}");
    }
    if let Err(e) = app
        .global_shortcut()
        .on_shortcut(quick.as_str(), move |app, _s, event| {
            if event.state == ShortcutState::Pressed {
                toggle_window(app, "quick");
            }
        })
    {
        tracing::error!("注册快速记录快捷键失败: {e}");
    }
    if let Err(e) = handle
        .global_shortcut()
        .on_shortcut(mainh.as_str(), |app, _s, event| {
            if event.state == ShortcutState::Pressed {
                toggle_window(app, "main");
            }
        })
    {
        tracing::error!("注册主窗口快捷键失败: {e}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let hidden_start = std::env::args().any(|a| a == "--hidden");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(move |app| {
            tracing_subscriber::fmt().with_env_filter("info").init();

            let app_data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data)?;

            let settings_path = app_data.join("settings.json");
            let settings =
                Settings::load_or_create(&settings_path).map_err(std::io::Error::other)?;
            let http = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?;

            let db = Db::open(&app_data.join("inbox.db"), &app_data.join("tasks.db"))
                .map_err(std::io::Error::other)?;
            attachments::migrate_legacy(&app_data.join("attachments"))
                .map_err(std::io::Error::other)?;

            // 使用设置中的快捷键（保存设置后也会重新注册）。
            register_hotkeys(app.handle(), &settings);

            // 开机自启：按设置启用/禁用。
            if settings.autostart {
                app.autolaunch().enable()?;
            } else {
                let _ = app.autolaunch().disable();
            }

            make_tray(app)?;

            app.manage(AppState {
                db,
                settings_path,
                settings: Arc::new(RwLock::new(settings)),
                http,
                ai_queue: ai_queue::AiQueue::new(),
            });

            ai_queue::resume_pending(app.handle()).map_err(std::io::Error::other)?;

            if !hidden_start {
                show_main(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let label = window.label();
                if label == "main" || label == "quick" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::capture_inbox,
            commands::capture_inbox_image,
            commands::read_inbox_image,
            commands::read_image_file_base64,
            commands::list_inbox,
            commands::list_tasks,
            commands::list_archived_tasks,
            commands::archive_task,
            commands::unarchive_task,
            commands::delete_task,
            commands::set_task_priority,
            commands::parse_inbox_item,
            commands::process_pending_inbox,
            commands::promote_inbox,
            commands::create_task,
            commands::update_task,
            commands::add_subtask,
            commands::set_subtask_done,
            commands::delete_subtask,
            commands::delete_inbox,
            commands::get_settings,
            commands::save_settings,
            commands::hide_window,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            show_startup_error(&error.to_string());
            std::process::exit(1);
        });

    app.run(|_app, event| {
        if let RunEvent::Exit = event {
            tracing::info!("应用退出");
        }
    });
}

fn show_startup_error(message: &str) {
    let message =
        format!("TaskVault 无法启动。为保护数据，应用没有重置配置或清空记录。\n\n{message}");
    eprintln!("{message}");
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                flags: u32,
            ) -> i32;
        }
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let caption: Vec<u16> = "TaskVault · 数据保护"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // Both nul-terminated UTF-16 buffers outlive the synchronous system call.
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10);
        }
    }
}
