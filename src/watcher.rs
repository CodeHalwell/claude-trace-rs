use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::event::TraceEvent;

/// Per-file reading state: tracks the last consumed byte offset and total
/// lines seen so far for stable line indexing.
#[derive(Debug, Default)]
pub struct FileState {
    /// Byte offset of the last character consumed from this file.
    pub offset: u64,
    /// Total number of non-empty lines consumed so far.
    pub line_count: usize,
}

/// Watches a root directory for `.jsonl` file activity, tails newly appended
/// lines, and broadcasts `TraceEvent` values onto a shared channel.
pub struct SessionWatcher {
    watch_root: PathBuf,
    tx: broadcast::Sender<TraceEvent>,
}

impl SessionWatcher {
    pub fn new(watch_root: PathBuf, tx: broadcast::Sender<TraceEvent>) -> Self {
        Self { watch_root, tx }
    }

    /// Seed existing JSONL files to their current EOF so that historical lines
    /// are not replayed on startup. The `line_count` is initialised to the
    /// actual number of non-empty lines already in the file so that the first
    /// newly appended event receives the correct `line_index`.
    fn seed_existing(
        watch_root: &Path,
        states: &mut HashMap<PathBuf, FileState>,
    ) {
        match std::fs::read_dir(watch_root) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        Self::seed_existing(&path, states);
                    } else if is_jsonl(&path) {
                        if let Ok(meta) = std::fs::metadata(&path) {
                            let line_count = count_nonempty_lines(&path);
                            states.insert(path, FileState {
                                offset: meta.len(),
                                line_count,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Could not read watch root {}: {}", watch_root.display(), e);
            }
        }
    }

    /// Start the watcher on a blocking thread (notify requires a sync context).
    /// Filesystem events are forwarded to an async Tokio task via a `std::sync`
    /// channel.
    pub fn run(self) -> anyhow::Result<()> {
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();

        // Seed existing files so we start from their current EOF.
        info!("Seeding existing JSONL files in {}", self.watch_root.display());
        Self::seed_existing(&self.watch_root, &mut states);
        info!("Seeded {} file(s)", states.len());

        let (fs_tx, fs_rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

        let mut watcher = RecommendedWatcher::new(fs_tx, Config::default())?;
        watcher.watch(&self.watch_root, RecursiveMode::Recursive)?;
        info!("Watching {} for changes", self.watch_root.display());

        for res in fs_rx {
            match res {
                Ok(event) => {
                    self.handle_event(event, &mut states);
                }
                Err(e) => {
                    error!("Filesystem watch error: {e}");
                }
            }
        }

        Ok(())
    }

    fn handle_event(&self, event: Event, states: &mut HashMap<PathBuf, FileState>) {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in event.paths {
                    if is_jsonl(&path) {
                        debug!("Processing event for {}", path.display());
                        process_file(&path, states, &self.tx);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Read newly appended lines from `path` since the last known offset, parse
/// each non-empty line as JSON, and broadcast a `TraceEvent` for each one.
pub fn process_file(
    path: &Path,
    states: &mut HashMap<PathBuf, FileState>,
    tx: &broadcast::Sender<TraceEvent>,
) {
    let session_id = path
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

    // Detect file truncation or replacement: if the current size is smaller
    // than the stored offset, the file was replaced or truncated.  Reset state
    // so we start from the beginning and don't miss newly written lines.
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
        // Capture the byte position *before* this read so we can backtrack if
        // we hit an unterminated (partial) line.
        let line_start = match reader.stream_position() {
            Ok(p) => p,
            Err(e) => {
                warn!("stream_position error in {}: {e}", path.display());
                return;
            }
        };
        match reader.read_line(&mut line) {
            Ok(0) => break, // Clean EOF
            Ok(_) => {
                // A line without a trailing newline is a partial write in
                // progress.  Back off the offset to before this incomplete
                // record so it is re-read on the next filesystem notification.
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
                        let event = TraceEvent::from_raw(&session_id, state.line_count, val);
                        if let Err(e) = tx.send(event) {
                            debug!("No active subscribers (send error): {e}");
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Malformed JSON at line {} of {}: {e}",
                            state.line_count,
                            path.display()
                        );
                    }
                }
                state.line_count += 1;
            }
            Err(e) => {
                warn!("Read error in {}: {e}", path.display());
                break;
            }
        }
    }

    // Update the byte offset to the current position so the next event only
    // reads newly appended content.  Use `reader.stream_position()` so the
    // BufReader's lookahead buffer is accounted for correctly.
    match reader.stream_position() {
        Ok(pos) => state.offset = pos,
        Err(e) => warn!("Could not get file position for {}: {e}", path.display()),
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
}

/// Count the non-empty lines in a JSONL file without broadcasting any events.
/// Used during startup to seed `line_count` so that the first newly appended
/// event receives the correct `line_index`.
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

    fn make_tx() -> broadcast::Sender<TraceEvent> {
        let (tx, _rx) = broadcast::channel(256);
        tx
    }

    #[test]
    fn test_process_file_reads_new_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"hello"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{}}}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx);

