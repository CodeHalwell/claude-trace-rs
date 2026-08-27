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

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::Context;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

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

/// Page of events for one session, plus the unfiltered total for pagination.
#[derive(Debug)]
pub struct EventPage {
    pub events: Vec<Value>,
    pub total: usize,
}

/// Thread-safe handle to the on-disk trace database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
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
            path: path.to_path_buf(),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database — used by tests.
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.path
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
             CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);",
        )?;
        Ok(())
    }

    /// Persist a single event. Idempotent: re-ingesting the same
    /// `(session_id, line_index)` is a no-op, so backfills never double-count.
    /// Returns `true` if a new row was inserted.
    pub fn insert_event(&self, ev: &TraceEvent) -> anyhow::Result<bool> {
        let conn = self.conn.lock().expect("db poisoned");
        let event_json = serde_json::to_string(ev)?;
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
                tool_uses, event_json, source)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
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
            ],
        )?;
        Ok(changed > 0)
    }

    /// Insert (or update) the rolled-up aggregates for a session.
    pub fn upsert_session(&self, s: &SessionStats) -> anyhow::Result<()> {
        let conn = self.conn.lock().expect("db poisoned");
        let tool_counts = serde_json::to_string(&s.tool_counts)?;
        conn.execute(
            "INSERT INTO sessions
               (id, source, cwd, git_branch, version, model, title, first_seen, last_seen,
                last_entry_timestamp, event_count, user_count, assistant_count,
                tool_use_count, tool_result_count, system_count, input_tokens,
                output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd,
                tool_counts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
             ON CONFLICT(id) DO UPDATE SET
                source=excluded.source,
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
            ],
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
        let conn = self.conn.lock().expect("db poisoned");
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
                    s.source
             FROM sessions s LEFT JOIN session_meta m ON m.id = s.id WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(q) = &f.search {
            sql.push_str(
                " AND (lower(s.id) LIKE ?1 OR lower(s.title) LIKE ?1
                       OR lower(s.cwd) LIKE ?1 OR lower(s.git_branch) LIKE ?1)",
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
        let conn = self.conn.lock().expect("db poisoned");
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
        let conn = self.conn.lock().expect("db poisoned");
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
        let conn = self.conn.lock().expect("db poisoned");
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
            if let Ok(v) = serde_json::from_str::<Value>(&r?) {
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
        let conn = self.conn.lock().expect("db poisoned");
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
                    if let Ok(v) = serde_json::from_str::<Value>(&r?) {
                        out.push(v);
                    }
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT event_json FROM events WHERE search_text LIKE ?1
                     ORDER BY observed_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![pattern, limit as i64], |r| {
                    r.get::<_, String>(0)
                })?;
                for r in rows {
                    if let Ok(v) = serde_json::from_str::<Value>(&r?) {
                        out.push(v);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Cross-session analytics rollups for the dashboard's Analytics tab.
    pub fn global_stats(&self) -> anyhow::Result<Value> {
        let conn = self.conn.lock().expect("db poisoned");

        let (sessions, events): (i64, i64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM events)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let (input, output, cache_read, cache_creation, cost): (i64, i64, i64, i64, f64) = conn
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_creation_tokens),0),
                        COALESCE(SUM(cost_usd),0) FROM events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;

        let by_type = map_rows(
            &conn,
            "SELECT event_type, COUNT(*) FROM events GROUP BY event_type ORDER BY 2 DESC",
        )?;
        let by_model = map_rows(&conn,
            "SELECT COALESCE(model,'(none)'), COUNT(*) FROM events WHERE model IS NOT NULL GROUP BY model ORDER BY 2 DESC")?;
        let by_source = map_rows(
            &conn,
            "SELECT source, COUNT(*) FROM events GROUP BY source ORDER BY 2 DESC",
        )?;
        let cost_by_source = {
            let mut stmt = conn.prepare(
                "SELECT source, SUM(cost_usd) FROM events GROUP BY source ORDER BY 2 DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({ "source": r.get::<_,String>(0)?, "cost_usd": r.get::<_,f64>(1)? }))
            })?;
            rows.filter_map(Result::ok).collect::<Vec<_>>()
        };
        let cost_by_model = {
            let mut stmt = conn.prepare(
                "SELECT COALESCE(model,'(none)'), SUM(cost_usd) FROM events
                 WHERE model IS NOT NULL GROUP BY model ORDER BY 2 DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({ "model": r.get::<_,String>(0)?, "cost_usd": r.get::<_,f64>(1)? }))
            })?;
            rows.filter_map(Result::ok).collect::<Vec<_>>()
        };

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

        // Daily activity timeline (last 30 days) keyed on the entry timestamp.
        let timeline = {
            let mut stmt = conn.prepare(
                "SELECT substr(COALESCE(timestamp, observed_at),1,10) AS day, COUNT(*), COALESCE(SUM(cost_usd),0.0)
                 FROM events GROUP BY day ORDER BY day DESC LIMIT 30")?;
            let rows = stmt.query_map([], |r| {
                Ok(json!({
                    "day": r.get::<_,String>(0)?,
                    "events": r.get::<_,i64>(1)?,
                    "cost_usd": r.get::<_,f64>(2)?,
                }))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v.reverse();
            v
        };

        Ok(json!({
            "sessions": sessions,
            "events": events,
            "tokens": {
                "input": input, "output": output,
                "cache_read": cache_read, "cache_creation": cache_creation,
            },
            "cost_usd": cost,
            "by_type": by_type,
            "by_model": by_model,
            "by_source": by_source,
            "cost_by_model": cost_by_model,
            "cost_by_source": cost_by_source,
            "top_tools": tools,
            "timeline": timeline,
        }))
    }

    /// Read user annotations (bookmark/tags/notes) for a session.
    pub fn get_meta(&self, id: &str) -> anyhow::Result<Value> {
        let conn = self.conn.lock().expect("db poisoned");
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

fn map_rows(conn: &Connection, sql: &str) -> anyhow::Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(json!({ "key": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)? }))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
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
    })
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

CREATE TABLE IF NOT EXISTS session_meta (
    id         TEXT PRIMARY KEY,
    bookmarked INTEGER NOT NULL DEFAULT 0,
    tags       TEXT    NOT NULL DEFAULT '[]',
    notes      TEXT    NOT NULL DEFAULT ''
);
"#;

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
            path: PathBuf::from(":memory:"),
        };
        db.migrate().unwrap();

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
