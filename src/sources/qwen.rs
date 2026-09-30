//! Qwen Code adapter.
//!
//! Since v0.4.0 Qwen Code writes a Claude-Code-like tree JSONL at
//! `<runtime>/projects/<sanitised cwd>/chats/<sessionId>.jsonl`, where the
//! runtime dir is `$QWEN_RUNTIME_DIR`, else `$QWEN_HOME`, else `~/.qwen`.
//! Every record carries `uuid`, `parentUuid`, `sessionId`, `timestamp`,
//! `type` (`user` / `assistant` / `tool_result` / `system` + `subtype`),
//! `cwd`, `version` and `gitBranch`; content is a Gemini `Content`
//! (`message.parts`: text, `thought` text, `functionCall`,
//! `functionResponse`) with `usageMetadata` on assistant records.
//!
//! Records that share a `uuid` are fragments of one message, so files are
//! read as documents and fragments merged (parts concatenated, the last
//! usage wins) before diffing — a plain line tail would double count.
//!
//! Qwen ≤ v0.3 used Gemini CLI's whole-file JSON under
//! `~/.qwen/tmp/<sha256>/chats/`, which is handed to the Gemini parser.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{
    env_path, estimate_cost_for, gemini, summarise_message, truncate, AgentSource, Enrichment,
    FileKind, SessionDoc,
};

pub fn runtime_dir(home: &Path) -> PathBuf {
    env_path("QWEN_RUNTIME_DIR")
        .or_else(|| env_path("QWEN_HOME"))
        .unwrap_or_else(|| home.join(".qwen"))
}

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let rt = runtime_dir(home);
    vec![rt.join("projects"), rt.join("tmp")]
}

fn is_uuid_stem(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    (32..=36).contains(&stem.len()) && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    let parent = path.parent()?.file_name()?.to_str()?;
    let grand = path.parent()?.parent()?.file_name()?.to_str()?;
    // Main transcripts (and archived ones); sidecars like `.ledger.jsonl`
    // are excluded by the UUID filename rule.
    if (parent == "chats" || (parent == "archive" && grand == "chats")) && is_uuid_stem(name) {
        return Some(FileKind::Document);
    }
    // Subagent transcripts.
    if grand == "subagents" && name.starts_with("agent-") && name.ends_with(".jsonl") {
        return Some(FileKind::Document);
    }
    // Legacy (≤ v0.3) Gemini-format sessions.
    if parent == "chats" && name.starts_with("session-") && name.ends_with(".json") {
        return Some(FileKind::Document);
    }
    None
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    if path.extension().and_then(|e| e.to_str()) == Some("json") {
        return gemini::parse_document(path, body, true);
    }
    let is_subagent = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("agent-"));
    let mut order: Vec<String> = Vec::new();
    let mut merged: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut session_id: Option<String> = None;
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if v.get("subtype").and_then(Value::as_str) == Some("managed_session_header_v1") {
            // `qwen serve` managed-session envelope: not a chat transcript.
            return Some(Vec::new());
        }
        if session_id.is_none() {
            session_id = v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let Some(uuid) = v.get("uuid").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        match merged.get_mut(&uuid) {
            None => {
                order.push(uuid.clone());
                merged.insert(uuid, v);
            }
            Some(existing) => merge_fragment(existing, &v),
        }
    }
    let base =
        session_id.or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))?;
    // Subagent transcripts carry the parent's sessionId; give them their own
    // session so their records cannot collide with the parent's.
    let session_id = if is_subagent {
        let agent = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent")
            .to_owned();
        format!("{base}:{agent}")
    } else {
        base
    };
    let records = order
        .into_iter()
        .filter_map(|u| merged.remove(&u))
        .map(|mut r| {
            if is_subagent {
                if let Some(obj) = r.as_object_mut() {
                    obj.insert("sessionId".into(), Value::String(session_id.clone()));
                }
            }
            r
        })
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

/// Fold a later fragment into the first record with the same uuid.
fn merge_fragment(existing: &mut Value, next: &Value) {
    let (Some(a), Some(b)) = (existing.as_object_mut(), next.as_object()) else {
        return;
    };
    if let Some(parts) = b
        .get("message")
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
    {
        let msg = a
            .entry("message")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(obj) = msg.as_object_mut() {
            let dst = obj
                .entry("parts")
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(arr) = dst.as_array_mut() {
                arr.extend(parts.iter().cloned());
            }
        }
    }
    if let Some(u) = b.get("usageMetadata") {
        a.insert("usageMetadata".into(), u.clone());
    }
    if let Some(t) = b.get("timestamp") {
        a.insert("timestamp".into(), t.clone());
    }
    for k in ["model", "toolCallResult"] {
        if !a.contains_key(k) {
            if let Some(v) = b.get(k) {
                a.insert(k.into(), v.clone());
            }
        }
    }
}

