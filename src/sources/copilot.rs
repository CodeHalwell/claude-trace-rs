//! GitHub Copilot CLI adapter.
//!
//! Sessions live under `$COPILOT_HOME` (default `~/.copilot`; older builds
//! used `$XDG_STATE_HOME/.copilot`):
//!
//! - **0.0.378+**: `session-state/<uuid>/events.jsonl` beside a
//!   `workspace.yaml` (cwd, branch, session name);
//! - **0.0.342–0.0.377**: flat `session-state/<uuid>.jsonl`;
//! - **0.0.326–0.0.341**: whole-file `history-session-state/session_<uuid>_<ms>.json`
//!   (`{sessionId, chatMessages: [OpenAI messages], timeline}`).
//!
//! Event lines are `{type, data, id, timestamp, parentId}` with dotted types:
//! `session.start` (cwd/branch in `data.context`), `user.message`,
//! `system.message`, `assistant.message` (`content`, `toolRequests[]`,
//! `reasoningText`), `tool.execution_start/complete`, `session.error`,
//! `abort`, `session.resume`, `session.shutdown`.
//!
//! Token usage is persisted only in `session.shutdown.modelMetrics` —
//! cumulative per session across resumes — so files are read as documents
//! and each shutdown is reduced to the delta since the previous one.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{
    env_path, estimate_cost_for, summarise_message, truncate, AgentSource, Enrichment, FileKind,
    SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let root = env_path("COPILOT_HOME").unwrap_or_else(|| home.join(".copilot"));
    let mut v = vec![
        root.join("session-state"),
        root.join("history-session-state"),
    ];
    if let Some(state) = env_path("XDG_STATE_HOME") {
        v.push(state.join(".copilot/session-state"));
    }
    v
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".jsonl") {
        return Some(FileKind::Document);
    }
    if name.starts_with("session_") && name.ends_with(".json") {
        return Some(FileKind::Document);
    }
    None
}

pub fn session_id_for_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    if stem == "events" {
        if let Some(dir) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            return dir.to_owned();
        }
    }
    // history-session-state/session_<uuid>_<ms>.json
    if let Some(rest) = stem.strip_prefix("session_") {
        if let Some((id, _)) = rest.rsplit_once('_') {
            return id.to_owned();
        }
    }
    stem.to_owned()
}

fn workspace_yaml(dir: &Path) -> Map<String, Value> {
    // A flat `key: value` file; no YAML library needed for the fields we use.
    let mut m = Map::new();
    if let Ok(body) = std::fs::read_to_string(dir.join("workspace.yaml")) {
        for line in body.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() && !line.starts_with(' ') {
                    m.insert(k.trim().to_owned(), json!(v));
                }
            }
        }
    }
    m
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let session_id = session_id_for_path(path);
    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        return parse_legacy_json(&session_id, body);
    }
    let ws = path.parent().map(workspace_yaml).unwrap_or_default();
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if let Some(c) = ws.get("cwd") {
        ctx.insert("cwd".into(), c.clone());
    }
    if let Some(b) = ws.get("branch") {
        ctx.insert("branch".into(), b.clone());
    }
    if let Some(n) = ws.get("name") {
        ctx.insert("title".into(), n.clone());
    }

    let mut records = Vec::new();
    let mut model: Option<String> = None;
    let mut prev_metrics: HashMap<String, TokenUsage> = HashMap::new();
    for line in body.lines() {
        let Ok(mut v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let t = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        match t.as_str() {
            "session.start" | "session.resume" => {
                if let Some(m) = data.get("selectedModel").and_then(Value::as_str) {
                    model = Some(m.to_owned());
                }
                if let Some(c) = data.pointer("/context/cwd") {
                    ctx.insert("cwd".into(), c.clone());
                }
                if let Some(b) = data.pointer("/context/branch") {
                    ctx.insert("branch".into(), b.clone());
                }
            }
            "session.context_changed" => {
                if let Some(c) = data.get("cwd") {
                    ctx.insert("cwd".into(), c.clone());
                }
            }
            "session.model_change" => {
                if let Some(m) = data.get("newModel").and_then(Value::as_str) {
                    model = Some(m.to_owned());
                }
            }
            "session.shutdown" => {
                // Cumulative → delta since the previous shutdown.
                let mut delta = Map::new();
                if let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) {
                    for (m, mm) in metrics {
                        let u = mm.get("usage").cloned().unwrap_or(Value::Null);
                        let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let now = TokenUsage {
                            input: g("inputTokens"),
                            output: g("outputTokens"),
                            cache_read: g("cacheReadTokens"),
                            cache_creation: g("cacheWriteTokens"),
                        };
                        let before = prev_metrics.get(m).cloned().unwrap_or_default();
                        let d = TokenUsage {
                            input: now.input.saturating_sub(before.input),
                            output: now.output.saturating_sub(before.output),
                            cache_read: now.cache_read.saturating_sub(before.cache_read),
                            cache_creation: now
                                .cache_creation
                                .saturating_sub(before.cache_creation),
                        };
                        prev_metrics.insert(m.clone(), now);
                        if d.input + d.output + d.cache_read + d.cache_creation > 0 {
                            delta
                                .insert(m.clone(), serde_json::to_value(&d).unwrap_or(Value::Null));
                        }
                    }
                }
                v["_usage_delta"] = Value::Object(delta);
            }
            _ => {}
        }
        let mut c = ctx.clone();
        if let Some(m) = &model {
            c.insert("model".into(), json!(m));
        }
        v["_trace"] = Value::Object(c);
        records.push(v);
    }
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

