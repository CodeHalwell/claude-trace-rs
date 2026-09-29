//! Kimi adapter — Moonshot's terminal agent, in both of its generations.
//!
//! - **Kimi Code** (current, TypeScript): `$KIMI_CODE_HOME` or `~/.kimi-code`,
//!   `sessions/wd_<slug>_<hash>/session_<uuid>/agents/<agent>/wire.jsonl`
//!   beside a `state.json` holding `cwd` and `title`. Records are flat dotted
//!   events with a millisecond `time`: `context.append_message` (user input),
//!   `context.append_loop_event` (assistant `content.part`, `tool.call`,
//!   `tool.result`, `step.end` with usage), `llm.request` (model) and
//!   `turn.ended`.
//! - **Kimi CLI** (legacy Python): `$KIMI_SHARE_DIR` or `~/.kimi`,
//!   `sessions/<md5(cwd)>/<uuid>/wire.jsonl` with `{timestamp, message:
//!   {type, payload}}` records (`TurnBegin`, `ContentPart`, `ToolCall`,
//!   `ToolResult`, `StatusUpdate.token_usage`, `TurnEnd`). The cwd is
//!   recovered by hashing the work dirs listed in `~/.kimi/kimi.json`.
//!
//! Both logs are event streams in which one assistant step spans many
//! records, so files are read as documents and regrouped into one record per
//! step (text, reasoning and tool calls together, usage attached) plus one
//! per tool result. Sessions Kimi Code imported from `~/.kimi` are skipped in
//! favour of the originals.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{parse_json_arguments, Block, Message, Role};
use crate::sources::{
    any_timestamp, env_path, estimate_cost_for, md5_hex, summarise_message, AgentSource,
    Enrichment, FileKind, SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        env_path("KIMI_CODE_HOME")
            .unwrap_or_else(|| home.join(".kimi-code"))
            .join("sessions"),
        env_path("KIMI_SHARE_DIR")
            .unwrap_or_else(|| home.join(".kimi"))
            .join("sessions"),
    ]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    (path.file_name()?.to_str()? == "wire.jsonl").then_some(FileKind::Document)
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    // Legacy records are `{timestamp, message: {type, payload}}`; Kimi Code
    // records are flat `{type: "dotted.name", …, time}`.
    let legacy = body
        .lines()
        .take(20)
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find_map(|v| {
            if v.get("message").and_then(|m| m.get("type")).is_some() {
                Some(true)
            } else if v
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t.contains('.'))
            {
                Some(false)
            } else {
                None
            }
        })
        .unwrap_or(false);
    if legacy {
        parse_legacy(path, body)
    } else {
        parse_code(path, body)
    }
}

// ---------------------------------------------------------------------------
// Kimi Code (~/.kimi-code)
// ---------------------------------------------------------------------------

