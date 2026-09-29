//! The notification loop: feeds live events through [`Activity`], turns the
//! resulting notices into desktop notifications and keeps the tray status
//! current.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{Local, SecondsFormat, Utc};
use claude_trace_rs::event::TraceEvent;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;
use tracing::warn;

use crate::{
    activity::{format_duration, Notice},
    ctx::Ctx,
    tray,
};

const TICK: Duration = Duration::from_secs(5);
/// How often today's spend is re-read from the database.
const REFRESH_SPEND: Duration = Duration::from_secs(30);

pub fn spawn(app: AppHandle, ctx: Arc<Ctx>, mut rx: mpsc::Receiver<TraceEvent>) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        let mut last_refresh = Instant::now();
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    let Some(ev) = ev else { break };
                    let notices = ctx
                        .activity
                        .lock()
                        .expect("activity poisoned")
                        .on_event(&ev, Instant::now());
                    deliver(&app, &ctx, notices);
                }
                _ = tick.tick() => {
                    let notices = ctx
                        .activity
                        .lock()
                        .expect("activity poisoned")
                        .tick(Instant::now());
                    deliver(&app, &ctx, notices);
                    if last_refresh.elapsed() >= REFRESH_SPEND {
                        last_refresh = Instant::now();
                        if let Some(b) = ctx.backend.get() {
                            if let Some(usd) = b.cost_since(&today_start_utc()).await {
                                let notices = ctx
                                    .activity
                                    .lock()
                                    .expect("activity poisoned")
                                    .refresh_spent_today(usd);
                                deliver(&app, &ctx, notices);
                            }
                        }
                    }
                    tray::refresh_status(&app, &ctx);
                }
            }
        }
    });
}

/// Local midnight as an RFC 3339 UTC timestamp.
pub fn today_start_utc() -> String {
    let midnight = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists");
    midnight
        .and_local_timezone(Local)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Is the dashboard in front of the user right now?
fn window_in_front(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
    })
}

fn deliver(app: &AppHandle, ctx: &Ctx, notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let s = ctx.settings();
    let may_interrupt = !s.notify_only_when_away || !window_in_front(app);
    for notice in notices {
        let (title, body) = match notice {
            Notice::TurnFinished {
                agent,
                project,
                duration,
                cost_usd,
                preview,
                idle,
                ..
            } if s.notify_turn_end && may_interrupt => {
                let title = if idle {
                    format!("{agent} may be waiting on you")
                } else {
                    format!("{agent} has finished")
                };
                let mut body = format!("{project} · {}", format_duration(duration));
                if cost_usd >= 0.005 {
                    body.push_str(&format!(" · ${cost_usd:.2}"));
                } else if cost_usd > 0.0 {
                    body.push_str(" · <$0.01");
                }
                if let Some(p) = preview.filter(|p| !p.is_empty()) {
                    body.push_str(&format!("\n“{p}”"));
                }
                (title, body)
            }
            Notice::NewSession { agent, project, .. } if s.notify_new_session && may_interrupt => {
                (format!("New {agent} session"), project)
            }
            Notice::BudgetExceeded {
                spent_usd,
                budget_usd,
            } => (
                "Daily budget reached".to_owned(),
                format!("Estimated spend today is ${spent_usd:.2} (budget ${budget_usd:.2})."),
            ),
            _ => continue,
        };
        show(app, &title, &body);
    }
}

pub fn show(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        warn!("Could not show a notification: {e}");
    }
}
