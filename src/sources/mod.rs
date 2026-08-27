//! Multi-agent source detection and per-agent event enrichment.
//!
//! `claude-trace-rs` is a harmonised tracer: it watches the session logs of
//! several terminal coding agents (Claude Code, Codex CLI, GitHub Copilot CLI,
//! Kimi Code, Cline, Cursor Agent) and normalises their different on-disk
//! shapes into one [`crate::event::TraceEvent`] model.
//!
//! Detection is two-stage:
//! 1. **Path-based** — a file living under a known agent root
//!    (`~/.claude/projects`, `~/.codex/sessions`, …) is assigned that agent.
//! 2. **Content-sniffing** — when the path is ambiguous (e.g. a custom
//!    `--watch-root`), the first parsed line is inspected for signature fields
//!    (`sessionId`+`gitBranch` ⇒ Claude Code, `turn_context`/`response_item`
//!    ⇒ Codex, …).
//!
//! Each agent has an adapter module exposing `enrich(&RawParts) -> Enrichment`
//! which fills in the normalised fields the dashboard/DB/exporters rely on.
//! Unknown formats fall back to the generic Claude-Code-shaped heuristics,
//! which tolerate missing fields gracefully.

pub mod claude;
pub mod cline;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod kimi;

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The coding agent that produced a trace file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentSource {
    ClaudeCode,
    Codex,
    Copilot,
    Kimi,
    Cline,
    Cursor,
    /// Anything we couldn't attribute — enriched with the generic
    /// Claude-Code-shaped fallback heuristics.
    Unknown,
}

impl AgentSource {
    /// Stable kebab-case identifier stored in the DB and emitted in the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentSource::ClaudeCode => "claude-code",
            AgentSource::Codex => "codex",
            AgentSource::Copilot => "copilot",
            AgentSource::Kimi => "kimi",
            AgentSource::Cline => "cline",
            AgentSource::Cursor => "cursor",
            AgentSource::Unknown => "unknown",
        }
    }

    /// Parse a kebab-case identifier (case-insensitive). Accepts a few common
    /// aliases (`claude`, `codex-cli`, …) for CLI friendliness.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claude_code" => Some(AgentSource::ClaudeCode),
            "codex" | "codex-cli" | "openai" => Some(AgentSource::Codex),
            "copilot" | "copilot-cli" | "github-copilot" => Some(AgentSource::Copilot),
            "kimi" | "kimi-code" | "moonshot" => Some(AgentSource::Kimi),
            "cline" => Some(AgentSource::Cline),
            "cursor" | "cursor-agent" => Some(AgentSource::Cursor),
            "unknown" => Some(AgentSource::Unknown),
            _ => None,
        }
    }

    /// Human-readable display name for the dashboard.
    #[allow(dead_code)] // used by the dashboard JS via the sources endpoint
    pub fn display_name(&self) -> &'static str {
        match self {
            AgentSource::ClaudeCode => "Claude Code",
            AgentSource::Codex => "Codex",
            AgentSource::Copilot => "Copilot",
            AgentSource::Kimi => "Kimi",
            AgentSource::Cline => "Cline",
            AgentSource::Cursor => "Cursor",
            AgentSource::Unknown => "Unknown",
        }
    }

    /// All known (non-Unknown) sources — used for default root discovery.
    pub fn all_known() -> &'static [AgentSource] {
        &[
            AgentSource::ClaudeCode,
            AgentSource::Codex,
            AgentSource::Copilot,
            AgentSource::Kimi,
            AgentSource::Cline,
            AgentSource::Cursor,
        ]
    }

    /// Default session-log directories for this agent, relative to the user's
    /// home directory (or, for Cline, the platform config dir which we resolve
    /// specially in [`default_roots`]).
    fn default_dirs(&self, home: &Path) -> Vec<std::path::PathBuf> {
        match self {
            AgentSource::ClaudeCode => vec![home.join(".claude/projects")],
            AgentSource::Codex => vec![home.join(".codex/sessions")],
            AgentSource::Copilot => vec![home.join(".copilot")],
            AgentSource::Kimi => vec![home.join(".kimi")],
            AgentSource::Cline => cline::default_task_dirs(),
            AgentSource::Cursor => vec![
                home.join(".cursor/projects"),
                home.join(".cursor-agent"),
            ],
            AgentSource::Unknown => vec![],
        }
    }
}

/// One watch root paired with the agent source it is authoritative for.
#[derive(Debug, Clone)]
pub struct WatchRoot {
    pub path: std::path::PathBuf,
    /// Forced source for everything under this root. `None` means
    /// "detect from path/content per file".
    pub source: Option<AgentSource>,
}

