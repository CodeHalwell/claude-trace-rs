//! Google Gemini CLI adapter (and Qwen Code up to v0.3, which kept Gemini's
//! format with `type: "qwen"` for model turns).
//!
//! Sessions live in `~/.gemini/tmp/<project>/chats/` (`GEMINI_CLI_HOME`
//! replaces the home directory):
//!
//! - **v0.39+**: `session-<ts>-<sid8>.jsonl`, an append-only *patch log*. The
//!   first line is a header (`sessionId`, `projectHash`, `startTime`); message
//!   lines are upserted by `id` (a message is re-appended whenever tokens or
//!   tool results are added); `{"$set": {...}}` patches metadata and, when it
//!   carries `messages`, replaces the whole history; `{"$rewindTo": id}`
//!   truncates it.
//! - **≤ v0.38**: `session-<ts>-<sid8>.json`, the whole `ConversationRecord`
//!   rewritten on every change.
//!
//! Both are therefore ingested as *documents*: the file is replayed into the
//! current message list and diffed against the previous parse, so a message
//! that gains tool calls or token counts is updated in place.
//!
//! The project directory is a slug (v0.29+) with the absolute path in
//! `tmp/<slug>/.project_root`, or `sha256(cwd)` before that; the cwd is read
//! from `.project_root` when present. Git branch is not recorded.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{estimate_cost_for, truncate, AgentSource, Enrichment, SessionDoc};

pub fn home_dir() -> Option<PathBuf> {
    let base = std::env::var_os("GEMINI_CLI_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()))?;
    Some(base.join(".gemini"))
}

pub fn default_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(h) = home_dir() {
        v.push(h.join("tmp"));
    }
    // macOS Seatbelt sandbox relocates runtime state.
    if let Some(d) = directories::BaseDirs::new() {
        v.push(d.home_dir().join(".cache/.gemini/tmp"));
    }
    v
}

/// Session files: `chats/session-*.json[l]` (and subagent logs nested under
/// `chats/<parent>/`). Everything else under `tmp/` (logs.json, checkpoints,
/// tool outputs, shell history) is ignored.
pub fn matches_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let in_chats = path
        .ancestors()
        .skip(1)
        .take(3)
        .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("chats"));
    in_chats
        && (name.ends_with(".jsonl") || name.ends_with(".json"))
        && !name.contains(".tmp-")
        && !name.contains(".unreadable-")
}

/// A Gemini CLI session file by name alone (`chats/session-*.json[l]`), for
/// roots that were not recognised as a Gemini directory.
pub fn is_session_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("session-") && matches_file(path)
}

pub fn skip_dir(dir: &Path) -> bool {
    matches!(
        dir.file_name().and_then(|n| n.to_str()),
        Some("tool-outputs" | "checkpoints" | "history" | "memory" | "logs" | "plans" | "tasks")
    )
}

