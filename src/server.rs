use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{from_fn, Next},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tokio::sync::broadcast;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{debug, info, warn};

use crate::{
    dashboard::dashboard_html,
    event::TraceEvent,
    export::{self, ExportFormat, SessionExport},
    state::SessionStore,
};

#[derive(Clone)]
pub struct AppState {
    pub tx: broadcast::Sender<TraceEvent>,
    pub watch_root: String,
    pub port: u16,
    pub store: SessionStore,
}

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], state.port));
    let port = state.port;

    // Restrict CORS to localhost origins.  The whole purpose of this dashboard
    // is to serve a same-origin local UI; without this gate, any third-party
    // page a user happens to be browsing could XHR/fetch from
    // http://127.0.0.1:<port>/api/* and exfiltrate trace data.
    let cors = CorsLayer::new().allow_origin(AllowOrigin::predicate(is_local_origin));

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/:id", get(api_session_detail))
        .route("/api/sessions/:id/events", get(api_session_events))
        .route("/api/sessions/:id/export", get(api_session_export))
        .route("/api/export", get(api_export_many))
        .route("/api/snapshot", get(api_snapshot))
        .layer(cors)
        .layer(from_fn(reject_cross_origin_api))
        .with_state(state);

    info!("Dashboard: http://127.0.0.1:{port}/");
    info!("WebSocket: ws://127.0.0.1:{port}/ws");
    info!("API:       http://127.0.0.1:{port}/api/sessions");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
    Html(dashboard_html(state.port))
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "watch_root": state.watch_root,
        "sessions": state.store.sessions().len(),
        "total_events": state.store.total_events(),
    }))
}

async fn api_sessions(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "watch_root": state.watch_root,
        "sessions": state.store.sessions(),
    }))
}

async fn api_session_detail(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    match state.store.session(&id) {
        Some(s) => Json(s).into_response(),
        None => (StatusCode::NOT_FOUND, "Unknown session").into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    /// Optional cap on the number of events returned (most recent N).
    limit: Option<usize>,
}

async fn api_session_events(
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut events = state.store.session_events(&id);
    if let Some(limit) = q.limit {
        if events.len() > limit {
            let drop = events.len() - limit;
            events.drain(0..drop);
        }
    }
    Json(json!({ "session_id": id, "events": events }))
}

#[derive(Debug, Deserialize)]
struct SnapshotQuery {
    /// Maximum number of recent global events to include.
    events: Option<usize>,
}

async fn api_snapshot(
    Query(q): Query<SnapshotQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let n = q.events.unwrap_or(500);
    Json(state.store.snapshot(n))
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    /// One of: messages | openai | sharegpt | jsonl | markdown | huggingface.
    #[serde(default = "default_export_format")]
    format: ExportFormat,
}

fn default_export_format() -> ExportFormat {
    ExportFormat::Messages
}

async fn api_session_export(
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(stats) = state.store.session(&id) else {
        return (StatusCode::NOT_FOUND, "Unknown session").into_response();
    };
    let events = state.store.session_events(&id);
    let exp = SessionExport {
        stats: &stats,
        events: events.as_slice(),
    };
    let body = export::render_session(&exp, q.format);
    let filename = format!("{}.{}", short_filename(&id), q.format.extension());
    download_response(body, q.format.mime(), &filename)
}

#[derive(Debug, Deserialize)]
struct ExportManyQuery {
    #[serde(default = "default_export_format")]
    format: ExportFormat,
    /// Comma-separated list of session IDs to include. If omitted, every session
    /// in the store is exported.
    sessions: Option<String>,
}

async fn api_export_many(
    Query(q): Query<ExportManyQuery>,
    State(state): State<AppState>,
) -> Response {
    let want: Option<std::collections::HashSet<String>> = q.sessions.as_ref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_owned())
            .collect()
    });

    let sessions = state.store.sessions();
    let stats_filtered: Vec<_> = sessions
        .into_iter()
        .filter(|s| match &want {
            Some(w) => w.contains(&s.id),
            None => true,
        })
        .collect();

    if stats_filtered.is_empty() {
        return (StatusCode::NOT_FOUND, "No matching sessions").into_response();
    }

    let pairs: Vec<_> = stats_filtered
        .iter()
        .map(|s| (s.clone(), state.store.session_events(&s.id)))
        .collect();
    let exports: Vec<SessionExport<'_>> = pairs
        .iter()
        .map(|(s, e)| SessionExport {
            stats: s,
            events: e.as_slice(),
        })
        .collect();
    let body = export::render_many(&exports, q.format);
    let filename = format!("claude-trace-{}.{}", chrono::Utc::now().format("%Y%m%dT%H%M%S"), q.format.extension());
    download_response(body, q.format.mime(), &filename)
}

