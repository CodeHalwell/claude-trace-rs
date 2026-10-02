//! Persistent, embedded SQLite store for Claude Code traces.
//!
//! Every observed event is written here so the dashboard can surface the full
//! history of every session across restarts — not just the bounded in-memory
//! ring buffers. SQLite is compiled directly into the binary (`rusqlite`'s
//! `bundled` feature), so there is nothing for the user to install.
//!
//! Two tables carry the data:
//! - `events` — one row per JSONL line, with the full enriched [`TraceEvent`]
//!   stored as JSON plus scalar columns for fast filtering and aggregation, and
//!   a `search_text` column for substring search.
//! - `sessions` — one row per session holding the rolled-up aggregates so the
//!   sidebar renders instantly without scanning every event.
//!
//! A third table, `session_meta`, persists user annotations (bookmarks, tags,
//! notes) server-side so they survive browser/localStorage resets and follow
//! the data rather than the device.
//!
//! `usage_rollup` keeps per-day usage totals current through triggers on
//! `events`, so analytics never scan the (multi-gigabyte) event table.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::{event::TraceEvent, state::SessionStats};

/// Filters accepted by [`Db::query_sessions`].
#[derive(Debug, Default, Clone)]
pub struct SessionFilter {
    /// Case-insensitive substring matched against id / title / cwd / branch.
    pub search: Option<String>,
    /// Only sessions whose `cwd` equals this project path.
    pub project: Option<String>,
    /// Only sessions from this agent source (kebab-case id).
    pub source: Option<String>,
    /// Only sessions bookmarked by the user.
    pub bookmarked_only: bool,
    /// Sort key: `last_seen` (default), `first_seen`, `events`, `cost`.
    pub sort: Option<String>,
    /// Maximum number of rows to return.
    pub limit: Option<usize>,
}

/// Result of [`Db::upsert_event`].
#[derive(Debug)]
pub enum Upsert {
    /// A new `(session_id, line_index)` row was written.
    Inserted,
    /// The row existed with different content and was replaced; carries the
    /// previous version so callers can retract its aggregates.
    Updated(Box<TraceEvent>),
    /// The row existed with identical content.
    Unchanged,
}

/// Persisted read position for one ingested file or database, so a restart
/// resumes where it left off instead of missing (or re-reading) records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileCheckpoint {
    pub path: String,
    pub source: String,
    /// Byte offset consumed so far (append-only JSONL).
    pub offset: u64,
    /// Non-empty lines consumed so far (the next record's line index).
    pub line_count: usize,
    /// File length when last processed.
    pub len: u64,
    /// Modification time (ms since the epoch) when last processed.
    pub mtime_ms: i64,
    /// Adapter-defined incremental cursor (e.g. a SQLite watermark).
    pub cursor: Option<String>,
    /// Sessions whose records a JSONL file produced, so records can be
    /// retracted if the file is truncated or replaced while stopped.
    pub sessions: Vec<String>,
    /// Signature of a JSONL file's first bytes (`len:hash`), to recognise a
    /// replacement that is not shorter than what was already read.
    pub head: Option<String>,
}

/// Page of events for one session, plus the unfiltered total for pagination.
#[derive(Debug)]
pub struct EventPage {
    pub events: Vec<Value>,
    pub total: usize,
}

/// Thread-safe handle to the on-disk trace database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    /// The one writer. Ingest reads go through it too, so they see its
    /// writes in order.
    conn: Arc<Mutex<Connection>>,
    /// Read-only connections for dashboard queries, so a slow scan never
    /// queues behind ingest or another page load. `None` in memory (tests),
    /// where reads share the writer.
    readers: Option<Arc<Readers>>,
    /// Set once `usage_rollup` holds every event; until then analytics
    /// compute the same totals from `events`.
    rollup_ready: Arc<AtomicBool>,
    path: PathBuf,
}

/// Idle read-only connections, opened on demand.
struct Readers {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

/// Idle readers kept open; more are opened under load and closed after.
const MAX_IDLE_READERS: usize = 4;

impl Readers {
    fn open(&self) -> anyhow::Result<Connection> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening database {} for reading", self.path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(conn)
    }
}

/// A connection for one read: pooled when the database is on disk, the
/// writer otherwise.
enum ReadConn<'a> {
    Pooled(Option<Connection>, &'a Readers),
    Writer(MutexGuard<'a, Connection>),
}

impl Deref for ReadConn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Self::Pooled(conn, _) => conn.as_ref().expect("reader taken"),
            Self::Writer(conn) => conn,
        }
    }
}

impl Drop for ReadConn<'_> {
    fn drop(&mut self) {
        if let Self::Pooled(conn, readers) = self {
            if let Some(conn) = conn.take() {
                let mut idle = readers.idle.lock().expect("readers poisoned");
                if idle.len() < MAX_IDLE_READERS {
                    idle.push(conn);
                }
            }
        }
    }
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").field("path", &self.path).finish()
    }
}