fn parse_code(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    // sessions/<wd>/<session>/agents/<agent>/wire.jsonl
    let agent_dir = path.parent()?;
    let agent = agent_dir.file_name()?.to_str()?.to_owned();
    let session_dir = agent_dir.parent()?.parent()?;
    let base_id = session_dir.file_name()?.to_str()?.to_owned();
    let state = read_json(&session_dir.join("state.json")).unwrap_or(Value::Null);
    if state
        .pointer("/custom/imported_from_kimi_cli")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Some(Vec::new()); // The ~/.kimi original is ingested instead.
    }
    let session_id = if agent == "main" {
        base_id
    } else {
        format!("{base_id}:{agent}")
    };
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if let Some(c) = state.get("cwd").and_then(Value::as_str) {
        ctx.insert("cwd".into(), json!(c));
    }
    if let Some(t) = state.get("title").and_then(Value::as_str) {
        ctx.insert("title".into(), json!(t));
    }

    let mut records: Vec<Value> = Vec::new();
    let mut model: Option<String> = None;
    // Index of the open assistant record per step uuid.
    let mut steps: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut saw_step_usage = false;
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let t = v.get("type").and_then(Value::as_str).unwrap_or("");
        let time = v.get("time").cloned();
        match t {
            "llm.request" => {
                if v.get("kind").and_then(Value::as_str) != Some("compaction") {
                    model = v.get("model").and_then(Value::as_str).map(str::to_owned);
                }
            }
            "context.append_message" => {
                let msg = v.get("message").cloned().unwrap_or(Value::Null);
                if msg.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let origin = msg.pointer("/origin/kind").and_then(Value::as_str);
                let injected = matches!(
                    origin,
                    Some("injection" | "system_trigger" | "retry" | "compaction_summary")
                );
                records.push(json!({
                    "kind": if injected { "context" } else { "user" },
                    "content": msg.get("content"),
                    "time": time,
                }));
            }
            "context.append_loop_event" => {
                let ev = v.get("event").cloned().unwrap_or(Value::Null);
                let et = ev.get("type").and_then(Value::as_str).unwrap_or("");
                let step = ev
                    .get("stepUuid")
                    .or_else(|| ev.get("uuid"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let open = |records: &mut Vec<Value>,
                            steps: &mut std::collections::HashMap<String, usize>|
                 -> usize {
                    *steps.entry(step.clone()).or_insert_with(|| {
                        records.push(json!({
                            "kind": "assistant", "blocks": [], "model": model, "time": time,
                        }));
                        records.len() - 1
                    })
                };
                match et {
                    "content.part" => {
                        let i = open(&mut records, &mut steps);
                        if let Some(p) = ev.get("part") {
                            push_block(&mut records[i], part_block(p));
                        }
                    }
                    "tool.call" => {
                        let i = open(&mut records, &mut steps);
                        push_block(
                            &mut records[i],
                            json!({
                                "type": "tool_use",
                                "id": ev.get("toolCallId"),
                                "name": ev.get("name"),
                                "input": ev.get("args").cloned().unwrap_or(json!({})),
                            }),
                        );
                    }
                    "tool.result" => {
                        let r = ev.get("result").cloned().unwrap_or(Value::Null);
                        records.push(json!({
                            "kind": "tool_result",
                            "tool_use_id": ev.get("toolCallId"),
                            "content": output_text(r.get("output")),
                            "is_error": r.get("isError").and_then(Value::as_bool).unwrap_or(false),
                            "time": time,
                        }));
                    }
                    "step.end" => {
                        let i = open(&mut records, &mut steps);
                        if let Some(u) = ev.get("usage") {
                            records[i]["usage"] = u.clone();
                            saw_step_usage = true;
                        }
                        records[i]["finish"] =
                            ev.get("finishReason").cloned().unwrap_or(Value::Null);
                    }
                    _ => {}
                }
            }
            "usage.record" if !saw_step_usage => {
                // Older builds without step-level usage.
                records.push(json!({
                    "kind": "usage", "usage": v.get("usage"), "model": v.get("model"), "time": time,
                }));
            }
            "turn.ended" => {
                if let Some(last) = records.iter_mut().rev().find(|r| r["kind"] == "assistant") {
                    last["turn_end"] = json!(true);
                }
            }
            _ => {}
        }
    }
    Some(vec![finish_doc(session_id, records, ctx)])
}

fn push_block(rec: &mut Value, block: Value) {
    if block.is_null() {
        return;
    }
    if let Some(arr) = rec.get_mut("blocks").and_then(Value::as_array_mut) {
        arr.push(block);
    }
}

/// Kimi content part → Anthropic-style block JSON.
fn part_block(p: &Value) -> Value {
    match p.get("type").and_then(Value::as_str) {
        Some("text") => json!({"type": "text", "text": p.get("text")}),
        Some("think") => {
            if p.get("hidden").and_then(Value::as_bool) == Some(true) {
                Value::Null
            } else {
                json!({"type": "thinking", "thinking": p.get("think")})
            }
        }
        Some("image_url") => json!({"type": "image", "source": p.get("image_url")}),
        _ => Value::Null,
    }
}

fn output_text(o: Option<&Value>) -> String {
    match o {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn finish_doc(session_id: String, records: Vec<Value>, ctx: Map<String, Value>) -> SessionDoc {
    let records = records
        .into_iter()
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                o.insert("_trace".into(), Value::Object(ctx.clone()));
            }
            r
        })
        .collect();
    SessionDoc {
        session_id,
        records,
    }
}