        let ev1 = rx.try_recv().expect("expected first event");
        let ev2 = rx.try_recv().expect("expected second event");
        assert_eq!(ev1.line_index, 0);
        assert_eq!(ev2.line_index, 1);
        assert!(rx.try_recv().is_err(), "should be no more events");
    }

    #[test]
    fn test_process_file_incremental_reads() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        // First append
        writeln!(file, r#"{{"type":"user","content":"first"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx);
        let ev1 = rx.try_recv().expect("expected first event");
        assert_eq!(ev1.line_index, 0);

        // Second append — should not re-emit the first line
        writeln!(file, r#"{{"type":"user","content":"second"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx);
        let ev2 = rx.try_recv().expect("expected second event");
        assert_eq!(ev2.line_index, 1, "line_index should continue from offset");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_skips_malformed_json() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, "{{not valid json}}").unwrap();
        writeln!(file, r#"{{"type":"user","content":"ok"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx);

        // Only the valid line should produce an event; line_count still advances
        let ev = rx.try_recv().expect("expected one event for valid line");
        assert_eq!(ev.line_index, 1, "malformed line should still increment line_count");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_skips_empty_lines() {
        let mut file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        writeln!(file, r#"{{"type":"user","content":"a"}}"#).unwrap();
        writeln!(file, "").unwrap();
        writeln!(file, r#"{{"type":"user","content":"b"}}"#).unwrap();
        file.flush().unwrap();

        process_file(&path, &mut states, &tx);

        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        // Both real lines emitted
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
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        // Write and process two lines.
        writeln!(file, r#"{{"type":"user","content":"a"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","content":"b"}}"#).unwrap();
        file.flush().unwrap();
        process_file(&path, &mut states, &tx);
        rx.try_recv().unwrap();
        rx.try_recv().unwrap();
        assert!(rx.try_recv().is_err());

        // Simulate a file replacement by re-creating the temp file with shorter content.
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

        process_file(&path, &mut states, &tx);
        // The state should have been reset and the new line re-emitted from index 0.
        let ev = rx.try_recv().expect("should have emitted the new line after reset");
        assert_eq!(ev.line_index, 0, "line_index should restart from 0 after truncation reset");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_process_file_partial_line_not_consumed() {
        use std::io::Write as _;
        let file = NamedTempFile::with_suffix(".jsonl").unwrap();
        let path = file.path().to_owned();
        let tx = make_tx();
        let mut states: HashMap<PathBuf, FileState> = HashMap::new();
        let mut rx = tx.subscribe();

        // Write a complete line followed by a partial line (no trailing newline).
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            // Write a complete JSONL line (with newline)
            f.write_all(b"{\"type\":\"user\",\"content\":\"complete\"}\n").unwrap();
            // Write a partial line without a trailing newline
            f.write_all(b"{\"type\":\"user\"").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, &mut states, &tx);

        // Only the complete line should have been emitted.
        let ev = rx.try_recv().expect("complete line should be emitted");
        assert_eq!(ev.line_index, 0);
        assert!(rx.try_recv().is_err(), "partial line must not be emitted");

        // Now complete the partial line by appending the rest with a newline.
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b",\"content\":\"rest\"}\n").unwrap();
            f.flush().unwrap();
        }

        process_file(&path, &mut states, &tx);
        // The completed line should now be processed without crashing.
        // It is valid JSON so it will produce an event.
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
}
