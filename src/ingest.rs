//! The ingestion engine shared by the live watcher and the one-shot loader.
//!
//! Coding agents persist sessions in four different ways, and the engine
//! handles each (see [`FileKind`]):
//!
//! - **Append-only JSON Lines** (Claude Code, Codex, Copilot CLI, …) are
//!   tailed from the last consumed byte, backing off partial writes and
//!   resetting on truncation.
//! - **Whole documents** rewritten in place (Gemini CLI, Cline, Continue,
//!   Aider's Markdown log, …) are re-parsed on change into per-session record
//!   lists.
//! - **Multi-file stores** (OpenCode's JSON storage) rebuild the owning
//!   session whenever one of its files changes.
//! - **SQLite databases** (OpenCode, Goose, Crush, …) are re-queried when the
//!   database or its WAL changes.
//!
//! The last three produce [`SessionDoc`]s, which are diffed record-by-record
//! against the previous parse: new records are inserted, changed ones are
//! updated in place (aggregates are retracted and re-applied), and records
//! that vanished are removed. Every change is broadcast to live subscribers.
//!
//! With a database attached, read positions are checkpointed so a restart
//! catches up on whatever was written while the tracer was not running.

use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::{
    db::FileCheckpoint,
    event::TraceEvent,
    sources::{self, AgentSource, FileKind, SessionDoc, WatchRoot},
    state::SessionStore,
};

/// Per-unit read state. A unit is a JSONL file, a document, a database, or
/// a multi-file store session.
#[derive(Debug, Default)]
pub struct FileState {
    /// Byte offset of the last consumed character (JSONL).
    pub offset: u64,
    /// Non-empty lines consumed so far — the next record's line index.
    pub line_count: usize,
    /// Source detected for this unit (set once conclusive).
    pub source: Option<AgentSource>,
    /// Length and mtime when last processed, to skip no-op change events.
    pub len: u64,
    pub mtime_ms: i64,
    /// Record content hashes per session, from the last document parse.
    pub docs: HashMap<String, Vec<u64>>,
    /// Adapter cursor for incremental database reads.
    pub cursor: Option<String>,
    /// Adapter state carried across a JSONL file's records.
    pub carry: serde_json::Map<String, serde_json::Value>,
    /// Sessions whose records this JSONL file has produced.
    pub sessions: HashSet<String>,
    /// Record count before the file was truncated or replaced; records from
    /// the new end up to here are retracted once it has been re-read.
    pub retract_from: Option<usize>,
}

/// Counters from one engine pass, for logging and the CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub units: usize,
    pub emitted: usize,
}

pub struct Engine {
    roots: Vec<WatchRoot>,
    store: SessionStore,
    tx: Option<broadcast::Sender<TraceEvent>>,
    states: HashMap<PathBuf, FileState>,
    /// Document / store / database units waiting for the debounce flush.
    pending: HashSet<(PathBuf, AgentSource, FileKind)>,
    /// Which record first reported each response's usage (see
    /// [`TraceEvent::usage_key`]).
    usage_owners: HashMap<String, (String, usize)>,
    /// See [`Db::checkpoint_horizon`]; read once, before this run's own
    /// checkpoints move it.
    horizon: Option<Option<i64>>,
    /// Set while seeding, so emitted events are marked as replayed history.
    replaying: bool,
    stats: Stats,
}

impl Engine {
    pub fn new(
        roots: Vec<WatchRoot>,
        store: SessionStore,
        tx: Option<broadcast::Sender<TraceEvent>>,
    ) -> Self {
        Self {
            roots,
            store,
            tx,
            states: HashMap::new(),
            pending: HashSet::new(),
            usage_owners: HashMap::new(),
            horizon: None,
            replaying: false,
            stats: Stats::default(),
        }
    }

    pub fn roots(&self) -> &[WatchRoot] {
        &self.roots
    }