// ---------------------------------------------------------------------------
// Legacy Kimi CLI (~/.kimi)
// ---------------------------------------------------------------------------

fn parse_legacy(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let session_dir = path.parent()?;
    let mut session_id = session_dir.file_name()?.to_str()?.to_owned();
    // Subagents live in <session>/subagents/<agent_id>/wire.jsonl.
    if session_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("subagents")
    {
        let parent = session_dir.parent()?.parent()?.file_name()?.to_str()?;
        session_id = format!("{parent}:{session_id}");
    }
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if let Some(cwd) = legacy_cwd(session_dir) {
        ctx.insert("cwd".into(), json!(cwd));
    }
    if let Some(state) = read_json(&session_dir.join("state.json")) {
        if let Some(t) = state.get("custom_title").and_then(Value::as_str) {
            ctx.insert("title".into(), json!(t));
        }
    }

    let mut records: Vec<Value> = Vec::new();
    let mut open: Option<usize> = None;
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(msg) = v.get("message") else {
            continue;
        };
        let ts = v.get("timestamp").cloned();
        let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "TurnBegin" | "SteerInput" => {
                open = None;
                let content = match payload.get("user_input") {
                    Some(Value::String(s)) => json!([{"type": "text", "text": s}]),
                    Some(other) => other.clone(),
                    None => json!([]),
                };
                records.push(json!({"kind": "user", "content": content, "time": ts}));
            }
            "StepBegin" => open = None,
            "ContentPart" | "ToolCall" => {
                let i = *open.get_or_insert_with(|| {
                    records.push(json!({"kind": "assistant", "blocks": [], "time": ts}));
                    records.len() - 1
                });
                let block = if msg["type"] == "ToolCall" {
                    let f = payload.get("function").cloned().unwrap_or(Value::Null);
                    json!({
                        "type": "tool_use",
                        "id": payload.get("id"),
                        "name": f.get("name"),
                        "input": parse_json_arguments(f.get("arguments")),
                    })
                } else {
                    part_block(&payload)
                };
                push_block(&mut records[i], block);
            }
            "ToolResult" => {
                // Content after a tool result belongs to the next step.
                open = None;
                let rv = payload.get("return_value").cloned().unwrap_or(Value::Null);
                let mut text = output_text(rv.get("output"));
                if text.is_empty() {
                    text = rv
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                }
                records.push(json!({
                    "kind": "tool_result",
                    "tool_use_id": payload.get("tool_call_id"),
                    "content": text,
                    "is_error": rv.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    "time": ts,
                }));
            }
            "StatusUpdate" => {
                if let (Some(i), Some(u)) =
                    (open, payload.get("token_usage").filter(|u| !u.is_null()))
                {
                    records[i]["usage"] = json!({
                        "inputOther": u.get("input_other"),
                        "output": u.get("output"),
                        "inputCacheRead": u.get("input_cache_read"),
                        "inputCacheCreation": u.get("input_cache_creation"),
                    });
                }
            }
            "TurnEnd" => {
                if let Some(last) = records.iter_mut().rev().find(|r| r["kind"] == "assistant") {
                    last["turn_end"] = json!(true);
                }
                open = None;
            }
            _ => {}
        }
    }
    Some(vec![finish_doc(session_id, records, ctx)])
}

