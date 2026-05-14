use serde::{Deserialize, Serialize};

/// Canonical transport object for a single Claude Code JSONL line.
///
/// We enrich the raw record with derived fields so the dashboard can render
/// useful information without having to walk the (sometimes very large) raw
/// JSON tree on every render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Session identifier — taken from the entry's `sessionId` field if
    /// present, otherwise the JSONL file stem.
    pub session_id: String,
    /// Zero-based line position within the source file.
    pub line_index: usize,
    /// Raw parsed Claude Code event record.
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
    /// Cost in USD — either pulled from `costUSD` on the entry or estimated
    /// from the model and token usage.
    pub cost_usd: f64,
    /// Whether the cost was estimated client-side (vs. provided by Claude Code).
    pub cost_estimated: bool,
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
    /// server-side metadata.
    pub fn from_raw(session_id_fallback: &str, line_index: usize, raw: serde_json::Value) -> Self {
        let event_type = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();

        let session_id = raw
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| session_id_fallback.to_owned());

        let timestamp = raw
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let cwd = raw
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let git_branch = raw
            .get("gitBranch")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let version = raw
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let model = raw
            .pointer("/message/model")
            .and_then(|v| v.as_str())
            .or_else(|| raw.get("model").and_then(|v| v.as_str()))
            .map(str::to_owned);

        let (tool_uses, tool_results) = extract_content_kinds(&raw);
        let usage = extract_usage(&raw);

        let (cost_usd, cost_estimated) = if let Some(c) = raw.get("costUSD").and_then(|v| v.as_f64())
        {
            (c, false)
        } else if let Some(u) = &usage {
            (estimate_cost(model.as_deref(), u), true)
        } else {
            (0.0, true)
        };

        Self {
            session_id,
            line_index,
            observed_at: chrono::Utc::now().to_rfc3339(),
            summary: summarise(&raw, &tool_uses),
            event_type,
            timestamp,
            cwd,
            git_branch,
            version,
            model,
            tool_uses,
            tool_results,
            usage,
            cost_usd,
            cost_estimated,
            entry: raw,
        }
    }
}

