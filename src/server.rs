use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, info, warn};

use crate::{dashboard::dashboard_html, event::TraceEvent, state::SessionStore};

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

    let cors = CorsLayer::new().allow_origin(Any);

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .route("/api/sessions", get(api_sessions))
        .route("/api/sessions/:id", get(api_session_detail))
        .route("/api/sessions/:id/events", get(api_session_events))
        .route("/api/snapshot", get(api_snapshot))
        .layer(cors)
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

    // 2) Send an initial snapshot so the dashboard renders sessions and recent
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

    // 3) Stream live events.
    let mut rx = state.tx.subscribe();
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
