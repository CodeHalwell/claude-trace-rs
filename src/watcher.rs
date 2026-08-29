use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::{
    event::TraceEvent,
    sources::{self, AgentSource, WatchRoot},
    state::SessionStore,
};

/// Per-file reading state: tracks the last consumed byte offset and total
/// lines seen so far for stable line indexing.
#[derive(Debug, Default)]
pub struct FileState {
    /// Byte offset of the last character consumed from this file.
    pub offset: u64,
    /// Total number of non-empty lines consumed so far.
    pub line_count: usize,
    /// Source detected for this file (set on first parse).
    pub source: Option<AgentSource>,
    /// For whole-file JSON array sources (Cline), the byte length we last
    /// ingested so we only re-parse when the file actually grew.
    pub last_len: u64,
}

/// Configuration for the watcher's startup behaviour.
#[derive(Debug, Clone, Copy)]
pub struct WatcherOptions {
    /// If true, replay every event already on disk into the store before
    /// switching to real-time tailing. Useful for getting context on sessions
    /// that started before the dashboard was launched.
    pub backfill: bool,
}

/// Watches one or more root directories for trace-file activity, tails newly
/// appended lines, broadcasts `TraceEvent` values onto a shared channel, and
/// updates an in-memory session store for late-joining clients.
pub struct SessionWatcher {
    roots: Vec<WatchRoot>,
    tx: broadcast::Sender<TraceEvent>,
    store: SessionStore,
    options: WatcherOptions,
}

impl SessionWatcher {
    /// Watch a single root with content/path-based source detection
    /// (backwards-compatible constructor).
    #[allow(dead_code)] // compat shim; production path uses SessionWatcher::multi
    pub fn new(
        watch_root: PathBuf,
        tx: broadcast::Sender<TraceEvent>,
        store: SessionStore,
        options: WatcherOptions,
    ) -> Self {
        Self::multi(
            vec![WatchRoot {
                path: watch_root,
                source: None,
                allowed_sources: None,
            }],
            tx,
            store,
            options,
        )
    }

    /// Watch several roots; each root may pin a forced agent source.
    pub fn multi(
        roots: Vec<WatchRoot>,
        tx: broadcast::Sender<TraceEvent>,
        store: SessionStore,
        options: WatcherOptions,
    ) -> Self {
        Self {
            roots,
            tx,
            store,
            options,
        }
    }

    /// Walk every existing trace file in every watch root. When `backfill`
    /// is true, process them from byte 0 so historical events populate the
    /// store. Otherwise seed offsets to current EOF so only new lines stream.
    fn seed_existing(&self, states: &mut HashMap<PathBuf, FileState>) {
        // Roots can nest (an explicit `--watch-root ~/.codex` above the
        // auto-discovered `~/.codex/sessions`). Seed the most specific root
        // first so it claims its own files, and skip files a previous root
        // already seeded — otherwise a backfill replays them once per covering
        // root. Live events already resolve the overlap via most_specific_root.
        let mut ordered: Vec<&WatchRoot> = self.roots.iter().collect();
        ordered.sort_by_key(|r| std::cmp::Reverse(r.path.as_os_str().len()));
        for root in ordered {
            seed_dir(
                &root.path,
                root.source,
                root.allowed_sources.as_ref(),
                states,
                &self.store,
                &self.tx,
                self.options.backfill,
            );
        }
    }

    /// Start the watcher on a blocking thread (notify requires a sync context).
    pub fn run(self) -> anyhow::Result<()> {
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();

        for root in &self.roots {
            info!(
                "Seeding trace files in {} (source={}, backfill={})",
                root.path.display(),
                root.source.map(|s| s.as_str()).unwrap_or("auto-detect"),
                self.options.backfill
            );
        }
        self.seed_existing(&mut states);
        info!(
            "Seeded {} file(s); store currently holds {} event(s)",
            states.len(),
            self.store.total_events()
        );

        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher = RecommendedWatcher::new(fs_tx, Config::default())?;
        for root in &self.roots {
            if root.path.is_dir() {
                watcher.watch(&root.path, RecursiveMode::Recursive)?;
                info!("Watching {} for changes", root.path.display());
            }
        }

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
            let root = most_specific_root(&self.roots, &path);
            let root_src = root.and_then(|r| r.source);
            if sources::matches_file(root_src, &path) {
                debug!("Processing event for {}", path.display());
                process_file(
                    &path,
                    root_src,
                    root.and_then(|r| r.allowed_sources.as_ref()),
                    states,
                    &self.tx,
                    &self.store,
                );
            }
        }
    }
}

