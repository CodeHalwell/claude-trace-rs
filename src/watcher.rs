//! Live filesystem watching: a notify loop that feeds the ingestion
//! [`Engine`](crate::ingest::Engine).

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::{
    event::TraceEvent,
    ingest::Engine,
    sources::{self, AgentSource, WatchRoot},
    state::SessionStore,
};

/// How long a document/database must be quiet before it is re-parsed.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Longest a queued unit waits when its writer never pauses.
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);
/// How often to look for agent directories that did not exist at start-up.
const DISCOVER_EVERY: Duration = Duration::from_secs(30);

/// Configuration for the watcher's startup behaviour.
#[derive(Debug, Clone, Default)]
pub struct WatcherOptions {
    /// Replay every record already on disk before tailing. Without it, files
    /// with a saved checkpoint resume from it and unseen files start at EOF.
    pub backfill: bool,
    /// Periodically add default agent directories that appear after start-up
    /// (an agent installed while the tracer runs). `Some(filter)` enables it,
    /// restricted to `filter`'s sources when that is `Some`.
    pub discover: Option<Option<HashSet<AgentSource>>>,
}

/// Shared, live view of the roots being watched (for `/health` and the
/// agents overview).
pub type SharedRoots = Arc<RwLock<Vec<WatchRoot>>>;

pub struct SessionWatcher {
    engine: Engine,
    options: WatcherOptions,
    shared_roots: SharedRoots,
}

impl SessionWatcher {
    /// Watch several roots; each root may pin a forced agent source.
    pub fn multi(
        roots: Vec<WatchRoot>,
        tx: broadcast::Sender<TraceEvent>,
        store: SessionStore,
        options: WatcherOptions,
    ) -> Self {
        let shared_roots = Arc::new(RwLock::new(roots.clone()));
        Self {
            engine: Engine::new(roots, store, Some(tx)),
            options,
            shared_roots,
        }
    }

    /// Publish the watched roots through `shared` (and keep it updated).
    pub fn with_shared_roots(mut self, shared: SharedRoots) -> Self {
        if let Ok(mut g) = shared.write() {
            *g = self.engine.roots().to_vec();
        }
        self.shared_roots = shared;
        self
    }

    /// Run forever on the current (blocking) thread.
    pub fn run(mut self) -> anyhow::Result<()> {
        for root in self.engine.roots() {
            info!(
                "Seeding {} (source={}, backfill={})",
                root.path.display(),
                root.source.map(|s| s.as_str()).unwrap_or("auto-detect"),
                self.options.backfill
            );
        }
        let stats = self.engine.scan(self.options.backfill);
        info!(
            "Seeded {} unit(s), {} new or updated event(s)",
            self.engine.tracked_units(),
            stats.emitted
        );

        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher = RecommendedWatcher::new(fs_tx, Config::default())?;
        let mut watched: HashSet<PathBuf> = HashSet::new();
        for root in self.engine.roots().to_vec() {
            watch_root(&mut watcher, &mut watched, &root.path);
        }

        // Debounce only on changes to queued units: JSONL appends and
        // unrelated files elsewhere under a root must not hold them back.
        let mut last_change = Instant::now();
        let mut pending_since: Option<Instant> = None;
        let mut last_discover = Instant::now();
        loop {
            match fs_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(event)) => {
                    let removed = matches!(event.kind, EventKind::Remove(_));
                    if removed || matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_))
                    {
                        for path in event.paths {
                            let queued = if removed {
                                self.engine.path_removed(&path)
                            } else {
                                self.engine.path_changed(&path)
                            };
                            if queued {
                                last_change = Instant::now();
                                pending_since.get_or_insert(last_change);
                            }
                        }
                    }
                }
                Ok(Err(e)) => error!("Filesystem watch error: {e}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let overdue = pending_since.is_some_and(|t| t.elapsed() >= MAX_DEBOUNCE);
            if self.engine.has_pending() && (last_change.elapsed() >= DEBOUNCE || overdue) {
                self.engine.flush();
                pending_since = None;
            }
            if last_discover.elapsed() >= DISCOVER_EVERY {
                last_discover = Instant::now();
                // Configured roots that did not exist at start-up (nothing
                // could be watched there) are picked up once they appear.
                for root in self.engine.roots().to_vec() {
                    if root.path.is_dir() && !watched.iter().any(|w| root.path.starts_with(w)) {
                        info!("Watch root {} now exists", root.path.display());
                        self.engine.scan_root(&root, true);
                        watch_root(&mut watcher, &mut watched, &root.path);
                    }
                }
                if let Some(only) = &self.options.discover {
                    for root in sources::default_roots() {
                        if let (Some(only), Some(src)) = (only, root.source) {
                            if !only.contains(&src) {
                                continue;
                            }
                        }
                        if self.engine.roots().iter().any(|r| r.path == root.path) {
                            continue;
                        }
                        info!(
                            "New agent directory detected: {} ({})",
                            root.path.display(),
                            root.source.map(|s| s.as_str()).unwrap_or("auto")
                        );
                        self.engine.add_root(root.clone());
                        // A directory that just appeared only holds new
                        // activity, so ingest all of it.
                        self.engine.scan_root(&root, true);
                        watch_root(&mut watcher, &mut watched, &root.path);
                        if let Ok(mut g) = self.shared_roots.write() {
                            *g = self.engine.roots().to_vec();
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Watch `path` once. It only counts as watched after registration
/// succeeds, so a failure (permissions, an inotify limit) is retried on the
/// next discovery pass rather than silently leaving the root unwatched.
fn watch_root(watcher: &mut RecommendedWatcher, watched: &mut HashSet<PathBuf>, path: &Path) {
    if !path.is_dir() || watched.contains(path) {
        return;
    }
    match watcher.watch(path, RecursiveMode::Recursive) {
        Ok(()) => {
            watched.insert(path.to_path_buf());
            info!("Watching {} for changes", path.display());
        }
        Err(e) => warn!("Could not watch {} (will retry): {e}", path.display()),
    }
}