/// Replay a session file into its current message list.
pub fn parse_document(path: &Path, body: &str, qwen: bool) -> Option<Vec<SessionDoc>> {
    let is_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
    if !is_jsonl {
        // A legacy .json that was migrated on resume has a .jsonl twin with
        // the same session; the twin is authoritative.
        if path.with_extension("jsonl").exists() {
            return Some(Vec::new());
        }
    }
    let (header, messages) = if is_jsonl && !body.trim_start().starts_with("{\n") {
        replay_jsonl(body)
    } else {
        let v: Value = serde_json::from_str(body).ok()?;
        let msgs = v
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        (v, msgs)
    };
    let session_id = header
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))?;
    let cwd = project_root(path).or_else(|| {
        header
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let summary = header.get("summary").and_then(Value::as_str);

    let records = messages
        .into_iter()
        .filter_map(|m| {
            let Value::Object(mut obj) = m else {
                return None;
            };
            // Context the per-record adapter needs, namespaced so it cannot
            // collide with the agent's own fields.
            let mut ctx = Map::new();
            ctx.insert("sessionId".into(), json!(session_id));
            if let Some(c) = &cwd {
                ctx.insert("cwd".into(), json!(c));
            }
            if let Some(s) = summary {
                ctx.insert("summary".into(), json!(s));
            }
            if qwen {
                ctx.insert("agent".into(), json!("qwen"));
            }
            obj.insert("_trace".into(), Value::Object(ctx));
            Some(Value::Object(obj))
        })
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

/// Apply the v0.39+ patch log: header, upserts by id, `$set`, `$rewindTo`.
fn replay_jsonl(body: &str) -> (Value, Vec<Value>) {
    let mut header = Map::new();
    let mut order: Vec<String> = Vec::new();
    let mut by_id: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // partial trailing write
        };
        if let Some(target) = v.get("$rewindTo").and_then(Value::as_str) {
            match order.iter().position(|id| id == target) {
                Some(pos) => {
                    for id in order.drain(pos..) {
                        by_id.remove(&id);
                    }
                }
                None => {
                    order.clear();
                    by_id.clear();
                }
            }
            continue;
        }
        if let Some(set) = v.get("$set").and_then(Value::as_object) {
            for (k, val) in set {
                if k == "messages" {
                    if let Some(arr) = val.as_array() {
                        order.clear();
                        by_id.clear();
                        for m in arr {
                            if let Some(id) = m.get("id").and_then(Value::as_str) {
                                order.push(id.to_owned());
                                by_id.insert(id.to_owned(), m.clone());
                            }
                        }
                    }
                } else {
                    header.insert(k.clone(), val.clone());
                }
            }
            continue;
        }
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            if !by_id.contains_key(id) {
                order.push(id.to_owned());
            }
            by_id.insert(id.to_owned(), v);
            continue;
        }
        if let Some(obj) = v.as_object() {
            if obj.contains_key("sessionId") {
                // Header (or a legacy single-line record with inline messages).
                for (k, val) in obj {
                    if k == "messages" {
                        if let Some(arr) = val.as_array() {
                            for m in arr {
                                if let Some(id) = m.get("id").and_then(Value::as_str) {
                                    if !by_id.contains_key(id) {
                                        order.push(id.to_owned());
                                    }
                                    by_id.insert(id.to_owned(), m.clone());
                                }
                            }
                        }
                    } else {
                        header.insert(k.clone(), val.clone());
                    }
                }
            }
        }
    }
    let messages = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    (Value::Object(header), messages)
}

