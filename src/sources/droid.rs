//! Factory Droid adapter.
//!
//! Droid appends sessions to
//! `~/.factory/sessions/-<cwd slug>/<uuid>.jsonl` (`$FACTORY_HOME_OVERRIDE`
//! replaces the home directory), with a `<uuid>.settings.json` sidecar. The
//! JSONL opens with a `session_start` record (`cwd`, `title`), followed by
//! `message` records whose Anthropic-style content puts tool results in user
//! messages. Token usage exists only as a cumulative `tokenUsage` in the
//! settings sidecar, so sessions are read as documents: the transcript plus
//! one trailing totals record that is updated in place as usage grows.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{anthropic_blocks, Block, Message, Role};
use crate::sources::{
    env_path, estimate_cost_for, summarise_message, AgentSource, Enrichment, FileKind, SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let base = env_path("FACTORY_HOME_OVERRIDE").unwrap_or_else(|| home.to_path_buf());
    vec![env_path("DROID_SESSIONS_DIR").unwrap_or_else(|| base.join(".factory/sessions"))]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".jsonl") {
        return Some(FileKind::Document);
    }
    // Usage lives in the settings sidecar; a change re-reads its session.
    if name.ends_with(".settings.json") {
        return Some(FileKind::Document);
    }
    None
}

/// `<uuid>.settings.json` belongs to `<uuid>.jsonl`.
pub fn unit_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name.strip_suffix(".settings.json") {
        Some(stem) => path.with_file_name(format!("{stem}.jsonl")),
        None => path.to_path_buf(),
    }
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return Some(Vec::new());
    }
    let stem = path.file_stem()?.to_str()?.to_owned();
    let mut ctx = Map::new();
    let mut records: Vec<Value> = Vec::new();
    let mut session_id = stem.clone();
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session_start") => {
                if let Some(id) = v.get("id").and_then(Value::as_str) {
                    session_id = id.to_owned();
                }
                for (from, to) in [
                    ("cwd", "cwd"),
                    ("title", "title"),
                    ("sessionTitle", "title"),
                ] {
                    if let Some(s) = v.get(from).and_then(Value::as_str) {
                        ctx.entry(to).or_insert(json!(s));
                    }
                }
            }
            Some("message") | Some("compaction_state") | Some("agent_turn_outcome") => {
                records.push(v)
            }
            _ => {}
        }
    }
    let settings = std::fs::read_to_string(path.with_file_name(format!("{stem}.settings.json")))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(s) = &settings {
        if let Some(m) = s.get("model").and_then(Value::as_str) {
            ctx.insert("model".into(), json!(m));
        }
        if let Some(u) = s.get("tokenUsage").filter(|u| u.is_object()) {
            records.push(json!({ "type": "_usage_totals", "tokenUsage": u }));
        }
    }
    ctx.insert("sessionId".into(), json!(session_id));
    let records = records
        .into_iter()
        .map(|mut r| {
            if let Some(o) = r.as_object_mut() {
                o.insert("_trace".into(), Value::Object(ctx.clone()));
            }
            r
        })
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title"),
        timestamp: raw
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    match raw.get("type").and_then(Value::as_str).unwrap_or("") {
        "message" => {
            let msg = raw.get("message").cloned().unwrap_or(Value::Null);
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            let blocks = anthropic_blocks(msg.get("content").unwrap_or(&Value::Null));
            for b in &blocks {
                match b {
                    Block::ToolUse { name, .. } => e.tool_uses.push(name.clone()),
                    Block::ToolResult { tool_use_id, .. } => {
                        e.tool_results.push(tool_use_id.clone())
                    }
                    _ => {}
                }
            }
            if role == "assistant" {
                e.event_type = "assistant".into();
                e.model = cs("model");
                e.message = Message::new(Role::Assistant, blocks).non_empty();
            } else {
                let is_hook = msg.get("hookEventName").is_some();
                let text = Message::new(Role::User, blocks.clone()).plain_text();
                if cs("cwd").is_none() {
                    e.cwd = text
                        .lines()
                        .find_map(|l| l.trim().strip_prefix("Current folder:"))
                        .map(|s| s.trim().to_owned());
                }
                e.event_type = if is_hook { "system" } else { "user" }.into();
                e.message = Message::new(if is_hook { Role::System } else { Role::User }, blocks)
                    .non_empty();
            }
            e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
        }
        "agent_turn_outcome" => {
            e.event_type = "system".into();
            e.turn_end = true;
            e.summary = format!(
                "⚙️  Turn {}",
                raw.get("reason").and_then(Value::as_str).unwrap_or("ended")
            );
        }
        "compaction_state" => {
            e.event_type = "summary".into();
            e.summary = "📝 Context compacted".into();
        }
        "_usage_totals" => {
            let u = raw.get("tokenUsage").cloned().unwrap_or(Value::Null);
            let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let usage = TokenUsage {
                input: g("inputTokens"),
                output: g("outputTokens") + g("thinkingTokens"),
                cache_read: g("cacheReadTokens"),
                cache_creation: g("cacheCreationTokens"),
            };
            e.event_type = "system".into();
            e.model = cs("model");
            e.cost_usd = Some(estimate_cost_for(
                AgentSource::Droid,
                e.model.as_deref(),
                &usage,
            ));
            e.usage = Some(usage);
            e.summary = "⚙️  Session token usage".into();
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

    #[test]
    fn session_with_settings_usage() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join(".factory/sessions/-Users-me-repo");
        std::fs::create_dir_all(&sdir).unwrap();
        let body = r#"{"type":"session_start","id":"6f1d","title":"Fix failing test","owner":"u","version":2,"cwd":"/Users/me/repo"}
{"type":"message","id":"a1","timestamp":"2026-09-29T20:14:05.120Z","message":{"role":"user","content":[{"type":"text","text":"fix the failing test"}]}}
{"type":"message","id":"b2","parentId":"a1","timestamp":"2026-09-29T20:14:09.870Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Run tests.","signature":"s"},{"type":"tool_use","id":"toolu_01","name":"Execute","input":{"command":"npm test"}}]}}
{"type":"message","id":"c3","parentId":"b2","timestamp":"2026-09-29T20:14:21.004Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"1 failing"}]}}
{"type":"todo_state","id":"d4","timestamp":"2026-09-29T20:14:22.000Z","todos":[],"messageIndex":3}
"#;
        let jsonl = sdir.join("6f1d.jsonl");
        std::fs::write(&jsonl, body).unwrap();
        std::fs::write(
            sdir.join("6f1d.settings.json"),
            r#"{"model":"claude-sonnet-4-5","tokenUsage":{"inputTokens":100,"outputTokens":50,"cacheCreationTokens":0,"cacheReadTokens":900,"thinkingTokens":10}}"#,
        )
        .unwrap();
        assert_eq!(unit_path(&sdir.join("6f1d.settings.json")), jsonl);
        let d = parse_document(&jsonl, body).unwrap().remove(0);
        assert_eq!(d.session_id, "6f1d");
        assert_eq!(d.records.len(), 4);
        let a = enrich(&d.records[1]);
        assert_eq!(a.tool_uses, vec!["Execute"]);
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(a.cwd.as_deref(), Some("/Users/me/repo"));
        let t = enrich(&d.records[3]);
        assert_eq!(t.usage.as_ref().unwrap().output, 60);
        assert!(t.cost_usd.unwrap() > 0.0);
    }
}
