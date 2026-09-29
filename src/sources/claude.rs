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

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::event::TokenUsage;
use crate::message::{Message, Role};
use crate::sources::{env_path, estimate_cost, truncate, Enrichment};

/// `$CLAUDE_CONFIG_DIR` (comma-separated, each with `projects/`), else
/// `~/.claude/projects` and the XDG location `~/.config/claude/projects`.
pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("CLAUDE_CONFIG_DIR") {
        let dirs: Vec<PathBuf> = v
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| PathBuf::from(s).join("projects"))
            .collect();
        if !dirs.is_empty() {
            return dirs;
        }
    }
    let _ = env_path;
    vec![
        home.join(".claude/projects"),
        home.join(".config/claude/projects"),
    ]
}

/// Normalise one Claude Code JSONL record.
pub fn enrich(raw: &Value) -> Enrichment {
    let event_type = raw
        .get("type")
        .and_then(|v| v.as_str())
        .or_else(|| {
            // Role-only records (generic fallback for unknown agents).
            raw.get("role").and_then(|v| v.as_str()).map(|r| match r {
                "user" | "tool" => "user",
                "system" | "developer" => "system",
                _ => "assistant",
            })
        })
        .unwrap_or("unknown")
        .to_owned();

    let base_session = raw
        .get("sessionId")
        .or_else(|| raw.get("session_id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    // Sub-agent transcripts (`isSidechain`, `agentId`) reuse the parent's
    // sessionId; give them their own session so their records cannot collide
    // with the parent's on the (session, line) key.
    let session_id = match (
        base_session,
        raw.get("isSidechain").and_then(|v| v.as_bool()),
        raw.get("agentId").and_then(|v| v.as_str()),
    ) {
        (Some(sid), Some(true), Some(agent)) => Some(format!("{sid}:agent-{agent}")),
        (sid, _, _) => sid,
    };

    let timestamp = raw
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let cwd = raw.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);

    let git_branch = raw
        .get("gitBranch")
        .and_then(|v| v.as_str())
        .filter(|b| !b.is_empty())
        .map(str::to_owned);

    let version = raw
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let model = raw
        .pointer("/message/model")
        .and_then(|v| v.as_str())
        .or_else(|| raw.get("model").and_then(|v| v.as_str()))
        .filter(|m| *m != "<synthetic>")
        .map(str::to_owned);

    let (tool_uses, tool_results) = extract_content_kinds(raw);
    let usage = extract_usage(raw);

    let cost_usd = raw.get("costUSD").and_then(|v| v.as_f64());
    let cost_explicit = cost_usd.is_some();

    let summary = summarise(raw, &tool_uses);

    // If neither the record nor the pricing table gives us a cost, compute
    // the estimate now so downstream doesn't have to.
    let cost_usd = match (cost_usd, &usage) {
        (Some(c), _) => Some(c),
        (None, Some(u)) => Some(estimate_cost(model.as_deref(), u)),
        (None, None) => None,
    };

    let message = canonical_message(raw, &event_type);
    let title = match event_type.as_str() {
        "ai-title" => raw.get("aiTitle").and_then(|v| v.as_str()),
        "summary" => raw.get("summary").and_then(|v| v.as_str()),
        "custom-title" => raw.get("customTitle").and_then(|v| v.as_str()),
        _ => None,
    }
    .map(str::to_owned);
    let turn_end = event_type == "assistant"
        && raw.pointer("/message/stop_reason").and_then(|v| v.as_str()) == Some("end_turn")
        && raw.get("isSidechain").and_then(|v| v.as_bool()) != Some(true);
    // Claude Code writes one line per content block and repeats the
    // response's usage on each; sidechain replays copy it again. The API
    // message id identifies the response.
    let usage_key = usage.as_ref().and_then(|_| {
        raw.pointer("/message/id")
            .and_then(|v| v.as_str())
            .map(|id| format!("anthropic-msg:{id}"))
    });

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
        cost_explicit,
        summary,
        message,
        title,
        turn_end,
        usage_key,
    }
}

/// The record's dialogue content as a canonical message.
fn canonical_message(raw: &Value, event_type: &str) -> Option<Message> {
    let role_str = raw
        .pointer("/message/role")
        .and_then(|v| v.as_str())
        .unwrap_or(event_type);
    let content = raw
        .pointer("/message/content")
        .or_else(|| raw.get("content"));
    match (event_type, role_str) {
        ("user" | "assistant", _) | (_, "user" | "assistant" | "tool" | "developer") => {}
        _ => return None,
    }
    // Plain OpenAI-shaped records (generic fallback).
    if raw.get("type").is_none() && raw.get("role").is_some() {
        return Message::from_openai(raw);
    }
    let content = content?;
    let is_meta = raw.get("isMeta").and_then(|v| v.as_bool()) == Some(true);
    let role = match role_str {
        "assistant" => Role::Assistant,
        "system" | "developer" => Role::System,
        _ if is_meta => Role::System,
        _ => Role::User,
    };
    Message::from_anthropic(role, content)
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
