//! OpenAI Codex CLI adapter.
//!
//! Codex writes one "rollout" JSONL per thread to
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<local ts>-<thread uuid>.jsonl`
//! (`~/.codex` by default) and moves archived threads, flat, into
//! `archived_sessions/`. Resuming appends to the same file.
//!
//! Since v0.32 each line is `{timestamp, ordinal?, type, payload}`:
//! - `session_meta` — thread identity, `cwd`, `git`, `cli_version`;
//! - `turn_context` — per-turn `model` and `cwd`;
//! - `response_item` — the conversation itself, written in every history
//!   mode: `message` (user / assistant / developer), `reasoning`,
//!   `function_call`, `custom_tool_call`, `local_shell_call`,
//!   `web_search_call` and their `*_output`s;
//! - `event_msg` — UI events. In legacy mode `user_message` /
//!   `agent_message` / `agent_reasoning` echo the response items; in
//!   paginated mode (v0.147+) `item_completed` does. Both are kept as
//!   events but never as transcript messages, so nothing is duplicated.
//!   `token_count` and `task_complete` matter for usage and turn ends;
//! - `token_usage_record` (v0.153+) — exact per-response usage.
//!
//! Up to v0.31 files had no envelope: a bare metadata line followed by bare
//! response items. Both are handled.
//!
//! Usage: `token_usage_record` and `token_count` both report the thread's
//! cumulative total alongside the per-response delta; the delta is counted
//! once, keyed on that cumulative total. Cached input is a subset of input.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{parse_json_arguments, Block, Message, Role};
use crate::sources::{
    env_path, estimate_cost_for, summarise_message, truncate, AgentSource, Enrichment, FileKind,
};

pub fn codex_home(home: &Path) -> PathBuf {
    env_path("CODEX_HOME").unwrap_or_else(|| home.join(".codex"))
}

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let h = codex_home(home);
    vec![h.join("sessions"), h.join("archived_sessions")]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    (name.starts_with("rollout-") && name.ends_with(".jsonl")).then_some(FileKind::Jsonl)
}

pub fn looks_like_rollout(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}

pub fn skip_dir(_dir: &Path) -> bool {
    false
}

/// Carry state across a rollout file's records: remember the model and cwd
/// from `turn_context` / `thread_settings_applied` and attach the model to
/// usage records, which do not name it.
pub fn annotate(carry: &mut Map<String, Value>, rec: &mut Value) {
    let t = rec.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = rec.get("payload").cloned().unwrap_or(Value::Null);
    match t {
        "turn_context" => {
            if let Some(m) = payload.get("model") {
                carry.insert("model".into(), m.clone());
            }
        }
        "event_msg"
            if payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied") =>
        {
            if let Some(m) = payload.pointer("/thread_settings/model") {
                carry.insert("model".into(), m.clone());
            }
        }
        _ => {}
    }
    let needs_model = t == "token_usage_record"
        || (t == "event_msg" && payload.get("type").and_then(Value::as_str) == Some("token_count"));
    if needs_model {
        if let (Some(m), Some(obj)) = (carry.get("model").cloned(), rec.as_object_mut()) {
            obj.insert("_trace".into(), json!({ "model": m }));
        }
    }
}

/// Normalise one Codex rollout record.
pub fn enrich(raw: &Value) -> Enrichment {
    let record_type = raw.get("type").and_then(Value::as_str).unwrap_or("");
    let mut e = Enrichment {
        timestamp: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };

    // Pre-v0.32 rollouts: no envelope.
    if raw.get("payload").is_none() {
        if raw.get("record_type").is_some() {
            e.event_type = "system".into();
            e.summary = "⚙️  State".into();
            return finish(e);
        }
        if raw.get("instructions").is_some() || (raw.get("id").is_some() && record_type.is_empty())
        {
            e.event_type = "system".into();
            e.git_branch = raw
                .pointer("/git/branch")
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.summary = "⚙️  Session start".into();
            return finish(e);
        }
        enrich_response_item(raw, &mut e);
        return finish(e);
    }

    let payload = raw.get("payload").cloned().unwrap_or(Value::Null);
    let s = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_owned);
    match record_type {
        "session_meta" => {
            e.event_type = "system".into();
            e.cwd = s("cwd");
            e.version = s("cli_version");
            e.git_branch = payload
                .pointer("/git/branch")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let origin = s("originator").unwrap_or_default();
            e.summary = format!(
                "⚙️  Session start {} {}",
                truncate(e.cwd.as_deref().unwrap_or(""), 70),
                origin
            )
            .trim_end()
            .to_owned();
        }
        "turn_context" => {
            e.event_type = "system".into();
            e.cwd = s("cwd");
            e.model = s("model");
            e.git_branch = payload
                .get("git_branch")
                .or_else(|| payload.pointer("/git/branch"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.summary = format!(
                "⚙️  Turn context: {} · {}",
                e.model.as_deref().unwrap_or("?"),
                truncate(e.cwd.as_deref().unwrap_or(""), 60)
            );
        }
        "response_item" => enrich_response_item(&payload, &mut e),
        "event_msg" => {
            enrich_event_msg(&payload, &mut e);
            if e.usage.is_some() && e.model.is_none() {
                e.model = raw
                    .pointer("/_trace/model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        "token_usage_record" => {
            e.event_type = "system".into();
            e.model = raw
                .pointer("/_trace/model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.usage = payload.get("usage").and_then(parse_usage);
            e.usage_key = cumulative_key(payload.get("thread_token_usage"))
                .or_else(|| s("response_id").map(|r| format!("codex-resp:{r}")));
            e.summary = usage_summary(e.usage.as_ref());
        }
        "compacted" => {
            e.event_type = "summary".into();
            let msg = s("message").unwrap_or_default();
            e.summary = format!("📝 Compacted: {}", truncate(&msg, 100));
        }
        "world_state"
        | "retained_context"
        | "security_risk_score"
        | "realtime_item"
        | "inter_agent_communication"
        | "inter_agent_communication_metadata" => {
            e.event_type = "system".into();
            e.summary = format!("⚙️  {}", record_type.replace('_', " "));
        }
        _ => {
            e.event_type = if record_type.is_empty() {
                "unknown".into()
            } else {
                record_type.into()
            };
            e.summary = format!("❓ {}", e.event_type);
        }
    }
    finish(e)
}

fn finish(mut e: Enrichment) -> Enrichment {
    if e.cost_usd.is_none() {
        if let Some(u) = &e.usage {
            e.cost_usd = Some(estimate_cost_for(AgentSource::Codex, e.model.as_deref(), u));
        }
    }
    e
}

/// Text prefixes of user-role messages that Codex injects as context.
const CONTEXT_PREFIXES: &[&str] = &[
    "<environment_context",
    "<user_instructions",
    "# AGENTS.md instructions",
    "<INSTRUCTIONS>",
    "<user_shell_command",
    "<turn_aborted",
    "<subagent_notification",
    "<skill",
    "<goal_context",
    "<external_",
    "<codex_internal_context",
    "<permissions instructions",
];

fn enrich_response_item(payload: &Value, e: &mut Enrichment) {
    let s = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    match payload.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let role = s("role");
            let blocks =
                crate::message::anthropic_blocks(payload.get("content").unwrap_or(&Value::Null));
            let text = Message::new(Role::User, blocks.clone()).plain_text();
            let kinds = payload
                .pointer("/internal_chat_message_metadata_passthrough/content_item_kinds")
                .and_then(Value::as_array);
            let tagged_context = kinds.is_some_and(|k| {
                k.iter()
                    .filter_map(Value::as_str)
                    .any(|x| x != "user.text" && x != "unknown")
            });
            match role.as_str() {
                "user"
                    if tagged_context
                        || CONTEXT_PREFIXES
                            .iter()
                            .any(|p| text.trim_start().starts_with(p)) =>
                {
                    e.event_type = "system".into();
                    e.message = Message::new(Role::System, blocks).non_empty();
                    e.summary = format!("⚙️  Context: {}", truncate(&text, 90));
                }
                "user" => {
                    e.event_type = "user".into();
                    e.message = Message::new(Role::User, blocks).non_empty();
                    e.summary = summarise_message("user", e.message.as_ref(), &[]);
                }
                "developer" | "system" => {
                    e.event_type = "system".into();
                    e.message = Message::new(Role::System, blocks).non_empty();
                    e.summary = format!("⚙️  Instructions: {}", truncate(&text, 90));
                }
                _ => {
                    e.event_type = "assistant".into();
                    e.message = Message::new(Role::Assistant, blocks).non_empty();
                    e.summary = summarise_message("assistant", e.message.as_ref(), &[]);
                    e.turn_end =
                        payload.get("phase").and_then(Value::as_str) == Some("final_answer");
                }
            }
        }
        "reasoning" => {
            e.event_type = "assistant".into();
            let mut parts: Vec<String> = Vec::new();
            for key in ["summary", "content"] {
                for b in payload
                    .get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        if !t.is_empty() {
                            parts.push(t.to_owned());
                        }
                    }
                }
            }
            let thinking = parts.join("\n\n");
            e.summary = if thinking.is_empty() {
                "💭 Reasoning".into()
            } else {
                format!("💭 {}", truncate(&thinking, 100))
            };
            e.message = (!thinking.is_empty())
                .then(|| Message::new(Role::Assistant, vec![Block::Thinking { thinking }]));
        }
        "function_call" | "custom_tool_call" | "tool_search_call" => {
            e.event_type = "tool_use".into();
            let ns = s("namespace");
            let name = if ns.is_empty() {
                s("name")
            } else {
                format!("{ns}__{}", s("name"))
            };
            let name = if name.is_empty() {
                "tool_search".into()
            } else {
                name
            };
            let input = match payload.get("input") {
                Some(Value::String(raw)) => json!({ "input": raw }),
                Some(other) => other.clone(),
                None => parse_json_arguments(payload.get("arguments")),
            };
            e.tool_uses.push(name.clone());
            e.message = Message::new(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: call_id(payload),
                    name: name.clone(),
                    input,
                }],
            )
            .non_empty();
            e.summary = format!("🔧 {name}");
        }
        "local_shell_call" => {
            e.event_type = "tool_use".into();
            e.tool_uses.push("local_shell".into());
            let cmd = payload
                .pointer("/action/command")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            e.message = Message::new(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: call_id(payload),
                    name: "local_shell".into(),
                    input: payload.get("action").cloned().unwrap_or_else(|| json!({})),
                }],
            )
            .non_empty();
            e.summary = format!("🔧 local_shell: {}", truncate(&cmd, 90));
        }
        "web_search_call" => {
            e.event_type = "tool_use".into();
            e.tool_uses.push("web_search".into());
            let q = payload
                .pointer("/action/query")
                .or_else(|| payload.pointer("/action/url"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            e.message = Message::new(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: payload
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    name: "web_search".into(),
                    input: payload.get("action").cloned().unwrap_or_else(|| json!({})),
                }],
            )
            .non_empty();
            e.summary = format!("🔎 {}", truncate(&q, 100));
        }
        "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
            e.event_type = "tool_result".into();
            let id = call_id(payload);
            let text = output_text(payload.get("output").or_else(|| payload.get("tools")));
            let is_error = looks_failed(&text);
            e.tool_results.push(id.clone());
            e.message = Message::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content: Value::String(text.clone()),
                    is_error,
                }],
            )
            .non_empty();
            e.summary = format!("📦 {}", truncate(&text, 100));
        }
        "compaction" | "compaction_summary" | "context_compaction" => {
            e.event_type = "summary".into();
            e.summary = "📝 Context compacted".into();
        }
        "agent_message" => {
            e.event_type = "system".into();
            e.summary = format!("🤝 Agent message from {}", s("author"));
        }
        other => {
            e.event_type = "system".into();
            e.summary = format!("⚙️  Item: {other}");
        }
    }
}

fn call_id(p: &Value) -> String {
    p.get("call_id")
        .or_else(|| p.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Tool output: a string or an array of content items.
fn output_text(o: Option<&Value>) -> String {
    match o {
        Some(Value::String(s)) => {
            // ≤ ~0.5x: a JSON-encoded {"output": …, "metadata": …}.
            if s.starts_with('{') {
                if let Ok(v) = serde_json::from_str::<Value>(s) {
                    if let Some(out) = v.get("output").and_then(Value::as_str) {
                        return out.to_owned();
                    }
                }
            }
            s.clone()
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| match i.get("text").and_then(Value::as_str) {
                Some(t) => t.to_owned(),
                None => format!(
                    "[{}]",
                    i.get("type").and_then(Value::as_str).unwrap_or("item")
                ),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Exit-code lines in Codex's exec output formats.
fn looks_failed(text: &str) -> bool {
    for line in text.lines().take(6) {
        let code = line
            .strip_prefix("Process exited with code ")
            .or_else(|| line.strip_prefix("Exit code: "));
        if let Some(c) = code {
            return c.trim() != "0";
        }
    }
    false
}

fn enrich_event_msg(payload: &Value, e: &mut Enrichment) {
    let s = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    e.event_type = "system".into();
    match kind {
        // Echoes of response items (legacy mode): keep as UI events only.
        "user_message" => e.summary = format!("💬 {}", truncate(&s("message"), 110)),
        "agent_message" => e.summary = format!("💬 {}", truncate(&s("message"), 110)),
        "agent_reasoning" | "agent_reasoning_raw_content" => {
            e.summary = format!("💭 {}", truncate(&s("text"), 110))
        }
        "item_completed" => {
            let item = payload.get("item").cloned().unwrap_or(Value::Null);
            let it = item.get("type").and_then(Value::as_str).unwrap_or("");
            e.summary = match it {
                "CommandExecution" => {
                    let cmd = item
                        .get("command")
                        .and_then(Value::as_array)
                        .and_then(|a| a.last())
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let code = item.get("exit_code").and_then(Value::as_i64);
                    format!(
                        "▶️  {} → exit {}",
                        truncate(cmd, 80),
                        code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                    )
                }
                "FileChange" => "✏️  File change".into(),
                "McpToolCall" => format!(
                    "🔌 {}.{}",
                    item.get("server").and_then(Value::as_str).unwrap_or("?"),
                    item.get("tool").and_then(Value::as_str).unwrap_or("?")
                ),
                other => format!("💬 {other}"),
            };
        }
        "token_count" => {
            let info = payload.get("info").cloned().unwrap_or(Value::Null);
            e.usage = info
                .get("last_token_usage")
                .and_then(parse_usage)
                .or_else(|| info.get("total_token_usage").and_then(parse_usage));
            // The same response is also reported by token_usage_record, and
            // token_count repeats on rate-limit refreshes: both carry the
            // thread's cumulative total, which identifies the response.
            e.usage_key = cumulative_key(info.get("total_token_usage"));
            e.summary = usage_summary(e.usage.as_ref());
        }
        "task_started" | "turn_started" => e.summary = "▶️  Turn started".into(),
        "task_complete" | "turn_complete" => {
            e.turn_end = true;
            e.summary = match payload.pointer("/error/message").and_then(Value::as_str) {
                Some(err) => {
                    e.event_type = "error".into();
                    format!("⛔ {}", truncate(err, 100))
                }
                None => "✅ Turn complete".into(),
            };
        }
        "turn_aborted" => {
            e.turn_end = true;
            e.summary = format!("⏹  Turn aborted ({})", s("reason"));
        }
        "thread_settings_applied" => {
            e.model = payload
                .pointer("/thread_settings/model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.cwd = payload
                .pointer("/thread_settings/cwd")
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.summary = format!("⚙️  Settings: {}", e.model.clone().unwrap_or_default());
        }
        "thread_rolled_back" => e.summary = "↩️  Rolled back".into(),
        other => e.summary = format!("⚙️  {}", other.replace('_', " ")),
    }
}

/// A stable key from the thread's cumulative usage. Only unique within the
/// thread, so it is marked for the engine to scope to the session.
fn cumulative_key(total: Option<&Value>) -> Option<String> {
    let t = total?;
    let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    (g("total_tokens") + g("input_tokens") + g("output_tokens") > 0).then(|| {
        format!(
            "{}codex-total:{}:{}:{}:{}",
            crate::sources::SESSION_SCOPED,
            g("input_tokens"),
            g("cached_input_tokens"),
            g("output_tokens"),
            g("total_tokens")
        )
    })
}

/// Codex `TokenUsage`: cached (and cache-write) input are subsets of
/// `input_tokens`; reasoning is a subset of `output_tokens`.
fn parse_usage(u: &Value) -> Option<TokenUsage> {
    let g = |a: &str, b: &str| {
        u.get(a)
            .or_else(|| u.get(b))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let input = g("input_tokens", "prompt_tokens");
    let cached = g("cached_input_tokens", "cache_read_input_tokens");
    let write = g("cache_write_input_tokens", "cache_creation_input_tokens");
    let output = g("output_tokens", "completion_tokens");
    if input == 0 && output == 0 {
        return None;
    }
    Some(TokenUsage {
        input: input.saturating_sub(cached + write),
        output,
        cache_read: cached,
        cache_creation: write,
    })
}

fn usage_summary(u: Option<&TokenUsage>) -> String {
    match u {
        Some(u) => format!(
            "⚙️  Tokens: {} in · {} cached · {} out",
            u.input, u.cache_read, u.output
        ),
        None => "⚙️  Token count".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_meta_and_turn_context() {
        let m = enrich(
            &json!({"timestamp":"t","type":"session_meta","payload":{"id":"u1","cwd":"/work/demo","cli_version":"0.159.1","git":{"branch":"master"}}}),
        );
        assert_eq!(m.cwd.as_deref(), Some("/work/demo"));
        assert_eq!(m.version.as_deref(), Some("0.159.1"));
        assert_eq!(m.git_branch.as_deref(), Some("master"));
        let t = enrich(
            &json!({"type":"turn_context","payload":{"cwd":"/home/me/proj","model":"gpt-5-codex"}}),
        );
        assert_eq!(t.model.as_deref(), Some("gpt-5-codex"));
    }

    #[test]
    fn user_message_and_injected_context() {
        let e = enrich(
            &json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix the bug"}]}}),
        );
        assert_eq!(e.event_type, "user");
        assert_eq!(e.message.unwrap().plain_text(), "fix the bug");
        let c = enrich(
            &json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/x</cwd>\n</environment_context>"}]}}),
        );
        assert_eq!(c.event_type, "system");
        assert_eq!(c.message.unwrap().role, Role::System);
        let d = enrich(
            &json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>"}]}}),
        );
        assert_eq!(d.message.unwrap().role, Role::System);
    }

    #[test]
    fn tool_calls_and_outputs() {
        let f = enrich(
            &json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\": \"echo hi\"}","call_id":"c1"}}),
        );
        assert_eq!(f.event_type, "tool_use");
        assert_eq!(f.tool_uses, vec!["exec_command"]);
        let m = f.message.unwrap();
        assert_eq!(m.tool_uses().next().unwrap().2["cmd"], "echo hi");
        let o = enrich(
            &json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Chunk ID: c5\nWall time: 0.0000 seconds\nProcess exited with code 2\nOutput:\nboom\n"}}),
        );
        assert_eq!(o.tool_results, vec!["c1"]);
        assert!(
            o.message.unwrap().tool_results().next().unwrap().2,
            "non-zero exit is an error"
        );
        let p = enrich(
            &json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"c4","name":"apply_patch","input":"*** Begin Patch\n*** End Patch\n"}}),
        );
        assert_eq!(p.tool_uses, vec!["apply_patch"]);
        let l = enrich(
            &json!({"type":"response_item","payload":{"type":"local_shell_call","call_id":"c5","status":"completed","action":{"type":"exec","command":["ls","-la"]}}}),
        );
        assert!(l.summary.contains("ls -la"));
        let arr = enrich(
            &json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c3","output":[{"type":"input_text","text":"see image"},{"type":"input_image","image_url":"data:"}]}}),
        );
        assert!(arr.summary.contains("see image"));
    }

    #[test]
    fn echoes_are_not_transcript_messages() {
        for p in [
            json!({"type":"user_message","message":"fix the bug"}),
            json!({"type":"agent_message","message":"Done."}),
            json!({"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"x"}]}}),
        ] {
            let e = enrich(&json!({"type":"event_msg","payload":p}));
            assert!(e.message.is_none());
            assert_eq!(e.event_type, "system");
        }
    }

    #[test]
    fn usage_deltas_and_shared_key() {
        let tc = enrich(
            &json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":10000,"cached_input_tokens":8192,"output_tokens":240,"total_tokens":10240},
            "last_token_usage":{"input_tokens":5000,"cached_input_tokens":4096,"output_tokens":120,"total_tokens":5120}}}}),
        );
        let u = tc.usage.clone().unwrap();
        assert_eq!(u.input, 904);
        assert_eq!(u.cache_read, 4096);
        assert_eq!(u.output, 120);
        let rec = enrich(
            &json!({"type":"token_usage_record","payload":{"response_id":"r2",
            "usage":{"input_tokens":5000,"cached_input_tokens":4096,"output_tokens":120,"total_tokens":5120},
            "thread_token_usage":{"input_tokens":10000,"cached_input_tokens":8192,"output_tokens":240,"total_tokens":10240}}}),
        );
        assert_eq!(
            rec.usage_key, tc.usage_key,
            "both describe the same response"
        );
        assert!(tc.usage_key.is_some());
    }

    #[test]
    fn unnamed_model_is_priced_as_gpt() {
        let e = enrich(
            &json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":1_000_000,"output_tokens":0},
            "total_token_usage":{"input_tokens":1_000_000,"output_tokens":0}}}}),
        );
        assert!((e.cost_usd.unwrap() - 1.25).abs() < 0.001);
    }

    #[test]
    fn annotate_carries_model_to_usage() {
        let mut carry = Map::new();
        let mut tc = json!({"type":"turn_context","payload":{"model":"o4-mini","cwd":"/x"}});
        annotate(&mut carry, &mut tc);
        let mut usage = json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":1_000_000,"output_tokens":0},
            "total_token_usage":{"input_tokens":1_000_000,"output_tokens":0}}}});
        annotate(&mut carry, &mut usage);
        assert_eq!(usage["_trace"]["model"], "o4-mini");
        let e = enrich(&usage);
        assert_eq!(e.model.as_deref(), Some("o4-mini"));
        assert!((e.cost_usd.unwrap() - 1.10).abs() < 0.001);
    }

    #[test]
    fn pre_envelope_rollouts() {
        let meta = enrich(
            &json!({"id":"abc","timestamp":"2025-08-01T00:00:00Z","instructions":null,"git":{"branch":"main"}}),
        );
        assert_eq!(meta.event_type, "system");
        let msg = enrich(
            &json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}),
        );
        assert_eq!(msg.event_type, "assistant");
        assert_eq!(enrich(&json!({"record_type":"state"})).event_type, "system");
    }

    #[test]
    fn task_complete_marks_turn_end() {
        let e = enrich(
            &json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":"t","last_agent_message":"Done"}}),
        );
        assert!(e.turn_end);
    }
}
