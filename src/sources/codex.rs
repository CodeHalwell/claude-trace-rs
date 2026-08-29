//! Codex CLI adapter.
//!
//! Codex CLI (OpenAI's terminal agent) writes session "rollout" JSONL files
//! to `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`.
//! Each line is a timestamped record with a `type`:
//!
//! - `turn_context` — session-level context (`payload.cwd`, `payload.model`).
//! - `response_item` — one Responses-API item in `payload`: a `message`
//!   (role user/assistant with `content` blocks of `input_text` /
//!   `output_text`), a `function_call` (`name`, `arguments`, `call_id`),
//!   or a `function_call_output` (`call_id`, `output`).
//! - `event_msg` — UI-facing events; `token_count` payloads carry usage.
//!
//! The adapter maps these onto the normalised TraceEvent model: messages
//! become `user`/`assistant` events, `function_call` becomes `tool_use`,
//! and `function_call_output` becomes `tool_result`.

use serde_json::Value;

use crate::event::TokenUsage;
use crate::sources::{estimate_cost, truncate, Enrichment};

/// Normalise one Codex rollout record.
pub fn enrich(raw: &Value) -> Enrichment {
    let record_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let payload = raw.get("payload").cloned().unwrap_or(Value::Null);

    let timestamp = raw
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let mut e = Enrichment {
        timestamp,
        session_id: raw
            .get("sessionId")
            .or_else(|| raw.get("session_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        ..Default::default()
    };

    match record_type {
        "turn_context" => {
            e.event_type = "system".to_owned();
            e.cwd = payload
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            e.git_branch = payload
                .get("git_branch")
                .or_else(|| payload.pointer("/git/branch"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            e.model = payload
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            e.version = payload
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let cwd = e.cwd.as_deref().unwrap_or("");
            e.summary = format!("⚙️  Turn context: {}", truncate(cwd, 80));
        }
        "response_item" => enrich_response_item(&payload, &mut e),
        "event_msg" => enrich_event_msg(&payload, &mut e),
        _ => {
            e.event_type = if record_type.is_empty() {
                "unknown".to_owned()
            } else {
                record_type.to_owned()
            };
            e.summary = format!("❓ {}", e.event_type);
        }
    }

    // Estimated cost when we have usage but no explicit figure.
    if e.cost_usd.is_none() {
        if let Some(u) = &e.usage {
            e.cost_usd = Some(estimate_cost(e.model.as_deref(), u));
        }
    }
    e
}

fn enrich_response_item(payload: &Value, e: &mut Enrichment) {
    let item_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match item_type {
        "message" => {
            let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
            e.event_type = if role == "user" { "user" } else { "assistant" }.to_owned();
            let text = message_text(payload);
            if e.event_type == "user" {
                e.summary = if text.is_empty() {
                    "👤 User".to_owned()
                } else {
                    format!("👤 {}", truncate(&text, 120))
                };
            } else {
                e.summary = if text.is_empty() {
                    "🤖 Assistant".to_owned()
                } else {
                    format!("🤖 {}", truncate(&text, 100))
                };
            }
        }
        "function_call" => {
            e.event_type = "tool_use".to_owned();
            let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            e.tool_uses.push(name.to_owned());
            e.summary = format!("🔧 {name}");
        }
        "function_call_output" | "custom_tool_call_output" => {
            e.event_type = "tool_result".to_owned();
            let id = payload
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            e.tool_results.push(id.to_owned());
            e.summary = format!("📦 Tool result: {id}");
        }
        "reasoning" => {
            e.event_type = "assistant".to_owned();
            e.summary = "💭 Reasoning".to_owned();
        }
        other => {
            e.event_type = "system".to_owned();
            e.summary = format!("⚙️  Item: {other}");
        }
    }
}

fn enrich_event_msg(payload: &Value, e: &mut Enrichment) {
    let msg_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match msg_type {
        "user_message" => {
            e.event_type = "user".to_owned();
            let text = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            e.summary = format!("👤 {}", truncate(text, 120));
        }
        "agent_message" | "agent_reasoning" => {
            e.event_type = "assistant".to_owned();
            let text = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            e.summary = format!("🤖 {}", truncate(text, 100));
        }
        "token_count" => {
            e.event_type = "system".to_owned();
            let info = payload.get("info").unwrap_or(&Value::Null);
            let usage = pick_usage(info);
            if let Some(u) = &usage {
                e.summary = format!("⚙️  Tokens: {} in · {} out", u.input, u.output);
            } else {
                e.summary = "⚙️  Token count".to_owned();
            }
            e.usage = usage;
            e.model = info
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
        }
        "task_started" | "task_complete" | "turn_started" | "turn_complete" => {
            e.event_type = "system".to_owned();
            e.summary = format!("⚙️  {}", msg_type.replace('_', " "));
        }
        other => {
            e.event_type = "system".to_owned();
            e.summary = format!("⚙️  {other}");
        }
    }
}

/// Concatenate the text of a Responses-API message's content blocks.
fn message_text(payload: &Value) -> String {
    let mut out = String::new();
    if let Some(arr) = payload.get("content").and_then(|c| c.as_array()) {
        for block in arr {
            // blocks: {"type":"input_text","text":…} / {"type":"output_text","text":…}
            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(t);
            }
        }
    }
    out
}

/// Pull token usage out of a `token_count` info payload, trying the common
/// shapes (`total_token_usage`, `last_token_usage`, flat fields).
fn pick_usage(info: &Value) -> Option<TokenUsage> {
    // `last_token_usage` first: SessionStats aggregates usage with `+=`, so it
    // needs the per-event delta. `total_token_usage` is a running cumulative
    // total for the session — summing those snapshots inflates tokens (and the
    // cost derived from them) more and more as a session goes on. The total is
    // kept only as a fallback for records that carry nothing else.
    let candidates = [
        info.get("last_token_usage"),
        info.get("total_token_usage"),
        Some(info),
    ];
    for c in candidates.into_iter().flatten() {
        let input = c
            .get("input_tokens")
            .or_else(|| c.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = c
            .get("output_tokens")
            .or_else(|| c.get("completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = c
            .get("cached_input_tokens")
            .or_else(|| c.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if input > 0 || output > 0 || cache_read > 0 {
            return Some(TokenUsage {
                input,
                output,
                cache_read,
                cache_creation: 0,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turn_context_captures_cwd_and_model() {
        let e = enrich(&json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "turn_context",
            "payload": {"cwd": "/home/me/proj", "model": "gpt-5-codex"}
        }));
        assert_eq!(e.event_type, "system");
        assert_eq!(e.cwd.as_deref(), Some("/home/me/proj"));
        assert_eq!(e.model.as_deref(), Some("gpt-5-codex"));
    }

    #[test]
    fn user_message_maps_to_user() {
        let e = enrich(&json!({
            "type": "response_item",
            "payload": {"type":"message","role":"user","content":[{"type":"input_text","text":"fix the bug"}]}
        }));
        assert_eq!(e.event_type, "user");
        assert!(e.summary.contains("fix the bug"));
    }

    #[test]
    fn function_call_maps_to_tool_use() {
        let e = enrich(&json!({
            "type": "response_item",
            "payload": {"type":"function_call","name":"shell","arguments":"{}","call_id":"c1"}
        }));
        assert_eq!(e.event_type, "tool_use");
        assert_eq!(e.tool_uses, vec!["shell"]);
    }

    #[test]
    fn function_call_output_maps_to_tool_result() {
        let e = enrich(&json!({
            "type": "response_item",
            "payload": {"type":"function_call_output","call_id":"c1","output":"done"}
        }));
        assert_eq!(e.event_type, "tool_result");
        assert_eq!(e.tool_results, vec!["c1"]);
    }

    #[test]
    fn token_count_extracts_usage_and_estimates_cost() {
        let e = enrich(&json!({
            "type": "event_msg",
            "payload": {"type":"token_count","info":{"model":"gpt-5-codex","total_token_usage":{"input_tokens":1_000_000,"output_tokens":0}}}
        }));
        let u = e.usage.expect("usage");
        assert_eq!(u.input, 1_000_000);
        assert!(e.cost_usd.unwrap() > 0.0);
    }
    #[test]
    fn token_count_prefers_per_event_delta() {
        let e = enrich(&json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 5000, "output_tokens": 900},
                "last_token_usage":  {"input_tokens": 120,  "output_tokens": 30}
            }}
        }));
        let u = e.usage.expect("usage");
        assert_eq!(u.input, 120);
        assert_eq!(u.output, 30);
    }

    #[test]
    fn token_count_falls_back_to_total_when_no_delta() {
        let e = enrich(&json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 42, "output_tokens": 7}
            }}
        }));
        let u = e.usage.expect("usage");
        assert_eq!(u.input, 42);
        assert_eq!(u.output, 7);
    }
}