fn most_specific_root<'a>(roots: &'a [WatchRoot], path: &Path) -> Option<&'a WatchRoot> {
    roots
        .iter()
        .filter(|r| path.starts_with(&r.path))
        .max_by_key(|r| r.path.as_os_str().len())
}

fn seed_dir(
    dir: &Path,
    root_source: Option<AgentSource>,
    allowed_sources: Option<&HashSet<AgentSource>>,
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
                    seed_dir(
                        &path,
                        root_source,
                        allowed_sources,
                        states,
                        store,
                        tx,
                        backfill,
                    );
                } else if sources::matches_file(root_source, &path) {
                    // Already seeded through a more specific overlapping root.
                    if states.contains_key(&path) {
                        continue;
                    }
                    if backfill {
                        // Process from the start of the file.
                        states.insert(path.clone(), FileState::default());
                        process_file(&path, root_source, allowed_sources, states, tx, store);
                    } else if let Ok(meta) = std::fs::metadata(&path) {
                        // Skip to EOF without emitting events; just count lines
                        // so future events get correct line indices.
                        let line_count = if crate::sources::cline::matches_file(&path) {
                            count_whole_file_events(&path).unwrap_or(0)
                        } else {
                            count_nonempty_lines(&path)
                        };
                        states.insert(
                            path,
                            FileState {
                                offset: meta.len(),
                                line_count,
                                source: root_source,
                                last_len: meta.len(),
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
    root_source: Option<AgentSource>,
    allowed_sources: Option<&HashSet<AgentSource>>,
    states: &mut HashMap<PathBuf, FileState>,
    tx: &broadcast::Sender<TraceEvent>,
    store: &SessionStore,
) {
    // Whole-file JSON array sources (Cline) are re-parsed on each change.
    // Routed on the filename rather than the root's source tag so Cline tasks
    // under an auto-detect root are not line-parsed as JSONL.
    if crate::sources::cline::matches_file(path) {
        process_whole_file(path, root_source, allowed_sources, states, tx, store);
        return;
    }

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
                        let source = match state.source {
                            Some(s) => s,
                            None => {
                                let s = sources::detect(root_source, path, Some(&val));
                                // Only cache a conclusive answer — see the note
                                // in loader::ingest_file.
                                if s != AgentSource::Unknown {
                                    state.source = Some(s);
                                }
                                s
                            }
                        };
                        if !source_allowed(allowed_sources, source) {
                            state.line_count += 1;
                            continue;
                        }
                        let event = TraceEvent::from_raw_as(
                            &session_fallback,
                            state.line_count,
                            val,
                            source,
                        );
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

/// Ingest a whole-file JSON array source (Cline), emitting only the newly
/// appended elements after growth and resetting to index 0 after shrinkage.
fn process_whole_file(
    path: &Path,
    root_source: Option<AgentSource>,
    allowed_sources: Option<&HashSet<AgentSource>>,
    states: &mut HashMap<PathBuf, FileState>,
    tx: &broadcast::Sender<TraceEvent>,
    store: &SessionStore,
) {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let state = states.entry(path.to_owned()).or_default();
    let previous_len = state.last_len;
    if len == previous_len {
        return; // unchanged
    }

    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) => {
            warn!("Could not read {}: {e}", path.display());
            return;
        }
    };
    let arr: Vec<serde_json::Value> = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            // File may be mid-write; try again on the next event.
            debug!("Whole-file parse not ready for {}: {e}", path.display());
            return;
        }
    };
    let arr_len = arr.len();

    // Session id: the task directory name (`tasks/<taskId>/<file>`).
    let session_fallback = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();

    let source = root_source.unwrap_or(AgentSource::Cline);
    if !source_allowed(allowed_sources, source) {
        state.last_len = len;
        state.line_count = arr_len;
        return;
    }
    state.source = Some(source);
    let start_idx = if len < previous_len {
        0
    } else {
        state.line_count.min(arr_len)
    };

    for (idx, val) in arr.into_iter().enumerate().skip(start_idx) {
        let event = TraceEvent::from_raw_as(&session_fallback, idx, val, source);
        store.ingest(&event);
        if tx.send(event).is_err() {
            // No subscribers yet — fine during backfill.
        }
    }
    state.last_len = len;
    state.line_count = arr_len;
}

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