fn parse_legacy_json(session_id: &str, body: &str) -> Option<Vec<SessionDoc>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let id = v
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or(session_id)
        .to_owned();
    let ts = v.get("startTime").cloned().unwrap_or(Value::Null);
    let records = v
        .get("chatMessages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(
            |m| json!({ "type": "_chat", "message": m, "_trace": { "sessionId": id, "time": ts } }),
        )
        .collect();
    Some(vec![SessionDoc {
        session_id: id,
        records,
    }])
}

/// Normalise one Copilot CLI record.
pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let data = raw.get("data").cloned().unwrap_or(Value::Null);
    let ds = |k: &str| data.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId").or_else(|| {
            raw.get("sessionId")
                .or_else(|| raw.get("session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        timestamp: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| cs("time")),
        cwd: cs("cwd"),
        git_branch: cs("branch"),
        title: cs("title"),
        ..Default::default()
    };
    let t = raw.get("type").and_then(Value::as_str).unwrap_or("");

    // Pre-0.0.342 chat messages, and older OpenAI-shaped custom logs.
    if t == "_chat" || (t.is_empty() && raw.get("role").is_some()) {
        let m = raw.get("message").unwrap_or(raw);
        e.message = Message::from_openai(m);
        e.event_type = match m.get("role").and_then(Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            Some("tool") => "tool_result",
            _ => "system",
        }
        .into();
        if let Some(msg) = &e.message {
            for (_, name, _) in msg.tool_uses() {
                e.tool_uses.push(name.to_owned());
            }
            for (id, _, _) in msg.tool_results() {
                e.tool_results.push(id.to_owned());
            }
        }
        e.model = m.get("model").and_then(Value::as_str).map(str::to_owned);
        e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
        return e;
    }

    match t {
        "session.start" => {
            e.event_type = "system".into();
            e.version = ds("copilotVersion");
            e.cwd = data
                .pointer("/context/cwd")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(e.cwd);
            e.git_branch = data
                .pointer("/context/branch")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(e.git_branch);
            e.model = ds("selectedModel");
            e.summary = format!(
                "⚙️  Session start ({})",
                e.model.clone().unwrap_or_else(|| "copilot".into())
            );
        }
        "session.resume" => {
            e.event_type = "system".into();
            e.summary = "⚙️  Session resumed".into();
        }
        "user.message" => {
            let text = ds("content").unwrap_or_default();
            let injected = data
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
                || data.get("isAutopilotContinuation").and_then(Value::as_bool) == Some(true);
            let role = if injected { Role::System } else { Role::User };
            e.event_type = if injected { "system" } else { "user" }.into();
            let mut blocks = Vec::new();
            if !text.is_empty() {
                blocks.push(Block::Text { text });
            }
            for a in data
                .get("attachments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(p) = a
                    .get("path")
                    .or_else(|| a.get("filePath"))
                    .and_then(Value::as_str)
                {
                    blocks.push(Block::Text {
                        text: format!("[attachment: {p}]"),
                    });
                }
            }
            e.message = Message::new(role, blocks).non_empty();
            e.summary = summarise_message(&e.event_type, e.message.as_ref(), &[]);
        }
        "system.message" => {
            e.event_type = "system".into();
            e.message = ds("content").and_then(|c| Message::text(Role::System, c).non_empty());
            e.summary = "⚙️  System prompt".into();
        }
        "assistant.message" => {
            e.event_type = "assistant".into();
            e.model = ds("model").or_else(|| cs("model"));
            let mut blocks = Vec::new();
            if let Some(r) = ds("reasoningText").filter(|r| !r.is_empty()) {
                blocks.push(Block::Thinking { thinking: r });
            }
            if let Some(c) = ds("content").filter(|c| !c.is_empty()) {
                blocks.push(Block::Text { text: c });
            }
            for tr in data
                .get("toolRequests")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = tr
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                e.tool_uses.push(name.clone());
                blocks.push(Block::ToolUse {
                    id: tr
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    name,
                    input: crate::message::parse_json_arguments(tr.get("arguments")),
                });
            }
            e.turn_end = e.tool_uses.is_empty()
                && blocks.iter().any(|b| matches!(b, Block::Text { .. }))
                && data.get("parentToolCallId").is_none();
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.summary = summarise_message("assistant", e.message.as_ref(), &e.tool_uses);
        }
        "assistant.reasoning" => {
            e.event_type = "assistant".into();
            e.message = ds("content").and_then(|c| {
                Message::new(Role::Assistant, vec![Block::Thinking { thinking: c }]).non_empty()
            });
            e.summary = "💭 Reasoning".into();
        }
        "tool.execution_start" => {
            // The call itself is on assistant.message.toolRequests.
            e.event_type = "system".into();
            e.summary = format!("▶️  {}", ds("toolName").unwrap_or_default());
        }
        "tool.execution_complete" => {
            e.event_type = "tool_result".into();
            let id = ds("toolCallId").unwrap_or_default();
            let ok = data.get("success").and_then(Value::as_bool).unwrap_or(true);
            let text = data
                .pointer("/result/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    data.pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            e.tool_results.push(id.clone());
            e.message = Message::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content: Value::String(text.clone()),
                    is_error: !ok,
                }],
            )
            .non_empty();
            e.summary = if ok {
                format!("📦 {}", truncate(&text, 100))
            } else {
                format!("⛔ {}", truncate(&text, 100))
            };
        }
        "session.shutdown" => {
            e.event_type = "system".into();
            let mut total = TokenUsage::default();
            let mut cost = 0.0;
            let mut last_model = None;
            if let Some(delta) = raw.get("_usage_delta").and_then(Value::as_object) {
                for (m, u) in delta {
                    let Ok(u) = serde_json::from_value::<TokenUsage>(u.clone()) else {
                        continue;
                    };
                    // Copilot reports input including cached tokens.
                    let billable = TokenUsage {
                        input: u.input.saturating_sub(u.cache_read + u.cache_creation),
                        ..u.clone()
                    };
                    cost += estimate_cost_for(AgentSource::Copilot, Some(m), &billable);
                    total.input += billable.input;
                    total.output += billable.output;
                    total.cache_read += billable.cache_read;
                    total.cache_creation += billable.cache_creation;
                    last_model = Some(m.clone());
                }
            }
            if total.input + total.output + total.cache_read + total.cache_creation > 0 {
                e.usage = Some(total);
                e.cost_usd = Some(cost);
                e.model = ds("currentModel").or(last_model);
            }
            e.summary = format!(
                "⚙️  Session ended ({})",
                ds("shutdownType").unwrap_or_else(|| "routine".into())
            );
        }
        "session.error" => {
            e.event_type = "error".into();
            e.summary = format!("⛔ {}", truncate(&ds("message").unwrap_or_default(), 110));
        }
        "abort" => {
            e.event_type = "system".into();
            e.turn_end = true;
            e.summary = format!("⏹  Aborted ({})", ds("reason").unwrap_or_default());
        }
        "session.model_change" => {
            e.event_type = "system".into();
            e.model = ds("newModel");
            e.summary = format!("⚙️  Model → {}", e.model.clone().unwrap_or_default());
        }
        "session.compaction_complete" => {
            e.event_type = "summary".into();
            e.summary = "📝 Context compacted".into();
        }
        other => {
            e.event_type = "system".into();
            e.summary = format!("⚙️  {other}");
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn real_bash_tool_session() {
        let body = fixture("copilot-1.0.89-bash-tool.events.jsonl");
        let p = Path::new("/nonexistent/.copilot/session-state/aaf955f6/events.jsonl");
        assert_eq!(session_id_for_path(p), "aaf955f6");
        let d = parse_document(p, &body).unwrap().remove(0);
        let recs: Vec<Enrichment> = d.records.iter().map(enrich).collect();
        let start = recs.iter().find(|e| e.version.is_some()).unwrap();
        assert_eq!(start.cwd.as_deref(), Some("/work/demo"));
        assert_eq!(start.git_branch.as_deref(), Some("master"));
        let user = recs.iter().find(|e| e.event_type == "user").unwrap();
        assert_eq!(
            user.message.as_ref().unwrap().plain_text(),
            "Run echo hello please"
        );
        let call = recs.iter().find(|e| e.tool_uses == vec!["bash"]).unwrap();
        assert_eq!(call.model.as_deref(), Some("gpt-4.1"));
        let result = recs.iter().find(|e| e.event_type == "tool_result").unwrap();
        assert!(result.summary.contains("hello-from-tool"));
        let shutdown = recs.iter().find(|e| e.usage.is_some()).unwrap();
        let u = shutdown.usage.as_ref().unwrap();
        assert_eq!(u.cache_read, 2000);
        assert_eq!(u.input, 468);
        assert!(recs.iter().any(|e| e.turn_end));
    }

    #[test]
    fn repeated_shutdowns_are_not_double_counted() {
        let body = fixture("copilot-1.0.89-denied-tool-resume-error.events.jsonl");
        let p = Path::new("/nonexistent/.copilot/session-state/s2/events.jsonl");
        let d = parse_document(p, &body).unwrap().remove(0);
        let recs: Vec<Enrichment> = d.records.iter().map(enrich).collect();
        let with_usage = recs.iter().filter(|e| e.usage.is_some()).count();
        assert_eq!(with_usage, 1, "second shutdown repeats the same totals");
        assert!(recs.iter().any(|e| e.event_type == "error"));
        let failed = recs.iter().find(|e| e.event_type == "tool_result").unwrap();
        assert!(
            failed
                .message
                .as_ref()
                .unwrap()
                .tool_results()
                .next()
                .unwrap()
                .2
        );
        assert!(recs.iter().any(|e| e.message.as_ref().is_some_and(|m| m
            .content
            .iter()
            .any(|b| matches!(b, Block::Thinking { .. })))));
    }

    #[test]
    fn legacy_history_session_json() {
        let body = r#"{"sessionId":"old-1","startTime":"2025-09-20T10:00:00Z","chatMessages":[
          {"role":"user","content":"hello copilot"},
          {"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"run_bash","arguments":"{}"}}]},
          {"role":"tool","tool_call_id":"c1","content":"output"}],"timeline":[]}"#;
        let p = Path::new("/x/.copilot/history-session-state/session_old-1_1758362400000.json");
        assert_eq!(session_id_for_path(p), "old-1");
        let d = parse_document(p, body).unwrap().remove(0);
        let recs: Vec<Enrichment> = d.records.iter().map(enrich).collect();
        assert_eq!(recs[0].event_type, "user");
        assert_eq!(recs[1].tool_uses, vec!["run_bash"]);
        assert_eq!(recs[2].tool_results, vec!["c1"]);
    }
}