pub fn enrich(raw: &Value) -> Enrichment {
    if raw.get("_trace").is_some() {
        // Legacy Gemini-format record.
        return gemini::enrich(raw);
    }
    let s = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_owned);
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
    let subtype = raw.get("subtype").and_then(Value::as_str);
    let mut e = Enrichment {
        session_id: s("sessionId"),
        timestamp: s("timestamp"),
        cwd: s("cwd"),
        git_branch: s("gitBranch").filter(|b| b != "HEAD"),
        version: s("version").filter(|v| v != "unknown"),
        ..Default::default()
    };
    let parts = raw
        .pointer("/message/parts")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));

    match (kind, subtype) {
        ("user", None) => {
            let (blocks, results) = gemini::parts_to_blocks(&parts, Role::User);
            let mut all = blocks;
            all.extend(results);
            e.event_type = "user".into();
            e.message = Message::new(Role::User, all).non_empty();
            let display = raw
                .pointer("/systemPayload/displayText")
                .and_then(Value::as_str);
            e.summary = match display {
                Some(d) if !d.trim().is_empty() => format!("👤 {}", truncate(d, 120)),
                _ => summarise_message("user", e.message.as_ref(), &[]),
            };
        }
        ("user", Some(sub)) => {
            // Background notifications, cron and mid-turn injections: not
            // prompts the user typed.
            let (blocks, _) = gemini::parts_to_blocks(&parts, Role::User);
            e.event_type = "system".into();
            e.message = Message::new(Role::System, blocks).non_empty();
            e.summary = format!(
                "⚙️  {sub}: {}",
                truncate(
                    &e.message
                        .as_ref()
                        .map(|m| m.plain_text())
                        .unwrap_or_default(),
                    90
                )
            );
        }
        ("assistant", _) => {
            e.event_type = "assistant".into();
            e.model = s("model");
            let (blocks, _) = gemini::parts_to_blocks(&parts, Role::Assistant);
            for b in &blocks {
                if let Block::ToolUse { name, .. } = b {
                    e.tool_uses.push(name.clone());
                }
            }
            e.turn_end =
                e.tool_uses.is_empty() && blocks.iter().any(|b| matches!(b, Block::Text { .. }));
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.summary = summarise_message("assistant", e.message.as_ref(), &e.tool_uses);
            e.usage = raw
                .get("usageMetadata")
                .and_then(|u| usage(u, e.model.as_deref()));
            if let Some(u) = &e.usage {
                e.cost_usd = Some(estimate_cost_for(AgentSource::Qwen, e.model.as_deref(), u));
            }
        }
        ("tool_result", _) => {
            e.event_type = "tool_result".into();
            let (_, mut results) = gemini::parts_to_blocks(&parts, Role::User);
            let failed = matches!(
                raw.pointer("/toolCallResult/status")
                    .and_then(Value::as_str),
                Some("error" | "cancelled")
            );
            for r in &mut results {
                if let Block::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } = r
                {
                    *is_error |= failed;
                    e.tool_results.push(tool_use_id.clone());
                }
            }
            e.message = Message::new(Role::User, results).non_empty();
            e.summary = if failed {
                "⛔ Tool error".into()
            } else {
                "📦 Tool result".into()
            };
        }
        ("system", Some("custom_title")) => {
            e.event_type = "ai-title".into();
            e.title = raw
                .pointer("/systemPayload/customTitle")
                .and_then(Value::as_str)
                .map(str::to_owned);
            e.summary = format!("🏷  {}", e.title.clone().unwrap_or_default());
        }
        ("system", Some("chat_compression")) => {
            e.event_type = "summary".into();
            e.summary = "📝 Context compressed".into();
        }
        ("system", Some("turn_result")) => {
            e.event_type = "system".into();
            e.turn_end = true;
            e.summary = "⚙️  Turn finished".into();
        }
        ("system", sub) => {
            e.event_type = "system".into();
            e.summary = format!("⚙️  {}", sub.unwrap_or("system"));
        }
        (other, _) => {
            e.event_type = if other.is_empty() { "unknown" } else { other }.into();
            e.summary = format!("❓ {}", e.event_type);
        }
    }
    e
}