    pub fn add_root(&mut self, root: WatchRoot) {
        if !self.roots.iter().any(|r| r.path == root.path) {
            self.roots.push(root);
        }
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn tracked_units(&self) -> usize {
        self.states.len()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Walk every root and ingest what is already on disk.
    ///
    /// `backfill` replays everything from the start. Otherwise files with a
    /// saved checkpoint resume from it (catching up on anything written while
    /// we were stopped) and files never seen before start at their end, so
    /// only new activity streams in.
    pub fn scan(&mut self, backfill: bool) -> Stats {
        // Pin the horizon before this scan saves any checkpoints.
        self.written_while_stopped(i64::MIN);
        self.replaying = true;
        // Roots can nest (an explicit `--watch-root ~/.codex` above the
        // auto-discovered `~/.codex/sessions`). Walk the most specific root
        // first so it claims its own files; later roots skip anything already
        // claimed, so nothing is ingested once per covering root.
        let mut ordered: Vec<WatchRoot> = self.roots.clone();
        ordered.sort_by_key(|r| std::cmp::Reverse(r.path.as_os_str().len()));
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for root in &ordered {
            let mut units: Vec<(PathBuf, AgentSource, FileKind)> = Vec::new();
            walk(&root.path, root, &mut units, &mut seen);
            for (path, source, kind) in units {
                self.seed_unit(&path, root, source, kind, backfill);
            }
        }
        self.flush();
        self.replaying = false;
        self.stats
    }

    /// Scan a single root (used when a new agent directory appears).
    pub fn scan_root(&mut self, root: &WatchRoot, backfill: bool) {
        let mut units = Vec::new();
        let mut seen: HashSet<PathBuf> = self.states.keys().cloned().collect();
        walk(&root.path, root, &mut units, &mut seen);
        self.replaying = true;
        for (path, source, kind) in units {
            self.seed_unit(&path, root, source, kind, backfill);
        }
        self.flush();
        self.replaying = false;
    }

    /// React to a filesystem change. JSONL is tailed immediately; other kinds
    /// are queued for [`Engine::flush`] so bursts of writes (a document being
    /// rewritten, a database committing) are processed once.
    ///
    /// Returns whether the change queued (or re-touched) a debounced unit,
    /// so the caller restarts the debounce only for those.
    pub fn path_changed(&mut self, path: &Path) -> bool {
        let Some(root) = most_specific_root(&self.roots, path).cloned() else {
            return false;
        };
        let Some((source, kind)) = sources::classify(root.source, path) else {
            return false;
        };
        match kind {
            FileKind::Jsonl => {
                self.process_jsonl(path, &root, source);
                false
            }
            FileKind::Document | FileKind::Sqlite => {
                let unit = sources::unit_path(source, kind, path);
                self.pending.insert((unit, source, kind));
                true
            }
            FileKind::StoreMember => match sources::store_unit(source, path) {
                Some(unit) => {
                    self.pending.insert((unit, source, kind));
                    true
                }
                None => false,
            },
        }
    }

    /// Process every queued document / store / database unit.
    pub fn flush(&mut self) {
        let pending: Vec<_> = self.pending.drain().collect();
        for (unit, source, kind) in pending {
            let root = most_specific_root(&self.roots, &unit)
                .cloned()
                .unwrap_or_else(|| WatchRoot {
                    path: unit.clone(),
                    source: Some(source),
                    allowed_sources: None,
                });
            self.process_unit(&unit, &root, source, kind, true);
        }
    }

    // ------------------------------------------------------------------
    // Seeding
    // ------------------------------------------------------------------

    fn seed_unit(
        &mut self,
        path: &Path,
        root: &WatchRoot,
        source: AgentSource,
        kind: FileKind,
        backfill: bool,
    ) {
        let checkpoint = self.checkpoint(path);
        match kind {
            FileKind::Jsonl => {
                let (len, mtime_ms) = file_meta(path);
                match checkpoint {
                    Some(c) if !backfill && c.offset <= len => {
                        let st = self.states.entry(path.to_path_buf()).or_default();
                        st.offset = c.offset;
                        st.line_count = c.line_count;
                        st.source = AgentSource::parse(&c.source);
                        self.process_jsonl(path, root, source);
                    }
                    None if !backfill && !self.written_while_stopped(mtime_ms) => {
                        // Never seen before and not backfilling: start at the
                        // end of the last complete record, so one still being
                        // written is read once its writer finishes it.
                        let (offset, line_count) = complete_prefix(path);
                        let st = self.states.entry(path.to_path_buf()).or_default();
                        st.offset = offset;
                        st.line_count = line_count;
                        st.len = len;
                        st.mtime_ms = mtime_ms;
                        st.source = (source != AgentSource::Unknown).then_some(source);
                        self.save_checkpoint(path, source);
                    }
                    _ => {
                        // Shorter than when we last read it: truncated or
                        // replaced while stopped.
                        let retract_from =
                            checkpoint.filter(|c| c.offset > len).map(|c| c.line_count);
                        self.states.insert(
                            path.to_path_buf(),
                            FileState {
                                retract_from,
                                ..Default::default()
                            },
                        );
                        self.process_jsonl(path, root, source);
                    }
                }
            }
            FileKind::Document | FileKind::Sqlite | FileKind::StoreMember => {
                let unit = match kind {
                    FileKind::StoreMember => match sources::store_unit(source, path) {
                        Some(u) => u,
                        None => return,
                    },
                    _ => sources::unit_path(source, kind, path),
                };
                if self.states.contains_key(&unit) {
                    return;
                }
                let checkpoint = if unit == path {
                    checkpoint
                } else {
                    self.checkpoint(&unit)
                };
                // A unit parsed by an earlier run is diffed against the record
                // hashes it saved, so only what changed since is emitted (and
                // history that run deliberately skipped stays skipped). A
                // brand-new unit is parsed silently, so only later changes
                // stream in, unless backfilling or it was written while we
                // were stopped. A backfill starts from nothing and replays all.
                let restored = match (&checkpoint, backfill, self.store.db()) {
                    (Some(_), false, Some(db)) => {
                        db.doc_hashes(&unit.to_string_lossy()).unwrap_or_default()
                    }
                    _ => HashMap::new(),
                };
                let emit = backfill
                    || !restored.is_empty()
                    || self.written_while_stopped(file_meta(&unit).1);
                if !restored.is_empty() {
                    self.states.entry(unit.clone()).or_default().docs = restored;
                }
                if let Some(c) = &checkpoint {
                    let st = self.states.entry(unit.clone()).or_default();
                    // A backfill reads databases in full; the saved cursor
                    // would skip the history it is meant to import.
                    if !backfill {
                        st.cursor = c.cursor.clone();
                    }
                    if !backfill && kind != FileKind::StoreMember {
                        let (len, mtime_ms) = file_meta(&unit);
                        if len == c.len && mtime_ms == c.mtime_ms {
                            // Unchanged since last run: nothing to catch up on.
                            // Parse lazily on the next change instead.
                            st.len = len;
                            st.mtime_ms = mtime_ms;
                            st.source = Some(source);
                            return;
                        }
                    }
                }
                self.process_unit(&unit, root, source, kind, emit);
            }
        }
    }

    // ------------------------------------------------------------------
    // JSONL tailing
    // ------------------------------------------------------------------

    fn process_jsonl(&mut self, path: &Path, root: &WatchRoot, root_detected: AgentSource) {
        let session_fallback = sources::session_id_for_path(root_detected, path);
        let mut emitted: Vec<TraceEvent> = Vec::new();
        let mut retract: Vec<(String, usize)> = Vec::new();
        {
            let state = self.states.entry(path.to_owned()).or_default();
            let mut file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    debug!("Could not open {}: {e}", path.display());
                    return;
                }
            };
            if let Ok(meta) = file.metadata() {
                if meta.len() < state.offset {
                    warn!(
                        "File {} was truncated or replaced (was {} bytes, now {}); resetting",
                        path.display(),
                        state.offset,
                        meta.len()
                    );
                    state.retract_from = Some(state.line_count);
                    state.offset = 0;
                    state.line_count = 0;
                }
            }
            if file.seek(SeekFrom::Start(state.offset)).is_err() {
                return;
            }
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            loop {
                line.clear();
                let line_start = match reader.stream_position() {
                    Ok(p) => p,
                    Err(_) => break,
                };
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        // Back off partial writes (no terminating newline yet).
                        if !line.ends_with('\n') {
                            state.offset = line_start;
                            break;
                        }
                        state.offset = line_start + line.len() as u64;
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let idx = state.line_count;
                        state.line_count += 1;
                        let mut val = match serde_json::from_str::<serde_json::Value>(trimmed) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("Malformed JSON at line {idx} of {}: {e}", path.display());
                                continue;
                            }
                        };
                        let source = match state.source {
                            Some(s) => s,
                            None => {
                                let forced =
                                    root.source.or((root_detected != AgentSource::Unknown)
                                        .then_some(root_detected));
                                let s = sources::detect(forced, path, Some(&val));
                                // Only cache a conclusive answer: a generic first
                                // record (a metadata header) sniffs as Unknown and
                                // caching that would stop us ever inspecting the
                                // records that do carry a signature.
                                if s != AgentSource::Unknown {
                                    state.source = Some(s);
                                }
                                s
                            }
                        };
                        if !root.allows(source) {
                            continue;
                        }
                        sources::annotate(source, &mut state.carry, &mut val);
                        let fallback = if source == root_detected {
                            session_fallback.clone()
                        } else {
                            sources::session_id_for_path(source, path)
                        };
                        let ev = TraceEvent::from_raw_as(&fallback, idx, val, source);
                        state.sessions.insert(ev.session_id.clone());
                        emitted.push(ev);
                    }
                    Err(e) => {
                        warn!("Read error in {}: {e}", path.display());
                        break;
                    }
                }
            }
            let (len, mtime_ms) = file_meta(path);
            state.len = len;
            state.mtime_ms = mtime_ms;
            // Records past the new end of a truncated file no longer exist.
            if let Some(old) = state.retract_from.take() {
                for sid in &state.sessions {
                    for idx in state.line_count..old {
                        retract.push((sid.clone(), idx));
                    }
                }
            }
        }
        let changed = !emitted.is_empty() || !retract.is_empty();
        for ev in emitted {
            self.emit(ev);
        }
        for (sid, idx) in retract {
            self.store.remove(&sid, idx);
        }
        if changed || self.store.db().is_some() {
            let src = self
                .states
                .get(path)
                .and_then(|s| s.source)
                .unwrap_or(root_detected);
            self.save_checkpoint(path, src);
        }
        self.stats.units = self.states.len();
    }

    // ------------------------------------------------------------------
    // Documents, stores and databases
    // ------------------------------------------------------------------

    fn process_unit(
        &mut self,
        unit: &Path,
        root: &WatchRoot,
        source: AgentSource,
        kind: FileKind,
        emit: bool,
    ) {
        let (len, mtime_ms) = match kind {
            FileKind::StoreMember => (0, 0),
            FileKind::Sqlite => sqlite_meta(unit),
            _ => file_meta(unit),
        };
        let (prev_len, prev_mtime, cursor) = {
            let st = self.states.entry(unit.to_path_buf()).or_default();
            (st.len, st.mtime_ms, st.cursor.clone())
        };
        // Documents are skipped when untouched. Databases are always
        // re-queried: a write can leave size and mtime unchanged, and the
        // cursor keeps the query cheap.
        if kind == FileKind::Document
            && prev_len == len
            && prev_mtime == mtime_ms
            && self.states.get(unit).is_some_and(|s| !s.docs.is_empty())
        {
            return; // No change since the last parse.
        }

        let mut cursor = cursor;
        let docs: Option<Vec<SessionDoc>> = match kind {
            FileKind::Document => match std::fs::read(unit) {
                Ok(bytes) => {
                    let body = String::from_utf8_lossy(&bytes);
                    let detected = if source == AgentSource::Unknown {
                        sources::sniff_document(unit, &body)
                    } else {
                        source
                    };
                    if detected == AgentSource::Unknown || !root.allows(detected) {
                        None
                    } else {
                        if let Some(st) = self.states.get_mut(unit) {
                            st.source = Some(detected);
                        }
                        sources::parse_document(detected, unit, &body)
                    }
                }
                Err(e) => {
                    debug!("Could not read {}: {e}", unit.display());
                    None
                }
            },
            FileKind::Sqlite => {
                // Databases are read incrementally: the cursor selects the
                // sessions that changed, and each is returned in full.
                if !root.allows(source) {
                    None
                } else {
                    match sources::read_sqlite(source, unit, &mut cursor) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            debug!("Could not read database {}: {e}", unit.display());
                            None
                        }
                    }
                }
            }
            FileKind::StoreMember => {
                if root.allows(source) {
                    sources::load_store_unit(source, unit)
                } else {
                    None
                }
            }
            FileKind::Jsonl => None,
        };
        let Some(docs) = docs else {
            // Unparseable right now (mid-write) or filtered out; retry on the
            // next change.
            return;
        };
        let source = self
            .states
            .get(unit)
            .and_then(|s| s.source)
            .unwrap_or(source);

        let mut to_emit: Vec<TraceEvent> = Vec::new();
        let mut to_remove: Vec<(String, usize)> = Vec::new();
        let mut changed_sessions: Vec<String> = Vec::new();
        let mut vanished: Vec<String> = Vec::new();
        {
            let st = self.states.entry(unit.to_path_buf()).or_default();
            st.len = len;
            st.mtime_ms = mtime_ms;
            st.cursor = cursor;
            st.source = Some(source);
            // A document holds all of its sessions, so one that is no longer
            // there was deleted (a multi-session log rewritten). Databases
            // only return the sessions that changed, and an empty parse of a
            // document is more likely mid-write than deliberate.
            if kind == FileKind::Document && !docs.is_empty() {
                let present: HashSet<&str> = docs.iter().map(|d| d.session_id.as_str()).collect();
                vanished = st
                    .docs
                    .keys()
                    .filter(|k| !present.contains(k.as_str()))
                    .cloned()
                    .collect();
                for sid in &vanished {
                    if let Some(prev) = st.docs.remove(sid) {
                        if emit {
                            to_remove.extend((0..prev.len()).map(|i| (sid.clone(), i)));
                        }
                    }
                }
            }
            for doc in docs {
                let hashes: Vec<u64> = doc.records.iter().map(hash_value).collect();
                let prev = st.docs.get(&doc.session_id).cloned().unwrap_or_default();
                if prev != hashes {
                    changed_sessions.push(doc.session_id.clone());
                }
                if emit {
                    for (idx, (rec, h)) in doc.records.into_iter().zip(&hashes).enumerate() {
                        if prev.get(idx) == Some(h) {
                            continue;
                        }
                        to_emit.push(TraceEvent::from_raw_as(&doc.session_id, idx, rec, source));
                    }
                    // Adapters always return complete sessions, so records
                    // past the new end were deleted at the source (a history
                    // rewind, a checkpoint restore).
                    for idx in hashes.len()..prev.len() {
                        to_remove.push((doc.session_id.clone(), idx));
                    }
                }
                st.docs.insert(doc.session_id, hashes);
            }
        }
        for ev in to_emit {
            self.emit(ev);
        }
        for (sid, idx) in to_remove {
            self.store.remove(&sid, idx);
        }
        self.save_checkpoint(unit, source);
        if let (Some(db), Some(st)) = (self.store.db(), self.states.get(unit)) {
            let changed: Vec<(&str, &[u64])> = changed_sessions
                .iter()
                .filter_map(|sid| st.docs.get(sid).map(|h| (sid.as_str(), h.as_slice())))
                .collect();
            let removed: Vec<&str> = vanished.iter().map(String::as_str).collect();
            if let Err(e) = db.save_doc_hashes(&unit.to_string_lossy(), &changed, &removed) {
                debug!("Could not save record hashes for {}: {e}", unit.display());
            }
        }
        self.stats.units = self.states.len();
    }

    // ------------------------------------------------------------------
    // Output & checkpoints
    // ------------------------------------------------------------------

    fn emit(&mut self, mut ev: TraceEvent) {
        ev.replayed = self.replaying;
        if let Some(key) = ev.usage_key.clone() {
            let me = (ev.session_id.clone(), ev.line_index);
            if self.usage_owners.len() > 200_000 {
                self.usage_owners.clear();
            }
            if !self.usage_owners.contains_key(&key) {
                // The owner may have been recorded before a restart.
                let stored = self
                    .store
                    .db()
                    .and_then(|db| db.usage_owner(&key).ok().flatten());
                self.usage_owners
                    .insert(key.clone(), stored.unwrap_or_else(|| me.clone()));
            }
            if self.usage_owners.get(&key) != Some(&me) {
                // Another record already reported this response's usage.
                ev.usage = None;
                ev.cost_usd = 0.0;
                // Only the owner carries the key, so lookups find it.
                ev.usage_key = None;
            }
        }
        if self.store.ingest(&ev).changed() {
            self.stats.emitted += 1;
            if let Some(tx) = &self.tx {
                // No subscribers (yet) is fine.
                let _ = tx.send(ev);
            }
        }
    }

    /// Was this never-seen file written after the tracer last ran? Such
    /// files are ingested in full rather than started at EOF, so sessions
    /// begun while the tracer was stopped are not lost.
    fn written_while_stopped(&mut self, mtime_ms: i64) -> bool {
        let horizon = *self.horizon.get_or_insert_with(|| {
            self.store
                .db()
                .and_then(|db| db.checkpoint_horizon().ok().flatten())
        });
        horizon.is_some_and(|h| mtime_ms > h)
    }

    fn checkpoint(&self, path: &Path) -> Option<FileCheckpoint> {
        let db = self.store.db()?;
        db.checkpoint(&path.to_string_lossy()).ok().flatten()
    }

    fn save_checkpoint(&self, path: &Path, source: AgentSource) {
        let Some(db) = self.store.db() else { return };
        let Some(st) = self.states.get(path) else {
            return;
        };
        let c = FileCheckpoint {
            path: path.to_string_lossy().to_string(),
            source: source.as_str().to_owned(),
            offset: st.offset,
            line_count: st.line_count,
            len: st.len,
            mtime_ms: st.mtime_ms,
            cursor: st.cursor.clone(),
        };
        if let Err(e) = db.save_checkpoint(&c) {
            debug!("Could not save checkpoint for {}: {e}", path.display());
        }
    }
}

