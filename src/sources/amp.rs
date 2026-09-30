//! Amp (Sourcegraph / ampcode.com) adapter — legacy local threads.
//!
//! Amp builds up to 2026-03-31 kept each thread as a JSON file at
//! `$XDG_DATA_HOME/amp/threads/T-<uuid>.json` (`~/.local/share` on every OS;
//! `$AMP_DATA_DIR` overrides), rewritten atomically on every change. Current
//! Amp stores threads server-side only; the files already on disk remain and
//! are ingested here. For live Amp sessions, pipe `amp -x --stream-json`
//! into a `.jsonl` file under a watch root — it is Claude-Code-compatible.
//!
//! Messages are Anthropic-shaped; tool results arrive in the next user
//! message as `{type: "tool_result", toolUseID, run: {status, result}}`;
//! assistant messages carry `usage` with the model.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{anthropic_blocks, Block, Message, Role};
use crate::sources::{
    any_timestamp, env_path, estimate_cost_for, file_uri_to_path, summarise_message, xdg_data_home,
    AgentSource, Enrichment, FileKind, SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let base = env_path("AMP_DATA_DIR").unwrap_or_else(|| xdg_data_home(home).join("amp"));
    vec![base.join("threads")]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    (name.starts_with("T-") && name.ends_with(".json")).then_some(FileKind::Document)
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))?;
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(id));
    if let Some(t) = v.get("title").and_then(Value::as_str) {
        ctx.insert("title".into(), json!(t));
    }
    if let Some(uri) = v
        .pointer("/env/initial/trees/0/uri")
        .and_then(Value::as_str)
    {
        if let Some(p) = file_uri_to_path(uri) {
            ctx.insert("cwd".into(), json!(p));
        }
    }
    if let Some(tags) = v.pointer("/env/initial/tags").and_then(Value::as_array) {
        if let Some(m) = tags
            .iter()
            .filter_map(Value::as_str)
            .find_map(|t| t.strip_prefix("model:"))
        {
            ctx.insert("model".into(), json!(m));
        }
    }
    if let Some(ver) = v
        .pointer("/env/initial/platform/clientVersion")
        .and_then(Value::as_str)
    {
        ctx.insert("version".into(), json!(ver));
    }
    ctx.insert(
        "created".into(),
        v.get("created").cloned().unwrap_or(Value::Null),
    );
    let records = v
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut m| {
            if let Some(o) = m.as_object_mut() {
                o.insert("_trace".into(), Value::Object(ctx.clone()));
            }
            m
        })
        .collect();
    Some(vec![SessionDoc {
        session_id: id,
        records,
    }])
}

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let role = raw.get("role").and_then(Value::as_str).unwrap_or("");
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title"),
        version: cs("version"),
        timestamp: raw
            .pointer("/usage/timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| any_timestamp(raw.pointer("/meta/sentAt")))
            .or_else(|| any_timestamp(ctx.get("created"))),
        ..Default::default()
    };
    let content = raw.get("content").cloned().unwrap_or(Value::Null);
    let mut blocks = anthropic_blocks(&content);
    // Amp tool results: {type:"tool_result", toolUseID, run:{status, result|error}}
    if let Value::Array(arr) = &content {
        for b in arr {
            if b.get("type").and_then(Value::as_str) == Some("tool_result")
                && b.get("toolUseID").is_some()
            {
                let run = b.get("run").cloned().unwrap_or(Value::Null);
                let status = run.get("status").and_then(Value::as_str).unwrap_or("");
                let content = run
                    .get("result")
                    .or_else(|| run.pointer("/error/message"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let content = match content {
                    Value::String(s) => Value::String(s),
                    Value::Null => Value::String(String::new()),
                    other => Value::String(
                        other
                            .get("output")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| other.to_string()),
                    ),
                };
                blocks.retain(|x| !matches!(x, Block::ToolResult { tool_use_id, .. } if tool_use_id.is_empty()));
                blocks.push(Block::ToolResult {
                    tool_use_id: b["toolUseID"].as_str().unwrap_or("").to_owned(),
                    content,
                    is_error: matches!(status, "error" | "cancelled" | "rejected-by-user"),
                });
            }
        }
    }
    for b in &blocks {
        match b {
            Block::ToolUse { name, .. } => e.tool_uses.push(name.clone()),
            Block::ToolResult { tool_use_id, .. } => e.tool_results.push(tool_use_id.clone()),
            _ => {}
        }
    }
    match role {
        "assistant" => {
            e.event_type = "assistant".into();
            e.model = raw
                .pointer("/usage/model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| cs("model"));
            if let Some(u) = raw.get("usage") {
                let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let usage = TokenUsage {
                    input: g("inputTokens"),
                    output: g("outputTokens"),
                    cache_read: g("cacheReadInputTokens"),
                    cache_creation: g("cacheCreationInputTokens"),
                };
                if usage.input + usage.output + usage.cache_read + usage.cache_creation > 0 {
                    e.cost_usd = Some(estimate_cost_for(
                        AgentSource::Amp,
                        e.model.as_deref(),
                        &usage,
                    ));
                    e.usage = Some(usage);
                }
            }
            e.turn_end =
                raw.pointer("/state/stopReason").and_then(Value::as_str) == Some("end_turn");
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.summary = summarise_message("assistant", e.message.as_ref(), &e.tool_uses);
        }
        "user" => {
            e.event_type = "user".into();
            e.message = Message::new(Role::User, blocks).non_empty();
            e.summary = summarise_message("user", e.message.as_ref(), &[]);
        }
        _ => {
            e.event_type = "system".into();
            e.summary = summarise_message(
                "system",
                Message::new(Role::System, blocks).non_empty().as_ref(),
                &[],
            );
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_thread() {
        let body = r#"{"v":57,"id":"T-3f1c","created":1757000000000,"title":"Fix failing test",
   "messages":[
    {"role":"user","messageId":0,"content":[{"type":"text","text":"fix the failing test"}],"meta":{"sentAt":1757000001000}},
    {"role":"assistant","messageId":1,"content":[{"type":"thinking","thinking":"Run tests first.","signature":"s"},{"type":"tool_use","id":"toolu_01A","name":"Bash","input":{"cmd":"cargo test"}}],
     "state":{"type":"complete","stopReason":"tool_use"},
     "usage":{"model":"claude-sonnet-4-5","inputTokens":12,"outputTokens":80,"cacheCreationInputTokens":2100,"cacheReadInputTokens":9000,"totalInputTokens":11112,"timestamp":"2025-09-04T15:33:30.120Z"}},
    {"role":"user","messageId":2,"content":[{"type":"tool_result","toolUseID":"toolu_01A","run":{"status":"done","result":{"output":"test result: ok","exitCode":0}}}]}
   ],
   "env":{"initial":{"trees":[{"displayName":"app","uri":"file:///home/u/my%20app"}],"tags":["model:claude-sonnet-4-5"]}}}"#;
        let d = parse_document(Path::new("/x/amp/threads/T-3f1c.json"), body)
            .unwrap()
            .remove(0);
        assert_eq!(d.session_id, "T-3f1c");
        let a = enrich(&d.records[1]);
        assert_eq!(a.cwd.as_deref(), Some("/home/u/my app"));
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(a.usage.as_ref().unwrap().cache_read, 9000);
        assert_eq!(a.tool_uses, vec!["Bash"]);
        let r = enrich(&d.records[2]);
        let (id, c, err) = r.message.as_ref().unwrap().tool_results().next().unwrap();
        assert_eq!(
            (id, c.as_str().unwrap(), err),
            ("toolu_01A", "test result: ok", false)
        );
        assert_eq!(
            enrich(&d.records[0]).title.as_deref(),
            Some("Fix failing test")
        );
    }
}
