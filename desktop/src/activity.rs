//! Turns the live event stream into desktop notices: an agent finished its
//! turn (or went quiet waiting on you), a new session started, or today's
//! spend crossed the budget.
//!
//! Pure logic with an injectable clock so it can be unit tested without a
//! window system.

use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, NaiveDate, Utc};
use claude_trace_rs::{event::TraceEvent, message::Role, sources::AgentSource};

/// A session we are not following yet only counts as live if its record is
/// at most this old (replayed history is also marked explicitly).
const FRESH: chrono::Duration = chrono::Duration::minutes(3);
/// A session counts as active if it produced an event this recently.
const ACTIVE_WINDOW: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    TurnFinished {
        session_id: String,
        agent: String,
        project: String,
        duration: Duration,
        cost_usd: f64,
        preview: Option<String>,
        /// Inferred from silence rather than an explicit end-of-turn marker.
        idle: bool,
    },
    NewSession {
        session_id: String,
        agent: String,
        project: String,
    },
    BudgetExceeded {
        spent_usd: f64,
        budget_usd: f64,
    },
}

#[derive(Debug)]
struct SessionActivity {
    agent: String,
    project: String,
    last_event: Instant,
    last_role: Option<Role>,
    turn_started: Option<Instant>,
    /// The record whose prompt started the current turn.
    turn_line: Option<usize>,
    turn_cost: f64,
    preview: Option<String>,
    /// Highest record index seen, if any. Documents and databases re-emit a
    /// record when it changes; a re-emitted earlier record is a revision,
    /// not a new turn.
    max_line: Option<usize>,
    /// Every record seen: its role and last cost, so a revision adds only
    /// what changed (a streamed record often gains its usage when it is
    /// finalised) and a retraction can take it back.
    lines: BTreeMap<usize, Line>,
}

#[derive(Debug, Clone, Copy)]
struct Line {
    role: Option<Role>,
    cost: f64,
}

#[derive(Debug)]
pub struct Activity {
    sessions: HashMap<String, SessionActivity>,
    idle: Duration,
    budget_usd: Option<f64>,
    day: NaiveDate,
    spent_today: f64,
    budget_alerted: bool,
}

impl Activity {
    pub fn new(idle: Duration, budget_usd: Option<f64>) -> Self {
        Self {
            sessions: HashMap::new(),
            idle,
            budget_usd,
            day: Local::now().date_naive(),
            spent_today: 0.0,
            budget_alerted: false,
        }
    }

    pub fn configure(&mut self, idle: Duration, budget_usd: Option<f64>) {
        self.idle = idle;
        if budget_usd != self.budget_usd {
            self.budget_alerted = false;
        }
        self.budget_usd = budget_usd;
    }

    /// Start the day's running total from the persisted history. A budget
    /// already exceeded at start-up does not alert (it did when it happened).
    pub fn seed_spent_today(&mut self, usd: f64) {
        self.roll_day();
        self.spent_today = usd;
        self.budget_alerted = self.budget_usd.is_some_and(|b| usd >= b);
    }

    /// Replace the running total with the database's figure. Live events keep
    /// it current between refreshes; the refresh corrects for revised records
    /// and history imported in the background.
    pub fn refresh_spent_today(&mut self, usd: f64) -> Vec<Notice> {
        self.roll_day();
        self.spent_today = usd;
        self.check_budget().into_iter().collect()
    }

    fn check_budget(&mut self) -> Option<Notice> {
        let budget = self.budget_usd?;
        if self.budget_alerted || self.spent_today < budget {
            return None;
        }
        self.budget_alerted = true;
        Some(Notice::BudgetExceeded {
            spent_usd: self.spent_today,
            budget_usd: budget,
        })
    }

    pub fn spent_today(&self) -> f64 {
        self.spent_today
    }

    pub fn active_sessions(&self, now: Instant) -> usize {
        self.sessions
            .values()
            .filter(|s| now.duration_since(s.last_event) < ACTIVE_WINDOW)
            .count()
    }