/// Recursively collect ingestible units under `dir`.
fn walk(
    dir: &Path,
    root: &WatchRoot,
    out: &mut Vec<(PathBuf, AgentSource, FileKind)>,
    seen: &mut HashSet<PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!("Could not read {}: {e}", dir.display());
            }
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Don't follow symlinked directories: they can loop.
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if sources::skip_dir(root.source, &path) {
                continue;
            }
            walk(&path, root, out, seen);
        } else if ft.is_file() || ft.is_symlink() {
            if let Some((source, kind)) = sources::classify(root.source, &path) {
                if seen.insert(path.clone()) {
                    out.push((path, source, kind));
                }
            }
        }
    }
}

pub fn most_specific_root<'a>(roots: &'a [WatchRoot], path: &Path) -> Option<&'a WatchRoot> {
    roots
        .iter()
        .filter(|r| path.starts_with(&r.path))
        .max_by_key(|r| r.path.as_os_str().len())
}

/// Content hash of a record. Stable across runs and toolchains, since hashes
/// are persisted (`DefaultHasher` is not). serde_json's map is ordered, so the
/// serialisation is canonical for identical content.
fn hash_value(v: &serde_json::Value) -> u64 {
    use md5::{Digest, Md5};
    let digest = Md5::digest(serde_json::to_vec(v).unwrap_or_default());
    u64::from_le_bytes(digest[..8].try_into().expect("md5 is 16 bytes"))
}

