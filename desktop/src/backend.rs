//! Starting the tracer inside the app, or attaching to one that is already
//! running (the CLI's background service), plus the live event feed the
//! notification loop consumes.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use claude_trace_rs::{
    db::{self, Db},
    event::TraceEvent,
    expand_tilde, runtime,
    sources::{AgentSource, WatchRoot},
};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The tracer runs inside this process.
    Embedded,
    /// Another claude-trace-rs server already answered on the port.
    Attached,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Embedded => "embedded",
            Mode::Attached => "attached",
        }
    }
}

/// What is running once start-up has finished.
pub struct Backend {
    pub mode: Mode,
    pub port: u16,
    /// The database this app knows about. In attached mode it is the one the
    /// settings name, which is what the server uses unless it was started
    /// with a different `--db`.
    pub db_path: PathBuf,
    /// Version reported by the server (ours when embedded).
    pub server_version: String,
    /// The embedded tracer, when there is one.
    pub tracer: Option<runtime::Tracer>,
}

impl Backend {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// Spend since `since` (RFC 3339), from the database or the server.
    pub async fn cost_since(&self, since: &str) -> Option<f64> {
        match &self.tracer {
            Some(t) => {
                let db = t.db.clone();
                let since = since.to_owned();
                tauri::async_runtime::spawn_blocking(move || db.cost_since(&since).ok())
                    .await
                    .ok()
                    .flatten()
            }
            None => {
                let path = format!("/api/db/cost?since={}", encode_query(since));
                runtime::get_local_json(self.port, &path)
                    .await
                    .and_then(|v| v.get("cost_usd").and_then(|c| c.as_f64()))
            }
        }
    }
}

/// Parse the settings' agent filter. Unknown ids are reported, not ignored
/// silently, so a typo does not quietly trace nothing.
pub fn parse_only(ids: &[String]) -> Result<Option<HashSet<AgentSource>>, String> {
    if ids.is_empty() {
        return Ok(None);
    }
    let mut out = HashSet::new();
    let mut unknown = Vec::new();
    for id in ids {
        match AgentSource::parse(id.trim()) {
            Some(s) => {
                out.insert(s);
            }
            None => unknown.push(id.clone()),
        }
    }
    if unknown.is_empty() {
        Ok(Some(out))
    } else {
        let known: Vec<&str> = AgentSource::all_known()
            .iter()
            .map(|s| s.as_str())
            .collect();
        Err(format!(
            "Unknown agent id(s): {}. Known ids: {}",
            unknown.join(", "),
            known.join(", ")
        ))
    }
}

/// `path` or `path=agent-id` (the suffix only counts when it names an agent,
/// so paths containing `=` still work).
pub fn parse_extra_root(spec: &str) -> Result<(PathBuf, Option<AgentSource>), String> {
    let spec = spec.trim();
    if let Some((path, id)) = spec.rsplit_once('=') {
        if let Some(src) = AgentSource::parse(id.trim()) {
            return Ok((expand_tilde(path.trim()), Some(src)));
        }
    }
    if spec.is_empty() {
        return Err("empty folder entry".into());
    }
    Ok((expand_tilde(spec), None))
}

/// Validate settings before they are saved.
pub fn validate(s: &Settings) -> Result<(), String> {
    if s.port == 0 {
        return Err("The port must be between 1 and 65535.".into());
    }
    parse_only(&s.only)?;
    for r in &s.extra_roots {
        parse_extra_root(r)?;
    }
    if let Some(b) = s.daily_budget_usd {
        if !b.is_finite() || b < 0.0 {
            return Err("The daily budget must be a positive amount.".into());
        }
    }
    Ok(())
}

pub fn db_path(settings: &Settings) -> PathBuf {
    settings
        .db_path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .map(expand_tilde)
        .unwrap_or_else(db::default_db_path)
}

/// The roots the embedded tracer watches: every agent directory found on
/// disk (filtered by `only`), plus any extra folders.
pub fn watch_roots(settings: &Settings) -> Result<Vec<WatchRoot>, String> {
    let only = parse_only(&settings.only)?;
    let mut roots = runtime::resolve_roots(&[], None, only.clone(), false);
    for spec in &settings.extra_roots {
        let (path, source) = parse_extra_root(spec)?;
        if let (Some(only), Some(src)) = (&only, source) {
            if !only.contains(&src) {
                continue;
            }
        }
        if roots.iter().any(|r| r.path == path) {
            continue;
        }
        roots.push(WatchRoot {
            path,
            source,
            allowed_sources: source.is_none().then(|| only.clone()).flatten(),
        });
    }
    Ok(roots)
}

