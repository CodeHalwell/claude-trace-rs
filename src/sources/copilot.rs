//! GitHub Copilot CLI adapter.
//!
//! Copilot CLI (`gh copilot` / `@github/copilot`) keeps session state under
//! `~/.copilot/` (session-state JSONL logs and history files). Its records
//! are OpenAI-chat-shaped: messages with `role` (`user` / `assistant` /
//! `tool`), assistant `tool_calls` arrays, and OpenAI-style `usage`
//! (`prompt_tokens` / `completion_tokens`).
//!
//! Copilot's on-disk schema has changed across versions, so this adapter is
//! deliberately defensive: it recognises the common shapes and falls back to
//! a readable generic summary when a record doesn't match any of them.

use serde_json::Value;

use crate::event::TokenUsage;
use crate::sources::{estimate_cost, truncate, Enrichment};

/// Normalise one Copilot CLI record.
pub fn enrich(raw: &Value) -> Enrichment {
    let mut e = Enrichment {
        session_id: raw
            .get("sessionId")
            .or_else(|| raw.get("session_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        timestamp: raw
            .get("timestamp")
            .or_else(|| raw.get("created_at"))
            .or_else(|| raw.get("time"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        cwd: raw.get("cwd").and_then(|v| v.as_str()).map(str::to_owned),
        git_branch: raw
            .get("gitBranch")
            .or_else(|| raw.get("git_branch"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        model: raw
            .get("model")
            .or_else(|| raw.pointer("/message/model"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        version: raw
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        ..Default::default()
    };

    let declared = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let role = raw
        .get("role")
        .or_else(|| raw.pointer("/message/role"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Decide the normalised event type.
    e.event_type = match declared {
        "user" | "assistant" | "system" | "tool_use" | "tool_result" | "summary" => {
            declared.to_owned()
        }
        _ => match role {
            "user" => "user".to_owned(),
            "assistant" => "assistant".to_owned(),
            "tool" => "tool_result".to_owned(),
            "system" => "system".to_owned(),
            _ => {
                if declared.is_empty() {
                    "unknown".to_owned()
                } else {
                    declared.to_owned()
                }
            }
        },
    };

    // Tool calls: OpenAI shape (`tool_calls: [{function:{name,…}, id}]`) or
    // Anthropic-ish content blocks.
    extract_tools(raw, &mut e);
    e.usage = extract_usage(raw);

    e.cost_usd = raw
        .get("costUSD")
        .or_else(|| raw.get("cost_usd"))
        .and_then(|v| v.as_f64())
        .or_else(|| {
            e.usage
                .as_ref()
                .map(|u| estimate_cost(e.model.as_deref(), u))
        });

    e.summary = summarise(raw, &e.event_type, &e.tool_uses);
    e
}

fn extract_tools(raw: &Value, e: &mut Enrichment) {
    // OpenAI tool_calls on the entry or on an embedded message.
    let candidates = [raw.get("tool_calls"), raw.pointer("/message/tool_calls")];
    for tc in candidates.into_iter().flatten() {
        if let Some(arr) = tc.as_array() {
            for call in arr {
                if let Some(name) = call
                    .pointer("/function/name")
                    .or_else(|| call.get("name"))
                    .and_then(|v| v.as_str())
                {
                    e.tool_uses.push(name.to_owned());
                }
            }
        }
    }
    // A tool message is the result side.
    if e.event_type == "tool_result" {
        if let Some(id) = raw
            .get("tool_call_id")
            .or_else(|| raw.pointer("/message/tool_call_id"))
            .and_then(|v| v.as_str())
        {
            e.tool_results.push(id.to_owned());
        }
    }
    // Anthropic-ish content blocks (some Copilot builds emit these).
    let blocks = [raw.get("content"), raw.pointer("/message/content")];
    for content in blocks.into_iter().flatten() {
        if let Some(arr) = content.as_array() {
            for b in arr {
                match b.get("type").and_then(|v| v.as_str()) {
                    Some("tool_use") => {
                        if let Some(n) = b.get("name").and_then(|v| v.as_str()) {
                            e.tool_uses.push(n.to_owned());
                        }
                    }
                    Some("tool_result") => {
                        if let Some(id) = b.get("tool_use_id").and_then(|v| v.as_str()) {
                            e.tool_results.push(id.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn extract_usage(raw: &Value) -> Option<TokenUsage> {
    let usage = raw.pointer("/message/usage").or_else(|| raw.get("usage"))?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if input == 0 && output == 0 && cache_read == 0 {
        return None;
    }
    Some(TokenUsage {
        input,
        output,
        cache_read,
        cache_creation: 0,
    })
}

fn summarise(raw: &Value, event_type: &str, tool_uses: &[String]) -> String {
    let preview = text_preview(raw, 110);
    match event_type {
        "user" => {
            if preview.is_empty() {
                "👤 User".to_owned()
            } else {
                format!("👤 {preview}")
            }
        }
        "assistant" => {
            let tools = if tool_uses.is_empty() {
                String::new()
            } else {
                format!(" · 🔧 {}", tool_uses.join(", "))
            };
            if preview.is_empty() {
                format!("🤖 Assistant{tools}")
            } else {
                format!("🤖 {preview}{tools}")
            }
        }
        "tool_use" => format!("🔧 {}", tool_uses.join(", ")),
        "tool_result" => "📦 Tool result".to_owned(),
        "system" => format!("⚙️  System: {preview}"),
        "summary" => format!("📝 Summary: {preview}"),
        other => format!("❓ {other}"),
    }
}

fn text_preview(raw: &Value, max_len: usize) -> String {
    let content = raw
        .pointer("/message/content")
        .or_else(|| raw.get("content"));
    match content {
        Some(Value::String(s)) => truncate(s, max_len),
        Some(Value::Array(arr)) => {
            for b in arr {
                if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        return truncate(t, max_len);
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn role_based_user() {
        let e = enrich(&json!({"role":"user","content":"hello copilot"}));
        assert_eq!(e.event_type, "user");
        assert!(e.summary.contains("hello copilot"));
    }

    #[test]
    fn openai_tool_calls() {
        let e = enrich(&json!({
            "role":"assistant",
            "content":null,
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"run_bash","arguments":"{}"}}],
            "model":"gpt-4o",
            "usage":{"prompt_tokens":100,"completion_tokens":20}
        }));
        assert_eq!(e.event_type, "assistant");
        assert_eq!(e.tool_uses, vec!["run_bash"]);
        let u = e.usage.unwrap();
        assert_eq!(u.input, 100);
        assert!(e.cost_usd.unwrap() > 0.0);
    }

    #[test]
    fn tool_message_is_result() {
        let e = enrich(&json!({"role":"tool","tool_call_id":"c1","content":"output"}));
        assert_eq!(e.event_type, "tool_result");
        assert_eq!(e.tool_results, vec!["c1"]);
    }

    #[test]
    fn declared_type_passthrough() {
        let e = enrich(&json!({"type":"user","content":"hi","sessionId":"s"}));
        assert_eq!(e.event_type, "user");
        assert_eq!(e.session_id.as_deref(), Some("s"));
    }
}
