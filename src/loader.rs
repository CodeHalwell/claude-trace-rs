//! One-shot loader: ingest every trace under a set of roots into a
//! [`SessionStore`] without setting up a filesystem watcher.
//!
//! Used by the CLI `export` and `list` subcommands so they can produce a
//! consistent snapshot of historical session data and exit. It runs the same
//! [`Engine`](crate::ingest::Engine) as the live watcher, so both see
//! identical sessions.

use std::path::Path;

use crate::{ingest::Engine, sources::WatchRoot, state::SessionStore};

/// Load every trace file under `root`, auto-detecting the agent per file.
/// Returns the number of events ingested.
pub fn ingest_directory(root: &Path, store: &SessionStore) -> std::io::Result<usize> {
    ingest_roots(
        &[WatchRoot {
            path: root.to_path_buf(),
            source: None,
            allowed_sources: None,
        }],
        store,
    )
}

/// Load every trace under each root (each with an optional forced source).
/// Returns the number of events ingested.
pub fn ingest_roots(roots: &[WatchRoot], store: &SessionStore) -> std::io::Result<usize> {
    let mut engine = Engine::new(roots.to_vec(), store.clone(), None);
    Ok(engine.scan(true).emitted)
}