/// Start the tracer in-process and serve the dashboard on loopback. Falls
/// back to a free port when the configured one is taken by something that
/// is not a claude-trace-rs server.
pub async fn start_embedded(settings: &Settings) -> anyhow::Result<Backend> {
    let roots = watch_roots(settings).map_err(anyhow::Error::msg)?;
    let only = parse_only(&settings.only).map_err(anyhow::Error::msg)?;
    let db_path = db_path(settings);

    // Import history on the very first run only; later runs resume from
    // their checkpoints (and pick up sessions written while stopped).
    let first_run = {
        let p = db_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            Db::open(&p).map(|db| {
                db.tracked_files_by_source()
                    .map(|v| v.is_empty())
                    .unwrap_or(true)
            })
        })
        .await??
    };
    let backfill = settings.import_history && first_run;
    info!(
        "Starting embedded tracer: {} root(s), database {}, backfill={backfill}",
        roots.len(),
        db_path.display()
    );

    let cfg = runtime::TracerConfig {
        roots,
        db_path: Some(db_path.clone()),
        backfill,
        discover: Some(only),
        // Default roots exist by construction; extra folders that do not
        // exist yet are watched once they appear rather than created.
        create_missing_roots: false,
        ..Default::default()
    };
    let tracer =
        tauri::async_runtime::spawn_blocking(move || runtime::Tracer::start(cfg)).await??;

    let listener = match runtime::bind_local(settings.port).await {
        Ok(l) => l,
        Err(e) => {
            warn!(
                "Port {} is unavailable ({e}); using a free port instead",
                settings.port
            );
            runtime::bind_local(0).await?
        }
    };
    let port = listener.local_addr()?.port();
    let serving = tracer.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serving.serve(listener).await {
            tracing::error!("Dashboard server stopped: {e}");
        }
    });
    Ok(Backend {
        mode: Mode::Embedded,
        port,
        db_path,
        server_version: claude_trace_rs::VERSION.to_owned(),
        tracer: Some(tracer),
    })
}

/// Forward live events into `out`: straight from the broadcast channel when
/// embedded, over the server's WebSocket (reconnecting) when attached.
pub fn spawn_feed(backend: &Backend, out: mpsc::Sender<TraceEvent>) {
    match &backend.tracer {
        Some(t) => {
            let mut rx = t.tx.subscribe();
            tauri::async_runtime::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            if out.send(ev).await.is_err() {
                                break;
                            }
                        }
                        Err(RecvError::Lagged(n)) => debug!("Notification feed lagged by {n}"),
                        Err(RecvError::Closed) => break,
                    }
                }
            });
        }
        None => {
            let port = backend.port;
            tauri::async_runtime::spawn(async move { websocket_feed(port, out).await });
        }
    }
}

async fn websocket_feed(port: u16, out: mpsc::Sender<TraceEvent>) {
    let url = format!("ws://127.0.0.1:{port}/ws");
    let mut backoff = Duration::from_secs(1);
    loop {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((mut ws, _)) => {
                info!("Connected to the trace server's live feed");
                backoff = Duration::from_secs(1);
                while let Some(msg) = ws.next().await {
                    let Ok(msg) = msg else { break };
                    let Ok(text) = msg.into_text() else { continue };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    // Skip the banner and snapshot; events have no `type`.
                    if value.get("type").is_some() && value.get("session_id").is_none() {
                        continue;
                    }
                    match serde_json::from_value::<TraceEvent>(value) {
                        Ok(mut ev) => {
                            if ev.message.is_none() {
                                ev.hydrate();
                            }
                            if out.send(ev).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => debug!("Ignoring unparseable feed message: {e}"),
                    }
                }
                warn!("Live feed disconnected; reconnecting");
            }
            Err(e) => debug!("Live feed unavailable ({e}); retrying in {backoff:?}"),
        }
        if out.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn encode_query(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_roots_parse_with_and_without_agent() {
        let (p, s) = parse_extra_root("/data/logs=codex").unwrap();
        assert_eq!(p, PathBuf::from("/data/logs"));
        assert_eq!(s, Some(AgentSource::Codex));
        let (p, s) = parse_extra_root("/data/a=b").unwrap();
        assert_eq!(p, PathBuf::from("/data/a=b"));
        assert_eq!(s, None);
        assert!(parse_extra_root("  ").is_err());
    }

    #[test]
    fn unknown_agents_are_rejected() {
        assert!(parse_only(&["codex".into(), "claude-code".into()])
            .unwrap()
            .is_some());
        let err = parse_only(&["codx".into()]).unwrap_err();
        assert!(err.contains("codx") && err.contains("codex"), "{err}");
        let s = Settings {
            daily_budget_usd: Some(-1.0),
            ..Default::default()
        };
        assert!(validate(&s).is_err());
    }

    #[test]
    fn query_encoding() {
        assert_eq!(
            encode_query("2026-09-29T00:00:00+01:00"),
            "2026-09-29T00%3A00%3A00%2B01%3A00"
        );
    }
}
