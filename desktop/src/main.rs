//! Agent Trace desktop app: the claude-trace-rs dashboard in a native window,
//! with a tray icon, desktop notifications when an agent finishes, launch at
//! login and native save dialogs for exports.
//!
//! The app embeds the tracer (watcher, database and HTTP server on loopback)
//! and points its window at the dashboard. If a claude-trace-rs server is
//! already running on the configured port — typically the CLI's background
//! service — it attaches to that instead, so the two never race over the
//! same files and database.

// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity;
mod backend;
mod command_names;
mod commands;
mod ctx;
mod notify;
mod settings;
mod tray;

use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex},
};

use claude_trace_rs::runtime;
use tauri::{
    ipc::CapabilityBuilder,
    webview::{NewWindowResponse, PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, Url, WebviewUrl, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_window_state::StateFlags;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::{backend::Mode, ctx::Ctx, settings::Settings};

fn main() {
    let hidden_arg = std::env::args().any(|a| a == "--hidden");

    let app = tauri::Builder::default()
        // Must come first: a second launch just focuses the running app.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                // Visibility is ours to decide (start hidden, close to tray).
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::desktop_info,
            commands::get_settings,
            commands::save_settings,
            commands::get_autostart,
            commands::set_autostart,
            commands::restart_app,
            commands::test_notification,
            commands::open_data_dir,
            commands::reveal_path,
            commands::save_export,
        ])
        .setup(move |app| {
            setup(app.handle(), hidden_arg)?;
            Ok(())
        })
        .on_window_event(on_window_event)
        .build(tauri::generate_context!())
        .expect("failed to build the Agent Trace app");

    app.run(|_app, _event| {});
}

fn setup(app: &AppHandle, hidden_arg: bool) -> anyhow::Result<()> {
    let log_path = init_logging(app.path().app_log_dir().ok());
    let config_dir = app.path().app_config_dir()?;
    let settings_path = settings::settings_path(&config_dir);
    let settings = Settings::load(&settings_path);
    if !settings_path.exists() {
        // Write the defaults so the file is there to find and edit.
        if let Err(e) = settings.save(&settings_path) {
            warn!("Could not write {}: {e}", settings_path.display());
        }
    }
    info!(
        "Agent Trace {} starting (settings: {})",
        claude_trace_rs::VERSION,
        settings_path.display()
    );

    let hidden = hidden_arg || settings.start_hidden;
    let ctx = Arc::new(Ctx::new(settings_path, settings, log_path));
    app.manage(ctx.clone());

    create_main_window(app, !hidden)?;
    let tray = tray::create(app, &ctx)?;
    let _ = ctx.tray.set(tray);

    tauri::async_runtime::spawn(start_backend(app.clone(), ctx));
    Ok(())
}

/// Attach to a running server or start the embedded tracer, then point the
/// window at the dashboard and start the notification loop.
async fn start_backend(app: AppHandle, ctx: Arc<Ctx>) {
    let settings = ctx.settings();
    set_status(&app, "Looking for a running tracer…");
    let backend = match runtime::probe_existing(settings.port).await {
        Some(version) => {
            info!(
                "Attaching to claude-trace-rs {version} already serving on port {}",
                settings.port
            );
            backend::Backend {
                mode: Mode::Attached,
                port: settings.port,
                db_path: backend::db_path(&settings),
                server_version: version,
                tracer: None,
            }
        }
        None => {
            set_status(&app, "Starting the tracer…");
            match backend::start_embedded(&settings).await {
                Ok(b) => b,
                Err(e) => return startup_failed(&app, &ctx, format!("{e:#}")),
            }
        }
    };
    if let Err(e) = grant_dashboard_ipc(&app, backend.port) {
        return startup_failed(&app, &ctx, format!("Could not set up the window: {e}"));
    }

    let today = notify::today_start_utc();
    if let Some(usd) = backend.cost_since(&today).await {
        ctx.activity
            .lock()
            .expect("activity poisoned")
            .seed_spent_today(usd);
    }
    let (tx, rx) = mpsc::channel(1024);
    backend::spawn_feed(&backend, tx);

    let url = backend.url();
    info!("Dashboard at {url} ({} mode)", backend.mode.as_str());
    let _ = ctx.backend.set(backend);
    if let Some(w) = app.get_webview_window("main") {
        match url.parse::<Url>() {
            Ok(u) => {
                if let Err(e) = w.navigate(u) {
                    error!("Could not open the dashboard: {e}");
                }
            }
            Err(e) => error!("Bad dashboard URL {url}: {e}"),
        }
    }
    notify::spawn(app.clone(), ctx.clone(), rx);
    tray::refresh_status(&app, &ctx);
}

