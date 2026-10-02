//! Start the whole tracer — database, in-memory store, filesystem watcher and
//! HTTP server — with one call. Shared by the CLI's `serve` command and the
//! desktop app so both behave identically.

use std::{
    collections::HashSet,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use tokio::{net::TcpListener, sync::broadcast};
use tracing::{info, warn};

use crate::{
    db::{self, Db},
    event::TraceEvent,
    pricing, server,
    sources::{AgentSource, WatchRoot},
    state::SessionStore,
    watcher::{self, SharedRoots},
};

/// Everything needed to start tracing.
#[derive(Debug, Clone)]
pub struct TracerConfig {
    pub roots: Vec<WatchRoot>,
    /// SQLite database file; `None` uses the platform default.
    pub db_path: Option<PathBuf>,
    /// Replay everything on disk at start-up.
    pub backfill: bool,
    /// Broadcast buffer per live subscriber.
    pub channel_capacity: usize,
    /// Keep looking for newly installed agents (see
    /// [`watcher::WatcherOptions::discover`]).
    pub discover: Option<Option<HashSet<AgentSource>>>,
    /// Create explicit roots that do not exist yet.
    pub create_missing_roots: bool,
}

impl Default for TracerConfig {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            db_path: None,
            backfill: false,
            channel_capacity: 1024,
            discover: None,
            create_missing_roots: true,
        }
    }
}

/// A running tracer. Cheap to clone.
#[derive(Clone)]
pub struct Tracer {
    pub tx: broadcast::Sender<TraceEvent>,
    pub store: SessionStore,
    pub db: Db,
    pub roots: SharedRoots,
    pub server_key: Option<Arc<Vec<u8>>>,
}

impl Tracer {
    /// Open the database, seed the store from it and start the watcher on a
    /// background thread. Returns once the watcher thread is spawned; the
    /// initial scan continues in the background.
    pub fn start(cfg: TracerConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(
            cfg.channel_capacity > 0,
            "channel capacity must be at least 1"
        );
        if cfg.create_missing_roots {
            for root in &cfg.roots {
                if !root.path.exists() {
                    info!(
                        "Watch root {} does not exist; creating it",
                        root.path.display()
                    );
                    if let Err(e) = std::fs::create_dir_all(&root.path) {
                        warn!("Could not create {}: {e}", root.path.display());
                    }
                }
            }
        }

        if let Some(p) = pricing::default_overrides_path() {
            match pricing::load_overrides(&p) {
                Ok(0) => {}
                Ok(n) => info!("Loaded {n} pricing override(s) from {}", p.display()),
                Err(e) => warn!("Ignoring pricing overrides in {}: {e}", p.display()),
            }
        }

        let db_path = cfg.db_path.clone().unwrap_or_else(db::default_db_path);
        let database = Db::open(&db_path)?;
        info!("Trace database: {}", database.path().display());
        database.build_usage_in_background();
        let store = SessionStore::with_db(database.clone());
        match database.load_sessions() {
            Ok(sessions) => {
                info!("Loaded {} session(s) from the database", sessions.len());
                store.seed_sessions(sessions);
            }
            Err(e) => warn!("Could not load sessions from the database: {e}"),
        }

        let (tx, _) = broadcast::channel::<TraceEvent>(cfg.channel_capacity);
        let roots: SharedRoots = Arc::new(RwLock::new(cfg.roots.clone()));

        let watcher = watcher::SessionWatcher::multi(
            cfg.roots,
            tx.clone(),
            store.clone(),
            watcher::WatcherOptions {
                backfill: cfg.backfill,
                discover: cfg.discover,
            },
        )
        .with_shared_roots(roots.clone());
        std::thread::Builder::new()
            .name("trace-watcher".into())
            .spawn(move || {
                if let Err(e) = watcher.run() {
                    tracing::error!("Watcher exited with error: {e}");
                }
            })?;

        let server_key = match load_or_create_server_key() {
            Ok(k) => Some(Arc::new(k)),
            Err(e) => {
                warn!("No server key ({e}); the desktop app will not attach to this server");
                None
            }
        };

        Ok(Self {
            tx,
            store,
            db: database,
            roots,
            server_key,
        })
    }

    pub fn app_state(&self, port: u16) -> server::AppState {
        server::AppState {
            tx: self.tx.clone(),
            port,
            store: self.store.clone(),
            db: self.db.clone(),
            roots: self.roots.clone(),
            server_key: self.server_key.clone(),
        }
    }

    /// Serve the dashboard and API on an already-bound listener.
    pub async fn serve(&self, listener: TcpListener) -> anyhow::Result<()> {
        let port = listener.local_addr()?.port();
        server::serve_listener(listener, self.app_state(port)).await
    }
}