/// Gemini `usageMetadata`: `promptTokenCount` includes cached tokens. For
/// Gemini models `candidatesTokenCount` excludes thoughts; for
/// OpenAI-compatible providers (the Qwen default) it already includes them.
fn usage(u: &Value, model: Option<&str>) -> Option<TokenUsage> {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let prompt = g("promptTokenCount");
    let cached = g("cachedContentTokenCount").min(prompt);
    let mut output = g("candidatesTokenCount");
    if model.is_some_and(|m| m.to_ascii_lowercase().contains("gemini")) {
        output += g("thoughtsTokenCount");
    }
    if prompt == 0 && output == 0 {
        return None;
    }
    Some(TokenUsage {
        input: prompt - cached,
        output,
        cache_read: cached,
        cache_creation: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"{"uuid":"a1","parentUuid":null,"sessionId":"1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23","timestamp":"2026-09-29T14:03:20.101Z","type":"user","provenance":"real_user","cwd":"/home/alice/src/my-app","version":"0.24.7","gitBranch":"main","message":{"role":"user","parts":[{"text":"run the tests"}]},"systemPayload":{"displayText":"run the tests","hookContext":""}}
{"uuid":"a2","parentUuid":"a1","sessionId":"1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23","timestamp":"2026-09-29T14:03:23.877Z","type":"assistant","cwd":"/home/alice/src/my-app","version":"0.24.7","gitBranch":"main","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"text":"I should run npm test first.","thought":true},{"text":"Running the test suite."}]},"usageMetadata":{"promptTokenCount":12034,"candidatesTokenCount":61,"totalTokenCount":12095,"cachedContentTokenCount":11008,"thoughtsTokenCount":9}}
{"uuid":"a2","parentUuid":"a1","sessionId":"1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23","timestamp":"2026-09-29T14:03:23.900Z","type":"assistant","cwd":"/home/alice/src/my-app","version":"0.24.7","gitBranch":"main","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"functionCall":{"id":"call_8f2d1c","name":"run_shell_command","args":{"command":"npm test"}}}]},"usageMetadata":{"promptTokenCount":12034,"candidatesTokenCount":61,"totalTokenCount":12095,"cachedContentTokenCount":11008,"thoughtsTokenCount":9}}
{"uuid":"a3","parentUuid":"a2","sessionId":"1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23","timestamp":"2026-09-29T14:03:31.204Z","type":"tool_result","cwd":"/home/alice/src/my-app","version":"0.24.7","gitBranch":"main","message":{"role":"user","parts":[{"functionResponse":{"id":"call_8f2d1c","name":"run_shell_command","response":{"output":"12 passing"}}}]},"toolCallResult":{"callId":"call_8f2d1c","status":"success"}}
{"uuid":"a5","parentUuid":"a3","sessionId":"1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23","timestamp":"2026-09-29T14:03:34.019Z","type":"assistant","cwd":"/home/alice/src/my-app","version":"0.24.7","gitBranch":"main","model":"qwen3-coder-plus","message":{"role":"model","parts":[{"text":"All 12 tests pass."}]},"usageMetadata":{"promptTokenCount":12160,"candidatesTokenCount":8,"totalTokenCount":12168,"cachedContentTokenCount":12032,"thoughtsTokenCount":0}}
"#;

    fn doc() -> SessionDoc {
        let p = Path::new("/h/.qwen/projects/-home-alice-src-my-app/chats/1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23.jsonl");
        parse_document(p, LOG).unwrap().remove(0)
    }

    #[test]
    fn fragments_merge_and_usage_counts_once() {
        let d = doc();
        assert_eq!(d.records.len(), 4);
        let a2 = enrich(&d.records[1]);
        assert_eq!(a2.tool_uses, vec!["run_shell_command"]);
        let m = a2.message.unwrap();
        assert!(matches!(m.content[0], Block::Thinking { .. }));
        let u = a2.usage.unwrap();
        assert_eq!(u.input, 12034 - 11008);
        assert_eq!(u.cache_read, 11008);
        assert_eq!(
            u.output, 61,
            "reasoning already inside candidates for OpenAI-compatible"
        );
        assert!(!a2.turn_end);
    }

    #[test]
    fn record_mapping() {
        let d = doc();
        let u = enrich(&d.records[0]);
        assert_eq!(u.event_type, "user");
        assert_eq!(u.cwd.as_deref(), Some("/home/alice/src/my-app"));
        assert_eq!(u.git_branch.as_deref(), Some("main"));
        let tr = enrich(&d.records[2]);
        assert_eq!(tr.event_type, "tool_result");
        assert_eq!(tr.tool_results, vec!["call_8f2d1c"]);
        assert!(enrich(&d.records[3]).turn_end);
    }

    #[test]
    fn classification() {
        assert_eq!(
            classify(Path::new(
                "/q/projects/p/chats/1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23.jsonl"
            )),
            Some(FileKind::Document)
        );
        assert_eq!(
            classify(Path::new(
                "/q/projects/p/chats/1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23.ledger.jsonl"
            )),
            None
        );
        assert_eq!(
            classify(Path::new("/q/projects/p/subagents/sid/agent-x.jsonl")),
            Some(FileKind::Document)
        );
        assert_eq!(
            classify(Path::new(
                "/q/tmp/abc/chats/session-2025-01-01T00-00-abcd.json"
            )),
            Some(FileKind::Document)
        );
    }

    #[test]
    fn subagents_get_their_own_session() {
        let p = Path::new("/q/projects/p/subagents/1d6a/agent-42.jsonl");
        let d = parse_document(p, LOG).unwrap().remove(0);
        assert_eq!(
            d.session_id,
            "1d6a1f3e-0c2b-4f55-9a0e-7b8c9d0e1f23:agent-42"
        );
        assert_eq!(
            enrich(&d.records[0]).session_id.as_deref(),
            Some(d.session_id.as_str())
        );
    }
}
