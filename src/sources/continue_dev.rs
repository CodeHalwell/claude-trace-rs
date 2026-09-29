//! Continue adapter (VS Code / JetBrains extension and the `cn` CLI).
//!
//! Sessions are whole JSON files at `<root>/sessions/<sessionId>.json`,
//! where `<root>` is `$CONTINUE_GLOBAL_DIR` or `~/.continue`; they are
//! rewritten (non-atomically) after each completed response. `sessions.json`
//! is only an index and is skipped.
//!
//! Each `history[]` item wraps a `message` (`user` / `assistant` /
//! `thinking` / `system` / `tool`). Assistant tool calls are OpenAI-style
//! `toolCalls`; the IDE also records results as `tool` messages, while the
//! CLI records them only in `toolCallStates[].output` — used when no `tool`
//! messages exist. The IDE stores the workspace as a `file://` URI and no
//! token counts; the CLI stores a plain path and per-message OpenAI-style
//! `usage` (with `model` and `cost_cents`).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{parse_json_arguments, Block, Message, Role};
use crate::sources::{
    env_path, estimate_cost_for, file_uri_to_path, summarise_message, AgentSource, Enrichment,
    FileKind, SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    vec![env_path("CONTINUE_GLOBAL_DIR")
        .unwrap_or_else(|| home.join(".continue"))
        .join("sessions")]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    (name.ends_with(".json") && name != "sessions.json").then_some(FileKind::Document)
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let history = v.get("history")?.as_array()?.clone();
    let session_id = v
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))?;
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if let Some(ws) = v
        .get("workspaceDirectory")
        .and_then(Value::as_str)
        .filter(|w| !w.is_empty())
    {
        let cwd = file_uri_to_path(ws).unwrap_or_else(|| ws.to_owned());
        ctx.insert("cwd".into(), json!(cwd));
    }
    if let Some(t) = v
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !matches!(*t, "New Session" | "Untitled Session"))
    {
        ctx.insert("title".into(), json!(t));
    }
    if let Some(m) = v.get("chatModelTitle").and_then(Value::as_str) {
        ctx.insert("model".into(), json!(m));
    }
    let has_tool_messages = history
        .iter()
        .any(|h| h.pointer("/message/role").and_then(Value::as_str) == Some("tool"));
    ctx.insert("toolMessages".into(), json!(has_tool_messages));
    let records = history
        .into_iter()
        .map(|mut h| {
            if let Some(o) = h.as_object_mut() {
                // Keep records compact: the context items and editor state can
                // be huge and are not part of the conversation.
                o.remove("editorState");
                o.insert("_trace".into(), Value::Object(ctx.clone()));
            }
            h
        })
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