/// Compute the default set of watch roots: every known agent directory that
/// currently exists on disk. Roots are tagged with their agent so detection
/// is exact rather than sniffed.
pub fn default_roots() -> Vec<WatchRoot> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_owned());
    let home = Path::new(&home);
    let mut out = Vec::new();
    for src in AgentSource::all_known() {
        for dir in src.default_dirs(home) {
            if dir.is_dir() {
                out.push(WatchRoot {
                    path: dir,
                    source: Some(*src),
                });
            }
        }
    }
    out
}

/// Decide whether a file at `path` (already known to live under `root`) is a
/// trace file we should ingest, given the root's forced source (if any).
///
/// This is the per-adapter file matcher: most agents write `.jsonl`, but
/// Cline writes whole-file JSON arrays (`api_conversation_history.json`,
/// `ui_messages.json`), which this matcher admits for Cline roots.
pub fn matches_file(root_source: Option<AgentSource>, path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    match root_source {
        Some(AgentSource::Cline) => cline::matches_file(path),
        _ => ext == Some("jsonl"),
    }
}

/// Detect the agent source for a file. Path-based detection runs first using
/// the forced source of the watch root (if any) and then well-known path
/// fragments; `sniff` is an optional first-line JSON value used for
/// content-based detection when the path is ambiguous.
pub fn detect(
    root_source: Option<AgentSource>,
    path: &Path,
    sniff: Option<&serde_json::Value>,
) -> AgentSource {
    if let Some(s) = root_source {
        return s;
    }

    // Path-fragment heuristics (handles custom --watch-root pointing at a
    // known agent directory layout).
    let p = path.to_string_lossy().to_ascii_lowercase();
    if p.contains("/.claude/projects/") || p.contains("\\.claude\\projects\\") {
        return AgentSource::ClaudeCode;
    }
    if p.contains("/.codex/") || p.contains("\\.codex\\") {
        return AgentSource::Codex;
    }
    if p.contains("/.copilot/") || p.contains("\\.copilot\\") {
        return AgentSource::Copilot;
    }
    if p.contains("/.kimi/") || p.contains("\\.kimi\\") {
        return AgentSource::Kimi;
    }
    if p.contains("saoudrizwan.claude-dev") || p.contains("/cline/") {
        return AgentSource::Cline;
    }
    if p.contains("/.cursor") || p.contains("\\.cursor") {
        return AgentSource::Cursor;
    }

    // Content sniffing.
    if let Some(v) = sniff {
        return sniff_source(v);
    }
    AgentSource::Unknown
}

/// Identify an agent from the shape of a single parsed record.
pub fn sniff_source(v: &serde_json::Value) -> AgentSource {
    // Codex rollout records: {"timestamp":…,"type":"turn_context",…} or
    // {"type":"response_item","payload":{…}}.
    if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
        if matches!(t, "turn_context" | "response_item" | "event_msg") {
            return AgentSource::Codex;
        }
    }
    // Cline task entries: {"ts":…,"type":"say","say":"text",…} or ask/api_req.
    if v.get("ts").is_some()
        && (v.get("say").is_some() || v.get("ask").is_some())
    {
        return AgentSource::Cline;
    }
    // Claude Code: has sessionId plus a Claude-ish type, or gitBranch/cwd.
    if v.get("sessionId").is_some() {
        return AgentSource::ClaudeCode;
    }
    AgentSource::Unknown
}

/// The normalised fields every adapter produces from one raw record. The
/// event constructor merges these with the transport-level fields
/// (session fallback, line index, observed-at).
#[derive(Debug, Default)]
pub struct Enrichment {
    pub event_type: String,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub model: Option<String>,
    pub tool_uses: Vec<String>,
    pub tool_results: Vec<String>,
    pub usage: Option<crate::event::TokenUsage>,
    /// Explicit cost reported by the agent (USD), if any.
    pub cost_usd: Option<f64>,
    pub summary: String,
}

/// Dispatch to the adapter for `source`.
pub fn enrich(source: AgentSource, raw: &serde_json::Value) -> Enrichment {
    match source {
        AgentSource::ClaudeCode | AgentSource::Unknown => claude::enrich(raw),
        AgentSource::Codex => codex::enrich(raw),
        AgentSource::Copilot => copilot::enrich(raw),
        AgentSource::Kimi => kimi::enrich(raw),
        AgentSource::Cline => cline::enrich(raw),
        AgentSource::Cursor => cursor::enrich(raw),
    }
}

/// Approximate USD pricing per million tokens. Each adapter calls this with a
/// model name; the table maps known model families (Claude, GPT, Kimi, …) to
/// rough public list prices. Adequate for surfacing cost trends in the
/// dashboard — not authoritative billing data.
#[derive(Debug, Clone, Copy)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_creation_per_mtok: f64,
}