/// `sessions/<md5(cwd)>/<uuid>` → the work dir whose MD5 matches, from
/// `~/.kimi/kimi.json`.
fn legacy_cwd(session_dir: &Path) -> Option<String> {
    let bucket = session_dir.parent()?.file_name()?.to_str()?;
    let hash = bucket.rsplit('_').next().unwrap_or(bucket);
    let root = session_dir.parent()?.parent()?.parent()?;
    let meta = read_json(&root.join("kimi.json"))?;
    meta.get("work_dirs")?
        .as_array()?
        .iter()
        .filter_map(|w| w.get("path").and_then(Value::as_str))
        .find(|p| md5_hex(p) == hash)
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Enrichment
// ---------------------------------------------------------------------------

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title"),
        timestamp: any_timestamp(raw.get("time")),
        ..Default::default()
    };
    match raw.get("kind").and_then(Value::as_str).unwrap_or("") {
        "user" | "context" => {
            let blocks =
                crate::message::anthropic_blocks(raw.get("content").unwrap_or(&Value::Null));
            let is_context = raw["kind"] == "context"
                || Message::new(Role::User, blocks.clone())
                    .plain_text()
                    .starts_with("<system");
            e.event_type = if is_context { "system" } else { "user" }.into();
            e.message = Message::new(if is_context { Role::System } else { Role::User }, blocks)
                .non_empty();
            e.summary = summarise_message(&e.event_type, e.message.as_ref(), &[]);
        }
        "assistant" => {
            e.event_type = "assistant".into();
            e.model = raw.get("model").and_then(Value::as_str).map(str::to_owned);
            let blocks =
                crate::message::anthropic_blocks(raw.get("blocks").unwrap_or(&Value::Null));
            for b in &blocks {
                if let Block::ToolUse { name, .. } = b {
                    e.tool_uses.push(name.clone());
                }
            }
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.summary = summarise_message("assistant", e.message.as_ref(), &e.tool_uses);
            e.usage = raw.get("usage").and_then(usage);
            if let Some(u) = &e.usage {
                e.cost_usd = Some(estimate_cost_for(AgentSource::Kimi, e.model.as_deref(), u));
            }
            e.turn_end = raw.get("turn_end").and_then(Value::as_bool) == Some(true);
        }
        "tool_result" => {
            e.event_type = "tool_result".into();
            let id = raw
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            e.tool_results.push(id.clone());
            e.message = Message::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content: raw.get("content").cloned().unwrap_or(Value::Null),
                    is_error: raw
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }],
            )
            .non_empty();
            e.summary = "📦 Tool result".into();
        }
        "usage" => {
            e.event_type = "system".into();
            e.model = raw.get("model").and_then(Value::as_str).map(str::to_owned);
            e.usage = raw.get("usage").and_then(usage);
            if let Some(u) = &e.usage {
                e.cost_usd = Some(estimate_cost_for(AgentSource::Kimi, e.model.as_deref(), u));
            }
            e.summary = "⚙️  Usage".into();
        }
        other => {
            e.event_type = "system".into();
            e.summary = format!("⚙️  {other}");
        }
    }
    e
}

