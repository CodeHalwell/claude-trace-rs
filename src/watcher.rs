use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::{event::TraceEvent, state::SessionStore};

/// Per-file reading state: tracks the last consumed byte offset and total
/// lines seen so far for stable line indexing.
#[derive(Debug, Default)]
pub struct FileState {
    /// Byte offset of the last character consumed from this file.
    pub offset: u64,
    /// Total number of non-empty lines consumed so far.
    pub line_count: usize,
}

/// Configuration for the watcher's startup behaviour.
#[derive(Debug, Clone, Copy)]
pub struct WatcherOptions {
    /// If true, replay every event already on disk into the store before
    /// switching to real-time tailing. Useful for getting context on sessions
    /// that started before the dashboard was launched.
    pub backfill: bool,
}

/// Watches a root directory for `.jsonl` file activity, tails newly appended
/// lines, broadcasts `TraceEvent` values onto a shared channel, and updates
/// an in-memory session store for late-joining clients.
pub struct SessionWatcher {
    watch_root: PathBuf,
    tx: broadcast::Sender<TraceEvent>,
    store: SessionStore,
    options: WatcherOptions,
}

impl SessionWatcher {
    pub fn new(
        watch_root: PathBuf,
        tx: broadcast::Sender<TraceEvent>,
        store: SessionStore,
        options: WatcherOptions,
    ) -> Self {
        Self {
            watch_root,
            tx,
            store,
            options,
        }
    }

    /// Walk every existing `.jsonl` file in the watch root. When `backfill`
    /// is true, process them from byte 0 so historical events populate the
    /// store. Otherwise seed offsets to current EOF so only new lines stream.
    fn seed_existing(&self, states: &mut HashMap<PathBuf, FileState>) {
        seed_dir(&self.watch_root, states, &self.store, &self.tx, self.options.backfill);
    }

    /// Start the watcher on a blocking thread (notify requires a sync context).
    pub fn run(self) -> anyhow::Result<()> {
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();

        info!(
            "Seeding existing JSONL files in {} (backfill={})",
            self.watch_root.display(),
            self.options.backfill
        );
        self.seed_existing(&mut states);
        info!(
            "Seeded {} file(s); store currently holds {} event(s)",
            states.len(),
            self.store.total_events()
        );

        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher = RecommendedWatcher::new(fs_tx, Config::default())?;
        watcher.watch(&self.watch_root, RecursiveMode::Recursive)?;
        info!("Watching {} for changes", self.watch_root.display());

        for res in fs_rx {
            match res {
                Ok(event) => self.handle_event(event, &mut states),
                Err(e) => error!("Filesystem watch error: {e}"),
            }
        }
        Ok(())
    }

    fn handle_event(&self, event: Event, states: &mut HashMap<PathBuf, FileState>) {
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            return;
        }
        for path in event.paths {
            if is_jsonl(&path) {
                debug!("Processing event for {}", path.display());
                process_file(&path, states, &self.tx, &self.store);
            }
        }
    }
}

fn seed_dir(
    dir: &Path,
    states: &mut HashMap<PathBuf, FileState>,
    store: &SessionStore,
    tx: &broadcast::Sender<TraceEvent>,
    backfill: bool,
) {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    seed_dir(&path, states, store, tx, backfill);
                } else if is_jsonl(&path) {
                    if backfill {
                        // Process from the start of the file.
                        states.insert(path.clone(), FileState::default());
                        process_file(&path, states, tx, store);
                    } else if let Ok(meta) = std::fs::metadata(&path) {
                        // Skip to EOF without emitting events; just count lines
                        // so future events get correct line indices.
                        let line_count = count_nonempty_lines(&path);
                        states.insert(
                            path,
                            FileState {
                                offset: meta.len(),
                                line_count,
                            },
                        );
                    }
                }
            }
        }
        Err(e) => warn!("Could not read watch root {}: {}", dir.display(), e),
    }
}