    /// Feed one event. Returns any notices it triggers.
    pub fn on_event(&mut self, ev: &TraceEvent, now: Instant) -> Vec<Notice> {
        let mut out = Vec::new();
        if ev.removed {
            self.retract(ev);
            return out;
        }
        if ev.replayed {
            return out;
        }
        let is_new = !self.sessions.contains_key(&ev.session_id);
        // A record's own timestamp only decides whether a session we are not
        // following yet is live. Once it is, in-place revisions keep their
        // creation time however late they arrive, and still count.
        if is_new && !is_fresh(ev) {
            return out;
        }
        self.roll_day();

        let s = self
            .sessions
            .entry(ev.session_id.clone())
            .or_insert_with(|| SessionActivity {
                agent: agent_name(&ev.source),
                project: project_name(ev.cwd.as_deref()),
                last_event: now,
                last_role: None,
                turn_started: None,
                turn_line: None,
                turn_cost: 0.0,
                preview: None,
                max_line: None,
                lines: BTreeMap::new(),
            });
        let revision = s.max_line.is_some_and(|m| ev.line_index <= m);
        s.max_line = Some(s.max_line.map_or(ev.line_index, |m| m.max(ev.line_index)));
        let role = ev
            .message
            .as_ref()
            .map(|m| m.role)
            .filter(|r| *r != Role::System);
        let line = Line {
            role,
            cost: ev.cost_usd,
        };
        let cost_delta = match s.lines.insert(ev.line_index, line) {
            Some(prev) => ev.cost_usd - prev.cost,
            // A revision of a record from before we started following the
            // session: its cost is already in the day's total.
            None if revision => 0.0,
            None => ev.cost_usd,
        };
        if s.project == "unknown project" {
            if let Some(cwd) = ev.cwd.as_deref() {
                s.project = project_name(Some(cwd));
            }
        }
        // Only the first record of a session announces it: a session that
        // was already running when the app started is not "new".
        if is_new && ev.line_index == 0 {
            out.push(Notice::NewSession {
                session_id: ev.session_id.clone(),
                agent: s.agent.clone(),
                project: s.project.clone(),
            });
        }

        if let Some(msg) = &ev.message {
            if msg.role == Role::User && msg.has_text() && s.turn_started.is_none() && !revision {
                s.turn_started = Some(now);
                s.turn_line = Some(ev.line_index);
                s.turn_cost = 0.0;
                let text = msg.plain_text();
                s.preview = Some(claude_trace_rs::sources::truncate(text.trim(), 90));
            }
            if msg.role != Role::System {
                s.last_role = Some(msg.role);
            }
        }
        s.last_event = now;
        s.turn_cost += cost_delta;

        if ev.turn_end {
            if let Some(start) = s.turn_started.take() {
                s.turn_line = None;
                out.push(Notice::TurnFinished {
                    session_id: ev.session_id.clone(),
                    agent: s.agent.clone(),
                    project: s.project.clone(),
                    duration: now.duration_since(start),
                    cost_usd: s.turn_cost,
                    preview: s.preview.take(),
                    idle: false,
                });
            }
        }

        self.spent_today += cost_delta;
        out.extend(self.check_budget());
        out
    }

    /// Periodic check for turns that ended without an explicit marker: the
    /// assistant spoke last and the session has been quiet for `idle`.
    pub fn tick(&mut self, now: Instant) -> Vec<Notice> {
        self.roll_day();
        let mut out = Vec::new();
        for (id, s) in self.sessions.iter_mut() {
            let Some(start) = s.turn_started else {
                continue;
            };
            if s.last_role == Some(Role::Assistant) && now.duration_since(s.last_event) >= self.idle
            {
                s.turn_started = None;
                s.turn_line = None;
                out.push(Notice::TurnFinished {
                    session_id: id.clone(),
                    agent: s.agent.clone(),
                    project: s.project.clone(),
                    duration: s.last_event.duration_since(start),
                    cost_usd: s.turn_cost,
                    preview: s.preview.take(),
                    idle: true,
                });
            }
        }
        // Forget sessions quiet for a long time to bound memory.
        self.sessions
            .retain(|_, s| now.duration_since(s.last_event) < Duration::from_secs(6 * 3600));
        out
    }

    /// A record was deleted at its source (a rewind, a compaction): take back
    /// its cost, and let the session's state reflect the records that remain.
    fn retract(&mut self, ev: &TraceEvent) {
        let Some(s) = self.sessions.get_mut(&ev.session_id) else {
            return;
        };
        let Some(line) = s.lines.remove(&ev.line_index) else {
            return;
        };
        if s.turn_line.is_some_and(|t| ev.line_index >= t) {
            s.turn_cost = (s.turn_cost - line.cost).max(0.0);
        }
        self.spent_today = (self.spent_today - line.cost).max(0.0);
        if s.turn_line == Some(ev.line_index) {
            // The prompt itself is gone, so the turn is too.
            s.turn_started = None;
            s.turn_line = None;
            s.turn_cost = 0.0;
            s.preview = None;
        }
        s.last_role = s.lines.values().rev().find_map(|l| l.role);
        // Records written in place of the deleted ones are new, not
        // revisions.
        s.max_line = s.lines.keys().next_back().copied();
    }

    fn roll_day(&mut self) {
        let today = Local::now().date_naive();
        if today != self.day {
            self.day = today;
            self.spent_today = 0.0;
            self.budget_alerted = false;
        }
    }
}

