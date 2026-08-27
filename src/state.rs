use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{db::Db, event::TraceEvent};

/// Cap on how many events we retain per session in memory for client backfill.
pub const PER_SESSION_RECENT_CAP: usize = 5_000;

/// Cap on how many events we retain across all sessions for the global feed.
pub const GLOBAL_RECENT_CAP: usize = 20_000;

/// Per-session aggregated stats and a bounded buffer of recent events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub id: String,
    /// Which coding agent produced this session (kebab-case id).
    #[serde(default = "default_session_source")]
    pub source: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub model: Option<String>,

    /// RFC 3339 timestamp of the first event observed for this session.
    pub first_seen: Option<String>,
    /// RFC 3339 timestamp of the latest event observed for this session.
    pub last_seen: Option<String>,
    /// Latest entry timestamp (from the JSONL record itself).
    pub last_entry_timestamp: Option<String>,

    pub event_count: usize,
    pub user_count: usize,
    pub assistant_count: usize,
    pub tool_use_count: usize,
    pub tool_result_count: usize,
    pub system_count: usize,

    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: f64,

    /// Tool name → invocation count.
    pub tool_counts: HashMap<String, usize>,

    /// AI-generated title from `ai-title` events, when present.
    pub title: Option<String>,

    /// Whether the user has bookmarked this session (persisted in the database).
    #[serde(default)]
    pub bookmarked: bool,
    /// Freeform user tags for this session (persisted in the database).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Backward-compat default: sessions recorded before the multi-agent upgrade
/// were all Claude Code sessions.
fn default_session_source() -> String {
    crate::sources::AgentSource::ClaudeCode.as_str().to_owned()
}

impl Default for SessionStats {
    fn default() -> Self {
        // `source` defaults to claude-code (not empty) so in-memory and
        // test-constructed sessions match the serde/DB default.
        Self {
            id: String::new(),
            source: default_session_source(),
            cwd: None,
            git_branch: None,
            version: None,
            model: None,
            first_seen: None,
            last_seen: None,
            last_entry_timestamp: None,
            event_count: 0,
            user_count: 0,
            assistant_count: 0,
            tool_use_count: 0,
            tool_result_count: 0,
            system_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
            tool_counts: HashMap::new(),
            title: None,
            bookmarked: false,
            tags: Vec::new(),
        }
    }
}

impl SessionStats {
    fn ingest(&mut self, ev: &TraceEvent) {
        if self.id.is_empty() {
            self.id = ev.session_id.clone();
        }
        // Adopt the event's source on the first event (a freshly defaulted
        // stats block carries the claude-code default; replace it unless the
        // event is itself unattributed).
        if self.event_count == 0 && ev.source != "unknown" {
            self.source = ev.source.clone();
        }
        if self.first_seen.is_none() {
            self.first_seen = Some(ev.observed_at.clone());
        }
        self.last_seen = Some(ev.observed_at.clone());
        if let Some(t) = &ev.timestamp {
            self.last_entry_timestamp = Some(t.clone());
        }
        if self.cwd.is_none() {
            self.cwd = ev.cwd.clone();
        }
        if self.git_branch.is_none() {
            self.git_branch = ev.git_branch.clone();
        } else if let Some(b) = &ev.git_branch {
            // Track the most recent branch a session was on.
            self.git_branch = Some(b.clone());
        }
        if let Some(v) = &ev.version {
            self.version = Some(v.clone());
        }
        if let Some(m) = &ev.model {
            self.model = Some(m.clone());
        }

        self.event_count += 1;

        match ev.event_type.as_str() {
            "user" => self.user_count += 1,
            "assistant" => self.assistant_count += 1,
            "tool_use" => {
                // Top-level tool_use entries whose adapter left `tool_uses`
                // empty (Claude Code) carry the name at the record root.
                // Adapters that already populated `tool_uses` (Codex, …) are
                // counted by the loop below, so skip here to avoid double
                // counting.
                if ev.tool_uses.is_empty() {
                    self.tool_use_count += 1;
                    if let Some(name) = ev.entry.get("name").and_then(|v| v.as_str()) {
                        *self.tool_counts.entry(name.to_owned()).or_insert(0) += 1;
                    }
                }
            }
            "tool_result" => self.tool_result_count += 1,
            "system" => self.system_count += 1,
            _ => {}
        }

        // Tool uses embedded in assistant content blocks.
        for name in &ev.tool_uses {
            self.tool_use_count += 1;
            *self.tool_counts.entry(name.clone()).or_insert(0) += 1;
        }
        self.tool_result_count += ev.tool_results.len();

        if let Some(u) = &ev.usage {
            self.input_tokens += u.input;
            self.output_tokens += u.output;
            self.cache_read_tokens += u.cache_read;
            self.cache_creation_tokens += u.cache_creation;
        }
        self.cost_usd += ev.cost_usd;

        // Capture AI-generated session title when emitted.
        if ev.event_type == "ai-title" {
            if let Some(t) = ev.entry.get("aiTitle").and_then(|v| v.as_str()) {
                self.title = Some(t.to_owned());
            }
        }
    }
}