/// Walk an entry's content blocks (top-level `content`, or `message.content`)
/// and pull out the names of tool_use blocks and IDs of tool_result blocks.
fn extract_content_kinds(val: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();

    let candidates = [
        val.get("content"),
        val.pointer("/message/content"),
    ];

    for content in candidates.into_iter().flatten() {
        if let Some(arr) = content.as_array() {
            for block in arr {
                match block.get("type").and_then(|v| v.as_str()) {
                    Some("tool_use") => {
                        if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                            tool_uses.push(name.to_owned());
                        }
                    }
                    Some("tool_result") => {
                        if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                            tool_results.push(id.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    (tool_uses, tool_results)
}

/// Extract token usage from common locations in a Claude Code JSONL entry.
fn extract_usage(val: &serde_json::Value) -> Option<TokenUsage> {
    let usage = val
        .pointer("/message/usage")
        .or_else(|| val.get("usage"))?;

    let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
        return None;
    }

    Some(TokenUsage {
        input,
        output,
        cache_read,
        cache_creation,
    })
}

/// Approximate USD pricing per million tokens for known Claude model families.
/// Numbers are rough public list prices — adequate for surfacing cost trends
/// in the dashboard but not authoritative billing data.
struct Pricing {
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_read_per_mtok: f64,
    cache_creation_per_mtok: f64,
}

fn pricing_for(model: Option<&str>) -> Pricing {
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
    } else if m.contains("sonnet") || m.is_empty() {
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        }
    } else {
        // Unknown model — fall back to Sonnet pricing.
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache_read_per_mtok: 0.30,
            cache_creation_per_mtok: 3.75,
        }
    }
}

pub fn estimate_cost(model: Option<&str>, u: &TokenUsage) -> f64 {
    let p = pricing_for(model);
    let mtok = 1_000_000.0;
    (u.input as f64) / mtok * p.input_per_mtok
        + (u.output as f64) / mtok * p.output_per_mtok
        + (u.cache_read as f64) / mtok * p.cache_read_per_mtok
        + (u.cache_creation as f64) / mtok * p.cache_creation_per_mtok
}

/// Produce a short human-readable summary for a raw Claude Code JSONL record.
pub fn summarise(val: &serde_json::Value, tool_uses: &[String]) -> String {
    let event_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match event_type {
        "user" => {
            let preview = extract_text_preview(val, 120);
            if preview.is_empty() {
                // user messages with only tool_result blocks have no text preview
                let n_tr = count_blocks_of_kind(val, "tool_result");
                if n_tr > 0 {
                    format!("📦 Tool result ×{n_tr}")
                } else {
                    "👤 User".to_owned()
                }
            } else {
                format!("👤 {preview}")
            }
        }
        "assistant" => {
            let preview = extract_text_preview(val, 100);
            if !tool_uses.is_empty() {
                let tools = tool_uses.join(", ");
                if preview.is_empty() {
                    format!("🔧 {tools}")
                } else {
                    format!("🤖 {preview} · 🔧 {tools}")
                }
            } else if !preview.is_empty() {
                format!("🤖 {preview}")
            } else {
                let n_thinking = count_blocks_of_kind(val, "thinking");
                if n_thinking > 0 {
                    format!("💭 Thinking ({n_thinking} block{})", if n_thinking > 1 { "s" } else { "" })
                } else {
                    "🤖 Assistant".to_owned()
                }
            }
        }
        "tool_use" => {
            let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("🔧 {name}")
        }
        "tool_result" => {
            let id = val
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("📦 Tool result: {id}")
        }
        "system" => {
            let preview = extract_text_preview(val, 100);
            format!("⚙️  System: {preview}")
        }
        "summary" => {
            let preview = val
                .get("summary")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 100))
                .unwrap_or_default();
            format!("📝 Summary: {preview}")
        }
        "attachment" => "📎 Attachment".to_owned(),
        "ai-title" => {
            let t = val
                .get("aiTitle")
                .and_then(|v| v.as_str())
                .map(|s| truncate(s, 100))
                .unwrap_or_default();
            format!("🏷  {t}")
        }
        "queue-operation" => {
            let op = val.get("operation").and_then(|v| v.as_str()).unwrap_or("?");
            let preview = extract_text_preview(val, 80);
            format!("⏳ Queue {op}: {preview}")
        }
        "last-prompt" => "📍 Last prompt marker".to_owned(),
        other => format!("❓ {other}"),
    }
}

/// Count how many content blocks of a given `type` an entry contains.
fn count_blocks_of_kind(val: &serde_json::Value, kind: &str) -> usize {
    let arrs = [val.pointer("/message/content"), val.get("content")];
    let mut n = 0;
    for arr in arrs.into_iter().flatten() {
        if let Some(a) = arr.as_array() {
            for b in a {
                if b.get("type").and_then(|v| v.as_str()) == Some(kind) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Extract a printable text preview from a JSON value.
fn extract_text_preview(val: &serde_json::Value, max_len: usize) -> String {
    if let Some(text) = val.get("text").and_then(|v| v.as_str()) {
        return truncate(text, max_len);
    }
    if let Some(s) = val.get("content").and_then(|v| v.as_str()) {
        return truncate(s, max_len);
    }
    if let Some(arr) = val.get("content").and_then(|v| v.as_array()) {
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    return truncate(text, max_len);
                }
            }
        }
    }
    if let Some(content) = val.pointer("/message/content") {
        if let Some(s) = content.as_str() {
            return truncate(s, max_len);
        }
        if let Some(arr) = content.as_array() {
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        return truncate(text, max_len);
                    }
                }
            }
        }
    }
    String::new()
}

fn truncate(s: &str, max_len: usize) -> String {
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
        assert!((ev.cost_usd - 75.0).abs() < 0.001, "got {}", ev.cost_usd);
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
        assert!(s.chars().count() < 250, "summary too long: {}", s.chars().count());
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