/// Read newly appended lines from `path` since the last known offset, parse
/// each non-empty line as JSON, and broadcast a `TraceEvent` for each one.
pub fn process_file(
    path: &Path,
    states: &mut HashMap<PathBuf, FileState>,
    tx: &broadcast::Sender<TraceEvent>,
    store: &SessionStore,
) {
    let session_fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();

    let state = states.entry(path.to_owned()).or_default();

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!("Could not open {}: {e}", path.display());
            return;
        }
    };

    // Detect file truncation or replacement.
    if let Ok(meta) = file.metadata() {
        if meta.len() < state.offset {
            warn!(
                "File {} was truncated or replaced (was {} bytes, now {}); resetting state",
                path.display(),
                state.offset,
                meta.len()
            );
            state.offset = 0;
            state.line_count = 0;
        }
    }

    if let Err(e) = file.seek(SeekFrom::Start(state.offset)) {
        warn!("Could not seek in {}: {e}", path.display());
        return;
    }

    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        line.clear();
        let line_start = match reader.stream_position() {
            Ok(p) => p,
            Err(e) => {
                warn!("stream_position error in {}: {e}", path.display());
                return;
            }
        };
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                // Back off partial writes (no terminating newline yet).
                if !line.ends_with('\n') {
                    state.offset = line_start;
                    return;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(trimmed) {
                    Ok(val) => {
                        let event =
                            TraceEvent::from_raw(&session_fallback, state.line_count, val);
                        store.ingest(&event);
                        if let Err(e) = tx.send(event) {
                            debug!("No active subscribers (send error): {e}");
                        }
                    }
                    Err(e) => warn!(
                        "Malformed JSON at line {} of {}: {e}",
                        state.line_count,
                        path.display()
                    ),
                }
                state.line_count += 1;
            }
            Err(e) => {
                warn!("Read error in {}: {e}", path.display());
                break;
            }
        }
    }

    match reader.stream_position() {
        Ok(pos) => state.offset = pos,
        Err(e) => warn!("Could not get file position for {}: {e}", path.display()),
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

/// Count the non-empty lines in a JSONL file without broadcasting any events.
fn count_nonempty_lines(path: &Path) -> usize {
    let Ok(f) = std::fs::File::open(path) else {
        return 0;
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tokio::sync::broadcast;

    fn make_tx() -> (broadcast::Sender<TraceEvent>, SessionStore) {
        let (tx, _rx) = broadcast::channel(256);
        (tx, SessionStore::new())
    }

    #[test]
    fn test_process_file_reads_new_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"hello"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{}}}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx, &store);

        let ev1 = rx.try_recv().expect("expected first event");
        let ev2 = rx.try_recv().expect("expected second event");
        assert_eq!(ev1.line_index, 0);
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
        assert_eq!(store.total_events(), 2);
    }

    #[test]
    fn test_process_file_incremental_reads() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"first"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx, &store);
        let ev1 = rx.try_recv().expect("expected first event");
        assert_eq!(ev1.line_index, 0);

        writeln!(file, r#"{{"type":"user","content":"second"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx, &store);
        let ev2 = rx.try_recv().expect("expected second event");
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_skips_malformed_json() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, "{{not valid json}}").unwrap();
        writeln!(file, r#"{{"type":"user","content":"ok"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx, &store);

        let ev = rx.try_recv().expect("expected one event for valid line");
        assert_eq!(ev.line_index, 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_skips_empty_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"a"}}"#).unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, r#"{{"type":"user","content":"b"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx, &store);

        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        assert_eq!(ev1.line_index, 0);
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_is_jsonl() {
        assert!(is_jsonl(Path::new("session.jsonl")));
        assert!(!is_jsonl(Path::new("session.json")));
        assert!(!is_jsonl(Path::new("session.txt")));
        assert!(!is_jsonl(Path::new("noext")));
    }

    #[test]
    fn test_process_file_resets_on_truncation() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"a"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","content":"b"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx, &store);
        rx.try_recv().unwrap();
        rx.try_recv().unwrap();
        assert!(rx.try_recv().is_err());

        {
            use std::io::Write as _;
            let mut fresh = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            writeln!(fresh, r#"{{"type":"user","content":"new"}}"#).unwrap();
            fresh.flush().unwrap();
        }

        process_file(&path, &mut states, &tx, &store);
        let ev = rx.try_recv().expect("should have emitted the new line after reset");
        assert_eq!(ev.line_index, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_partial_line_not_consumed() {
        use std::io::Write as _;
        let file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{\"type\":\"user\",\"content\":\"complete\"}\n").unwrap();
            f.write_all(b"{\"type\":\"user\"").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, &mut states, &tx, &store);
        let ev = rx.try_recv().expect("complete line should be emitted");
        assert_eq!(ev.line_index, 0);
        assert!(rx.try_recv().is_err());

        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b",\"content\":\"rest\"}\n").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, &mut states, &tx, &store);
        let ev2 = rx.try_recv().expect("completed line should be emitted");
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_count_nonempty_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, r#"{{"type":"assistant"}}"#).unwrap();
        file.flush().unwrap();
        assert_eq!(count_nonempty_lines(file.path()), 2);
    }

    #[test]
    fn test_process_file_routes_session_by_entry() {
        // Filename one-id, entry says another — store should key on the entry's sessionId.
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();

        writeln!(
            file,
            r#"{{"type":"user","sessionId":"real-sid","content":"hello"}}"#
        )
        .unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx, &store);
        assert!(store.session("real-sid").is_some());
        assert_eq!(store.sessions().len(), 1);
    }
}
