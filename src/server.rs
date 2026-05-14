use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde_json::json;
use std::net::SocketAddr;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, info, warn};

use crate::{dashboard::dashboard_html, event::TraceEvent};

#[derive(Clone)]
pub struct AppState {
    pub tx: broadcast::Sender<TraceEvent>,
    pub watch_root: String,
    pub port: u16,
}

pub async fn serve(state: AppState) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], state.port));
    let port = state.port;

    let cors = CorsLayer::new().allow_origin(Any);

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .layer(cors)
        .with_state(state);

    info!("Dashboard: http://127.0.0.1:{port}/");
    info!("WebSocket: ws://127.0.0.1:{port}/ws");
    info!("Health:    http://127.0.0.1:{port}/health");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
    Html(dashboard_html(state.port))
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    // Reject connections that carry a non-localhost Origin header to prevent
    // cross-site WebSocket hijacking of local trace data.  Requests without an
    // Origin header (e.g. from CLI tools) are allowed through.
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
    // Send a connection banner so clients know the stream is live.
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

    let mut rx = state.tx.subscribe();

    loop {
        match rx.recv().await {
            Ok(event) => {
                match serde_json::to_string(&event) {
                    Ok(json) => {
                        if socket.send(Message::Text(json)).await.is_err() {
                            debug!("WebSocket client disconnected");
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to serialise event: {e}");
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("WebSocket client lagged by {n} messages");
                // Continue serving — do not crash the producer.
            }
            Err(broadcast::error::RecvError::Closed) => {
                break;
            }
        }
    }
}