impl Db {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating data dir {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        // WAL gives us concurrent readers while the watcher writes; the other
        // pragmas trade a little durability for throughput, which is fine for a
        // local observability cache.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: Some(Arc::new(Readers {
                path: path.to_path_buf(),
                idle: Mutex::new(Vec::new()),
            })),
            rollup_ready: Arc::new(AtomicBool::new(false)),
            path: path.to_path_buf(),
        };
        db.migrate()?;
        db.prepare_usage()?;
        Ok(db)
    }

    /// Open an in-memory database — used by tests.
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
            rollup_ready: Arc::new(AtomicBool::new(false)),
            path: PathBuf::from(":memory:"),
        };
        db.migrate()?;
        db.prepare_usage()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A connection for a dashboard read.
    fn reader(&self) -> anyhow::Result<ReadConn<'_>> {
        let Some(readers) = self.readers.as_deref() else {
            return Ok(ReadConn::Writer(self.conn.lock().expect("db poisoned")));
        };
        let idle = readers.idle.lock().expect("readers poisoned").pop();
        let conn = match idle {
            Some(conn) => conn,
            None => readers.open()?,
        };
        Ok(ReadConn::Pooled(Some(conn), readers))
    }

    /// Note whether the usage rollup is built, and build it (with the spend
    /// index) right away when that is free: a new, empty database.
    fn prepare_usage(&self) -> anyhow::Result<()> {
        let (rollup, index, empty) = {
            let conn = self.conn.lock().expect("db poisoned");
            let (rollup, index) = usage_state(&conn)?;
            let empty = !conn.query_row("SELECT EXISTS (SELECT 1 FROM events)", [], |r| {
                r.get::<_, bool>(0)
            })?;
            (rollup, index, empty)
        };
        if rollup {
            self.rollup_ready.store(true, Ordering::Release);
        }
        if !(rollup && index) && (empty || self.readers.is_none()) {
            self.build_usage()?;
        }
        Ok(())
    }

    /// Build the usage rollup and spend index for a database that predates
    /// them. On a large one that takes a minute or so, so it runs in the
    /// background; analytics scan `events` until it is done. Call it from
    /// the process that keeps the database open, not from short-lived opens.
    pub fn build_usage_in_background(&self) {
        let built = {
            let conn = self.conn.lock().expect("db poisoned");
            matches!(usage_state(&conn), Ok((true, true)))
        };
        if built {
            return;
        }
        info!(
            "Building usage summary for {} (one-time; analytics are slower until it finishes)",
            self.path.display()
        );
        let db = self.clone();
        let spawned = std::thread::Builder::new()
            .name("usage-rollup".into())
            .spawn(move || {
                let started = Instant::now();
                // Another process may be writing (or building this) right
                // now; wait for it rather than give up.
                for attempt in 1.. {
                    match db.build_usage() {
                        Ok(()) => {
                            info!("Usage summary built in {:.0?}", started.elapsed());
                            return;
                        }
                        Err(e) if is_busy(&e) && attempt < 60 => {
                            std::thread::sleep(Duration::from_secs(10))
                        }
                        Err(e) => {
                            warn!("Could not build the usage summary: {e}");
                            return;
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            warn!("Could not start the usage summary build: {e}");
        }
    }

    /// Fill `usage_rollup` from `events` and install the triggers that keep
    /// it current — in one transaction, so no event is missed or counted
    /// twice — then index event time for [`Db::cost_since`].
    fn build_usage(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().expect("db poisoned");
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !usage_state(&tx)?.0 {
            tx.execute_batch(USAGE_ROLLUP_BUILD)?;
        }
        tx.commit()?;
        self.rollup_ready.store(true, Ordering::Release);
        conn.execute_batch(SPEND_INDEX)?;
        Ok(())
    }

    fn migrate(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute_batch(SCHEMA)?;
        // Additive migrations for databases created before the multi-agent
        // upgrade: a `source` column on both tables, defaulting to
        // 'claude-code' so historical rows stay correctly attributed.
        for ddl in [
            "ALTER TABLE events ADD COLUMN source TEXT NOT NULL DEFAULT 'claude-code'",
            "ALTER TABLE sessions ADD COLUMN source TEXT NOT NULL DEFAULT 'claude-code'",
            "ALTER TABLE sessions ADD COLUMN first_prompt TEXT",
            "ALTER TABLE events ADD COLUMN usage_key TEXT",
            "ALTER TABLE ingest_files ADD COLUMN sessions TEXT",
            "ALTER TABLE ingest_files ADD COLUMN head TEXT",
        ] {
            if let Err(e) = conn.execute_batch(ddl) {
                // "duplicate column name" means the migration already ran.
                if !e.to_string().contains("duplicate column") {
                    return Err(e.into());
                }
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_events_source ON events(source);
             CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
             CREATE INDEX IF NOT EXISTS idx_events_usage_key ON events(usage_key)
                WHERE usage_key IS NOT NULL;",
        )?;
        Ok(())
    }

    /// Persist a single event. Idempotent: re-ingesting the same
    /// `(session_id, line_index)` is a no-op, so backfills never double-count.
    /// Returns `true` if a new row was inserted.
    pub fn insert_event(&self, ev: &TraceEvent) -> anyhow::Result<bool> {
        let conn = self.conn.lock().expect("db poisoned");
        Self::insert_or_ignore(&conn, ev)
    }

    /// Persist an event, replacing a stored record whose content changed.
    ///
    /// Append-only logs never change a record once written, but whole-file
    /// documents (Gemini CLI, Cline, …) and databases (OpenCode, Goose, …)
    /// rewrite records in place as a turn streams in — a tool result lands on
    /// an existing message, token counts are filled in at the end. Comparing
    /// the raw entry lets those updates through while keeping re-reads of
    /// unchanged data free.
    pub fn upsert_event(&self, ev: &TraceEvent) -> anyhow::Result<Upsert> {
        let mut conn = self.conn.lock().expect("db poisoned");
        // One transaction, so a failed write never loses the stored version.
        let tx = conn.transaction()?;
        if Self::insert_or_ignore(&tx, ev)? {
            tx.commit()?;
            return Ok(Upsert::Inserted);
        }
        let stored: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT event_json, usage_key FROM events WHERE session_id = ?1 AND line_index = ?2",
                params![ev.session_id, ev.line_index as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((stored, stored_key)) = stored else {
            return Ok(Upsert::Inserted);
        };
        let mut old: TraceEvent = serde_json::from_str(&stored)?;
        if old.entry == ev.entry && old.source == ev.source && stored_key == ev.usage_key {
            return Ok(Upsert::Unchanged);
        }
        tx.execute(
            "DELETE FROM events WHERE session_id = ?1 AND line_index = ?2",
            params![ev.session_id, ev.line_index as i64],
        )?;
        Self::insert_or_ignore(&tx, ev)?;
        tx.commit()?;
        old.hydrate();
        Ok(Upsert::Updated(Box::new(old)))
    }

    /// The record that owns a response's usage (see
    /// [`TraceEvent::usage_key`]), so repeated usage is counted once even
    /// across restarts.
    pub fn usage_owner(&self, key: &str) -> anyhow::Result<Option<(String, usize)>> {
        let conn = self.conn.lock().expect("db poisoned");
        Ok(conn
            .query_row(
                "SELECT session_id, line_index FROM events WHERE usage_key = ?1 LIMIT 1",
                params![key],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)),
            )
            .optional()?)
    }

    /// Delete one event, returning it if it existed.
    pub fn delete_event(
        &self,
        session_id: &str,
        line_index: usize,
    ) -> anyhow::Result<Option<TraceEvent>> {
        let conn = self.conn.lock().expect("db poisoned");
        let stored: Option<String> = conn
            .query_row(
                "SELECT event_json FROM events WHERE session_id = ?1 AND line_index = ?2",
                params![session_id, line_index as i64],
                |r| r.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        conn.execute(
            "DELETE FROM events WHERE session_id = ?1 AND line_index = ?2",
            params![session_id, line_index as i64],
        )?;
        let mut ev: TraceEvent = serde_json::from_str(&stored)?;
        ev.hydrate();
        Ok(Some(ev))
    }

    fn insert_or_ignore(conn: &Connection, ev: &TraceEvent) -> anyhow::Result<bool> {
        let event_json = stored_event_json(ev)?;
        let tool_uses = serde_json::to_string(&ev.tool_uses)?;
        let (input, output, cr, cc) = ev
            .usage
            .as_ref()
            .map(|u| (u.input, u.output, u.cache_read, u.cache_creation))
            .unwrap_or((0, 0, 0, 0));
        let changed = conn.execute(
            "INSERT OR IGNORE INTO events
               (session_id, line_index, event_type, observed_at, timestamp, model,
                cost_usd, cost_estimated, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens, summary, search_text,
                tool_uses, event_json, source, usage_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                ev.session_id,
                ev.line_index as i64,
                ev.event_type,
                ev.observed_at,
                ev.timestamp,
                ev.model,
                ev.cost_usd,
                ev.cost_estimated as i64,
                input as i64,
                output as i64,
                cr as i64,
                cc as i64,
                ev.summary,
                ev.search_text().to_lowercase(),
                tool_uses,
                event_json,
                ev.source,
                ev.usage_key,
            ],
        )?;
        Ok(changed > 0)
    }

    /// Insert (or update) the rolled-up aggregates for a session.
    /// Drop a session's aggregate row (its last event was retracted).
    /// Bookmarks, tags and notes in `session_meta` are kept.
    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn upsert_session(&self, s: &SessionStats) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        let tool_counts = serde_json::to_string(&s.tool_counts)?;
        conn.execute(
            "INSERT INTO sessions
               (id, source, cwd, git_branch, version, model, title, first_seen, last_seen,
                last_entry_timestamp, event_count, user_count, assistant_count,
                tool_use_count, tool_result_count, system_count, input_tokens,
                output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd,
                tool_counts, first_prompt)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)
             ON CONFLICT(id) DO UPDATE SET
                source=excluded.source,
                first_prompt=COALESCE(sessions.first_prompt, excluded.first_prompt),
                cwd=excluded.cwd, git_branch=excluded.git_branch,
                version=excluded.version, model=excluded.model,
                title=COALESCE(excluded.title, sessions.title),
                last_seen=excluded.last_seen,
                last_entry_timestamp=excluded.last_entry_timestamp,
                event_count=excluded.event_count, user_count=excluded.user_count,
                assistant_count=excluded.assistant_count,
                tool_use_count=excluded.tool_use_count,
                tool_result_count=excluded.tool_result_count,
                system_count=excluded.system_count,
                input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
                cache_read_tokens=excluded.cache_read_tokens,
                cache_creation_tokens=excluded.cache_creation_tokens,
                cost_usd=excluded.cost_usd, tool_counts=excluded.tool_counts",
            params![
                s.id,
                s.source,
                s.cwd,
                s.git_branch,
                s.version,
                s.model,
                s.title,
                s.first_seen,
                s.last_seen,
                s.last_entry_timestamp,
                s.event_count as i64,
                s.user_count as i64,
                s.assistant_count as i64,
                s.tool_use_count as i64,
                s.tool_result_count as i64,
                s.system_count as i64,
                s.input_tokens as i64,
                s.output_tokens as i64,
                s.cache_read_tokens as i64,
                s.cache_creation_tokens as i64,
                s.cost_usd,
                tool_counts,
                s.first_prompt,
            ],
        )?;
        Ok(())
    }

    /// Overwrite a session's labels after they were re-derived from its
    /// remaining records ([`Db::upsert_session`] keeps the stored first
    /// prompt and title).
    pub fn set_session_labels(&self, s: &SessionStats) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "UPDATE sessions SET first_prompt = ?2, title = ?3, last_entry_timestamp = ?4
             WHERE id = ?1",
            params![s.id, s.first_prompt, s.title, s.last_entry_timestamp],
        )?;
        Ok(())
    }

    /// Load every session's aggregates — used to seed the in-memory store at
    /// startup so historical sessions appear immediately.
    pub fn load_sessions(&self) -> anyhow::Result<Vec<SessionStats>> {
        self.query_sessions(&SessionFilter::default())
    }

    /// Query sessions with optional filtering/sorting for the dashboard sidebar.
    pub fn query_sessions(&self, f: &SessionFilter) -> anyhow::Result<Vec<SessionStats>> {
        let conn = self.reader()?;
        let order = match f.sort.as_deref() {
            Some("first_seen") => "first_seen DESC",
            Some("events") => "event_count DESC",
            Some("cost") => "cost_usd DESC",
            _ => "last_seen DESC",
        };
        let mut sql = String::from(
            "SELECT s.id, s.cwd, s.git_branch, s.version, s.model, s.title,
                    s.first_seen, s.last_seen, s.last_entry_timestamp,
                    s.event_count, s.user_count, s.assistant_count, s.tool_use_count,
                    s.tool_result_count, s.system_count, s.input_tokens, s.output_tokens,
                    s.cache_read_tokens, s.cache_creation_tokens, s.cost_usd, s.tool_counts,
                    COALESCE(m.bookmarked,0), COALESCE(m.tags,'[]'), COALESCE(m.notes,''),
                    s.source, s.first_prompt
             FROM sessions s LEFT JOIN session_meta m ON m.id = s.id WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(q) = &f.search {
            sql.push_str(
                " AND (lower(s.id) LIKE ?1 OR lower(s.title) LIKE ?1
                       OR lower(s.cwd) LIKE ?1 OR lower(s.git_branch) LIKE ?1
                       OR lower(s.first_prompt) LIKE ?1)",
            );
            args.push(Box::new(format!("%{}%", q.to_lowercase())));
        }
        if let Some(p) = &f.project {
            let idx = args.len() + 1;
            sql.push_str(&format!(" AND s.cwd = ?{idx}"));
            args.push(Box::new(p.clone()));
        }
        if let Some(src) = &f.source {
            let idx = args.len() + 1;
            sql.push_str(&format!(" AND s.source = ?{idx}"));
            args.push(Box::new(src.clone()));
        }
        if f.bookmarked_only {
            sql.push_str(" AND COALESCE(m.bookmarked,0) = 1");
        }
        sql.push_str(&format!(" ORDER BY {order}"));
        if let Some(l) = f.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }

        let mut stmt = conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Per-agent-source rollup: session count, event count, total cost.
    pub fn sources(&self) -> anyhow::Result<Vec<Value>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT s.source, COUNT(*) AS n_sessions,
                    COALESCE(SUM(s.event_count),0), COALESCE(SUM(s.cost_usd),0.0)
             FROM sessions s GROUP BY s.source ORDER BY n_sessions DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "source": r.get::<_, String>(0)?,
                "sessions": r.get::<_, i64>(1)?,
                "events": r.get::<_, i64>(2)?,
                "cost_usd": r.get::<_, f64>(3)?,
            }))
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Distinct project directories, most-recently-active first, with counts.
    pub fn projects(&self) -> anyhow::Result<Vec<Value>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT COALESCE(cwd,''), COUNT(*), MAX(last_seen)
             FROM sessions GROUP BY cwd ORDER BY MAX(last_seen) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "cwd": r.get::<_, String>(0)?,
                "sessions": r.get::<_, i64>(1)?,
                "last_seen": r.get::<_, Option<String>>(2)?,
            }))
        })?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// A page of events for one session, optionally filtered by type / search.
    pub fn session_events(
        &self,
        session_id: &str,
        type_filter: Option<&str>,
        search: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<EventPage> {
        let conn = self.reader()?;
        let mut where_sql = String::from("session_id = ?1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(session_id.to_string())];
        if let Some(t) = type_filter.filter(|t| !t.is_empty() && *t != "all") {
            args.push(Box::new(t.to_string()));
            where_sql.push_str(&format!(" AND event_type = ?{}", args.len()));
        }
        if let Some(q) = search.filter(|q| !q.is_empty()) {
            args.push(Box::new(format!("%{}%", q.to_lowercase())));
            where_sql.push_str(&format!(" AND search_text LIKE ?{}", args.len()));
        }

        let total: i64 = {
            let arg_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
            conn.query_row(
                &format!("SELECT COUNT(*) FROM events WHERE {where_sql}"),
                arg_refs.as_slice(),
                |r| r.get(0),
            )?
        };

        let sql = format!(
            "SELECT event_json FROM events WHERE {where_sql}
             ORDER BY line_index ASC LIMIT {limit} OFFSET {offset}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let arg_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(arg_refs.as_slice(), |r| r.get::<_, String>(0))?;
        let mut events = Vec::new();
        for r in rows {
            if let Some(v) = hydrated_value(&r?) {
                events.push(v);
            }
        }
        Ok(EventPage {
            events,
            total: total as usize,
        })
    }

    /// Global full-text-ish search across all events, optionally restricted
    /// to one agent source.
    pub fn search_events(
        &self,
        query: &str,
        limit: usize,
        source: Option<&str>,
    ) -> anyhow::Result<Vec<Value>> {
        let conn = self.reader()?;
        let pattern = format!("%{}%", query.to_lowercase());
        let mut out = Vec::new();
        match source.filter(|s| !s.is_empty()) {
            Some(src) => {
                let mut stmt = conn.prepare(
                    "SELECT event_json FROM events WHERE search_text LIKE ?1 AND source = ?2
                     ORDER BY observed_at DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![pattern, src, limit as i64], |r| {
                    r.get::<_, String>(0)
                })?;
                for r in rows {
                    if let Some(v) = hydrated_value(&r?) {
                        out.push(v);
                    }
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT event_json FROM events WHERE search_text LIKE ?1
                     ORDER BY observed_at DESC LIMIT ?2",
                )?;
                let rows =
                    stmt.query_map(params![pattern, limit as i64], |r| r.get::<_, String>(0))?;
                for r in rows {
                    if let Some(v) = hydrated_value(&r?) {
                        out.push(v);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Cross-session analytics rollups for the dashboard's Analytics tab.
    pub fn global_stats(&self) -> anyhow::Result<Value> {
        let conn = self.reader()?;
        let sessions: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;

        // One row per (day, source, model, event type): the rollup once it is
        // built, otherwise the same rows from a single scan of `events`.
        // Another handle (or process) may have finished the build.
        let ready = self.rollup_ready.load(Ordering::Acquire) || {
            let built = usage_state(&conn)?.0;
            if built {
                self.rollup_ready.store(true, Ordering::Release);
            }
            built
        };
        let cube = if ready {
            USAGE_CUBE_FROM_ROLLUP
        } else {
            USAGE_CUBE_FROM_EVENTS
        };
        let mut events = 0i64;
        let (mut input, mut output, mut cache_read, mut cache_creation) = (0i64, 0i64, 0i64, 0i64);
        let mut cost = 0f64;
        let mut by_type: HashMap<String, i64> = HashMap::new();
        let mut by_model: HashMap<String, i64> = HashMap::new();
        let mut by_source: HashMap<String, i64> = HashMap::new();
        let mut cost_by_model: HashMap<String, f64> = HashMap::new();
        let mut cost_by_source: HashMap<String, f64> = HashMap::new();
        let mut days: BTreeMap<String, (i64, f64)> = BTreeMap::new();
        {
            let mut stmt = conn.prepare(cube)?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let day: String = r.get(0)?;
                let source: String = r.get(1)?;
                let model: String = r.get(2)?;
                let event_type: String = r.get(3)?;
                let n: i64 = r.get(4)?;
                let c: f64 = r.get(5)?;
                events += n;
                cost += c;
                input += r.get::<_, i64>(6)?;
                output += r.get::<_, i64>(7)?;
                cache_read += r.get::<_, i64>(8)?;
                cache_creation += r.get::<_, i64>(9)?;
                *by_type.entry(event_type).or_default() += n;
                *by_source.entry(source.clone()).or_default() += n;
                *cost_by_source.entry(source).or_default() += c;
                // '' stands for "no model" in the rollup's key.
                if !model.is_empty() {
                    *by_model.entry(model.clone()).or_default() += n;
                    *cost_by_model.entry(model).or_default() += c;
                }
                let d = days.entry(day).or_default();
                d.0 += n;
                d.1 += c;
            }
        }

        // Tool leaderboard from the per-session tool_counts JSON blobs.
        let mut tool_totals: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare("SELECT tool_counts FROM sessions")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                let r = r?;
                if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, i64>>(&r)
                {
                    for (k, v) in map {
                        *tool_totals.entry(k).or_insert(0) += v;
                    }
                }
            }
        }
        let mut tools: Vec<Value> = tool_totals
            .into_iter()
            .map(|(name, count)| json!({ "name": name, "count": count }))
            .collect();
        tools.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
        tools.truncate(20);

        // Daily activity timeline: the last 30 days with activity, keyed on
        // the entry timestamp.
        let mut timeline: Vec<Value> = days
            .into_iter()
            .rev()
            .take(30)
            .map(|(day, (n, c))| json!({ "day": day, "events": n, "cost_usd": c }))
            .collect();
        timeline.reverse();

        Ok(json!({
            "sessions": sessions,
            "events": events,
            "tokens": {
                "input": input, "output": output,
                "cache_read": cache_read, "cache_creation": cache_creation,
            },
            "cost_usd": cost,
            "by_type": ranked_counts(by_type),
            "by_model": ranked_counts(by_model),
            "by_source": ranked_counts(by_source),
            "cost_by_model": ranked_costs(cost_by_model, "model"),
            "cost_by_source": ranked_costs(cost_by_source, "source"),
            "top_tools": tools,
            "timeline": timeline,
        }))
    }

    /// Total cost of events stamped at or after `since` (an RFC 3339 UTC
    /// timestamp). Used to seed the desktop app's daily budget.
    pub fn cost_since(&self, since: &str) -> anyhow::Result<f64> {
        let conn = self.reader()?;
        Ok(conn.query_row(
            // Compare instants, not strings: records carry assorted UTC
            // offsets, and `01:00+02:00` sorts after `00:00Z` as text.
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM events
             WHERE julianday(COALESCE(timestamp, observed_at)) >= julianday(?1)",
            params![since],
            |r| r.get(0),
        )?)
    }

    /// Every stored event for one session in order, hydrated — used to export
    /// full histories that no longer fit the in-memory ring buffers.
    pub fn all_session_events(&self, session_id: &str) -> anyhow::Result<Vec<TraceEvent>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare(
            "SELECT event_json FROM events WHERE session_id = ?1 ORDER BY line_index ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(mut ev) = serde_json::from_str::<TraceEvent>(&r?) {
                ev.hydrate();
                out.push(ev);
            }
        }
        Ok(out)
    }

    /// The first of a session's stored events, in line order (or newest
    /// first), that `pred` accepts. Rows are decoded one at a time, so a
    /// match near the start costs little however long the session is.
    pub fn find_session_event(
        &self,
        session_id: &str,
        newest_first: bool,
        mut pred: impl FnMut(&mut TraceEvent) -> bool,
    ) -> anyhow::Result<Option<TraceEvent>> {
        let conn = self.conn.lock().expect("db poisoned");
        let sql = if newest_first {
            "SELECT event_json FROM events WHERE session_id = ?1 ORDER BY line_index DESC"
        } else {
            "SELECT event_json FROM events WHERE session_id = ?1 ORDER BY line_index ASC"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(params![session_id])?;
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            if let Ok(mut ev) = serde_json::from_str::<TraceEvent>(&json) {
                if pred(&mut ev) {
                    return Ok(Some(ev));
                }
            }
        }
        Ok(None)
    }

    /// The saved read position for a file, if any.
    pub fn checkpoint(&self, path: &str) -> anyhow::Result<Option<FileCheckpoint>> {
        let conn = self.conn.lock().expect("db poisoned");
        Ok(conn
            .query_row(
                "SELECT path, source, byte_offset, line_count, len, mtime_ms, cursor, sessions, head
                 FROM ingest_files WHERE path = ?1",
                params![path],
                |r| {
                    Ok(FileCheckpoint {
                        path: r.get(0)?,
                        source: r.get(1)?,
                        offset: r.get::<_, i64>(2)? as u64,
                        line_count: r.get::<_, i64>(3)? as usize,
                        len: r.get::<_, i64>(4)? as u64,
                        mtime_ms: r.get(5)?,
                        cursor: r.get(6)?,
                        sessions: r
                            .get::<_, Option<String>>(7)?
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .unwrap_or_default(),
                        head: r.get(8)?,
                    })
                },
            )
            .optional()?)
    }

    /// Save a file's read position.
    pub fn save_checkpoint(&self, c: &FileCheckpoint) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        conn.execute(
            "INSERT INTO ingest_files
                (path, source, byte_offset, line_count, len, mtime_ms, cursor, sessions, head)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(path) DO UPDATE SET
                source=excluded.source, byte_offset=excluded.byte_offset,
                line_count=excluded.line_count, len=excluded.len,
                mtime_ms=excluded.mtime_ms, cursor=excluded.cursor,
                sessions=excluded.sessions, head=excluded.head",
            params![
                c.path,
                c.source,
                c.offset as i64,
                c.line_count as i64,
                c.len as i64,
                c.mtime_ms,
                c.cursor,
                (!c.sessions.is_empty())
                    .then(|| serde_json::to_string(&c.sessions).unwrap_or_default()),
                c.head,
            ],
        )?;
        Ok(())
    }

    /// Record hashes saved for a unit by [`Db::save_doc_hashes`].
    pub fn doc_hashes(&self, path: &str) -> anyhow::Result<HashMap<String, Vec<u64>>> {
        let conn = self.conn.lock().expect("db poisoned");
        let mut stmt =
            conn.prepare("SELECT session_id, hashes FROM ingest_docs WHERE path = ?1")?;
        let rows = stmt.query_map(params![path], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (sid, blob) = row?;
            let hashes = blob
                .chunks_exact(8)
                .map(|c| u64::from_le_bytes(c.try_into().expect("8-byte chunk")))
                .collect();
            out.insert(sid, hashes);
        }
        Ok(out)
    }

    /// The most records any unit other than `path` holds for `session_id`.
    pub fn doc_len_elsewhere(&self, path: &str, session_id: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock().expect("db poisoned");
        let bytes: i64 = conn.query_row(
            "SELECT COALESCE(MAX(length(hashes)), 0) FROM ingest_docs
             WHERE session_id = ?1 AND path <> ?2",
            params![session_id, path],
            |r| r.get(0),
        )?;
        Ok(bytes as usize / 8)
    }

    /// Save the record hashes of the sessions that changed in a unit, and
    /// drop those of sessions that disappeared from it.
    pub fn save_doc_hashes(
        &self,
        path: &str,
        changed: &[(&str, &[u64])],
        removed: &[&str],
    ) -> anyhow::Result<()> {
        if changed.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().expect("db poisoned");
        let tx = conn.transaction()?;
        for (sid, hashes) in changed {
            let blob: Vec<u8> = hashes.iter().flat_map(|h| h.to_le_bytes()).collect();
            tx.execute(
                "INSERT INTO ingest_docs (path, session_id, hashes) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path, session_id) DO UPDATE SET hashes = excluded.hashes",
                params![path, sid, blob],
            )?;
        }
        for sid in removed {
            tx.execute(
                "DELETE FROM ingest_docs WHERE path = ?1 AND session_id = ?2",
                params![path, sid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Newest file modification time among saved checkpoints: roughly when
    /// the tracer last saw activity. Files first seen after a restart and
    /// modified after this were written while it was not running.
    pub fn checkpoint_horizon(&self) -> anyhow::Result<Option<i64>> {
        let conn = self.conn.lock().expect("db poisoned");
        Ok(conn.query_row(
            "SELECT MAX(mtime_ms) FROM ingest_files WHERE mtime_ms > 0",
            [],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    /// Per-agent counts of files being tracked, for the agents overview.
    pub fn tracked_files_by_source(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let conn = self.reader()?;
        let mut stmt = conn.prepare("SELECT source, COUNT(*) FROM ingest_files GROUP BY source")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    /// Read user annotations (bookmark/tags/notes) for a session.
    pub fn get_meta(&self, id: &str) -> anyhow::Result<Value> {
        let conn = self.reader()?;
        let row = conn
            .query_row(
                "SELECT bookmarked, tags, notes FROM session_meta WHERE id = ?1",
                params![id],
                |r| {
                    Ok(json!({
                        "bookmarked": r.get::<_, i64>(0)? != 0,
                        "tags": serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!([])),
                        "notes": r.get::<_, String>(2)?,
                    }))
                },
            )
            .optional()?;
        Ok(row.unwrap_or_else(|| json!({ "bookmarked": false, "tags": [], "notes": "" })))
    }

    /// Persist user annotations for a session (full replace of provided fields).
    pub fn set_meta(
        &self,
        id: &str,
        bookmarked: bool,
        tags: &[String],
        notes: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        let tags_json = serde_json::to_string(tags)?;
        conn.execute(
            "INSERT INTO session_meta (id, bookmarked, tags, notes)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET
                bookmarked=excluded.bookmarked, tags=excluded.tags, notes=excluded.notes",
            params![id, bookmarked as i64, tags_json, notes],
        )?;
        Ok(())
    }
}

/// `{key, count}` rows, largest count first.
fn ranked_counts(counts: HashMap<String, i64>) -> Vec<Value> {
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .map(|(key, count)| json!({ "key": key, "count": count }))
        .collect()
}

/// `{<label>, cost_usd}` rows, most expensive first.
fn ranked_costs(costs: HashMap<String, f64>, label: &str) -> Vec<Value> {
    let mut rows: Vec<_> = costs.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.into_iter()
        .map(|(key, cost)| {
            let mut row = serde_json::Map::new();
            row.insert(label.to_string(), key.into());
            row.insert("cost_usd".to_string(), cost.into());
            Value::Object(row)
        })
        .collect()
}

/// Whether an error is SQLite reporting the database busy or locked.
fn is_busy(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Whether the usage rollup (its triggers) and the spend index exist.
fn usage_state(conn: &Connection) -> anyhow::Result<(bool, bool)> {
    let exists = |kind: &str, name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = ?1 AND name = ?2)",
            params![kind, name],
            |r| r.get(0),
        )
    };
    Ok((
        exists("trigger", "usage_rollup_insert")?,
        exists("index", "idx_events_spend")?,
    ))
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionStats> {
    let tool_counts: String = r.get(20)?;
    let tags: String = r.get(22)?;
    Ok(SessionStats {
        id: r.get(0)?,
        cwd: r.get(1)?,
        git_branch: r.get(2)?,
        version: r.get(3)?,
        model: r.get(4)?,
        title: r.get(5)?,
        first_seen: r.get(6)?,
        last_seen: r.get(7)?,
        last_entry_timestamp: r.get(8)?,
        event_count: r.get::<_, i64>(9)? as usize,
        user_count: r.get::<_, i64>(10)? as usize,
        assistant_count: r.get::<_, i64>(11)? as usize,
        tool_use_count: r.get::<_, i64>(12)? as usize,
        tool_result_count: r.get::<_, i64>(13)? as usize,
        system_count: r.get::<_, i64>(14)? as usize,
        input_tokens: r.get::<_, i64>(15)? as u64,
        output_tokens: r.get::<_, i64>(16)? as u64,
        cache_read_tokens: r.get::<_, i64>(17)? as u64,
        cache_creation_tokens: r.get::<_, i64>(18)? as u64,
        cost_usd: r.get(19)?,
        tool_counts: serde_json::from_str(&tool_counts).unwrap_or_default(),
        bookmarked: r.get::<_, i64>(21)? != 0,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        source: r.get(24)?,
        first_prompt: r.get(25)?,
    })
}

/// The JSON stored in `events.event_json`: the full event minus the
/// canonical message, which [`TraceEvent::hydrate`] re-derives on read.
fn stored_event_json(ev: &TraceEvent) -> anyhow::Result<String> {
    let mut v = serde_json::to_value(ev)?;
    if let Some(obj) = v.as_object_mut() {
        obj.remove("message");
        obj.remove("replayed");
        obj.remove("removed");
    }
    Ok(serde_json::to_string(&v)?)
}

/// Parse a stored event row and re-derive its canonical message, returning
/// the API-facing JSON.
fn hydrated_value(stored: &str) -> Option<Value> {
    let mut ev: TraceEvent = serde_json::from_str(stored).ok()?;
    ev.hydrate();
    serde_json::to_value(ev).ok()
}

/// Resolve the default on-disk database path in the platform data directory,
/// e.g. `~/.local/share/claude-trace-rs/trace.db` (Linux),
/// `~/Library/Application Support/claude-trace-rs/trace.db` (macOS), or
/// `%APPDATA%\claude-trace-rs\data\trace.db` (Windows).
pub fn default_db_path() -> PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("rs", "claude-trace", "claude-trace-rs") {
        dirs.data_dir().join("trace.db")
    } else {
        PathBuf::from("claude-trace.db")
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    session_id            TEXT    NOT NULL,
    line_index            INTEGER NOT NULL,
    event_type            TEXT    NOT NULL,
    observed_at           TEXT    NOT NULL,
    timestamp             TEXT,
    model                 TEXT,
    cost_usd              REAL    NOT NULL DEFAULT 0,
    cost_estimated        INTEGER NOT NULL DEFAULT 0,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    summary               TEXT    NOT NULL DEFAULT '',
    search_text           TEXT    NOT NULL DEFAULT '',
    tool_uses             TEXT    NOT NULL DEFAULT '[]',
    event_json            TEXT    NOT NULL,
    source                TEXT    NOT NULL DEFAULT 'claude-code',
    PRIMARY KEY (session_id, line_index)
);
CREATE INDEX IF NOT EXISTS idx_events_session  ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_type     ON events(event_type);
CREATE INDEX IF NOT EXISTS idx_events_observed ON events(observed_at);

CREATE TABLE IF NOT EXISTS sessions (
    id                    TEXT PRIMARY KEY,
    cwd                   TEXT,
    git_branch            TEXT,
    version               TEXT,
    model                 TEXT,
    title                 TEXT,
    first_seen            TEXT,
    last_seen             TEXT,
    last_entry_timestamp  TEXT,
    event_count           INTEGER NOT NULL DEFAULT 0,
    user_count            INTEGER NOT NULL DEFAULT 0,
    assistant_count       INTEGER NOT NULL DEFAULT 0,
    tool_use_count        INTEGER NOT NULL DEFAULT 0,
    tool_result_count     INTEGER NOT NULL DEFAULT 0,
    system_count          INTEGER NOT NULL DEFAULT 0,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd              REAL    NOT NULL DEFAULT 0,
    tool_counts           TEXT    NOT NULL DEFAULT '{}',
    source                TEXT    NOT NULL DEFAULT 'claude-code'
);
CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON sessions(last_seen);
CREATE INDEX IF NOT EXISTS idx_sessions_cwd       ON sessions(cwd);

CREATE TABLE IF NOT EXISTS ingest_files (
    path        TEXT PRIMARY KEY,
    source      TEXT    NOT NULL,
    byte_offset INTEGER NOT NULL DEFAULT 0,
    line_count  INTEGER NOT NULL DEFAULT 0,
    len         INTEGER NOT NULL DEFAULT 0,
    mtime_ms    INTEGER NOT NULL DEFAULT 0,
    cursor      TEXT
);

-- Record hashes per session from the last parse of a document, store or
-- database, so a restart diffs against what was actually seen: history that
-- was deliberately skipped stays skipped, and only real changes are emitted.
CREATE TABLE IF NOT EXISTS ingest_docs (
    path       TEXT NOT NULL,
    session_id TEXT NOT NULL,
    hashes     BLOB NOT NULL,
    PRIMARY KEY (path, session_id)
);

CREATE TABLE IF NOT EXISTS session_meta (
    id         TEXT PRIMARY KEY,
    bookmarked INTEGER NOT NULL DEFAULT 0,
    tags       TEXT    NOT NULL DEFAULT '[]',
    notes      TEXT    NOT NULL DEFAULT ''
);

-- Usage totals per (entry day, source, model, event type), kept current by
-- the triggers in USAGE_ROLLUP_BUILD. `model` is '' when the event has none.
CREATE TABLE IF NOT EXISTS usage_rollup (
    day                   TEXT    NOT NULL,
    source                TEXT    NOT NULL,
    model                 TEXT    NOT NULL,
    event_type            TEXT    NOT NULL,
    events                INTEGER NOT NULL DEFAULT 0,
    cost_usd              REAL    NOT NULL DEFAULT 0,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, source, model, event_type)
) WITHOUT ROWID;
"#;

/// Rebuild `usage_rollup` from `events` and install its triggers. Runs in
/// one transaction with the writer held, so nothing lands in between.
const USAGE_ROLLUP_BUILD: &str = r#"
DELETE FROM usage_rollup;
INSERT INTO usage_rollup
    (day, source, model, event_type, events, cost_usd,
     input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)
SELECT substr(COALESCE(timestamp, observed_at), 1, 10), source, COALESCE(model, ''),
       event_type, COUNT(*), COALESCE(SUM(cost_usd), 0), COALESCE(SUM(input_tokens), 0),
       COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_read_tokens), 0),
       COALESCE(SUM(cache_creation_tokens), 0)
FROM events GROUP BY 1, 2, 3, 4;

CREATE TRIGGER IF NOT EXISTS usage_rollup_insert AFTER INSERT ON events BEGIN
    INSERT INTO usage_rollup
        (day, source, model, event_type, events, cost_usd,
         input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)
    VALUES (substr(COALESCE(NEW.timestamp, NEW.observed_at), 1, 10), NEW.source,
            COALESCE(NEW.model, ''), NEW.event_type, 1, NEW.cost_usd, NEW.input_tokens,
            NEW.output_tokens, NEW.cache_read_tokens, NEW.cache_creation_tokens)
    ON CONFLICT (day, source, model, event_type) DO UPDATE SET
        events                = events + 1,
        cost_usd              = cost_usd + excluded.cost_usd,
        input_tokens          = input_tokens + excluded.input_tokens,
        output_tokens         = output_tokens + excluded.output_tokens,
        cache_read_tokens     = cache_read_tokens + excluded.cache_read_tokens,
        cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens;
END;

CREATE TRIGGER IF NOT EXISTS usage_rollup_delete AFTER DELETE ON events BEGIN
    UPDATE usage_rollup SET
        events                = events - 1,
        cost_usd              = cost_usd - OLD.cost_usd,
        input_tokens          = input_tokens - OLD.input_tokens,
        output_tokens         = output_tokens - OLD.output_tokens,
        cache_read_tokens     = cache_read_tokens - OLD.cache_read_tokens,
        cache_creation_tokens = cache_creation_tokens - OLD.cache_creation_tokens
    WHERE day = substr(COALESCE(OLD.timestamp, OLD.observed_at), 1, 10)
      AND source = OLD.source AND model = COALESCE(OLD.model, '')
      AND event_type = OLD.event_type;
END;

CREATE TRIGGER IF NOT EXISTS usage_rollup_update AFTER UPDATE OF
    timestamp, observed_at, source, model, event_type, cost_usd,
    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
ON events BEGIN
    UPDATE usage_rollup SET
        events                = events - 1,
        cost_usd              = cost_usd - OLD.cost_usd,
        input_tokens          = input_tokens - OLD.input_tokens,
        output_tokens         = output_tokens - OLD.output_tokens,
        cache_read_tokens     = cache_read_tokens - OLD.cache_read_tokens,
        cache_creation_tokens = cache_creation_tokens - OLD.cache_creation_tokens
    WHERE day = substr(COALESCE(OLD.timestamp, OLD.observed_at), 1, 10)
      AND source = OLD.source AND model = COALESCE(OLD.model, '')
      AND event_type = OLD.event_type;
    INSERT INTO usage_rollup
        (day, source, model, event_type, events, cost_usd,
         input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens)
    VALUES (substr(COALESCE(NEW.timestamp, NEW.observed_at), 1, 10), NEW.source,
            COALESCE(NEW.model, ''), NEW.event_type, 1, NEW.cost_usd, NEW.input_tokens,
            NEW.output_tokens, NEW.cache_read_tokens, NEW.cache_creation_tokens)
    ON CONFLICT (day, source, model, event_type) DO UPDATE SET
        events                = events + 1,
        cost_usd              = cost_usd + excluded.cost_usd,
        input_tokens          = input_tokens + excluded.input_tokens,
        output_tokens         = output_tokens + excluded.output_tokens,
        cache_read_tokens     = cache_read_tokens + excluded.cache_read_tokens,
        cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens;
END;
"#;

/// Event time as [`Db::cost_since`] compares it, so today's spend is a range
/// lookup rather than a scan of every event.
const SPEND_INDEX: &str = "CREATE INDEX IF NOT EXISTS idx_events_spend
    ON events(julianday(COALESCE(timestamp, observed_at)), cost_usd)";

/// The analytics cube from the rollup.
const USAGE_CUBE_FROM_ROLLUP: &str = "
SELECT day, source, model, event_type, events, cost_usd,
       input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
FROM usage_rollup WHERE events > 0";

/// The same cube from `events`, while the rollup is still being built.
const USAGE_CUBE_FROM_EVENTS: &str = "
SELECT substr(COALESCE(timestamp, observed_at), 1, 10), source, COALESCE(model, ''),
       event_type, COUNT(*), COALESCE(SUM(cost_usd), 0), COALESCE(SUM(input_tokens), 0),
       COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_read_tokens), 0),
       COALESCE(SUM(cache_creation_tokens), 0)
FROM events GROUP BY 1, 2, 3, 4";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(session: &str, line: usize, kind: &str, body: Value) -> TraceEvent {
        let mut val = body;
        val["type"] = json!(kind);
        val["sessionId"] = json!(session);
        TraceEvent::from_raw("fallback", line, val)
    }

    #[test]
    fn insert_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let e = ev("a", 0, "user", json!({ "content": "hello world" }));
        assert!(db.insert_event(&e).unwrap());
        assert!(!db.insert_event(&e).unwrap(), "re-insert should be ignored");
    }

    #[test]
    fn session_events_paginate_and_filter() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..10 {
            let kind = if i % 2 == 0 { "user" } else { "assistant" };
            db.insert_event(&ev("a", i, kind, json!({ "content": format!("msg {i}") })))
                .unwrap();
        }
        let page = db.session_events("a", None, None, 3, 0).unwrap();
        assert_eq!(page.total, 10);
        assert_eq!(page.events.len(), 3);

        let users = db.session_events("a", Some("user"), None, 50, 0).unwrap();
        assert_eq!(users.total, 5);

        let hits = db.session_events("a", None, Some("msg 4"), 50, 0).unwrap();
        assert_eq!(hits.total, 1);
    }

    #[test]
    fn search_text_indexes_content() {
        let db = Db::open_in_memory().unwrap();
        db.insert_event(&ev(
            "a",
            0,
            "assistant",
            json!({ "message": { "content": [{ "type": "text", "text": "refactor the parser" }] } }),
        ))
        .unwrap();
        let hits = db.search_events("refactor", 10, None).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn cost_since_filters_by_timestamp() {
        let db = Db::open_in_memory().unwrap();
        let usage = |ts: &str, line, cost: f64| {
            ev(
                "a",
                line,
                "assistant",
                json!({ "timestamp": ts, "costUSD": cost, "message": { "content": "x" } }),
            )
        };
        let rows = [
            ("2026-09-28T23:59:00Z", 1.0),     // before midnight
            ("2026-09-29T08:00:00.123Z", 2.0), // after
            // Offsets are compared as instants, not text:
            ("2026-09-29T01:00:00+02:00", 4.0), // 23:00Z the day before
            ("2026-09-28T20:00:00.5-07:00", 8.0), // 03:00Z today
        ];
        for (i, (ts, cost)) in rows.iter().enumerate() {
            db.insert_event(&usage(ts, i, *cost)).unwrap();
        }
        let today = db.cost_since("2026-09-29T00:00:00Z").unwrap();
        assert!((today - 10.0).abs() < 1e-9, "{today}");
    }

    #[test]
    fn usage_rollup_matches_a_scan_of_events() {
        let db = Db::open_in_memory().unwrap();
        let usage = |line, ts: &str, model: Option<&str>, cost: f64, input: u64| {
            let mut message = json!({ "content": "x", "usage": { "input_tokens": input } });
            if let Some(m) = model {
                message["model"] = json!(m);
            }
            ev(
                "a",
                line,
                "assistant",
                json!({ "timestamp": ts, "costUSD": cost, "message": message }),
            )
        };
        db.insert_event(&usage(0, "2026-09-28T10:00:00Z", Some("opus"), 0.5, 10))
            .unwrap();
        db.insert_event(&usage(1, "2026-09-28T11:00:00Z", Some("sonnet"), 0.25, 20))
            .unwrap();
        db.insert_event(&usage(2, "2026-09-29T09:00:00Z", Some("opus"), 2.0, 40))
            .unwrap();
        db.insert_event(&usage(3, "2026-09-29T09:30:00Z", None, 0.0, 0))
            .unwrap();
        db.insert_event(&ev(
            "a",
            4,
            "user",
            json!({ "timestamp": "2026-09-29T09:31:00Z" }),
        ))
        .unwrap();
        // A record rewritten in place moves its usage; a retracted one drops it.
        assert!(matches!(
            db.upsert_event(&usage(1, "2026-09-29T11:00:00Z", Some("sonnet"), 1.0, 80))
                .unwrap(),
            Upsert::Updated(_)
        ));
        db.delete_event("a", 0).unwrap();

        let rolled = db.global_stats().unwrap();
        assert_eq!(rolled["events"], json!(4));
        assert_eq!(rolled["cost_usd"], json!(3.0));
        assert_eq!(rolled["tokens"]["input"], json!(120));
        assert_eq!(
            rolled["by_model"],
            json!([{ "key": "opus", "count": 1 }, { "key": "sonnet", "count": 1 }])
        );
        assert_eq!(
            rolled["timeline"],
            json!([{ "day": "2026-09-29", "events": 4, "cost_usd": 3.0 }])
        );

        // A database from before the rollup: analytics scan `events` and get
        // the same answer, and building it later matches too.
        db.conn
            .lock()
            .unwrap()
            .execute_batch(
                "DROP TRIGGER usage_rollup_insert; DROP TRIGGER usage_rollup_delete;
                 DROP TRIGGER usage_rollup_update; DELETE FROM usage_rollup;",
            )
            .unwrap();
        db.rollup_ready.store(false, Ordering::Release);
        let scanned = db.global_stats().unwrap();
        assert!(!db.rollup_ready.load(Ordering::Acquire));
        assert_eq!(scanned, rolled);
        db.build_usage().unwrap();
        assert_eq!(db.global_stats().unwrap(), rolled);
    }

    #[test]
    fn upsert_replaces_changed_records_and_keeps_usage_owner() {
        let db = Db::open_in_memory().unwrap();
        let mut e = ev("a", 0, "user", json!({ "content": "draft" }));
        e.usage_key = Some("k1".into());
        assert!(matches!(db.upsert_event(&e).unwrap(), Upsert::Inserted));
        assert!(matches!(db.upsert_event(&e).unwrap(), Upsert::Unchanged));
        let mut changed = ev("a", 0, "user", json!({ "content": "final" }));
        changed.usage_key = Some("k1".into());
        assert!(matches!(
            db.upsert_event(&changed).unwrap(),
            Upsert::Updated(_)
        ));
        assert_eq!(db.usage_owner("k1").unwrap(), Some(("a".to_owned(), 0)));
        let page = db.session_events("a", None, None, 10, 0).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.events[0]["entry"]["content"], json!("final"));
    }

    #[test]
    fn doc_hashes_roundtrip_and_delete() {
        let db = Db::open_in_memory().unwrap();
        db.save_doc_hashes("/x", &[("s1", &[1, u64::MAX]), ("s2", &[7])], &[])
            .unwrap();
        let h = db.doc_hashes("/x").unwrap();
        assert_eq!(h["s1"], vec![1, u64::MAX]);
        db.save_doc_hashes("/y", &[("s2", &[7, 8, 9])], &[])
            .unwrap();
        assert_eq!(db.doc_len_elsewhere("/x", "s2").unwrap(), 3);
        assert_eq!(db.doc_len_elsewhere("/y", "s2").unwrap(), 1);
        assert_eq!(db.doc_len_elsewhere("/y", "none").unwrap(), 0);
        db.save_doc_hashes("/x", &[], &["s1"]).unwrap();
        assert_eq!(db.doc_hashes("/x").unwrap().len(), 1);
    }

    #[test]
    fn meta_roundtrips() {
        let db = Db::open_in_memory().unwrap();
        db.set_meta("a", true, &["important".into(), "wip".into()], "look here")
            .unwrap();
        let m = db.get_meta("a").unwrap();
        assert_eq!(m["bookmarked"], json!(true));
        assert_eq!(m["tags"], json!(["important", "wip"]));
        assert_eq!(m["notes"], json!("look here"));
    }

    #[test]
    fn query_sessions_filters_and_sorts() {
        let db = Db::open_in_memory().unwrap();
        let mut s = SessionStats {
            id: "a".into(),
            cwd: Some("/proj/one".into()),
            event_count: 5,
            cost_usd: 1.0,
            last_seen: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        db.upsert_session(&s).unwrap();
        s.id = "b".into();
        s.cwd = Some("/proj/two".into());
        s.event_count = 50;
        s.cost_usd = 9.0;
        s.last_seen = Some("2026-02-01T00:00:00Z".into());
        db.upsert_session(&s).unwrap();

        let all = db.load_sessions().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "b", "default sort is last_seen desc");

        let by_events = db
            .query_sessions(&SessionFilter {
                sort: Some("events".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_events[0].id, "b");

        let one = db
            .query_sessions(&SessionFilter {
                project: Some("/proj/one".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, "a");
    }

    #[test]
    fn source_column_defaults_to_claude_code_and_filters() {
        // Simulate a pre-multi-agent database: a schema without `source`,
        // then run the migration and confirm old rows read as claude-code.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                session_id TEXT NOT NULL, line_index INTEGER NOT NULL,
                event_type TEXT NOT NULL, observed_at TEXT NOT NULL,
                timestamp TEXT, model TEXT, cost_usd REAL NOT NULL DEFAULT 0,
                cost_estimated INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                summary TEXT NOT NULL DEFAULT '', search_text TEXT NOT NULL DEFAULT '',
                tool_uses TEXT NOT NULL DEFAULT '[]', event_json TEXT NOT NULL,
                PRIMARY KEY (session_id, line_index));
             CREATE TABLE sessions (
                id TEXT PRIMARY KEY, cwd TEXT, git_branch TEXT, version TEXT,
                model TEXT, title TEXT, first_seen TEXT, last_seen TEXT,
                last_entry_timestamp TEXT, event_count INTEGER NOT NULL DEFAULT 0,
                user_count INTEGER NOT NULL DEFAULT 0,
                assistant_count INTEGER NOT NULL DEFAULT 0,
                tool_use_count INTEGER NOT NULL DEFAULT 0,
                tool_result_count INTEGER NOT NULL DEFAULT 0,
                system_count INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL NOT NULL DEFAULT 0,
                tool_counts TEXT NOT NULL DEFAULT '{}');
             CREATE TABLE session_meta (
                id TEXT PRIMARY KEY, bookmarked INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]', notes TEXT NOT NULL DEFAULT '');
             INSERT INTO sessions (id, event_count) VALUES ('old', 3);",
        )
        .unwrap();

        let db = Db {
            conn: std::sync::Arc::new(std::sync::Mutex::new(conn)),
            readers: None,
            rollup_ready: Arc::new(AtomicBool::new(false)),
            path: PathBuf::from(":memory:"),
        };
        db.migrate().unwrap();
        db.prepare_usage().unwrap();

        let sessions = db.load_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "claude-code");

        // New multi-source sessions upsert and filter correctly.
        let mut s = SessionStats {
            id: "cx".into(),
            event_count: 1,
            ..Default::default()
        };
        s.source = "codex".into();
        db.upsert_session(&s).unwrap();
        let codex_only = db
            .query_sessions(&SessionFilter {
                source: Some("codex".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(codex_only.len(), 1);
        assert_eq!(codex_only[0].id, "cx");

        let srcs = db.sources().unwrap();
        assert_eq!(srcs.len(), 2);
    }
}