fn is_fresh(ev: &TraceEvent) -> bool {
    let ts = ev
        .timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    match ts {
        Some(t) => Utc::now().signed_duration_since(t) < FRESH,
        // No record timestamp: trust the observation time.
        None => DateTime::parse_from_rfc3339(&ev.observed_at)
            .map(|t| Utc::now().signed_duration_since(t.with_timezone(&Utc)) < FRESH)
            .unwrap_or(true),
    }
}

pub fn agent_name(source: &str) -> String {
    AgentSource::parse(source)
        .map(|s| s.display_name().to_owned())
        .unwrap_or_else(|| source.to_owned())
}

pub fn project_name(cwd: Option<&str>) -> String {
    cwd.and_then(|c| {
        std::path::Path::new(c)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    })
    .unwrap_or_else(|| "unknown project".to_owned())
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_trace_rs::sources::AgentSource;
    use serde_json::json;

    /// Each session's records are numbered from 0, as in a real log.
    fn ev(session: &str, raw: serde_json::Value) -> TraceEvent {
        use std::sync::Mutex;
        static LINES: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
        let line = {
            let mut g = LINES.lock().unwrap();
            let n = g
                .get_or_insert_with(HashMap::new)
                .entry(session.into())
                .or_insert(0);
            *n += 1;
            *n - 1
        };
        let mut raw = raw;
        raw["sessionId"] = json!(session);
        raw["timestamp"] = json!(Utc::now().to_rfc3339());
        raw["cwd"] = json!("/home/me/widgets");
        TraceEvent::from_raw_as(session, line, raw, AgentSource::ClaudeCode)
    }

    fn user(session: &str, text: &str) -> TraceEvent {
        ev(
            session,
            json!({"type":"user","message":{"role":"user","content":text}}),
        )
    }

    fn assistant(session: &str, end: bool) -> TraceEvent {
        ev(
            session,
            json!({"type":"assistant","message":{"role":"assistant",
                "content":[{"type":"text","text":"done"}],
                "stop_reason": if end {"end_turn"} else {"tool_use"},
                "usage":{"input_tokens":1000,"output_tokens":100}}}),
        )
    }

    #[test]
    fn explicit_turn_end_notifies_once() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let t0 = Instant::now();
        let n = a.on_event(&user("s1", "fix the build"), t0);
        assert!(matches!(n[0], Notice::NewSession { .. }));
        assert!(a.on_event(&assistant("s1", false), t0).is_empty());
        let n = a.on_event(&assistant("s1", true), t0 + Duration::from_secs(5));
        match &n[0] {
            Notice::TurnFinished {
                agent,
                project,
                idle,
                preview,
                ..
            } => {
                assert_eq!(agent, "Claude Code");
                assert_eq!(project, "widgets");
                assert!(!idle);
                assert_eq!(preview.as_deref(), Some("fix the build"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // No second notification from the idle path.
        assert!(a.tick(t0 + Duration::from_secs(120)).is_empty());
    }

    #[test]
    fn idle_fallback_for_agents_without_markers() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let t0 = Instant::now();
        a.on_event(&user("s2", "hello"), t0);
        a.on_event(&assistant("s2", false), t0 + Duration::from_secs(2));
        assert!(a.tick(t0 + Duration::from_secs(10)).is_empty());
        let n = a.tick(t0 + Duration::from_secs(40));
        assert!(matches!(n[0], Notice::TurnFinished { idle: true, .. }));
    }

    #[test]
    fn events_marked_as_replayed_never_notify() {
        let mut a = Activity::new(Duration::from_secs(30), Some(0.0001));
        let mut ev = user("s7", "from the start-up scan");
        ev.replayed = true;
        assert!(a.on_event(&ev, Instant::now()).is_empty());
        assert_eq!(a.active_sessions(Instant::now()), 0);
    }

    #[test]
    fn live_revisions_count_even_with_old_timestamps() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let t0 = Instant::now();
        a.on_event(&user("s8", "long job"), t0);
        // The reply is streamed into one record: first without usage...
        let mut draft = assistant("s8", false);
        draft.usage = None;
        draft.cost_usd = 0.0;
        a.on_event(&draft, t0);
        // ...then finalised in place with its cost and an end-of-turn
        // marker, long after the record's own timestamp.
        let mut done = assistant("s8", true);
        done.line_index = draft.line_index;
        done.timestamp = Some("2020-01-01T00:00:00Z".into());
        let n = a.on_event(&done, t0 + Duration::from_secs(600));
        match &n[..] {
            [Notice::TurnFinished { cost_usd, .. }] => {
                assert!((cost_usd - done.cost_usd).abs() < 1e-12 && *cost_usd > 0.0)
            }
            other => panic!("expected a finished turn, got {other:?}"),
        }
        assert!((a.spent_today() - done.cost_usd).abs() < 1e-12);
    }

    #[test]
    fn tombstones_are_ignored() {
        let mut a = Activity::new(Duration::from_secs(30), Some(0.0001));
        let mut ev = assistant("s9", true);
        ev.removed = true;
        assert!(a.on_event(&ev, Instant::now()).is_empty());
        assert_eq!(a.spent_today(), 0.0);
    }

    #[test]
    fn a_retracted_reply_is_taken_back() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let t0 = Instant::now();
        a.on_event(&user("s10", "try this"), t0);
        let reply = assistant("s10", false);
        a.on_event(&reply, t0);
        assert!(a.spent_today() > 0.0);
        // A rewind deletes the reply: nothing has answered the prompt now.
        let mut gone = reply.clone();
        gone.removed = true;
        assert!(a.on_event(&gone, t0).is_empty());
        assert_eq!(a.spent_today(), 0.0);
        assert!(
            a.tick(t0 + Duration::from_secs(60)).is_empty(),
            "a deleted reply finished the turn"
        );
        // The reply written in its place counts, and ends the turn.
        let mut again = assistant("s10", false);
        again.line_index = reply.line_index;
        a.on_event(&again, t0 + Duration::from_secs(61));
        assert!((a.spent_today() - again.cost_usd).abs() < 1e-12);
        match &a.tick(t0 + Duration::from_secs(120))[..] {
            [Notice::TurnFinished { cost_usd, .. }] => {
                assert!((cost_usd - again.cost_usd).abs() < 1e-12)
            }
            other => panic!("expected a finished turn, got {other:?}"),
        }
    }

    #[test]
    fn a_retracted_prompt_cancels_its_turn() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let t0 = Instant::now();
        let prompt = user("s11", "never mind");
        a.on_event(&prompt, t0);
        let reply = assistant("s11", false);
        a.on_event(&reply, t0);
        for ev in [&reply, &prompt] {
            let mut gone = ev.clone();
            gone.removed = true;
            a.on_event(&gone, t0);
        }
        assert!(a.tick(t0 + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn replayed_history_is_ignored() {
        let mut a = Activity::new(Duration::from_secs(30), Some(0.0001));
        let mut old = user("s3", "old prompt");
        old.timestamp = Some("2020-01-01T00:00:00Z".into());
        assert!(a.on_event(&old, Instant::now()).is_empty());
        assert_eq!(a.active_sessions(Instant::now()), 0);
    }

    #[test]
    fn budget_alert_fires_once() {
        let mut a = Activity::new(Duration::from_secs(30), Some(0.001));
        let t0 = Instant::now();
        let n = a.on_event(&assistant("s4", false), t0);
        assert!(n.iter().any(|x| matches!(x, Notice::BudgetExceeded { .. })));
        let n = a.on_event(&assistant("s4", false), t0);
        assert!(!n.iter().any(|x| matches!(x, Notice::BudgetExceeded { .. })));
    }

    #[test]
    fn revised_records_do_not_start_turns_or_double_count() {
        let mut a = Activity::new(Duration::from_secs(30), Some(1000.0));
        let t0 = Instant::now();
        let prompt = user("s5", "first");
        a.on_event(&prompt, t0);
        let reply = assistant("s5", true);
        a.on_event(&reply, t0);
        let spent = a.spent_today();
        // The same records re-emitted after an upsert.
        assert!(a.on_event(&prompt, t0).is_empty());
        assert!(a.on_event(&reply, t0).is_empty());
        assert_eq!(a.spent_today(), spent);
        assert!(a.tick(t0 + Duration::from_secs(120)).is_empty());
    }

    #[test]
    fn sessions_already_running_are_not_announced() {
        let mut a = Activity::new(Duration::from_secs(30), None);
        let mut mid = user("s6", "carry on");
        mid.line_index = 40;
        let n = a.on_event(&mid, Instant::now());
        assert!(!n.iter().any(|x| matches!(x, Notice::NewSession { .. })));
    }

    #[test]
    fn refresh_can_trigger_budget_but_seed_does_not() {
        let mut a = Activity::new(Duration::from_secs(30), Some(5.0));
        a.seed_spent_today(7.0);
        assert!(
            a.refresh_spent_today(8.0).is_empty(),
            "already over at start"
        );
        let mut b = Activity::new(Duration::from_secs(30), Some(5.0));
        b.seed_spent_today(1.0);
        assert!(b.refresh_spent_today(2.0).is_empty());
        assert!(matches!(
            b.refresh_spent_today(6.0)[..],
            [Notice::BudgetExceeded { .. }]
        ));
        assert!(b.refresh_spent_today(9.0).is_empty(), "alerts once a day");
    }

    #[test]
    fn durations_format() {
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(format_duration(Duration::from_secs(3720)), "1h 02m");
    }
}