/// `tmp/<slug>/.project_root` holds the absolute project path (v0.29+).
fn project_root(path: &Path) -> Option<String> {
    for dir in path.ancestors().skip(1).take(4) {
        let marker = dir.join(".project_root");
        if let Ok(s) = std::fs::read_to_string(&marker) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    None
}

/// Normalise one Gemini message record (with its `_trace` context).
pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let source = if ctx.get("agent").and_then(Value::as_str) == Some("qwen") {
        AgentSource::Qwen
    } else {
        AgentSource::Gemini
    };
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
    let mut e = Enrichment {
        session_id: ctx
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd: ctx.get("cwd").and_then(Value::as_str).map(str::to_owned),
        timestamp: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: ctx
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    let content = raw.get("content").cloned().unwrap_or(Value::Null);

    match kind {
        "user" => {
            let (text_blocks, results) = parts_to_blocks(&content, Role::User);
            let only_results = text_blocks.is_empty() && !results.is_empty();
            if only_results {
                // v0.44+ records each tool-response round as a synthetic user
                // message; the same results are on the gemini turn's
                // toolCalls, which is where we take them from.
                e.event_type = "tool_result".into();
                e.summary = format!("📦 Tool result ×{}", results.len());
            } else {
                e.event_type = "user".into();
                let text = Message::new(Role::User, text_blocks.clone()).plain_text();
                e.summary = if text.is_empty() {
                    "👤 User".into()
                } else {
                    format!("👤 {}", truncate(&text, 120))
                };
                let is_context =
                    text.starts_with("<session_context>") || text.starts_with("<hook_context>");
                e.message = if is_context {
                    Message::new(Role::System, text_blocks).non_empty()
                } else {
                    Message::new(Role::User, text_blocks).non_empty()
                };
            }
        }
        "gemini" | "qwen" | "model" => {
            e.event_type = "assistant".into();
            e.model = raw.get("model").and_then(Value::as_str).map(str::to_owned);
            let mut blocks: Vec<Block> = Vec::new();
            if let Some(thoughts) = raw.get("thoughts").and_then(Value::as_array) {
                for t in thoughts {
                    let subject = t.get("subject").and_then(Value::as_str).unwrap_or("");
                    let desc = t.get("description").and_then(Value::as_str).unwrap_or("");
                    let text = match (subject.is_empty(), desc.is_empty()) {
                        (false, false) => format!("**{subject}** {desc}"),
                        (false, true) => subject.to_owned(),
                        _ => desc.to_owned(),
                    };
                    if !text.is_empty() {
                        blocks.push(Block::Thinking { thinking: text });
                    }
                }
            }
            let (content_blocks, _) = parts_to_blocks(&content, Role::Assistant);
            blocks.extend(content_blocks);
            let mut results: Vec<Block> = Vec::new();
            if let Some(calls) = raw.get("toolCalls").and_then(Value::as_array) {
                for c in calls {
                    let id = c.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                    let name = c
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    e.tool_uses.push(name.clone());
                    // A checkpointed Part[] may already carry the functionCall.
                    if !blocks
                        .iter()
                        .any(|b| matches!(b, Block::ToolUse { id: bid, .. } if *bid == id))
                    {
                        blocks.push(Block::ToolUse {
                            id: id.clone(),
                            name,
                            input: c.get("args").cloned().unwrap_or_else(|| json!({})),
                        });
                    }
                    if let Some(result) = c.get("result").filter(|r| !r.is_null()) {
                        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
                        e.tool_results.push(id.clone());
                        results.push(Block::ToolResult {
                            tool_use_id: id,
                            content: Value::String(function_response_text(result)),
                            is_error: matches!(status, "error" | "cancelled"),
                        });
                    }
                }
            }
            let tools = if e.tool_uses.is_empty() {
                String::new()
            } else {
                format!(" · 🔧 {}", e.tool_uses.join(", "))
            };
            let text = Message::new(Role::Assistant, blocks.clone()).plain_text();
            e.summary = if !text.is_empty() {
                format!("🤖 {}{tools}", truncate(&text, 100))
            } else if !tools.is_empty() {
                format!("🔧 {}", e.tool_uses.join(", "))
            } else if blocks.iter().any(|b| matches!(b, Block::Thinking { .. })) {
                "💭 Thinking".into()
            } else {
                "🤖 Assistant".into()
            };
            // Tool results are carried on the model turn in this format; emit
            // them as a following user-side message in the canonical model by
            // appending them after the calls. Exporters split them out.
            blocks.extend(results);
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.usage = raw.get("tokens").and_then(tokens_usage);
            if let Some(u) = &e.usage {
                e.cost_usd = Some(estimate_cost_for(source, e.model.as_deref(), u));
            }
            e.turn_end = e.tool_uses.is_empty() && !text.is_empty();
        }
        "info" | "warning" | "error" => {
            e.event_type = if kind == "error" { "error" } else { "system" }.into();
            let text = content_text(&content);
            let icon = match kind {
                "error" => "⛔",
                "warning" => "⚠️",
                _ => "ℹ️",
            };
            e.summary = format!("{icon} {}", truncate(&text, 110));
        }
        other => {
            e.event_type = if other.is_empty() { "unknown" } else { other }.into();
            e.summary = format!("❓ {}", e.event_type);
        }
    }
    e
}

/// Gemini `tokens`: `input` includes cached tokens and `output` excludes
/// thoughts, so bill `input - cached` as fresh input and add thoughts to
/// output.
fn tokens_usage(t: &Value) -> Option<TokenUsage> {
    let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    let input = g("input");
    let cached = g("cached").min(input);
    let output = g("output") + g("thoughts");
    if input == 0 && output == 0 {
        return None;
    }
    Some(TokenUsage {
        input: input - cached,
        output,
        cache_read: cached,
        cache_creation: 0,
    })
}

/// Gemini `PartListUnion` (string | Part | Part[]) → canonical blocks.
/// Returns (content blocks, tool-result blocks).
pub fn parts_to_blocks(content: &Value, role: Role) -> (Vec<Block>, Vec<Block>) {
    let mut blocks = Vec::new();
    let mut results = Vec::new();
    let parts: Vec<Value> = match content {
        Value::String(s) if !s.is_empty() => vec![json!({ "text": s })],
        Value::Array(a) => a
            .iter()
            .map(|p| match p {
                Value::String(s) => json!({ "text": s }),
                other => other.clone(),
            })
            .collect(),
        Value::Object(_) => vec![content.clone()],
        _ => Vec::new(),
    };
    for p in parts {
        if let Some(text) = p.get("text").and_then(Value::as_str) {
            if text.is_empty() {
                continue;
            }
            if p.get("thought").and_then(Value::as_bool) == Some(true) && role == Role::Assistant {
                blocks.push(Block::Thinking {
                    thinking: text.to_owned(),
                });
            } else {
                blocks.push(Block::Text {
                    text: text.to_owned(),
                });
            }
        } else if let Some(fc) = p.get("functionCall") {
            blocks.push(Block::ToolUse {
                id: fc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                name: fc
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                input: fc.get("args").cloned().unwrap_or_else(|| json!({})),
            });
        } else if let Some(fr) = p.get("functionResponse") {
            let resp = fr.get("response").cloned().unwrap_or(Value::Null);
            let is_error = resp.get("error").is_some();
            results.push(Block::ToolResult {
                tool_use_id: fr
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                content: Value::String(response_text(&resp)),
                is_error,
            });
        } else if let Some(data) = p.get("inlineData").or_else(|| p.get("fileData")) {
            blocks.push(Block::Image {
                source: data.clone(),
            });
        }
    }
    (blocks, results)
}

fn response_text(resp: &Value) -> String {
    for k in ["output", "error", "content", "result"] {
        if let Some(v) = resp.get(k) {
            return match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
        }
    }
    match resp {
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A tool call's `result`: Part[] of functionResponse parts (plus media).
fn function_response_text(result: &Value) -> String {
    let (_, results) = parts_to_blocks(result, Role::User);
    let texts: Vec<String> = results
        .into_iter()
        .filter_map(|b| match b {
            Block::ToolResult { content, .. } => content.as_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        content_text(result)
    } else {
        texts.join("\n")
    }
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"{"sessionId":"4f1c9a2e-7b3d","projectHash":"af06","startTime":"2026-09-29T14:03:11.482Z","lastUpdated":"2026-09-29T14:03:11.482Z","kind":"main"}
{"$set":{"messages":[{"id":"ctx","timestamp":"2026-09-29T14:03:11.530Z","type":"user","content":[{"text":"<session_context>\nThis is the Gemini CLI.\n</session_context>"}]}],"lastUpdated":"2026-09-29T14:03:11.531Z"}}
{"id":"u1","timestamp":"2026-09-29T14:03:20.101Z","type":"user","content":[{"text":"run the tests"}]}
{"$set":{"lastUpdated":"2026-09-29T14:03:20.102Z"}}
{"id":"g1","timestamp":"2026-09-29T14:03:23.877Z","type":"gemini","content":"","thoughts":[{"subject":"Running the test suite","description":"I'll run npm test.","timestamp":"x"}],"tokens":{"input":8421,"output":37,"cached":6100,"thoughts":112,"tool":0,"total":8570},"model":"gemini-2.5-pro"}
{"id":"g1","timestamp":"2026-09-29T14:03:23.877Z","type":"gemini","content":"","thoughts":[],"tokens":{"input":8421,"output":37,"cached":6100,"thoughts":112,"tool":0,"total":8570},"model":"gemini-2.5-pro","toolCalls":[{"id":"run_shell_command__1","name":"run_shell_command","args":{"command":"npm test"},"result":[{"functionResponse":{"id":"run_shell_command__1","name":"run_shell_command","response":{"output":"12 passing"}}}],"status":"success","timestamp":"x"}]}
{"id":"u2","timestamp":"2026-09-29T14:03:31.260Z","type":"user","content":[{"functionResponse":{"id":"run_shell_command__1","name":"run_shell_command","response":{"output":"12 passing"}}}]}
{"id":"g2","timestamp":"2026-09-29T14:03:34.019Z","type":"gemini","content":"All 12 tests pass.","thoughts":[],"tokens":{"input":8702,"output":9,"cached":8000,"thoughts":0,"tool":0,"total":8711},"model":"gemini-2.5-pro"}
{"$set":{"summary":"Ran the project's test suite"}}
"#;

    fn parse(body: &str) -> SessionDoc {
        let p =
            Path::new("/nonexistent/.gemini/tmp/app/chats/session-2026-09-29T14-03-4f1c9a2e.jsonl");
        parse_document(p, body, false).unwrap().remove(0)
    }

    #[test]
    fn replays_upserts_and_set_patches() {
        let doc = parse(LOG);
        assert_eq!(doc.session_id, "4f1c9a2e-7b3d");
        // ctx, u1, g1 (upserted once), u2, g2
        assert_eq!(doc.records.len(), 5);
        let g1 = enrich(&doc.records[2]);
        assert_eq!(g1.event_type, "assistant");
        assert_eq!(g1.tool_uses, vec!["run_shell_command"]);
        assert_eq!(g1.model.as_deref(), Some("gemini-2.5-pro"));
        let u = g1.usage.unwrap();
        assert_eq!(u.input, 8421 - 6100);
        assert_eq!(u.cache_read, 6100);
        assert_eq!(u.output, 37 + 112);
        let msg = g1.message.unwrap();
        assert!(msg
            .tool_uses()
            .any(|(_, n, i)| n == "run_shell_command" && i["command"] == "npm test"));
        assert!(msg
            .tool_results()
            .any(|(id, c, _)| id == "run_shell_command__1" && c == "12 passing"));
        assert_eq!(g1.title.as_deref(), Some("Ran the project's test suite"));
    }

    #[test]
    fn context_and_synthetic_results_are_not_user_prompts() {
        let doc = parse(LOG);
        let ctx = enrich(&doc.records[0]);
        assert_eq!(ctx.message.unwrap().role, Role::System);
        let synthetic = enrich(&doc.records[3]);
        assert_eq!(synthetic.event_type, "tool_result");
        assert!(synthetic.message.is_none());
        let u1 = enrich(&doc.records[1]);
        assert_eq!(u1.event_type, "user");
        assert_eq!(u1.message.unwrap().plain_text(), "run the tests");
        let g2 = enrich(&doc.records[4]);
        assert!(g2.turn_end);
    }

    #[test]
    fn rewind_truncates() {
        let body = format!("{LOG}{{\"$rewindTo\":\"u2\"}}\n");
        let doc = parse(&body);
        assert_eq!(doc.records.len(), 3);
    }

    #[test]
    fn legacy_whole_file_json() {
        let body = r#"{
  "sessionId": "legacy-1",
  "projectHash": "af06",
  "startTime": "2025-10-02T09:15:02.110Z",
  "lastUpdated": "2025-10-02T09:15:40.020Z",
  "messages": [
    { "id": "a", "timestamp": "t", "type": "user", "content": "run the tests" },
    { "id": "b", "timestamp": "t", "type": "gemini", "content": "ok", "model": "gemini-2.5-flash",
      "tokens": { "input": 100, "output": 10, "cached": 0, "thoughts": 0, "tool": 0, "total": 110 } }
  ]
}"#;
        let p = Path::new("/nonexistent/chats/session-2025-10-02T09-15-legacy1.json");
        let doc = parse_document(p, body, false).unwrap().remove(0);
        assert_eq!(doc.session_id, "legacy-1");
        assert_eq!(doc.records.len(), 2);
        assert_eq!(
            enrich(&doc.records[0]).message.unwrap().plain_text(),
            "run the tests"
        );
    }

    #[test]
    fn file_matching() {
        assert!(matches_file(Path::new(
            "/h/.gemini/tmp/app/chats/session-1.jsonl"
        )));
        assert!(matches_file(Path::new(
            "/h/.gemini/tmp/app/chats/parent/agent.jsonl"
        )));
        assert!(!matches_file(Path::new("/h/.gemini/tmp/app/logs.json")));
        assert!(!matches_file(Path::new(
            "/h/.gemini/tmp/app/checkpoint-x.json"
        )));
        assert!(!matches_file(Path::new(
            "/h/.gemini/tmp/app/chats/s.jsonl.tmp-123"
        )));
    }
}
