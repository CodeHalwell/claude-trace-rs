//! Commands the dashboard calls through `window.__TAURI_INTERNALS__.invoke`.
//! Each is listed in `command_names.rs`, which grants it to the dashboard.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::DialogExt;

use crate::{backend, ctx::Ctx, notify, settings::Settings, tray};

type Res<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub fn desktop_info(ctx: State<'_, Arc<Ctx>>) -> Value {
    let b = ctx.backend.get();
    json!({
        "mode": b.map(|b| b.mode.as_str()).unwrap_or("starting"),
        "ready": b.is_some(),
        "port": b.map(|b| b.port),
        "url": b.map(|b| b.url()),
        "db_path": b.map(|b| b.db_path.clone()).unwrap_or_else(|| backend::db_path(&ctx.settings())),
        "version": claude_trace_rs::VERSION,
        "server_version": b.map(|b| b.server_version.clone()),
        "settings_path": ctx.settings_path,
        "log_path": ctx.log_path,
        "error": ctx.startup_error.lock().ok().and_then(|e| e.clone()),
    })
}

#[tauri::command]
pub fn get_settings(ctx: State<'_, Arc<Ctx>>) -> Settings {
    ctx.settings()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, ctx: State<'_, Arc<Ctx>>, settings: Settings) -> Res<Value> {
    let mut settings = settings;
    settings.idle_seconds = settings.idle_seconds.clamp(10, 3600);
    settings.only.retain(|s| !s.trim().is_empty());
    settings.extra_roots.retain(|s| !s.trim().is_empty());
    backend::validate(&settings)?;
    settings.save(&ctx.settings_path).map_err(err)?;
    ctx.activity.lock().map_err(err)?.configure(
        Duration::from_secs(settings.idle_seconds),
        settings.daily_budget_usd,
    );
    let restart_required = ctx.launched_with.needs_restart(&settings);
    *ctx.settings.lock().map_err(err)? = settings;
    tray::sync_checks(&app, &ctx);
    Ok(json!({ "restart_required": restart_required }))
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Res<bool> {
    app.autolaunch().is_enabled().map_err(err)
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, ctx: State<'_, Arc<Ctx>>, enabled: bool) -> Res<()> {
    let al = app.autolaunch();
    if al.is_enabled().map_err(err)? != enabled {
        if enabled {
            al.enable().map_err(err)?;
        } else {
            al.disable().map_err(err)?;
        }
    }
    tray::sync_checks(&app, &ctx);
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    // Goes through the normal exit path so the single-instance lock is
    // released before the new process starts.
    app.request_restart();
}

#[tauri::command]
pub fn test_notification(app: AppHandle) {
    notify::show(
        &app,
        "Notifications are working",
        "Agent Trace will let you know when an agent finishes and is waiting on you.",
    );
}

#[tauri::command]
pub fn open_data_dir(ctx: State<'_, Arc<Ctx>>) -> Res<()> {
    open_dir(&ctx.data_dir())
}

/// Show a session's project folder. Directories open in the file manager;
/// a file is revealed in its folder, never opened (it could be a program).
#[tauri::command]
pub async fn reveal_path(path: String) -> Res<()> {
    let p = PathBuf::from(&path);
    if p.is_dir() {
        open_dir(&p)
    } else if p.exists() {
        tauri_plugin_opener::reveal_item_in_dir(&p).map_err(err)
    } else {
        Err(format!("{path} no longer exists"))
    }
}

pub fn open_dir(dir: &Path) -> Res<()> {
    if !dir.is_dir() {
        return Err(format!("{} does not exist yet", dir.display()));
    }
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(err)
}

/// Save an export the dashboard fetched. `filename` is a suggested file
/// name, or for `huggingface` the name of the dataset folder to create.
/// Returns where it was written, or `None` if the user cancelled.
#[tauri::command]
pub async fn save_export(
    app: AppHandle,
    format: String,
    filename: String,
    content: String,
) -> Res<Option<String>> {
    let name = safe_file_name(&filename);
    let downloads = app.path().download_dir().ok();
    let window = app.get_webview_window("main");
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        if let Some(w) = &window {
            dialog = dialog.set_parent(w);
        }
        if let Some(d) = &downloads {
            dialog = dialog.set_directory(d);
        }
        if format == "huggingface" {
            let Some(folder) = dialog
                .set_title("Choose where to create the dataset folder")
                .blocking_pick_folder()
            else {
                return Ok(None);
            };
            let dir = folder.into_path().map_err(err)?.join(&name);
            claude_trace_rs::export::write_huggingface_from_jsonl(&dir, &content).map_err(err)?;
            return Ok(Some(dir.display().to_string()));
        }
        let (label, ext) = if format == "markdown" {
            ("Markdown", "md")
        } else {
            ("JSON Lines", "jsonl")
        };
        let Some(file) = dialog
            .set_title("Save export")
            .set_file_name(&name)
            .add_filter(label, &[ext])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = file.into_path().map_err(err)?;
        std::fs::write(&path, content).map_err(err)?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(err)?
}

fn safe_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '-');
    if trimmed.is_empty() {
        "agent-trace-export".to_owned()
    } else {
        trimmed.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::safe_file_name;

    #[test]
    fn export_names_are_sanitised() {
        assert_eq!(safe_file_name("abc-123.jsonl"), "abc-123.jsonl");
        assert_eq!(safe_file_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(safe_file_name("a b/c"), "a-b-c");
        assert_eq!(safe_file_name("..."), "agent-trace-export");
    }
}
