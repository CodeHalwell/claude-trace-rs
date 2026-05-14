use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::event::TraceEvent;

/// Cap on how many events we retain per session in memory for client backfill.
pub const PER_SESSION_RECENT_CAP: usize = 5_000;

/// Cap on how many events we retain across all sessions for the global feed.
pub const GLOBAL_RECENT_CAP: usize = 20_000;

/// Per-session aggregated stats and a bounded buffer of recent events.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub id: String,
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
    pub thinking_count: usize,

    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_usd: f64,

    /// Tool name → invocation count.
    pub tool_counts: HashMap<String, usize>,

    /// AI-generated title from `ai-title` events, when present.
    pub title: Option<String>,
}

impl SessionStats {
    fn ingest(&mut self, ev: &TraceEvent) {
        if self.id.is_empty() {
            self.id = ev.session_id.clone();
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
            "tool_use" => self.tool_use_count += 1,
            "tool_result" => self.tool_result_count += 1,
            "system" => self.system_count += 1,
            _ => {}
        }

        // Tool uses can be top-level (rare) or embedded in assistant content.
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

#[derive(Debug)]
struct Inner {
    sessions: HashMap<String, SessionStats>,
    /// Per-session ring buffer of recent events.
    per_session_events: HashMap<String, VecDeque<TraceEvent>>,
    /// Global ring buffer for the live feed.
    global_events: VecDeque<TraceEvent>,
    total_events: usize,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            per_session_events: HashMap::new(),
            global_events: VecDeque::new(),
            total_events: 0,
        }
    }
}

/// Shared, thread-safe session store. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct SessionStore {
    inner: Arc<RwLock<Inner>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an event in the store, updating aggregates and ring buffers.
    pub fn ingest(&self, ev: &TraceEvent) {
        let mut g = self.inner.write().expect("session store poisoned");
        g.total_events += 1;

        let stats = g
            .sessions
            .entry(ev.session_id.clone())
            .or_insert_with(SessionStats::default);
        stats.ingest(ev);

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

        let n = recent_events.min(g.global_events.len());
        let events: Vec<TraceEvent> = g
            .global_events
            .iter()
            .rev()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
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
        self.inner.read().expect("session store poisoned").total_events
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