/// Snapshot for the dashboard: per-session stats keyed by ID plus a recent
/// global feed.
#[derive(Debug, Default, Serialize)]
pub struct Snapshot {
    pub sessions: Vec<SessionStats>,
    pub events: Vec<TraceEvent>,
    pub total_events: usize,
}

#[derive(Debug, Default)]
struct Inner {
    sessions: HashMap<String, SessionStats>,
    /// Per-session ring buffer of recent events.
    per_session_events: HashMap<String, VecDeque<TraceEvent>>,
    /// Global ring buffer for the live feed.
    global_events: VecDeque<TraceEvent>,
    total_events: usize,
}

/// Shared, thread-safe session store. Cheap to clone.
///
/// Holds the bounded in-memory state that powers the live WebSocket feed. When
/// constructed with [`SessionStore::with_db`], every ingested event is also
/// persisted to the on-disk SQLite database so the full history survives
/// restarts and can be queried beyond the in-memory ring buffers.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<RwLock<Inner>>,
    db: Option<Db>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store that also persists every event to the given database.
    pub fn with_db(db: Db) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            db: Some(db),
        }
    }

    /// Seed in-memory aggregates from a previously persisted set of sessions,
    /// without re-counting or re-persisting them. Used at startup so historical
    /// sessions appear in the dashboard immediately.
    pub fn seed_sessions(&self, sessions: Vec<SessionStats>) {
        let mut g = self.inner.write().expect("session store poisoned");
        for s in sessions {
            // Keep the global event total consistent with the seeded per-session
            // counts so /health and WebSocket snapshots report sane numbers
            // before any new events arrive.
            g.total_events += s.event_count;
            g.sessions.insert(s.id.clone(), s);
        }
    }

    /// Update a session's persisted annotations (bookmark / tags) in the
    /// in-memory store so live snapshots and `/api/sessions` don't go stale
    /// after a metadata write.
    pub fn update_meta(&self, id: &str, bookmarked: bool, tags: Vec<String>) {
        let mut g = self.inner.write().expect("session store poisoned");
        if let Some(stats) = g.sessions.get_mut(id) {
            stats.bookmarked = bookmarked;
            stats.tags = tags;
        }
    }

    /// Record an event in the store, updating aggregates and ring buffers, and
    /// persisting to the database when one is attached.
    ///
    /// When a database is attached it is the authority for de-duplication: an
    /// event whose `(session_id, line_index)` is already stored is skipped
    /// entirely, so re-ingestion (e.g. `--backfill` over already-persisted data)
    /// never double-counts the in-memory aggregates. All writes happen while the
    /// in-memory lock is held, so the persisted aggregates can never be
    /// clobbered by an out-of-order snapshot.
    pub fn ingest(&self, ev: &TraceEvent) {
        let mut g = self.inner.write().expect("session store poisoned");

        if let Some(db) = &self.db {
            match db.insert_event(ev) {
                // Already persisted — it has already been counted; skip it.
                Ok(false) => return,
                Ok(true) => {}
                Err(e) => warn!("Failed to persist event to database: {e}"),
            }
        }

        g.total_events += 1;

        let stats = g.sessions.entry(ev.session_id.clone()).or_default();
        stats.ingest(ev);
        if let Some(db) = &self.db {
            if let Err(e) = db.upsert_session(stats) {
                warn!("Failed to persist session aggregates: {e}");
            }
        }

        let per = g
            .per_session_events
            .entry(ev.session_id.clone())
            .or_default();
        per.push_back(ev.clone());
        while per.len() > PER_SESSION_RECENT_CAP {
            per.pop_front();
        }

        g.global_events.push_back(ev.clone());
        while g.global_events.len() > GLOBAL_RECENT_CAP {
            g.global_events.pop_front();
        }
    }

    /// Snapshot of all known sessions and the global event tail.
    pub fn snapshot(&self, recent_events: usize) -> Snapshot {
        let g = self.inner.read().expect("session store poisoned");
        let mut sessions: Vec<SessionStats> = g.sessions.values().cloned().collect();
        // Sort by last_seen descending (most recently active first).
        sessions.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));

        let skip = g.global_events.len().saturating_sub(recent_events);
        let events: Vec<TraceEvent> = g.global_events.iter().skip(skip).cloned().collect();
        Snapshot {
            sessions,
            events,
            total_events: g.total_events,
        }
    }

    /// All recent events for a specific session.
    pub fn session_events(&self, session_id: &str) -> Vec<TraceEvent> {
        let g = self.inner.read().expect("session store poisoned");
        g.per_session_events
            .get(session_id)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Lookup a single session's stats.
    pub fn session(&self, session_id: &str) -> Option<SessionStats> {
        let g = self.inner.read().expect("session store poisoned");
        g.sessions.get(session_id).cloned()
    }

    /// All session stats, most recently active first.
    pub fn sessions(&self) -> Vec<SessionStats> {
        let g = self.inner.read().expect("session store poisoned");
        let mut v: Vec<SessionStats> = g.sessions.values().cloned().collect();
        v.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        v
    }

    pub fn total_events(&self) -> usize {
        self.inner
            .read()
            .expect("session store poisoned")
            .total_events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(session: &str, kind: &str, body: serde_json::Value) -> TraceEvent {
        let mut val = body;
        val["type"] = json!(kind);
        val["sessionId"] = json!(session);
        TraceEvent::from_raw("fallback", 0, val)
    }

    #[test]
    fn store_aggregates_by_session() {
        let store = SessionStore::new();
        store.ingest(&ev("a", "user", json!({ "content": "hi" })));
        store.ingest(&ev(
            "a",
            "assistant",
            json!({
                "message": {
                    "model": "claude-sonnet-4-6",
                    "content": [{ "type": "text", "text": "hello" }],
                    "usage": { "input_tokens": 10, "output_tokens": 5 }
                }
            }),
        ));
        store.ingest(&ev("b", "user", json!({ "content": "another" })));

        let snap = store.snapshot(50);
        assert_eq!(snap.total_events, 3);
        assert_eq!(snap.sessions.len(), 2);

        let a = store.session("a").unwrap();
        assert_eq!(a.event_count, 2);
        assert_eq!(a.user_count, 1);
        assert_eq!(a.assistant_count, 1);
        assert_eq!(a.input_tokens, 10);
        assert_eq!(a.output_tokens, 5);
        assert!(a.cost_usd > 0.0);
    }

    #[test]
    fn store_tracks_tool_counts() {
        let store = SessionStore::new();
        store.ingest(&ev(
            "a",
            "assistant",
            json!({
                "message": {
                    "content": [
                        { "type": "tool_use", "name": "Read" },
                        { "type": "tool_use", "name": "Bash" }
                    ]
                }
            }),
        ));
        store.ingest(&ev(
            "a",
            "assistant",
            json!({
                "message": {
                    "content": [{ "type": "tool_use", "name": "Read" }]
                }
            }),
        ));
        let s = store.session("a").unwrap();
        assert_eq!(s.tool_counts.get("Read"), Some(&2));
        assert_eq!(s.tool_counts.get("Bash"), Some(&1));
        assert_eq!(s.tool_use_count, 3);
    }

    #[test]
    fn store_counts_top_level_tool_use_names() {
        let store = SessionStore::new();
        store.ingest(&ev("a", "tool_use", json!({ "name": "WebFetch" })));
        store.ingest(&ev("a", "tool_use", json!({ "name": "WebFetch" })));
        let s = store.session("a").unwrap();
        assert_eq!(s.tool_counts.get("WebFetch"), Some(&2));
        assert_eq!(s.tool_use_count, 2);
    }

    #[test]
    fn store_per_session_buffer_caps() {
        // Sanity-check cap behaviour with a smaller artificial sequence;
        // we just verify that ingesting more than the cap retains the most recent.
        let store = SessionStore::new();
        for i in 0..(PER_SESSION_RECENT_CAP + 50) {
            store.ingest(&ev(
                "x",
                "user",
                json!({ "content": format!("msg {i}"), "_marker": i }),
            ));
        }
        let evs = store.session_events("x");
        assert_eq!(evs.len(), PER_SESSION_RECENT_CAP);
        let last = evs.last().unwrap();
        assert_eq!(
            last.entry.get("_marker").and_then(|v| v.as_u64()),
            Some((PER_SESSION_RECENT_CAP + 49) as u64)
        );
    }

    #[test]
    fn snapshot_orders_by_last_seen() {
        let store = SessionStore::new();
        store.ingest(&ev("old", "user", json!({})));
        // Sleep a tick so observed_at differs reliably.
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.ingest(&ev("new", "user", json!({})));
        let snap = store.snapshot(10);
        assert_eq!(snap.sessions[0].id, "new");
        assert_eq!(snap.sessions[1].id, "old");
    }
}