fn content_blocks(content: &Value) -> Vec<Block> {
    match content {
        Value::String(s) if !s.is_empty() => vec![Block::Text { text: s.clone() }],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .map(|t| Block::Text { text: t.to_owned() }),
                Some("imageUrl") => Some(Block::Image {
                    source: p.get("imageUrl").cloned().unwrap_or(Value::Null),
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let msg = raw.get("message").cloned().unwrap_or(Value::Null);
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title"),
        timestamp: raw
            .pointer("/reasoning/startAt")
            .and_then(crate::sources::epoch_to_rfc3339),
        ..Default::default()
    };
    let mut blocks = content_blocks(msg.get("content").unwrap_or(&Value::Null));
    match role {
        "user" => {
            e.event_type = "user".into();
            e.message = Message::new(Role::User, blocks).non_empty();
        }
        "tool" => {
            e.event_type = "tool_result".into();
            let id = msg
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            e.tool_results.push(id.clone());
            e.message = Message::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content: msg.get("content").cloned().unwrap_or(Value::Null),
                    is_error: false,
                }],
            )
            .non_empty();
        }
        "assistant" | "thinking" => {
            e.event_type = "assistant".into();
            if role == "thinking" {
                blocks = blocks
                    .into_iter()
                    .map(|b| match b {
                        Block::Text { text } => Block::Thinking { thinking: text },
                        other => other,
                    })
                    .collect();
            }
            if let Some(r) = raw.pointer("/reasoning/text").and_then(Value::as_str) {
                if !r.is_empty() {
                    blocks.insert(
                        0,
                        Block::Thinking {
                            thinking: r.to_owned(),
                        },
                    );
                }
            }
            for call in msg
                .get("toolCalls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let f = call.get("function").cloned().unwrap_or(Value::Null);
                let name = f
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                e.tool_uses.push(name.clone());
                blocks.push(Block::ToolUse {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    name,
                    input: parse_json_arguments(f.get("arguments")),
                });
            }
            // The CLI keeps tool output only on the call states.
            if ctx.get("toolMessages").and_then(Value::as_bool) != Some(true) {
                for st in raw
                    .get("toolCallStates")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(out) = st.get("output").and_then(Value::as_array) else {
                        continue;
                    };
                    let text = out
                        .iter()
                        .filter_map(|o| o.get("content").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let id = st
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    e.tool_results.push(id.clone());
                    blocks.push(Block::ToolResult {
                        tool_use_id: id,
                        content: Value::String(text),
                        is_error: matches!(
                            st.get("status").and_then(Value::as_str),
                            Some("errored" | "canceled")
                        ),
                    });
                }
            }
            e.model = msg
                .pointer("/usage/model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    raw.get("promptLogs")
                        .and_then(Value::as_array)
                        .and_then(|l| l.last())
                        .and_then(|l| l.get("modelTitle"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or_else(|| cs("model"));
            if let Some(u) = msg.get("usage") {
                let g = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                let cached = g("/prompt_tokens_details/cache_read_tokens")
                    .max(g("/prompt_tokens_details/cached_tokens"));
                let write = g("/prompt_tokens_details/cache_write_tokens");
                let prompt = g("/prompt_tokens").max(g("/promptTokens"));
                let usage = TokenUsage {
                    // OpenAI semantics: cached tokens are a subset of prompt.
                    input: prompt.saturating_sub(cached),
                    output: g("/completion_tokens").max(g("/completionTokens")),
                    cache_read: cached,
                    cache_creation: write,
                };
                if let Some(cents) = u.get("cost_cents").and_then(Value::as_f64) {
                    e.cost_usd = Some(cents / 100.0);
                    e.cost_explicit = true;
                }
                if usage.input + usage.output + usage.cache_read > 0 {
                    if e.cost_usd.is_none() {
                        e.cost_usd = Some(estimate_cost_for(
                            AgentSource::Continue,
                            e.model.as_deref(),
                            &usage,
                        ));
                    }
                    e.usage = Some(usage);
                }
            }
            e.turn_end =
                e.tool_uses.is_empty() && blocks.iter().any(|b| matches!(b, Block::Text { .. }));
            e.message = Message::new(Role::Assistant, blocks).non_empty();
        }
        _ => {
            e.event_type = "system".into();
            e.message = Message::new(Role::System, blocks).non_empty();
        }
    }
    e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_session() {
        let body = r#"{
  "sessionId": "0f9a", "title": "Refactor tail loop", "workspaceDirectory": "file:///home/dan/claude-trace-rs",
  "history": [
    {"message": {"role": "user", "content": [{"type": "text", "text": "Why does tail_file re-read the file?"}], "id": "u1"}, "contextItems": []},
    {"message": {"id": "a1", "role": "assistant", "content": "Let me read it.",
      "toolCalls": [{"id": "toolu_01", "type": "function", "function": {"name": "read_file", "arguments": "{\"filepath\":\"src/tail.rs\"}"}}]},
     "contextItems": [], "toolCallStates": [{"toolCallId": "toolu_01", "status": "done", "output": [{"content": "dup"}]}],
     "promptLogs": [{"modelTitle": "Claude Sonnet 4.5", "modelProvider": "anthropic"}]},
    {"message": {"role": "tool", "content": "fn tail() {}", "toolCallId": "toolu_01", "id": "t1"}, "contextItems": []},
    {"message": {"role": "assistant", "content": "It seeks to 0 on every event", "id": "a2"}, "contextItems": []}
  ],
  "mode": "agent", "chatModelTitle": "Claude Sonnet 4.5"
}"#;
        assert_eq!(
            classify(Path::new("/x/sessions/0f9a.json")),
            Some(FileKind::Document)
        );
        assert_eq!(classify(Path::new("/x/sessions/sessions.json")), None);
        let d = parse_document(Path::new("/x/sessions/0f9a.json"), body)
            .unwrap()
            .remove(0);
        let recs: Vec<Enrichment> = d.records.iter().map(enrich).collect();
        assert_eq!(recs[0].cwd.as_deref(), Some("/home/dan/claude-trace-rs"));
        assert_eq!(recs[1].tool_uses, vec!["read_file"]);
        assert_eq!(recs[1].model.as_deref(), Some("Claude Sonnet 4.5"));
        // Tool output comes from the tool message, not duplicated from states.
        assert_eq!(recs[1].tool_results.len(), 0);
        assert_eq!(recs[2].tool_results, vec!["toolu_01"]);
        assert!(recs[3].turn_end);
        assert_eq!(recs[0].title.as_deref(), Some("Refactor tail loop"));
    }

    #[test]
    fn cli_session_usage_and_states() {
        let body = r#"{"sessionId":"c1","title":"Untitled Session","workspaceDirectory":"/work/app","history":[
          {"message":{"role":"user","content":"hi"},"contextItems":[]},
          {"message":{"role":"assistant","content":"ok","toolCalls":[{"id":"x","type":"function","function":{"name":"ls","arguments":"{}"}}],
            "usage":{"prompt_tokens":812,"completion_tokens":64,"prompt_tokens_details":{"cached_tokens":500},"model":"gpt-5","cost_cents":1}},
           "contextItems":[],"toolCallStates":[{"toolCallId":"x","status":"done","output":[{"content":"a.rs","name":"Tool Result"}]}]}]}"#;
        let d = parse_document(Path::new("/x/sessions/c1.json"), body)
            .unwrap()
            .remove(0);
        let a = enrich(&d.records[1]);
        assert_eq!(a.model.as_deref(), Some("gpt-5"));
        assert_eq!(a.usage.as_ref().unwrap().input, 312);
        assert_eq!(a.cost_usd, Some(0.01));
        assert_eq!(a.tool_results, vec!["x"]);
        assert!(a.title.is_none());
        assert_eq!(a.cwd.as_deref(), Some("/work/app"));
    }
}
