//! `claude-trace-rs` — local-first tracing for terminal and IDE coding agents.
//!
//! The crate is both the `claude-trace-rs` CLI and a library the desktop app
//! embeds. The moving parts:
//!
//! - [`sources`] — per-agent detection and adapters that normalise each
//!   agent's on-disk format into [`event::TraceEvent`]s carrying a canonical
//!   [`message::Message`].
//! - [`ingest`] — the engine that tails JSONL, re-parses documents, rebuilds
//!   multi-file stores and polls SQLite databases.
//! - [`watcher`] / [`loader`] — live and one-shot drivers for the engine.
//! - [`state`] / [`db`] — in-memory aggregates and the persistent SQLite
//!   trace database.
//! - [`server`] / [`dashboard`] — the HTTP/WebSocket API and built-in UI.
//! - [`export`] — training-dataset exporters.
//! - [`runtime`] — one call to start everything, shared by the CLI and the
//!   desktop app.

pub mod dashboard;
pub mod db;
pub mod event;
pub mod export;
pub mod ingest;
pub mod loader;
pub mod message;
pub mod pricing;
pub mod runtime;
pub mod server;
pub mod service;
pub mod sources;
pub mod state;
pub mod watcher;

/// Crate version, reported by `/health` so clients can recognise the server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Expand a leading `~/` or bare `~` in a path string to the user's home.
pub fn expand_tilde(raw: &str) -> std::path::PathBuf {
    use std::path::PathBuf;
    if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_owned());
        let rest = raw.strip_prefix("~/").unwrap_or("");
        if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest)
        }
    } else {
        PathBuf::from(raw)
    }
}