/// Kimi usage: `inputOther` excludes cache reads and writes.
fn usage(u: &Value) -> Option<TokenUsage> {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let t = TokenUsage {
        input: g("inputOther"),
        output: g("output"),
        cache_read: g("inputCacheRead"),
        cache_creation: g("inputCacheCreation"),
    };
    (t.input + t.output + t.cache_read + t.cache_creation > 0).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = r#"{"type":"metadata","protocol_version":"1.5","created_at":1790000000000}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"fix the failing test"}],"origin":{"kind":"user"}},"time":1790000001001}
{"type":"llm.request","agentId":"main","kind":"loop","provider":"kimi","model":"kimi-for-coding","time":1790000001010}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.begin","uuid":"s1","turnId":"0","step":1},"time":1790000001011}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"content.part","uuid":"c1","turnId":"0","step":1,"stepUuid":"s1","part":{"type":"think","think":"Need to run tests first."}},"time":1790000002000}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"tool.call","uuid":"t1","turnId":"0","step":1,"stepUuid":"s1","toolCallId":"Bash:0","name":"Bash","args":{"command":"cargo test"}},"time":1790000002001}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"tool.result","parentUuid":"t1","toolCallId":"Bash:0","result":{"output":"test result: FAILED","isError":false}},"time":1790000007400}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"step.end","uuid":"s1","turnId":"0","step":1,"finishReason":"tool_calls","usage":{"inputOther":1840,"output":96,"inputCacheRead":12288,"inputCacheCreation":0}},"time":1790000007401}
{"type":"usage.record","agentId":"main","model":"kimi-for-coding","usage":{"inputOther":1840,"output":96,"inputCacheRead":12288,"inputCacheCreation":0},"time":1790000007402}
{"type":"turn.ended","agentId":"main","turnId":0,"reason":"completed","time":1790000010000}
"#;

    #[test]
    fn kimi_code_groups_steps_and_counts_usage_once() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join("sessions/wd_app_abc/session_1");
        std::fs::create_dir_all(sdir.join("agents/main")).unwrap();
        std::fs::write(
            sdir.join("state.json"),
            r#"{"id":"session_1","cwd":"/work/app","title":"Fix tests"}"#,
        )
        .unwrap();
        let p = sdir.join("agents/main/wire.jsonl");
        let d = parse_document(&p, CODE).unwrap().remove(0);
        assert_eq!(d.session_id, "session_1");
        // user, assistant step, tool result (usage.record ignored)
        assert_eq!(d.records.len(), 3);
        let a = enrich(&d.records[1]);
        assert_eq!(a.model.as_deref(), Some("kimi-for-coding"));
        assert_eq!(a.tool_uses, vec!["Bash"]);
        assert_eq!(a.usage.as_ref().unwrap().cache_read, 12288);
        assert!(a.turn_end);
        assert_eq!(a.cwd.as_deref(), Some("/work/app"));
        let r = enrich(&d.records[2]);
        assert_eq!(r.tool_results, vec!["Bash:0"]);
    }

    #[test]
    fn imported_sessions_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join("sessions/wd/ses_x");
        std::fs::create_dir_all(sdir.join("agents/main")).unwrap();
        std::fs::write(
            sdir.join("state.json"),
            r#"{"custom":{"imported_from_kimi_cli":true}}"#,
        )
        .unwrap();
        assert!(parse_document(&sdir.join("agents/main/wire.jsonl"), CODE)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn legacy_wire_with_md5_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kimi");
        let bucket = md5_hex("/work/golden-proj");
        let sdir = root.join("sessions").join(&bucket).join("11111111-aaaa");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            root.join("kimi.json"),
            r#"{"work_dirs":[{"path":"/work/golden-proj","kaos":"local"}]}"#,
        )
        .unwrap();
        let body = r#"{"type": "metadata", "protocol_version": "1.8"}
{"timestamp": 1774949785.23238, "message": {"type": "TurnBegin", "payload": {"user_input": "run echo hi"}}}
{"timestamp": 1774949785.233404, "message": {"type": "StepBegin", "payload": {"n": 1}}}
{"timestamp": 1774949785.2350621, "message": {"type": "ToolCall", "payload": {"type": "function", "id": "tc1", "function": {"name": "Shell", "arguments": "{\"command\": \"echo hi\"}"}, "extras": null}}}
{"timestamp": 1774949785.2407029, "message": {"type": "StatusUpdate", "payload": {"token_usage": {"input_other": 10, "output": 2, "input_cache_read": 0, "input_cache_creation": 0}}}}
{"timestamp": 1774949785.877887, "message": {"type": "ToolResult", "payload": {"tool_call_id": "tc1", "return_value": {"is_error": true, "output": "", "message": "Shell blocked by hook"}}}}
{"timestamp": 1774949785.8802822, "message": {"type": "ContentPart", "payload": {"type": "text", "text": "OK, shell was blocked."}}}
{"timestamp": 1776961639.3107672, "message": {"type": "TurnEnd", "payload": {}}}
"#;
        let d = parse_document(&sdir.join("wire.jsonl"), body)
            .unwrap()
            .remove(0);
        assert_eq!(d.session_id, "11111111-aaaa");
        let recs: Vec<Enrichment> = d.records.iter().map(enrich).collect();
        assert_eq!(recs[0].event_type, "user");
        assert_eq!(recs[0].cwd.as_deref(), Some("/work/golden-proj"));
        assert_eq!(recs[1].tool_uses, vec!["Shell"]);
        assert_eq!(recs[1].usage.as_ref().unwrap().input, 10);
        assert!(
            recs[2]
                .message
                .as_ref()
                .unwrap()
                .tool_results()
                .next()
                .unwrap()
                .2
        );
        assert!(recs[3].turn_end);
        assert!(recs[0]
            .timestamp
            .as_deref()
            .unwrap()
            .starts_with("2026-03-31"));
    }
}