fn startup_failed(app: &AppHandle, ctx: &Ctx, message: String) {
    error!("Start-up failed: {message}");
    *ctx.startup_error.lock().expect("poisoned") = Some(message.clone());
    tray::refresh_status(app, ctx);
    eval_on_start_page(app, "showError", &message);
    show_main(app);
}

fn set_status(app: &AppHandle, text: &str) {
    eval_on_start_page(app, "setStatus", text);
}

fn eval_on_start_page(app: &AppHandle, func: &str, arg: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let arg = serde_json::to_string(arg).unwrap_or_default();
        let _ = w.eval(format!("window.{func} && window.{func}({arg})"));
    }
}

/// The dashboard is served over loopback HTTP, which Tauri treats as a
/// remote origin: it gets exactly the app's own commands, and only on the
/// port the tracer is actually using.
fn grant_dashboard_ipc(app: &AppHandle, port: u16) -> tauri::Result<()> {
    let mut cap = CapabilityBuilder::new(format!("dashboard-{port}"))
        .local(false)
        .window("main");
    for host in ["127.0.0.1", "localhost"] {
        cap = cap.remote(format!("http://{host}:{port}"));
    }
    for cmd in command_names::COMMANDS {
        cap = cap.permission(format!("allow-{}", cmd.replace('_', "-")));
    }
    app.add_capability(cap)
}

fn create_main_window(app: &AppHandle, visible: bool) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Agent Trace")
        .inner_size(1360.0, 880.0)
        .min_inner_size(900.0, 600.0)
        .center()
        .visible(visible)
        // Links out of the dashboard (agent homepages, docs) belong in the
        // user's browser, not in this window.
        .on_navigation(|url| {
            if stays_in_app(url) {
                true
            } else {
                open_external(url);
                false
            }
        })
        .on_new_window(|url, _features| {
            open_external(&url);
            NewWindowResponse::Deny
        })
        .on_page_load(|window, payload| {
            if payload.event() == PageLoadEvent::Finished && payload.url().scheme() == "http" {
                info!("Dashboard loaded: {}", payload.url());
                // Development hook for exercising the IPC bridge end to end
                // (see desktop/README.md); not compiled into release builds.
                #[cfg(debug_assertions)]
                if let Ok(js) = std::env::var("AGENT_TRACE_DEV_EVAL") {
                    let _ = window.eval(js);
                }
                #[cfg(not(debug_assertions))]
                let _ = window;
            }
        })
        .build()?;
    Ok(())
}

fn stays_in_app(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "asset" | "about" | "data" | "blob" => true,
        "http" | "https" => matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "tauri.localhost" | "[::1]")
        ),
        _ => false,
    }
}

fn open_external(url: &Url) {
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        if let Err(e) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
            warn!("Could not open {url}: {e}");
        }
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != "main" {
        return;
    }
    if let WindowEvent::CloseRequested { api, .. } = event {
        let Some(ctx) = window.try_state::<Arc<Ctx>>() else {
            return;
        };
        if ctx.settings().close_to_tray && !ctx.is_quitting() {
            api.prevent_close();
            let _ = window.hide();
            if !ctx.hid_once.swap(true, Ordering::SeqCst) {
                notify::show(
                    window.app_handle(),
                    "Agent Trace is still running",
                    "Tracing continues in the background. Use the tray icon to reopen or quit.",
                );
            }
        }
    }
}

/// Log to stderr and to `agent-trace.log` in the app's log folder (the app
/// usually has no terminal). The file is rotated once it passes 5 MB.
fn init_logging(dir: Option<PathBuf>) -> Option<PathBuf> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file = dir.and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        let path = d.join("agent-trace.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::rename(&path, d.join("agent-trace.log.old"));
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        Some((path, f))
    });
    let (path, file_layer) = match file {
        Some((p, f)) => (
            Some(p),
            Some(fmt::layer().with_ansi(false).with_writer(Mutex::new(f))),
        ),
        None => (None, None),
    };
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .try_init();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_and_app_urls_stay_in_the_window() {
        let ok = |s: &str| stays_in_app(&s.parse().unwrap());
        assert!(ok("http://127.0.0.1:7779/"));
        assert!(ok("http://localhost:7779/api/agents"));
        assert!(ok("tauri://localhost/index.html"));
        assert!(ok("http://tauri.localhost/index.html"));
        assert!(!ok("https://github.com/openai/codex"));
        assert!(!ok("http://127.0.0.1.attacker.example/"));
        assert!(!ok("file:///etc/passwd"));
    }
}
