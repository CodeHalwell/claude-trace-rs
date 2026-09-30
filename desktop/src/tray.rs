//! The system-tray icon and its menu.

use std::sync::{Arc, Mutex};

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Wry,
};
use tauri_plugin_autostart::ManagerExt as _;
use tracing::warn;

use crate::{ctx::Ctx, show_main};

pub struct Tray {
    icon: TrayIcon<Wry>,
    /// Disabled first entry showing live status (tooltips do not exist on
    /// every Linux desktop, so the menu carries the same text).
    status: MenuItem<Wry>,
    notify: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    last_status: Mutex<String>,
}

pub fn create(app: &AppHandle, ctx: &Ctx) -> tauri::Result<Tray> {
    let s = ctx.settings();
    let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show dashboard", true, None::<&str>)?;
    let browser = MenuItem::with_id(app, "browser", "Open in browser", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let notify = CheckMenuItem::with_id(
        app,
        "notify",
        "Notify when an agent finishes",
        true,
        s.notify_turn_end,
        None::<&str>,
    )?;
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "Launch at login",
        true,
        app.autolaunch().is_enabled().unwrap_or(false),
        None::<&str>,
    )?;
    let data = MenuItem::with_id(app, "data", "Open data folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Agent Trace", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &show,
            &browser,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &notify,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &data,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Agent Trace")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder.build(app)?;
    Ok(Tray {
        icon,
        status,
        notify,
        autostart,
        last_status: Mutex::new(String::new()),
    })
}

fn on_menu(app: &AppHandle, id: &str) {
    let ctx = app.state::<Arc<Ctx>>();
    match id {
        "show" => show_main(app),
        "browser" => {
            if let Some(b) = ctx.backend.get() {
                if let Err(e) = tauri_plugin_opener::open_url(b.url(), None::<&str>) {
                    warn!("Could not open the browser: {e}");
                }
            }
        }
        "settings" => {
            show_main(app);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.eval("typeof openSettings === 'function' && openSettings()");
            }
        }
        "notify" => {
            let mut s = ctx.settings();
            s.notify_turn_end = !s.notify_turn_end;
            if let Err(e) = s.save(&ctx.settings_path) {
                warn!("Could not save settings: {e}");
            }
            *ctx.settings.lock().expect("settings poisoned") = s;
            sync_checks(app, &ctx);
        }
        "autostart" => {
            let al = app.autolaunch();
            let result = if al.is_enabled().unwrap_or(false) {
                al.disable()
            } else {
                al.enable()
            };
            if let Err(e) = result {
                warn!("Could not change launch at login: {e}");
            }
            sync_checks(app, &ctx);
        }
        "data" => {
            if let Err(e) = crate::commands::open_dir(&ctx.data_dir()) {
                warn!("{e}");
            }
        }
        "quit" => {
            ctx.quitting
                .store(true, std::sync::atomic::Ordering::SeqCst);
            app.exit(0);
        }
        _ => {}
    }
}

/// Bring the check marks in line with the saved settings.
pub fn sync_checks(app: &AppHandle, ctx: &Ctx) {
    if let Some(t) = ctx.tray.get() {
        let _ = t.notify.set_checked(ctx.settings().notify_turn_end);
        let _ = t
            .autostart
            .set_checked(app.autolaunch().is_enabled().unwrap_or(false));
    }
}

/// Update the status line and tooltip ("2 active · $1.84 today").
pub fn refresh_status(_app: &AppHandle, ctx: &Ctx) {
    let Some(t) = ctx.tray.get() else { return };
    let text = match ctx.backend.get() {
        None => match ctx.startup_error.lock().expect("poisoned").as_ref() {
            Some(_) => "Could not start — see the window".to_owned(),
            None => "Starting…".to_owned(),
        },
        Some(b) => {
            let (active, spent) = {
                let a = ctx.activity.lock().expect("activity poisoned");
                (
                    a.active_sessions(std::time::Instant::now()),
                    a.spent_today(),
                )
            };
            let mut s = match active {
                0 => "No active sessions".to_owned(),
                1 => "1 active session".to_owned(),
                n => format!("{n} active sessions"),
            };
            s.push_str(&format!(" · ${spent:.2} today"));
            if b.mode == crate::backend::Mode::Attached {
                s.push_str(" · attached");
            }
            s
        }
    };
    let mut last = t.last_status.lock().expect("poisoned");
    if *last != text {
        let _ = t.status.set_text(&text);
        let _ = t.icon.set_tooltip(Some(format!("Agent Trace — {text}")));
        *last = text;
    }
}