pub fn pricing_for(model: Option<&str>) -> Pricing {
    let m = model.unwrap_or("").to_ascii_lowercase();
    // Order matters: most specific first.
    if m.contains("opus") {
        Pricing {
            input_per_mtok: 15.0,
            output_per_mtok: 75.0,
            cache_read_per_mtok: 1.50,
            cache_creation_per_mtok: 18.75,
        }
    } else if m.contains("haiku") {
        Pricing {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.10,
            cache_creation_per_mtok: 1.25,
        }
    } else if m.contains("kimi") || m.contains("moonshot") || m.contains("k2") {
        // Kimi k2-class models (Moonshot AI).
        Pricing {
            input_per_mtok: 0.60,
            output_per_mtok: 2.50,
            cache_read_per_mtok: 0.15,
            cache_creation_per_mtok: 0.60,
        }
    } else if m.contains("gpt-5") || m.contains("gpt5") || m.contains("o3") || m.contains("o4") {
        Pricing {
            input_per_mtok: 1.25,
            output_per_mtok: 10.0,
            cache_read_per_mtok: 0.125,
            cache_creation_per_mtok: 1.25,
        }
    } else if m.contains("gpt-4") || m.contains("gpt4") {
        Pricing {
            input_per_mtok: 2.50,
            output_per_mtok: 10.0,
            cache_read_per_mtok: 1.25,
            cache_creation_per_mtok: 2.50,
        }
    } else {
        // Sonnet is also the default for empty/unknown model names.
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        }
    }
}

/// Compute an estimated USD cost for a token usage breakdown.
pub fn estimate_cost(model: Option<&str>, u: &crate::event::TokenUsage) -> f64 {
    let p = pricing_for(model);
    let mtok = 1_000_000.0;
    (u.input as f64) / mtok * p.input_per_mtok
        + (u.output as f64) / mtok * p.output_per_mtok
        + (u.cache_read as f64) / mtok * p.cache_read_per_mtok
        + (u.cache_creation as f64) / mtok * p.cache_creation_per_mtok
}

// ---------------------------------------------------------------------------
// Shared helpers used by several adapters
// ---------------------------------------------------------------------------

/// Recursively gather human-readable text from an entry. Indexes every string
/// value (so tool inputs like `path`/`command`/`pattern` are searchable)
/// except for a blacklist of noisy metadata keys.
pub fn collect_text(val: &serde_json::Value, out: &mut String) {
    match val {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if matches!(
                    k.as_str(),
                    "uuid"
                        | "parentUuid"
                        | "id"
                        | "tool_use_id"
                        | "sessionId"
                        | "session_id"
                        | "signature"
                        | "timestamp"
                        | "call_id"
                ) {
                    continue;
                }
                if let Some(s) = v.as_str() {
                    out.push(' ');
                    out.push_str(s);
                } else {
                    collect_text(v, out);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_text(v, out);
            }
        }
        _ => {}
    }
}

/// Trim a string to `max_len` chars, collapsing newlines, char-boundary safe.
pub fn truncate(s: &str, max_len: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.chars().count() <= max_len {
        s
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_ids_roundtrip() {
        for s in AgentSource::all_known() {
            assert_eq!(AgentSource::parse(s.as_str()), Some(*s));
        }
        assert_eq!(AgentSource::parse("claude"), Some(AgentSource::ClaudeCode));
        assert_eq!(AgentSource::parse("CODEX"), Some(AgentSource::Codex));
        assert_eq!(AgentSource::parse("nope"), None);
    }

    #[test]
    fn sniff_detects_codex() {
        let v = json!({"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{}});
        assert_eq!(sniff_source(&v), AgentSource::Codex);
    }

    #[test]
    fn sniff_detects_claude() {
        let v = json!({"type":"user","sessionId":"abc","content":"hi"});
        assert_eq!(sniff_source(&v), AgentSource::ClaudeCode);
    }

    #[test]
    fn sniff_detects_cline() {
        let v = json!({"ts":123,"type":"say","say":"text","text":"hello"});
        assert_eq!(sniff_source(&v), AgentSource::Cline);
    }

    #[test]
    fn path_detection_prefers_forced_root() {
        let p = Path::new("/tmp/whatever/rollout-1.jsonl");
        assert_eq!(
            detect(Some(AgentSource::Codex), p, None),
            AgentSource::Codex
        );
        // Path heuristics without a forced source.
        let c = Path::new("/home/me/.claude/projects/proj/s.jsonl");
        assert_eq!(detect(None, c, None), AgentSource::ClaudeCode);
        let x = Path::new("/home/me/.codex/sessions/2026/01/01/rollout-x.jsonl");
        assert_eq!(detect(None, x, None), AgentSource::Codex);
    }

    #[test]
    fn matches_file_by_source() {
        let jsonl = Path::new("s.jsonl");
        let json = Path::new("api_conversation_history.json");
        assert!(matches_file(Some(AgentSource::ClaudeCode), jsonl));
        assert!(!matches_file(Some(AgentSource::ClaudeCode), json));
        assert!(matches_file(Some(AgentSource::Cline), json));
        assert!(!matches_file(Some(AgentSource::Cline), jsonl));
        assert!(matches_file(None, jsonl));
    }
}
