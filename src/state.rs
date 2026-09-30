use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    db::{Db, Upsert},
    event::TraceEvent,
};

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

    /// Session title reported by the agent (AI-generated titles, session
    /// names), when present.
    pub title: Option<String>,
    /// The first real user prompt of the session, truncated — a readable
    /// label for agents that never title their sessions.
    #[serde(default)]
    pub first_prompt: Option<String>,

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
            first_prompt: None,
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
        // Adopt the event's source on the first event so freshly created stats
        // blocks do not keep the legacy claude-code default when the session is
        // actually attributed to some other source (including "unknown").
        if self.event_count == 0 {
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
            "tool_result" => {
                // Mirror of the `tool_use` guard above: adapters that already
                // populated `tool_results` (Codex, Cursor, Copilot) are counted
                // by the loop below, so only count the record itself when it
                // carries no explicit result ids.
                if ev.tool_results.is_empty() {
                    self.tool_result_count += 1;
                }
            }
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

        if let Some(t) = &ev.title {
            // Claude Code's `summary` records are older-style short titles
            // (and can describe the conversation a resumed session came
            // from): a fallback only, never over an AI or user-set title.
            if ev.event_type != "summary" || self.title.is_none() {
                self.title = Some(t.clone());
            }
        }
        if self.first_prompt.is_none() {
            self.first_prompt = first_prompt_of(ev);
        }
    }

    /// Which labels taken from a single record `old` supplied and its
    /// replacement `new` (none when it is removed) does not carry forward.
    /// Those are re-derived from the records that remain.
    fn stale_labels(&self, old: &TraceEvent, new: Option<&TraceEvent>) -> StaleLabels {
        let prompt = first_prompt_of(old);
        StaleLabels {
            prompt: prompt.is_some()
                && prompt == self.first_prompt
                && new.map_or(true, |n| first_prompt_of(n) != self.first_prompt),
            title: old.title.is_some()
                && old.title == self.title
                && new.map_or(true, |n| n.title != self.title),
            // A replacement with a timestamp sets its own on ingest.
            timestamp: old.timestamp.is_some()
                && old.timestamp == self.last_entry_timestamp
                && new.map_or(true, |n| n.timestamp.is_none()),
        }
    }

    /// Undo [`SessionStats::ingest`] for an event that is being replaced or
    /// removed, so in-place record updates never double count. Identity and
    /// timing fields (cwd, first/last seen) are left alone; labels taken
    /// from the record are handled by [`SessionStats::stale_labels`].
    fn retract(&mut self, ev: &TraceEvent) {
        self.event_count = self.event_count.saturating_sub(1);
        match ev.event_type.as_str() {
            "user" => self.user_count = self.user_count.saturating_sub(1),
            "assistant" => self.assistant_count = self.assistant_count.saturating_sub(1),
            "tool_use" if ev.tool_uses.is_empty() => {
                self.tool_use_count = self.tool_use_count.saturating_sub(1);
                if let Some(name) = ev.entry.get("name").and_then(|v| v.as_str()) {
                    decrement(&mut self.tool_counts, name);
                }
            }
            "tool_result" if ev.tool_results.is_empty() => {
                self.tool_result_count = self.tool_result_count.saturating_sub(1);
            }
            "system" => self.system_count = self.system_count.saturating_sub(1),
            _ => {}
        }
        for name in &ev.tool_uses {
            self.tool_use_count = self.tool_use_count.saturating_sub(1);
            decrement(&mut self.tool_counts, name);
        }
        self.tool_result_count = self.tool_result_count.saturating_sub(ev.tool_results.len());
        if let Some(u) = &ev.usage {
            self.input_tokens = self.input_tokens.saturating_sub(u.input);
            self.output_tokens = self.output_tokens.saturating_sub(u.output);
            self.cache_read_tokens = self.cache_read_tokens.saturating_sub(u.cache_read);
            self.cache_creation_tokens =
                self.cache_creation_tokens.saturating_sub(u.cache_creation);
        }
        self.cost_usd = (self.cost_usd - ev.cost_usd).max(0.0);
    }
}

/// Session labels that came from a record that has changed or gone.
#[derive(Debug, Default, Clone, Copy)]
struct StaleLabels {
    prompt: bool,
    title: bool,
    timestamp: bool,
}

impl StaleLabels {
    fn any(self) -> bool {
        self.prompt || self.title || self.timestamp
    }
}