fn download_response(body: String, mime: &'static str, filename: &str) -> Response {
    use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    (
        [
            (CONTENT_TYPE, mime.to_owned()),
            (CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename)),
        ],
        body,
    )
        .into_response()
}

fn short_filename(id: &str) -> String {
    id.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-")
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    // Reject non-localhost Origins (CSWSH defence). Origin-less connections
    // (CLI clients) are still permitted.
    if let Some(origin) = headers.get(header::ORIGIN) {
        if let Ok(origin_str) = origin.to_str() {
            let allowed = origin_str.starts_with("http://127.0.0.1")
                || origin_str.starts_with("http://localhost")
                || origin_str.starts_with("https://127.0.0.1")
                || origin_str.starts_with("https://localhost");
            if !allowed {
                return (
                    StatusCode::FORBIDDEN,
                    "Forbidden: connections from non-local origins are not permitted",
                )
                    .into_response();
            }
        }
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
        .into_response()
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    // 1) Send a connection banner.
    let banner = json!({
        "type": "connected",
        "watch_root": state.watch_root,
        "message": "Streaming Claude Code traces in real time."
    });
    if socket
        .send(Message::Text(banner.to_string()))
        .await
        .is_err()
    {
        return;
    }

    // 2) Subscribe BEFORE taking the snapshot to close the race window where
    //    events could be ingested between snapshot creation and subscription
    //    and silently dropped. The client de-duplicates events that appear in
    //    both the snapshot and the live stream by (session_id, line_index).
    let mut rx = state.tx.subscribe();

    // 3) Send an initial snapshot so the dashboard renders sessions and recent
    //    events immediately, even if no new events are flowing.
    let snapshot = state.store.snapshot(500);
    let snapshot_msg = json!({
        "type": "snapshot",
        "sessions": snapshot.sessions,
        "events": snapshot.events,
        "total_events": snapshot.total_events,
    });
    if socket
        .send(Message::Text(snapshot_msg.to_string()))
        .await
        .is_err()
    {
        return;
    }

    // 4) Stream live events.
    loop {
        match rx.recv().await {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => {
                    if socket.send(Message::Text(json)).await.is_err() {
                        debug!("WebSocket client disconnected");
                        break;
                    }
                }
                Err(e) => warn!("Failed to serialise event: {e}"),
            },
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("WebSocket client lagged by {n} messages");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// CORS predicate: accept Origin headers from any localhost port over http(s).
fn is_local_origin(origin: &HeaderValue, _req_headers: &axum::http::request::Parts) -> bool {
    let Ok(s) = origin.to_str() else { return false };
    s.starts_with("http://127.0.0.1")
        || s.starts_with("http://localhost")
        || s.starts_with("https://127.0.0.1")
        || s.starts_with("https://localhost")
        || s.starts_with("http://[::1]")
        || s.starts_with("https://[::1]")
}

/// Middleware: for /api/* requests that carry an Origin header, reject if the
/// origin isn't local. Requests without an Origin header (curl, server-to-server)
/// pass through. The same-origin dashboard never sends Origin on its own fetch
/// calls, so this only kicks in when a third-party page tries to read.
async fn reject_cross_origin_api(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") {
        if let Some(origin) = req.headers().get(header::ORIGIN) {
            if let Ok(s) = origin.to_str() {
                let local = s.starts_with("http://127.0.0.1")
                    || s.starts_with("http://localhost")
                    || s.starts_with("https://127.0.0.1")
                    || s.starts_with("https://localhost")
                    || s.starts_with("http://[::1]")
                    || s.starts_with("https://[::1]");
                if !local {
                    return (
                        StatusCode::FORBIDDEN,
                        "Forbidden: API access from non-local origins is not permitted",
                    )
                        .into_response();
                }
            }
        }
    }
    next.run(req).await
}
