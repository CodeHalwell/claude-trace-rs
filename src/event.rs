use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::sources::{self, AgentSource};

/// Canonical transport object for a single trace record from any supported
/// coding agent (see [`crate::sources::AgentSource`] for the full list).
///
/// We enrich the raw record with derived fields so the dashboard can render
/// useful information without having to walk the (sometimes very large) raw
/// JSON tree on every render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Session identifier — taken from the entry's session field if present,
    /// otherwise the source-file stem.
    pub session_id: String,
    /// Which coding agent produced this event.
    #[serde(default = "default_source")]
    pub source: String,
    /// Zero-based line position within the source file.
    pub line_index: usize,
    /// Raw parsed agent event record.
    pub entry: serde_json::Value,
    /// Server timestamp (RFC 3339) for when this line was observed.
    pub observed_at: String,
    /// Short, operator-friendly description of the event.
    pub summary: String,
    /// Top-level entry type (user, assistant, system, summary, etc.).
    pub event_type: String,
    /// Human-readable event timestamp from the entry itself, if present.
    pub timestamp: Option<String>,
    /// Working directory recorded in the entry, if present.
    pub cwd: Option<String>,
    /// Git branch recorded in the entry, if present.
    pub git_branch: Option<String>,
    /// Claude Code version recorded in the entry, if present.
    pub version: Option<String>,
    /// Model name (assistant entries).
    pub model: Option<String>,
    /// Names of any embedded `tool_use` blocks in the entry's content.
    #[serde(default)]
    pub tool_uses: Vec<String>,
    /// IDs of any embedded `tool_result` blocks in the entry's content.
    #[serde(default)]
    pub tool_results: Vec<String>,
    /// Token usage breakdown (assistant entries).
    pub usage: Option<TokenUsage>,
    /// Cost in USD — either pulled from an explicit cost field on the entry
    /// or estimated from the model and token usage.
    pub cost_usd: f64,
    /// Whether the cost was estimated client-side (vs. provided by the agent).
    pub cost_estimated: bool,
    /// The record as an agent-neutral transcript message, when it is part of
    /// the conversation. Not persisted (it is re-derived from `entry` when an
    /// event is read back from the database), so it never doubles storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// A session title carried by this record (AI-generated titles, agent
    /// session names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// True when this record marks the end of an agent turn — the agent has
    /// finished and is waiting on the user. Drives "agent finished"
    /// notifications in the desktop app.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turn_end: bool,
    /// Identity of the API response this record's usage belongs to, used by
    /// the ingestion engine to count repeated usage once. Transient.
    #[serde(skip)]
    pub usage_key: Option<String>,
}

/// Backward-compat default: traces recorded before the multi-agent upgrade
/// were all Claude Code sessions.
fn default_source() -> String {
    AgentSource::ClaudeCode.as_str().to_owned()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TraceEvent {
    /// Construct a `TraceEvent` from a raw JSON value, enriching it with
    /// server-side metadata. Equivalent to `from_raw_as` with
    /// [`AgentSource::Unknown`] — the record is enriched with the generic
    /// Claude-Code-shaped fallback heuristics.
    #[allow(dead_code)] // compat shim; all production paths use from_raw_as
    pub fn from_raw(session_id_fallback: &str, line_index: usize, raw: serde_json::Value) -> Self {
        Self::from_raw_as(session_id_fallback, line_index, raw, AgentSource::Unknown)
    }

    /// Construct a `TraceEvent` attributed to a specific agent source; the
    /// source's adapter performs the field extraction and summarisation.
    pub fn from_raw_as(
        session_id_fallback: &str,
        line_index: usize,
        raw: serde_json::Value,
        source: AgentSource,
    ) -> Self {
        let en = sources::enrich(source, &raw);

        let session_id = en
            .session_id
            .unwrap_or_else(|| session_id_fallback.to_owned());

        // Distinguish "cost the agent reported" from "cost we estimated".
        let explicit_cost = en.cost_explicit
            || raw
                .get("costUSD")
                .or_else(|| raw.get("cost_usd"))
                .or_else(|| raw.get("cost"))
                .and_then(|v| v.as_f64())
                .is_some();

        Self {
            session_id,
            source: source.as_str().to_owned(),
            line_index,
            observed_at: chrono::Utc::now().to_rfc3339(),
            summary: en.summary,
            event_type: en.event_type,
            timestamp: en.timestamp,
            cwd: en.cwd,
            git_branch: en.git_branch,
            version: en.version,
            model: en.model,
            tool_uses: en.tool_uses,
            tool_results: en.tool_results,
            usage: en.usage,
            cost_usd: en.cost_usd.unwrap_or(0.0),
            cost_estimated: !explicit_cost,
            message: en.message,
            title: en.title,
            turn_end: en.turn_end,
            usage_key: en.usage_key,
            entry: raw,
        }
    }
}

impl TraceEvent {
    /// The `(session_id, line_index)` identity used for de-duplication and
    /// in-place updates.
    pub fn key(&self) -> (&str, usize) {
        (&self.session_id, self.line_index)
    }

    /// Re-derive the fields that are not persisted (the canonical message)
    /// after reading an event back from storage.
    pub fn hydrate(&mut self) {
        if self.message.is_none() {
            let source = AgentSource::parse(&self.source).unwrap_or(AgentSource::Unknown);
            self.message = sources::enrich(source, &self.entry).message;
        }
    }

    /// Concatenated, plain-text representation of this event's textual content,
    /// used to populate the database's searchable column. Includes the summary
    /// plus any `text`/`thinking` blocks and string content found in the entry.
    pub fn search_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.summary);
        sources::collect_text(&self.entry, &mut out);
        // Keep the indexed text bounded so giant tool payloads don't bloat the DB.
        if out.len() > 16_384 {
            let mut end = 16_384;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
        }
        out
    }
}