fn decrement(map: &mut HashMap<String, usize>, key: &str) {
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

/// Text of a genuine user prompt, skipping tool results and the context
/// blocks agents inject as user turns (environment info, command wrappers,
/// instructions files).
fn first_prompt_of(ev: &TraceEvent) -> Option<String> {
    let msg = ev.message.as_ref()?;
    if msg.role != crate::message::Role::User {
        return None;
    }
    let text = msg.plain_text();
    let t = text.trim();
    if t.is_empty() || is_injected_context(t) {
        return None;
    }
    Some(crate::sources::truncate(t, 160))
}

fn is_injected_context(t: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<environment_context",
        "<user_instructions",
        "<command-",
        "<local-command",
        "<system-reminder",
        "Caveat:",
        "# AGENTS.md",
        "<user_action",
        "This session is being continued",
    ];
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// What [`SessionStore::ingest`] did with an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    /// A record not seen before.
    New,
    /// A record whose content changed in place (documents, databases).
    Updated,
    /// Already stored with identical content; nothing changed.
    Unchanged,
}

impl Ingested {
    pub fn changed(self) -> bool {
        self != Ingested::Unchanged
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
    pub fn ingest(&self, ev: &TraceEvent) -> Ingested {
        let mut g = self.inner.write().expect("session store poisoned");

        // Find the previous version of this record, if any. With a database
        // that is authoritative; without one, the in-memory ring buffer is the
        // best we have (records evicted from it are treated as new).
        let previous: Option<TraceEvent> = match &self.db {
            Some(db) => match db.upsert_event(ev) {
                Ok(Upsert::Inserted) => None,
                Ok(Upsert::Unchanged) => return Ingested::Unchanged,
                Ok(Upsert::Updated(old)) => Some(*old),
                Err(e) => {
                    warn!("Failed to persist event to database: {e}");
                    None
                }
            },
            None => {
                let prior = g
                    .per_session_events
                    .get(&ev.session_id)
                    .and_then(|q| q.iter().rev().find(|e| e.line_index == ev.line_index));
                match prior {
                    Some(p) if p.entry == ev.entry => return Ingested::Unchanged,
                    Some(p) => Some(p.clone()),
                    None => None,
                }
            }
        };

        let outcome = if previous.is_some() {
            Ingested::Updated
        } else {
            g.total_events += 1;
            Ingested::New
        };

        let stats = g.sessions.entry(ev.session_id.clone()).or_default();
        let stale = match &previous {
            Some(old) => {
                let stale = stats.stale_labels(old, Some(ev));
                stats.retract(old);
                stale
            }
            None => StaleLabels::default(),
        };
        stats.ingest(ev);

        let per = g
            .per_session_events
            .entry(ev.session_id.clone())
            .or_default();
        if previous.is_some() {
            if let Some(slot) = per.iter_mut().rev().find(|e| e.line_index == ev.line_index) {
                *slot = ev.clone();
            } else {
                per.push_back(ev.clone());
            }
        } else {
            per.push_back(ev.clone());
        }
        while per.len() > PER_SESSION_RECENT_CAP {
            per.pop_front();
        }

        let replaced = previous.is_some()
            && g.global_events
                .iter_mut()
                .rev()
                .find(|e| e.line_index == ev.line_index && e.session_id == ev.session_id)
                .map(|slot| *slot = ev.clone())
                .is_some();
        if !replaced {
            g.global_events.push_back(ev.clone());
        }
        while g.global_events.len() > GLOBAL_RECENT_CAP {
            g.global_events.pop_front();
        }
        self.relabel(&mut g, &ev.session_id, stale);
        self.persist_session(&g, &ev.session_id, stale);
        outcome
    }

    /// Re-derive the labels a changed or removed record supplied from the
    /// session's remaining records: the first prompt, the title (the latest
    /// non-summary title, else the first summary) and the latest record
    /// timestamp.
    fn relabel(&self, g: &mut Inner, session_id: &str, stale: StaleLabels) {
        if !stale.any() {
            return;
        }
        let find = |newest_first: bool, pred: &mut dyn FnMut(&mut TraceEvent) -> bool| {
            self.find_event(g, session_id, newest_first, pred)
        };
        let prompt = stale.prompt.then(|| {
            find(false, &mut |e| {
                e.hydrate();
                first_prompt_of(e).is_some()
            })
            .and_then(|e| first_prompt_of(&e))
        });
        let title = stale.title.then(|| {
            find(true, &mut |e| {
                e.title.is_some() && e.event_type != "summary"
            })
            .or_else(|| find(false, &mut |e| e.title.is_some()))
            .and_then(|e| e.title)
        });
        let timestamp = stale
            .timestamp
            .then(|| find(true, &mut |e| e.timestamp.is_some()).and_then(|e| e.timestamp));
        if let Some(stats) = g.sessions.get_mut(session_id) {
            if let Some(p) = prompt {
                stats.first_prompt = p;
            }
            if let Some(t) = title {
                stats.title = t;
            }
            if let Some(t) = timestamp {
                stats.last_entry_timestamp = t;
            }
        }
    }

    /// The first of a session's records, in line order (or newest first),
    /// that `pred` accepts: from the database when there is one, else from
    /// the in-memory buffer.
    fn find_event(
        &self,
        g: &Inner,
        session_id: &str,
        newest_first: bool,
        pred: &mut dyn FnMut(&mut TraceEvent) -> bool,
    ) -> Option<TraceEvent> {
        if let Some(db) = &self.db {
            return db
                .find_session_event(session_id, newest_first, pred)
                .unwrap_or_else(|e| {
                    warn!("Failed to read session events: {e}");
                    None
                });
        }
        let mut evs: Vec<&TraceEvent> = g.per_session_events.get(session_id)?.iter().collect();
        evs.sort_by_key(|e| e.line_index);
        if newest_first {
            evs.reverse();
        }
        evs.into_iter().find_map(|e| {
            let mut e = e.clone();
            pred(&mut e).then_some(e)
        })
    }

    fn persist_session(&self, g: &Inner, session_id: &str, relabelled: StaleLabels) {
        if let (Some(db), Some(stats)) = (&self.db, g.sessions.get(session_id)) {
            let mut res = db.upsert_session(stats);
            if res.is_ok() && relabelled.any() {
                res = db.set_session_labels(stats);
            }
            if let Err(e) = res {
                warn!("Failed to persist session aggregates: {e}");
            }
        }
    }

    /// Remove a record that disappeared from its source (a document rewritten
    /// shorter, a rewind). Returns it if it existed. A session left with no
    /// events is dropped altogether (its annotations are kept).
    pub fn remove(&self, session_id: &str, line_index: usize) -> Option<TraceEvent> {
        let mut g = self.inner.write().expect("session store poisoned");
        let old: Option<TraceEvent> = match &self.db {
            Some(db) => match db.delete_event(session_id, line_index) {
                Ok(o) => o,
                Err(e) => {
                    warn!("Failed to delete event from database: {e}");
                    None
                }
            },
            None => g
                .per_session_events
                .get(session_id)
                .and_then(|q| q.iter().rev().find(|e| e.line_index == line_index).cloned()),
        };
        let mut old = old?;
        old.hydrate();
        g.total_events = g.total_events.saturating_sub(1);
        let mut emptied = false;
        let mut stale = StaleLabels::default();
        if let Some(stats) = g.sessions.get_mut(session_id) {
            stale = stats.stale_labels(&old, None);
            stats.retract(&old);
            emptied = stats.event_count == 0;
        }
        if emptied {
            g.sessions.remove(session_id);
            g.per_session_events.remove(session_id);
            if let Some(db) = &self.db {
                if let Err(e) = db.delete_session(session_id) {
                    warn!("Failed to persist session aggregates: {e}");
                }
            }
        } else {
            if let Some(q) = g.per_session_events.get_mut(session_id) {
                q.retain(|e| e.line_index != line_index);
            }
            self.relabel(&mut g, session_id, stale);
            self.persist_session(&g, session_id, stale);
        }
        g.global_events
            .retain(|e| !(e.session_id == session_id && e.line_index == line_index));
        Some(old)
    }

    /// The attached database, if any.
    pub fn db(&self) -> Option<&Db> {
        self.db.as_ref()
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

    /// Each call gets a fresh line index: the store keys records on
    /// (session, line), so reusing one would model an in-place update.
    fn ev(session: &str, kind: &str, body: serde_json::Value) -> TraceEvent {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static LINE: AtomicUsize = AtomicUsize::new(0);
        let mut val = body;
        val["type"] = json!(kind);
        val["sessionId"] = json!(session);
        TraceEvent::from_raw("fallback", LINE.fetch_add(1, Ordering::Relaxed), val)
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

    /// A record at a fixed line, so tests can revise or remove it.
    fn at(session: &str, line: usize, body: serde_json::Value) -> TraceEvent {
        let mut val = body;
        val["sessionId"] = json!(session);
        TraceEvent::from_raw("fallback", line, val)
    }

    fn labels_follow_the_records_that_supply_them(store: SessionStore) {
        let prompt = |line, text: &str, ts: &str| {
            at(
                "s",
                line,
                json!({ "type": "user", "timestamp": ts,
                        "message": { "role": "user", "content": text } }),
            )
        };
        let title = |line, t: &str| at("s", line, json!({ "type": "ai-title", "aiTitle": t }));
        store.ingest(&prompt(0, "first question", "2026-01-01T00:00:00Z"));
        store.ingest(&title(1, "Early title"));
        store.ingest(&prompt(2, "second question", "2026-01-01T00:01:00Z"));
        store.ingest(&title(3, "Better title"));
        store.ingest(&prompt(4, "third question", "2026-01-01T00:02:00Z"));
        let s = store.session("s").unwrap();
        assert_eq!(s.first_prompt.as_deref(), Some("first question"));
        assert_eq!(s.title.as_deref(), Some("Better title"));
        assert_eq!(
            s.last_entry_timestamp.as_deref(),
            Some("2026-01-01T00:02:00Z")
        );

        // The first prompt revised in place.
        store.ingest(&prompt(
            0,
            "first question, reworded",
            "2026-01-01T00:00:00Z",
        ));
        let s = store.session("s").unwrap();
        assert_eq!(s.first_prompt.as_deref(), Some("first question, reworded"));

        // The records behind each label removed.
        store.remove("s", 0);
        store.remove("s", 3);
        store.remove("s", 4);
        let s = store.session("s").unwrap();
        assert_eq!(s.first_prompt.as_deref(), Some("second question"));
        assert_eq!(s.title.as_deref(), Some("Early title"));
        assert_eq!(
            s.last_entry_timestamp.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
        if let Some(db) = store.db() {
            let saved = db.load_sessions().unwrap().remove(0);
            assert_eq!(saved.first_prompt.as_deref(), Some("second question"));
            assert_eq!(saved.title.as_deref(), Some("Early title"));
        }
    }

    #[test]
    fn labels_follow_their_records_in_memory() {
        labels_follow_the_records_that_supply_them(SessionStore::new());
    }

    #[test]
    fn labels_follow_their_records_in_the_database() {
        labels_follow_the_records_that_supply_them(SessionStore::with_db(
            Db::open_in_memory().unwrap(),
        ));
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
    fn store_adopts_unknown_source_on_first_event() {
        let store = SessionStore::new();
        let ev = TraceEvent::from_raw_as(
            "fallback",
            0,
            json!({
                "type": "user",
                "sessionId": "u1",
                "content": "hi"
            }),
            crate::sources::AgentSource::Unknown,
        );

        store.ingest(&ev);

        assert_eq!(store.session("u1").unwrap().source, "unknown");
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
    #[test]
    fn store_counts_tool_results_once_per_record() {
        // A Codex `function_call_output` sets both `event_type = "tool_result"`
        // and `tool_results = ["c1"]`; it must count as one result, not two.
        let store = SessionStore::new();
        let ev = TraceEvent::from_raw_as(
            "s",
            0,
            json!({
                "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "c1", "output": "done"}
            }),
            crate::sources::AgentSource::Codex,
        );
        assert_eq!(ev.event_type, "tool_result");
        assert_eq!(ev.tool_results, vec!["c1"]);

        store.ingest(&ev);
        assert_eq!(store.session("s").unwrap().tool_result_count, 1);
    }

    #[test]
    fn store_counts_claude_tool_result_blocks() {
        // Claude Code carries tool results as blocks inside a `user` record —
        // the guard above must not stop those from being counted.
        let store = SessionStore::new();
        let ev = TraceEvent::from_raw_as(
            "s",
            0,
            json!({
                "type": "user",
                "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "a"},
                    {"type": "tool_result", "tool_use_id": "b"}
                ]}
            }),
            crate::sources::AgentSource::ClaudeCode,
        );
        store.ingest(&ev);
        assert_eq!(store.session("s").unwrap().tool_result_count, 2);
    }
    #[test]
    fn store_aggregates_codex_token_deltas_not_cumulative_totals() {
        // Codex `token_count` records carry a cumulative `total_token_usage`
        // alongside the per-event `last_token_usage`. Aggregation is `+=`, so
        // summing the cumulative snapshots would report 750 here instead of the
        // true session total of 400.
        let store = SessionStore::new();
        for (i, (total, last)) in [(100u64, 100u64), (250, 150), (400, 150)]
            .iter()
            .enumerate()
        {
            let ev = TraceEvent::from_raw_as(
                "s",
                i,
                json!({
                    "type": "event_msg",
                    "payload": {"type": "token_count", "info": {
                        "total_token_usage": {"input_tokens": total, "output_tokens": 0},
                        "last_token_usage":  {"input_tokens": last,  "output_tokens": 0}
                    }}
                }),
                crate::sources::AgentSource::Codex,
            );
            store.ingest(&ev);
        }
        assert_eq!(store.session("s").unwrap().input_tokens, 400);
    }
}
