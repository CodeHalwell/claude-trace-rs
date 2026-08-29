//! Claude Code adapter — the reference implementation.
//!
//! Claude Code writes JSONL session logs to `~/.claude/projects/<project>/
//! <sessionId>.jsonl`. Each line is a record with a top-level `type`
//! (`user`, `assistant`, `system`, `summary`, …), `sessionId`, `timestamp`,
//! `cwd`, `gitBranch`, `version`, and for assistant turns a `message` object
//! holding `content` blocks (`text` / `thinking` / `tool_use` /
//! `tool_result`) and `usage` token counters.
//!
//! These heuristics are deliberately tolerant: they tolerate missing fields
//! and double as the generic fallback for `AgentSource::Unknown`, so a trace
//! from an unrecognised agent still renders as best it can.

use serde_json::Value;

use crate::event::TokenUsage;
use crate::sources::{estimate_cost, truncate, Enrichment};

/// Normalise one Claude Code JSONL record.
pub fn enrich(raw: &Value) -> Enrichment {
    let event_type = raw
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();

    let session_id = raw
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let timestamp = raw
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let cwd = raw.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);

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

    let (tool_uses, tool_results) = extract_content_kinds(raw);
    let usage = extract_usage(raw);

    let cost_usd = raw.get("costUSD").and_then(|v| v.as_f64());

    let summary = summarise(raw, &tool_uses);

    // If neither the record nor the pricing table gives us a cost, compute
    // the estimate now so downstream doesn't have to.
    let cost_usd = match (cost_usd, &usage) {
        (Some(c), _) => Some(c),
        (None, Some(u)) => Some(estimate_cost(model.as_deref(), u)),
        (None, None) => None,
    };

    Enrichment {
        event_type,
        session_id,
        timestamp,
        cwd,
        git_branch,
        version,
        model,
        tool_uses,
        tool_results,
        usage,
        cost_usd,
        summary,
    }
}

/// Walk an entry's content blocks (top-level `content`, or `message.content`)
/// and pull out the names of tool_use blocks and IDs of tool_result blocks.
fn extract_content_kinds(val: &Value) -> (Vec<String>, Vec<String>) {
    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();

    let candidates = [val.get("content"), val.pointer("/message/content")];

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
fn extract_usage(val: &Value) -> Option<TokenUsage> {
    let usage = val.pointer("/message/usage").or_else(|| val.get("usage"))?;

    let input = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
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

/// Produce a short human-readable summary for a raw Claude Code JSONL record.
pub fn summarise(val: &Value, tool_uses: &[String]) -> String {
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
                    format!(
                        "💭 Thinking ({n_thinking} block{})",
                        if n_thinking > 1 { "s" } else { "" }
                    )
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
fn count_blocks_of_kind(val: &Value, kind: &str) -> usize {
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
fn extract_text_preview(val: &Value, max_len: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enrich_basic_user() {
        let e = enrich(&json!({"type":"user","sessionId":"s1","content":"hello"}));
        assert_eq!(e.event_type, "user");
        assert_eq!(e.session_id.as_deref(), Some("s1"));
        assert!(e.summary.starts_with("👤"));
    }

    #[test]
    fn enrich_assistant_tools_and_usage() {
        let e = enrich(&json!({
            "type": "assistant",
            "message": {
                "model": "claude-sonnet-4-6",
                "content": [
                    {"type":"text","text":"here"},
                    {"type":"tool_use","name":"Read","id":"t1"}
                ],
                "usage": {"input_tokens":100,"output_tokens":50}
            }
        }));
        assert_eq!(e.tool_uses, vec!["Read"]);
        assert_eq!(e.model.as_deref(), Some("claude-sonnet-4-6"));
        let u = e.usage.unwrap();
        assert_eq!(u.input, 100);
        // cost estimated via pricing table
        assert!(e.cost_usd.unwrap() > 0.0);
    }

    #[test]
    fn explicit_cost_wins() {
        let e = enrich(
            &json!({"type":"assistant","costUSD":0.5,"message":{"usage":{"input_tokens":10}}}),
        );
        assert_eq!(e.cost_usd, Some(0.5));
    }
}