fn count_whole_file_events(path: &Path) -> Option<usize> {
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Vec<serde_json::Value>>(&body)
        .ok()
        .map(|arr| arr.len())
}

fn source_allowed(allowed_sources: Option<&HashSet<AgentSource>>, source: AgentSource) -> bool {
    allowed_sources
        .map(|allowed| allowed.contains(&source))
        .unwrap_or(true)
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

        process_file(&path, None, None, &mut states, &tx, &store);

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
        process_file(&path, None, None, &mut states, &tx, &store);
        let ev1 = rx.try_recv().expect("expected first event");
        assert_eq!(ev1.line_index, 0);

        writeln!(file, r#"{{"type":"user","content":"second"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, None, None, &mut states, &tx, &store);
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

        process_file(&path, None, None, &mut states, &tx, &store);

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
        writeln!(file).unwrap();
        writeln!(file, r#"{{"type":"user","content":"b"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, None, None, &mut states, &tx, &store);

        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        assert_eq!(ev1.line_index, 0);
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
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
        process_file(&path, None, None, &mut states, &tx, &store);
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

        process_file(&path, None, None, &mut states, &tx, &store);
        let ev = rx
            .try_recv()
            .expect("should have emitted the new line after reset");
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
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"{\"type\":\"user\",\"content\":\"complete\"}\n")
                .unwrap();
            f.write_all(b"{\"type\":\"user\"").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, None, None, &mut states, &tx, &store);
        let ev = rx.try_recv().expect("complete line should be emitted");
        assert_eq!(ev.line_index, 0);
        assert!(rx.try_recv().is_err());

        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b",\"content\":\"rest\"}\n").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, None, None, &mut states, &tx, &store);
        let ev2 = rx.try_recv().expect("completed line should be emitted");
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_count_nonempty_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        writeln!(file, r#"{{"type":"user"}}"#).unwrap();
        writeln!(file).unwrap();
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

        process_file(&path, None, None, &mut states, &tx, &store);
        assert!(store.session("real-sid").is_some());
        assert_eq!(store.sessions().len(), 1);
    }

    #[test]
    fn test_forced_source_is_tagged() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"hi"}}"#).unwrap();
        file.flush().unwrap();
        process_file(
            &path,
            Some(AgentSource::Codex),
            None,
            &mut states,
            &tx,
            &store,
        );

        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.source, "codex");
    }

    #[test]
    fn test_sniffed_source_from_content() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(
            file,
            r#"{{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{{"cwd":"/tmp"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();
        process_file(&path, None, None, &mut states, &tx, &store);

        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.source, "codex");
        assert_eq!(ev.cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn test_whole_file_cline_ingest_and_growth() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("task-123");
        std::fs::create_dir_all(&task).unwrap();
        let path = task.join("api_conversation_history.json");
        std::fs::write(
            &path,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]"#,
        )
        .unwrap();

        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        process_file(
            &path,
            Some(AgentSource::Cline),
            None,
            &mut states,
            &tx,
            &store,
        );
        assert_eq!(rx.try_recv().unwrap().session_id, "task-123");
        assert_eq!(rx.try_recv().unwrap().line_index, 1);
        assert!(rx.try_recv().is_err());

        // Grow the file: one more element appended (rewrite whole array).
        std::fs::write(
            &path,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"user","content":"more"}]"#,
        )
        .unwrap();
        process_file(
            &path,
            Some(AgentSource::Cline),
            None,
            &mut states,
            &tx,
            &store,
        );
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.line_index, 2);
        assert!(rx.try_recv().is_err());
        assert_eq!(store.session("task-123").unwrap().source, "cline");
    }

    #[test]
    fn test_whole_file_cline_backfill_false_only_emits_appended_events_and_resets_on_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("task-123");
        std::fs::create_dir_all(&task).unwrap();
        let path = task.join("api_conversation_history.json");
        std::fs::write(&path, r#"[{"role":"user","content":"hi"}]"#).unwrap();

        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        seed_dir(
            &task,
            Some(AgentSource::Cline),
            None,
            &mut states,
            &store,
            &tx,
            false,
        );
        assert_eq!(store.total_events(), 0);
        assert!(rx.try_recv().is_err());

        std::fs::write(
            &path,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]"#,
        )
        .unwrap();
        process_file(
            &path,
            Some(AgentSource::Cline),
            None,
            &mut states,
            &tx,
            &store,
        );
        let appended = rx.try_recv().unwrap();
        assert_eq!(appended.line_index, 1);
        assert!(rx.try_recv().is_err());

        std::fs::write(&path, r#"[{"role":"user","content":"fresh"}]"#).unwrap();
        process_file(
            &path,
            Some(AgentSource::Cline),
            None,
            &mut states,
            &tx,
            &store,
        );
        let reset = rx.try_recv().unwrap();
        assert_eq!(reset.line_index, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_respects_allowed_sources() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let (tx, store) = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();
        let allowed = HashSet::from([AgentSource::Codex]);

        writeln!(file, r#"{{"type":"user","content":"hi"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, None, Some(&allowed), &mut states, &tx, &store);
        assert_eq!(store.total_events(), 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_handle_event_uses_most_specific_root() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let path = child.join("trace.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":\"c1\"}}\n",
        )
        .unwrap();

        let (tx, store) = make_tx();
        let watcher = SessionWatcher::multi(
            vec![
                WatchRoot {
                    path: parent,
                    source: Some(AgentSource::ClaudeCode),
                    allowed_sources: None,
                },
                WatchRoot {
                    path: child,
                    source: Some(AgentSource::Codex),
                    allowed_sources: None,
                },
            ],
            tx,
            store.clone(),
            WatcherOptions { backfill: false },
        );

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::Any),
            paths: vec![path],
            attrs: Default::default(),
        };
        watcher.handle_event(event, &mut HashMap::new());

        let session = store.sessions().pop().unwrap();
        assert_eq!(session.source, "codex");
    }

    #[test]
    fn test_multi_root_watcher_seeds_both() {
        let dir = tempfile::tempdir().unwrap();
        let claude_root = dir.path().join("claude");
        let codex_root = dir.path().join("codex");
        std::fs::create_dir_all(&claude_root).unwrap();
        std::fs::create_dir_all(&codex_root).unwrap();
        std::fs::write(
            claude_root.join("s1.jsonl"),
            "{\"type\":\"user\",\"sessionId\":\"cs\",\"content\":\"hi\"}\n",
        )
        .unwrap();
        std::fs::write(
            codex_root.join("rollout-1.jsonl"),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hey\"}]}}\n",
        )
        .unwrap();

        let (tx, store) = make_tx();
        let mut rx = tx.subscribe();
        let w = SessionWatcher::multi(
            vec![
                WatchRoot {
                    path: claude_root,
                    source: Some(AgentSource::ClaudeCode),
                    allowed_sources: None,
                },
                WatchRoot {
                    path: codex_root,
                    source: Some(AgentSource::Codex),
                    allowed_sources: None,
                },
            ],
            tx,
            store.clone(),
            WatcherOptions { backfill: true },
        );
        let mut states = HashMap::new();
        w.seed_existing(&mut states);
        assert_eq!(states.len(), 2);
        assert_eq!(store.sessions().len(), 2);
        let ev = rx.try_recv().unwrap();
        assert!(ev.source == "claude-code" || ev.source == "codex");
    }
}