fn file_meta(path: &Path) -> (u64, i64) {
    match std::fs::metadata(path) {
        Ok(m) => (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
        ),
        Err(_) => (0, 0),
    }
}

/// A SQLite database changes through its WAL as often as the main file, so
/// fold both into the change signature.
fn sqlite_meta(path: &Path) -> (u64, i64) {
    let (len, mtime) = file_meta(path);
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let (wlen, wmtime) = file_meta(Path::new(&wal));
    (len.wrapping_add(wlen), mtime.max(wmtime))
}

/// Byte offset just past the last newline-terminated line, and how many
/// non-empty complete lines precede it (the next record's index), matching
/// what tailing would have consumed.
fn complete_prefix(path: &Path) -> (u64, usize) {
    let Ok(f) = std::fs::File::open(path) else {
        return (0, 0);
    };
    let mut reader = BufReader::new(f);
    let mut buf = Vec::new();
    let (mut offset, mut count) = (0u64, 0usize);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(n) if n > 0 && buf.last() == Some(&b'\n') => {
                offset += n as u64;
                if !String::from_utf8_lossy(&buf).trim().is_empty() {
                    count += 1;
                }
            }
            _ => break,
        }
    }
    (offset, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::io::Write;

    fn root(path: &Path, source: Option<AgentSource>) -> WatchRoot {
        WatchRoot {
            path: path.to_path_buf(),
            source,
            allowed_sources: None,
        }
    }

    fn engine(roots: Vec<WatchRoot>) -> (Engine, SessionStore, broadcast::Receiver<TraceEvent>) {
        let (tx, rx) = broadcast::channel(4096);
        let store = SessionStore::new();
        (Engine::new(roots, store.clone(), Some(tx)), store, rx)
    }

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    fn drain(rx: &mut broadcast::Receiver<TraceEvent>) -> Vec<TraceEvent> {
        let mut v = Vec::new();
        while let Ok(e) = rx.try_recv() {
            v.push(e);
        }
        v
    }

    #[test]
    fn jsonl_tail_incremental_partial_and_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        append(&p, "{\"type\":\"user\",\"content\":\"a\"}\n{not json}\n\n");
        let (mut e, store, mut rx) = engine(vec![root(dir.path(), Some(AgentSource::ClaudeCode))]);
        e.scan(true);
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].line_index, 0);

        // A partial line is not consumed until its newline arrives.
        append(&p, "{\"type\":\"user\",\"con");
        e.path_changed(&p);
        assert!(drain(&mut rx).is_empty());
        append(&p, "tent\":\"b\"}\n");
        e.path_changed(&p);
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        // The malformed line still occupies an index.
        assert_eq!(evs[0].line_index, 2);
        assert_eq!(store.total_events(), 2);
    }

    #[test]
    fn jsonl_truncation_restarts_indexing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        append(
            &p,
            "{\"type\":\"user\",\"content\":\"a\"}\n{\"type\":\"user\",\"content\":\"b\"}\n",
        );
        let (mut e, _store, mut rx) = engine(vec![root(dir.path(), None)]);
        e.scan(true);
        assert_eq!(drain(&mut rx).len(), 2);
        std::fs::write(&p, "{\"type\":\"user\",\"content\":\"new\"}\n").unwrap();
        e.path_changed(&p);
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].line_index, 0);
    }

    #[test]
    fn no_backfill_starts_at_eof() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        append(&p, "{\"type\":\"user\",\"content\":\"old\"}\n");
        let (mut e, store, mut rx) = engine(vec![root(dir.path(), None)]);
        e.scan(false);
        assert_eq!(store.total_events(), 0);
        append(&p, "{\"type\":\"user\",\"content\":\"new\"}\n");
        e.path_changed(&p);
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].line_index, 1);
    }

    #[test]
    fn nested_roots_ingest_once_with_most_specific_source() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir_all(&child).unwrap();
        append(
            &child.join("rollout-1.jsonl"),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"arguments\":\"{}\",\"call_id\":\"c1\"}}\n",
        );
        let (mut e, store, _rx) = engine(vec![
            root(dir.path(), Some(AgentSource::ClaudeCode)),
            root(&child, Some(AgentSource::Codex)),
        ]);
        e.scan(true);
        assert_eq!(store.total_events(), 1);
        assert_eq!(store.sessions()[0].source, "codex");
    }

    #[test]
    fn allowed_sources_filter_auto_detected_records() {
        let dir = tempfile::tempdir().unwrap();
        append(
            &dir.path().join("a.jsonl"),
            "{\"type\":\"user\",\"sessionId\":\"c\",\"content\":\"hi\"}\n",
        );
        let (mut e, store, _rx) = engine(vec![WatchRoot {
            path: dir.path().to_path_buf(),
            source: None,
            allowed_sources: Some(HashSet::from([AgentSource::Codex])),
        }]);
        e.scan(true);
        assert_eq!(store.total_events(), 0);
    }

    #[test]
    fn content_sniffing_attributes_codex() {
        let dir = tempfile::tempdir().unwrap();
        append(
            &dir.path().join("x.jsonl"),
            "{\"timestamp\":\"t\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/tmp\",\"model\":\"gpt-5\"}}\n",
        );
        let (mut e, _store, mut rx) = engine(vec![root(dir.path(), None)]);
        e.scan(true);
        let ev = drain(&mut rx).remove(0);
        assert_eq!(ev.source, "codex");
        assert_eq!(ev.cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn documents_upsert_changed_records_and_remove_vanished_ones() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("tasks/task-1");
        std::fs::create_dir_all(&task).unwrap();
        let p = task.join("api_conversation_history.json");
        std::fs::write(
            &p,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"}]"#,
        )
        .unwrap();
        let (mut e, store, mut rx) = engine(vec![root(dir.path(), Some(AgentSource::Cline))]);
        e.scan(true);
        assert_eq!(drain(&mut rx).len(), 2);
        assert_eq!(store.session("task-1").unwrap().source, "cline");

        // Growth emits only the new record.
        std::fs::write(
            &p,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello"},{"role":"user","content":"more"}]"#,
        )
        .unwrap();
        e.path_changed(&p);
        e.flush();
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].line_index, 2);

        // An in-place edit updates that record; a shrink removes the tail.
        std::fs::write(
            &p,
            r#"[{"role":"user","content":"hi"},{"role":"assistant","content":"hello, edited"}]"#,
        )
        .unwrap();
        e.path_changed(&p);
        e.flush();
        let evs = drain(&mut rx);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].line_index, 1);
        let s = store.session("task-1").unwrap();
        assert_eq!(s.event_count, 2, "edited in place, tail removed");
        assert_eq!(s.user_count, 1);
    }

    #[test]
    fn mid_write_documents_are_retried() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("tasks/t");
        std::fs::create_dir_all(&task).unwrap();
        let p = task.join("api_conversation_history.json");
        std::fs::write(&p, r#"[{"role":"user","content":"hi"},{"role":"assis"#).unwrap();
        let (mut e, store, _rx) = engine(vec![root(dir.path(), Some(AgentSource::Cline))]);
        e.scan(true);
        assert_eq!(store.total_events(), 0);
        std::fs::write(&p, r#"[{"role":"user","content":"hi"}]"#).unwrap();
        e.path_changed(&p);
        e.flush();
        assert_eq!(store.total_events(), 1);
    }

    #[test]
    fn checkpoints_let_a_restart_catch_up() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let p = logs.join("s.jsonl");
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"one\"}\n",
        );
        let db = Db::open(&dir.path().join("trace.db")).unwrap();

        let store = SessionStore::with_db(db.clone());
        let mut e = Engine::new(vec![root(&logs, None)], store.clone(), None);
        e.scan(false); // first run, no backfill: start at EOF
        assert_eq!(store.total_events(), 0);
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"two\"}\n",
        );
        e.path_changed(&p);
        assert_eq!(store.total_events(), 1);

        // "Stopped"; the agent keeps writing.
        drop(e);
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"three\"}\n",
        );

        // A session started while stopped is imported in full, not skipped.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let q = logs.join("t.jsonl");
        append(
            &q,
            "{\"type\":\"user\",\"sessionId\":\"t\",\"content\":\"new\"}\n",
        );

        // Restart without backfill: resumes from the checkpoint.
        let store2 = SessionStore::with_db(db.clone());
        store2.seed_sessions(db.load_sessions().unwrap());
        let mut e2 = Engine::new(vec![root(&logs, None)], store2.clone(), None);
        e2.scan(false);
        let s = store2.session("s").unwrap();
        assert_eq!(
            s.event_count, 2,
            "caught up on the line written while stopped"
        );
        let page = db.session_events("s", None, None, 10, 0).unwrap();
        assert_eq!(page.total, 2);
        // And the persisted rows come back hydrated with canonical messages.
        assert!(page.events[1]["message"]["content"][0]["text"] == "three");
        assert_eq!(store2.session("t").map(|s| s.event_count), Some(1));
    }

    #[test]
    fn repeated_usage_is_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        // Claude Code writes one line per content block, repeating usage.
        let line = |block: &str| {
            format!(
                "{{\"type\":\"assistant\",\"sessionId\":\"s\",\"message\":{{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-6\",\"content\":[{block}],\"usage\":{{\"input_tokens\":100,\"output_tokens\":10}}}}}}\n"
            )
        };
        let p = dir.path().join("s.jsonl");
        append(&p, &line(r#"{"type":"text","text":"a"}"#));
        append(
            &p,
            &line(r#"{"type":"tool_use","id":"t","name":"Read","input":{}}"#),
        );
        let (mut e, store, _rx) = engine(vec![root(dir.path(), Some(AgentSource::ClaudeCode))]);
        e.scan(true);
        let s = store.session("s").unwrap();
        assert_eq!(s.event_count, 2);
        assert_eq!(s.input_tokens, 100);
        assert_eq!(s.output_tokens, 10);
    }

    #[test]
    fn real_codex_rollout_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("sessions/2026/09/29");
        std::fs::create_dir_all(&day).unwrap();
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/codex-0.159.1-paginated-exec-then-resume.jsonl"),
        )
        .unwrap();
        let p = day.join("rollout-2026-09-29T22-32-46-01a0ef4c-8e5e-76c0-9ba7-4dcfac11a0b3.jsonl");
        std::fs::write(&p, &body).unwrap();
        let (mut e, store, _rx) = engine(vec![root(
            &dir.path().join("sessions"),
            Some(AgentSource::Codex),
        )]);
        e.scan(true);
        let sessions = store.sessions();
        assert_eq!(sessions.len(), 1, "one rollout, one session");
        let s = &sessions[0];
        assert_eq!(s.cwd.as_deref(), Some("/work/demo"));
        assert_eq!(s.git_branch.as_deref(), Some("master"));
        assert_eq!(s.version.as_deref(), Some("0.159.1"));
        assert!(s.model.as_deref().is_some_and(|m| m.starts_with("gpt-")));
        assert_eq!(s.tool_counts.get("exec_command"), Some(&1));
        // Each response is reported by both token_usage_record and
        // token_count; count it once.
        let events = store.session_events(&s.id);
        let usage_events = events.iter().filter(|e| e.usage.is_some()).count();
        let records = body.matches("\"type\":\"token_usage_record\"").count();
        assert_eq!(usage_events, records);
        // The transcript holds the real prompts, not echoes or context.
        let users: Vec<String> = events
            .iter()
            .filter(|e| e.event_type == "user")
            .filter_map(|e| e.message.as_ref().map(|m| m.plain_text()))
            .collect();
        assert!(users.iter().any(|u| u == "Please run echo"), "{users:?}");
        assert!(users.iter().all(|u| !u.starts_with('<')));
        assert_eq!(s.first_prompt.as_deref(), Some("Please run echo"));
        assert!(events.iter().any(|e| e.turn_end));
    }

    #[test]
    fn multi_agent_scan_with_documents_and_databases() {
        let dir = tempfile::tempdir().unwrap();
        // Gemini CLI patch log.
        let g = dir.path().join("gemini/tmp/app/chats");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(
            g.join("session-2026-09-29T14-03-4f1c9a2e.jsonl"),
            "{\"sessionId\":\"g1\",\"projectHash\":\"x\",\"startTime\":\"t\"}\n{\"id\":\"u1\",\"timestamp\":\"t\",\"type\":\"user\",\"content\":[{\"text\":\"hi gemini\"}]}\n",
        )
        .unwrap();
        // OpenCode database.
        let oc = dir.path().join("opencode");
        std::fs::create_dir_all(&oc).unwrap();
        let conn = rusqlite::Connection::open(oc.join("opencode.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id text PRIMARY KEY, project_id text, parent_id text, directory text, title text, version text, time_created integer, time_updated integer);
             CREATE TABLE message (id text PRIMARY KEY, session_id text, time_created integer, time_updated integer, data text);
             CREATE TABLE part (id text PRIMARY KEY, message_id text, session_id text, time_created integer, time_updated integer, data text);
             INSERT INTO session VALUES ('ses_1','p',NULL,'/p','T','1.18',1,1);
             INSERT INTO message VALUES ('msg_1','ses_1',1,1,'{\"role\":\"user\",\"time\":{\"created\":1}}');
             INSERT INTO part VALUES ('prt_1','msg_1','ses_1',1,1,'{\"type\":\"text\",\"text\":\"hi opencode\"}');",
        )
        .unwrap();
        drop(conn);
        let (mut e, store, _rx) = engine(vec![
            root(&dir.path().join("gemini/tmp"), Some(AgentSource::Gemini)),
            root(&oc, Some(AgentSource::OpenCode)),
        ]);
        e.scan(true);
        let mut srcs: Vec<String> = store.sessions().into_iter().map(|s| s.source).collect();
        srcs.sort();
        assert_eq!(srcs, vec!["gemini", "opencode"]);
        assert_eq!(
            store.session("g1").unwrap().first_prompt.as_deref(),
            Some("hi gemini")
        );

        // A later database write is picked up on change.
        let conn = rusqlite::Connection::open(oc.join("opencode.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO message VALUES ('msg_2','ses_1',2,5,'{\"role\":\"assistant\",\"modelID\":\"gpt-5\",\"cost\":0.01,\"time\":{\"created\":2,\"completed\":5},\"finish\":\"stop\"}');
             INSERT INTO part VALUES ('prt_2','msg_2','ses_1',2,5,'{\"type\":\"text\",\"text\":\"hello\"}');",
        )
        .unwrap();
        drop(conn);
        e.path_changed(&oc.join("opencode.db"));
        e.flush();
        let s = store.session("ses_1").unwrap();
        assert_eq!(s.event_count, 2);
        assert!((s.cost_usd - 0.01).abs() < 1e-9);
    }

    #[test]
    fn truncated_jsonl_retracts_records_past_the_new_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let line =
            |t: &str| format!("{{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"{t}\"}}\n");
        append(&p, &(line("one") + &line("two") + &line("three")));
        let (mut e, store, _rx) = engine(vec![root(dir.path(), None)]);
        e.scan(true);
        assert_eq!(store.session("s").unwrap().event_count, 3);
        // Replaced by a shorter file.
        std::fs::write(&p, line("uno")).unwrap();
        e.path_changed(&p);
        let s = store.session("s").unwrap();
        assert_eq!(s.event_count, 1, "stale records past the new end remain");
        assert_eq!(store.session_events("s").len(), 1);
    }

    const AIDER_TWO: &str = "# aider chat started at 2026-09-01 10:00:00\n\n#### first question\n\nFirst answer.\n\n# aider chat started at 2026-09-02 11:00:00\n\n#### second question\n\nSecond answer.\n";

    #[test]
    fn sessions_removed_from_a_document_are_retracted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".aider.chat.history.md");
        std::fs::write(&p, AIDER_TWO).unwrap();
        let (mut e, store, _rx) = engine(vec![root(dir.path(), Some(AgentSource::Aider))]);
        e.scan(true);
        assert_eq!(store.sessions().len(), 2);
        let second = AIDER_TWO
            .split_at(
                AIDER_TWO
                    .find("# aider chat started at 2026-09-02")
                    .unwrap(),
            )
            .1;
        std::fs::write(&p, second).unwrap();
        e.path_changed(&p);
        e.flush();
        let live: Vec<_> = store
            .sessions()
            .into_iter()
            .filter(|s| s.event_count > 0)
            .collect();
        assert_eq!(live.len(), 1, "the removed session still has events");
    }

    #[test]
    fn silently_seeded_documents_stay_skipped_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let p = logs.join(".aider.chat.history.md");
        std::fs::write(&p, AIDER_TWO).unwrap();
        let db = Db::open(&dir.path().join("trace.db")).unwrap();

        // First run without backfill: existing history is not imported.
        let store = SessionStore::with_db(db.clone());
        let mut e = Engine::new(
            vec![root(&logs, Some(AgentSource::Aider))],
            store.clone(),
            None,
        );
        e.scan(false);
        assert_eq!(store.total_events(), 0);
        drop(e);

        // Written while stopped: one more turn in the second session.
        append(&p, "\n#### third question\n\nThird answer.\n");

        let store2 = SessionStore::with_db(db.clone());
        let mut e2 = Engine::new(
            vec![root(&logs, Some(AgentSource::Aider))],
            store2.clone(),
            None,
        );
        e2.scan(false);
        let stored: usize = db
            .query_sessions(&Default::default())
            .unwrap()
            .iter()
            .map(|s| s.event_count)
            .sum();
        assert!(stored > 0, "the new turn was not caught up");
        assert!(
            db.search_events("first question", 10, None)
                .unwrap()
                .is_empty(),
            "history skipped on the first run was imported on restart"
        );
        assert!(!db
            .search_events("third question", 10, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn repeated_usage_is_counted_once_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let p = logs.join("s.jsonl");
        let line = |block: &str| {
            format!(
                "{{\"type\":\"assistant\",\"sessionId\":\"s\",\"message\":{{\"id\":\"msg_1\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5\",\"content\":[{block}],\"usage\":{{\"input_tokens\":1000,\"output_tokens\":10}}}}}}\n"
            )
        };
        append(&p, &line(r#"{"type":"thinking","thinking":"hm"}"#));
        let db = Db::open(&dir.path().join("trace.db")).unwrap();
        let store = SessionStore::with_db(db.clone());
        let mut e = Engine::new(
            vec![root(&logs, Some(AgentSource::ClaudeCode))],
            store.clone(),
            None,
        );
        e.scan(true);
        drop(e);

        // The rest of the same response arrives while stopped.
        append(&p, &line(r#"{"type":"text","text":"done"}"#));
        let store2 = SessionStore::with_db(db.clone());
        store2.seed_sessions(db.load_sessions().unwrap());
        let mut e2 = Engine::new(
            vec![root(&logs, Some(AgentSource::ClaudeCode))],
            store2.clone(),
            None,
        );
        e2.scan(false);
        let s = store2.session("s").unwrap();
        assert_eq!(s.event_count, 2);
        assert_eq!(
            s.input_tokens, 1000,
            "usage counted twice across the restart"
        );
    }

    #[test]
    fn seeding_marks_events_as_replayed_and_live_ones_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"old\"}\n",
        );
        let (mut e, _store, mut rx) = engine(vec![root(dir.path(), None)]);
        e.scan(true);
        assert!(rx.try_recv().unwrap().replayed);
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"new\"}\n",
        );
        e.path_changed(&p);
        assert!(!rx.try_recv().unwrap().replayed);
    }

    #[test]
    fn silent_seed_keeps_a_record_still_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        append(
            &p,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"old\"}\n{\"type\":\"user\",",
        );
        let (mut e, store, _rx) = engine(vec![root(dir.path(), None)]);
        e.scan(false);
        assert_eq!(store.total_events(), 0);
        // The writer finishes the record it was in the middle of.
        append(&p, "\"sessionId\":\"s\",\"content\":\"live\"}\n");
        e.path_changed(&p);
        let evs = store.session_events("s");
        assert_eq!(evs.len(), 1, "the half-written record was lost");
        assert_eq!(evs[0].line_index, 1);
        assert_eq!(evs[0].entry["content"], "live");
    }

    #[test]
    fn backfill_reads_databases_from_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("x.db");
        let db = Db::open(&dir.path().join("trace.db")).unwrap();
        db.save_checkpoint(&FileCheckpoint {
            path: unit.to_string_lossy().to_string(),
            source: "goose".into(),
            offset: 0,
            line_count: 0,
            len: 1,
            mtime_ms: 1,
            cursor: Some("99|2026-01-01".into()),
        })
        .unwrap();
        let store = SessionStore::with_db(db);
        let r = root(dir.path(), Some(AgentSource::Goose));
        let mut e = Engine::new(vec![r.clone()], store.clone(), None);
        e.seed_unit(&unit, &r, AgentSource::Goose, FileKind::Sqlite, true);
        assert_eq!(e.states.get(&unit).and_then(|s| s.cursor.clone()), None);
        let mut e2 = Engine::new(vec![r.clone()], store, None);
        e2.seed_unit(&unit, &r, AgentSource::Goose, FileKind::Sqlite, false);
        assert!(e2
            .states
            .get(&unit)
            .and_then(|s| s.cursor.clone())
            .is_some());
    }

    #[test]
    fn only_debounced_units_report_a_pending_change() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::create_dir_all(&docs).unwrap();
        let j = logs.join("s.jsonl");
        append(
            &j,
            "{\"type\":\"user\",\"sessionId\":\"s\",\"content\":\"hi\"}\n",
        );
        let d = docs.join(".aider.chat.history.md");
        std::fs::write(&d, AIDER_TWO).unwrap();
        let (mut e, _store, _rx) = engine(vec![
            root(&logs, None),
            root(&docs, Some(AgentSource::Aider)),
        ]);
        assert!(
            !e.path_changed(&j),
            "JSONL is tailed at once, not debounced"
        );
        assert!(!e.path_changed(&logs.join("notes.txt")));
        assert!(e.path_changed(&d));
        assert!(e.has_pending());
    }
}