/// Estimate a USD cost from a model name and token usage. Thin wrapper kept
/// for API compatibility; the pricing table lives in [`crate::sources`].
#[allow(dead_code)] // compat shim
pub fn estimate_cost(model: Option<&str>, u: &TokenUsage) -> f64 {
    sources::estimate_cost(model, u)
}

/// Produce a short human-readable summary for a raw Claude-Code-shaped JSONL
/// record. Kept for API compatibility; new code should use the per-agent
/// adapters via [`crate::sources::enrich`].
#[allow(dead_code)] // compat shim
pub fn summarise(val: &serde_json::Value, tool_uses: &[String]) -> String {
    sources::claude::summarise(val, tool_uses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_summarise_user() {
        let val = json!({ "type": "user", "content": "Hello, world!" });
        let s = summarise(&val, &[]);
        assert!(s.starts_with("👤"), "got: {s}");
        assert!(s.contains("Hello, world!"));
    }

    #[test]
    fn test_summarise_assistant_with_tools() {
        let val = json!({
            "type": "assistant",
            "message": {
                "content": [
                    { "type": "text", "text": "Here is the code" },
                    { "type": "tool_use", "name": "Read", "id": "abc" }
                ],
                "usage": { "input_tokens": 100, "output_tokens": 50 }
            }
        });
        let ev = TraceEvent::from_raw("fallback", 0, val);
        assert_eq!(ev.tool_uses, vec!["Read"]);
        assert!(ev.summary.contains("Read"), "got: {}", ev.summary);
    }

    #[test]
    fn test_session_id_from_entry() {
        let val = json!({ "type": "user", "sessionId": "real-session", "content": "hi" });
        let ev = TraceEvent::from_raw("fallback", 0, val);
        assert_eq!(ev.session_id, "real-session");
    }

    #[test]
    fn test_session_id_fallback() {
        let val = json!({ "type": "user", "content": "hi" });
        let ev = TraceEvent::from_raw("fallback", 0, val);
        assert_eq!(ev.session_id, "fallback");
    }

    #[test]
    fn test_cost_estimation_sonnet() {
        let val = json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "usage": { "input_tokens": 1_000_000, "output_tokens": 0 }
            }
        });
        let ev = TraceEvent::from_raw("s", 0, val);
        assert!(ev.cost_estimated);
        assert!((ev.cost_usd - 3.0).abs() < 0.001, "got {}", ev.cost_usd);
    }

    #[test]
    fn test_cost_estimation_opus() {
        let val = json!({
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-7",
                "usage": { "output_tokens": 1_000_000 }
            }
        });
        let ev = TraceEvent::from_raw("s", 0, val);
        // Opus 4.5+ is $25/Mtok output (Opus 4/4.1 were $75).
        assert!((ev.cost_usd - 25.0).abs() < 0.001, "got {}", ev.cost_usd);
    }

    #[test]
    fn test_cost_explicit_overrides_estimate() {
        let val = json!({
            "type": "assistant",
            "costUSD": 0.5,
            "message": { "model": "claude-sonnet-4-6", "usage": { "input_tokens": 10 } }
        });
        let ev = TraceEvent::from_raw("s", 0, val);
        assert!(!ev.cost_estimated);
        assert_eq!(ev.cost_usd, 0.5);
    }

    #[test]
    fn test_summarise_tool_use_top_level() {
        let val = json!({ "type": "tool_use", "name": "read_file" });
        let s = summarise(&val, &[]);
        assert_eq!(s, "🔧 read_file");
    }

    #[test]
    fn test_summarise_summary_entry() {
        let val = json!({ "type": "summary", "summary": "Build dashboard" });
        let s = summarise(&val, &[]);
        assert!(s.contains("Build dashboard"));
    }

    #[test]
    fn test_user_tool_results() {
        let val = json!({
            "type": "user",
            "message": {
                "content": [{ "type": "tool_result", "tool_use_id": "abc", "content": "..." }]
            }
        });
        let ev = TraceEvent::from_raw("s", 0, val);
        assert_eq!(ev.tool_results, vec!["abc"]);
        assert!(ev.summary.contains("Tool result"), "got: {}", ev.summary);
    }

    #[test]
    fn test_truncate_long_text() {
        let long = "a".repeat(500);
        let val = json!({ "type": "user", "content": long });
        let s = summarise(&val, &[]);
        assert!(
            s.chars().count() < 250,
            "summary too long: {}",
            s.chars().count()
        );
    }

    #[test]
    fn test_extract_cache_tokens() {
        let val = json!({
            "type": "assistant",
            "message": {
                "model": "claude-opus-4-7",
                "usage": {
                    "input_tokens": 6,
                    "output_tokens": 161,
                    "cache_creation_input_tokens": 25667,
                    "cache_read_input_tokens": 0
                }
            }
        });
        let ev = TraceEvent::from_raw("s", 0, val);
        let u = ev.usage.expect("usage");
        assert_eq!(u.input, 6);
        assert_eq!(u.output, 161);
        assert_eq!(u.cache_creation, 25667);
        assert_eq!(u.cache_read, 0);
    }

    #[test]
    fn test_truncate_multibyte_safe() {
        // Build a string that is longer than max_len in chars and contains multibyte chars.
        let s: String = "é".repeat(100);
        let v = json!({ "type": "user", "content": s });
        let out = summarise(&v, &[]);
        assert!(out.starts_with("👤"));
        // Must not panic; output is a valid String.
    }
}
