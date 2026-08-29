//! Cursor Agent adapter.
//!
//! Cursor's headless agent (`cursor-agent`) can stream JSONL events, and
//! Cursor keeps workspace logs under `~/.cursor/`. There is no single
//! published schema, so this adapter is a tolerant union of the shapes Cursor
//! emits: Claude-Code-ish records (`type`/`sessionId`/`message`), OpenAI-ish
//! records (`role`/`content`/`tool_calls`), and stream events
//! (`{type:"assistant", message:{…}}` / `{type:"tool_call", …}` /
//! `{type:"result", …}`).
//!
//! Because the format is best-effort, this source is only enabled via an
//! explicit watch root or when a `~/.cursor` directory exists.

use serde_json::Value;

use crate::event::TokenUsage;
use crate::sources::{estimate_cost, truncate, Enrichment};

/// Normalise one Cursor agent record.
pub fn enrich(raw: &Value) -> Enrichment {
    let declared = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let mut e = Enrichment {
        session_id: raw
            .get("sessionId")
            .or_else(|| raw.get("session_id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        timestamp: raw
            .get("timestamp")
            .or_else(|| raw.get("ts"))
            .and_then(|v| {
                v.as_str().map(str::to_owned).or_else(|| {
                    v.as_i64().and_then(|ms| {
                        chrono::DateTime::from_timestamp_millis(ms).map(|d| d.to_rfc3339())
                    })
                })
            }),
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
        ..Default::default()
    };

    match declared {
        "user" | "assistant" | "system" | "summary" => {
            e.event_type = declared.to_owned();
            extract_block_tools(raw, &mut e);
        }
        "tool_call" | "function_call" => {
            e.event_type = "tool_use".to_owned();
            // cursor-agent stream: {"type":"tool_call","tool_call":{…}} or
            // a bare function_call object.
            let inner = raw.get("tool_call").unwrap_or(raw);
            let name = inner
                .get("name")
                .or_else(|| inner.pointer("/function/name"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            e.tool_uses.push(name.to_owned());
        }
        "result" | "tool_result" | "tool_call_result" => {
            e.event_type = "tool_result".to_owned();
            let inner = raw.get("tool_call").unwrap_or(raw);
            if let Some(id) = inner
                .get("call_id")
                .or_else(|| inner.get("tool_use_id"))
                .or_else(|| inner.get("id"))
                .and_then(|v| v.as_str())
            {
                e.tool_results.push(id.to_owned());
            }
        }
        _ => {
            // OpenAI-ish role-based records, else fall back.
            let role = raw
                .get("role")
                .or_else(|| raw.pointer("/message/role"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            e.event_type = match role {
                "user" => "user",
                "assistant" => "assistant",
                "tool" => "tool_result",
                _ => "unknown",
            }
            .to_owned();
            extract_block_tools(raw, &mut e);
        }
    }

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

/// Tool blocks from Anthropic-style content arrays and OpenAI tool_calls.
fn extract_block_tools(raw: &Value, e: &mut Enrichment) {
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
    let calls = [raw.get("tool_calls"), raw.pointer("/message/tool_calls")];
    for tc in calls.into_iter().flatten() {
        if let Some(arr) = tc.as_array() {
            for call in arr {
                if let Some(n) = call
                    .pointer("/function/name")
                    .or_else(|| call.get("name"))
                    .and_then(|v| v.as_str())
                {
                    e.tool_uses.push(n.to_owned());
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
    if input == 0 && output == 0 {
        return None;
    }
    Some(TokenUsage {
        input,
        output,
        cache_read: 0,
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
            if !tool_uses.is_empty() {
                let t = tool_uses.join(", ");
                if preview.is_empty() {
                    format!("🔧 {t}")
                } else {
                    format!("🤖 {preview} · 🔧 {t}")
                }
            } else if preview.is_empty() {
                "🤖 Assistant".to_owned()
            } else {
                format!("🤖 {preview}")
            }
        }
        "tool_use" => format!("🔧 {}", tool_uses.join(", ")),
        "tool_result" => "📦 Tool result".to_owned(),
        "system" => format!("⚙️  System: {preview}"),
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
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    return truncate(t, max_len);
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
    fn stream_tool_call() {
        let e = enrich(&json!({
            "type": "tool_call",
            "tool_call": {"name": "read_file", "arguments": "{}"}
        }));
        assert_eq!(e.event_type, "tool_use");
        assert_eq!(e.tool_uses, vec!["read_file"]);
    }

    #[test]
    fn stream_result() {
        let e = enrich(&json!({
            "type": "result",
            "tool_call": {"call_id": "c9", "output": "ok"}
        }));
        assert_eq!(e.event_type, "tool_result");
        assert_eq!(e.tool_results, vec!["c9"]);
    }

    #[test]
    fn assistant_message() {
        let e = enrich(&json!({
            "type": "assistant",
            "message": {"role":"assistant","content":[{"type":"text","text":"done"}]}
        }));
        assert_eq!(e.event_type, "assistant");
        assert!(e.summary.contains("done"));
    }

    #[test]
    fn role_based_fallback() {
        let e = enrich(&json!({"role":"user","content":"cursor hi"}));
        assert_eq!(e.event_type, "user");
        assert!(e.summary.contains("cursor hi"));
    }
}