/// Turn the CLI flags into a concrete set of watch roots.
///
/// - Any explicit `--watch-root` entries are always included (tagged with
///   `--source` if given, else auto-detect per file within any `--only` filter).
/// - Unless `--no-default-roots`, every known agent log directory that exists
///   on disk is added (tagged with its agent), filtered by `--only`.
pub fn resolve_roots(
    explicit: &[String],
    forced_source: Option<crate::sources::AgentSource>,
    only: Option<std::collections::HashSet<crate::sources::AgentSource>>,
    no_default_roots: bool,
) -> Vec<crate::sources::WatchRoot> {
    let mut roots: Vec<crate::sources::WatchRoot> = Vec::new();

    for raw in explicit {
        let path = crate::expand_tilde(raw);
        // Honour --only for explicit roots too, either by dropping a forced
        // source outside the allow-list or by carrying the allow-list forward
        // for per-file auto-detection.
        if let (Some(only), Some(src)) = (&only, forced_source) {
            if !only.contains(&src) {
                continue;
            }
        }
        roots.push(crate::sources::WatchRoot {
            path,
            source: forced_source,
            allowed_sources: forced_source.is_none().then(|| only.clone()).flatten(),
        });
    }

    if !no_default_roots {
        for r in crate::sources::default_roots() {
            if let Some(only) = &only {
                if let Some(src) = r.source {
                    if !only.contains(&src) {
                        continue;
                    }
                }
            }
            // Avoid double-adding a directory the user already listed.
            if roots.iter().any(|e| e.path == r.path) {
                continue;
            }
            roots.push(r);
        }
    }

    // Fallback: if nothing was specified and nothing exists on disk yet, use
    // the historical Claude Code default so `claude-trace-rs` with no args
    // behaves exactly as before (and creates the directory).
    //
    // This only applies when the user has not narrowed the source set: with
    // `--only codex` or `--no-default-roots` an empty result is the honest
    // answer, and the caller reports "no watch roots" rather than silently
    // watching (and creating) a Claude Code directory the user excluded.
    let claude_code_wanted = only
        .as_ref()
        .map(|o| o.contains(&crate::sources::AgentSource::ClaudeCode))
        .unwrap_or(true);
    if roots.is_empty() && explicit.is_empty() && !no_default_roots && claude_code_wanted {
        roots.push(crate::sources::WatchRoot {
            path: crate::expand_tilde("~/.claude/projects"),
            source: Some(crate::sources::AgentSource::ClaudeCode),
            allowed_sources: None,
        });
    }

    roots
}

/// Bind the loopback interface on `port`.
pub async fn bind_local(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await
}

/// A claude-trace-rs server found answering on a port.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub version: String,
    /// It proved knowledge of this user's server key, so it is ours and not
    /// another user's (or any other) process that happens to hold the port.
    pub verified: bool,
}

/// Is a claude-trace-rs server already answering on `port`? Used by the
/// desktop app to attach to a running background service instead of starting
/// a second watcher on the same database.
pub async fn probe_existing(port: u16) -> Option<ProbeResult> {
    let key = read_server_key();
    let challenge = random_hex(16);
    let v = get_local_json(port, &format!("/health?challenge={challenge}")).await?;
    if v.get("app").and_then(|a| a.as_str()) != Some("claude-trace-rs") {
        return None;
    }
    let verified = match (key, v.get("proof").and_then(|p| p.as_str())) {
        (Some(key), Some(proof)) => {
            constant_time_eq(health_proof(&key, &challenge).as_bytes(), proof.as_bytes())
        }
        _ => false,
    };
    Some(ProbeResult {
        version: v
            .get("version")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_owned(),
        verified,
    })
}

/// Where the per-user server key lives (next to the default database).
pub fn server_key_path() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("rs", "claude-trace", "claude-trace-rs")
        .map(|d| d.data_dir().join("server.key"))
}

fn read_server_key() -> Option<Vec<u8>> {
    let raw = std::fs::read_to_string(server_key_path()?).ok()?;
    let key = raw.trim();
    (key.len() >= 32).then(|| key.as_bytes().to_vec())
}

/// Read the per-user server key, creating it (readable by this user only)
/// on first use.
pub fn load_or_create_server_key() -> std::io::Result<Vec<u8>> {
    if let Some(k) = read_server_key() {
        return Ok(k);
    }
    let path = server_key_path()
        .ok_or_else(|| std::io::Error::other("no data directory for this user"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let key = random_hex(32);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(key.as_bytes())?;
            Ok(key.into_bytes())
        }
        // Another process created it first; use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read_server_key().ok_or(e),
        Err(e) => Err(e),
    }
}

/// HMAC-SHA256 of a health challenge under the server key, hex encoded.
pub fn health_proof(key: &[u8], challenge: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(b"claude-trace-rs health v1:");
    mac.update(challenge.as_bytes());
    hex(&mac.finalize().into_bytes())
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("OS random number generator");
    hex(&buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Minimal HTTP/1.1 GET of a small JSON endpoint on the loopback server.
/// Enough for the health probe and summary queries without pulling an HTTP
/// client into the crate; responses must carry a plain (non-chunked) body,
/// which axum's `Json` responses do.
pub async fn get_local_json(port: u16, path: &str) -> Option<serde_json::Value> {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = tokio::time::timeout(
        Duration::from_millis(600),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .ok()?
    .ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return None;
    }
    serde_json::from_str(body.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_proof_depends_on_key_and_challenge() {
        let a = health_proof(b"key-one-key-one-key-one-key-one!", "00ff");
        assert_eq!(a.len(), 64);
        assert_eq!(a, health_proof(b"key-one-key-one-key-one-key-one!", "00ff"));
        assert_ne!(a, health_proof(b"key-two-key-two-key-two-key-two!", "00ff"));
        assert_ne!(a, health_proof(b"key-one-key-one-key-one-key-one!", "00fe"));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert_eq!(random_hex(16).len(), 32);
        assert_ne!(random_hex(16), random_hex(16));
    }
}
