//! Cursor adapter (Cursor CLI / `cursor-agent`, and Cursor agent transcripts).
//!
//! Two readable local sources exist:
//!
//! - **`store.db`** — `<config>/chats/<md5(cwd)>/<agentId>/store.db` (config
//!   dir: `$CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else
//!   `~/.cursor`). A content-addressed blob store: `meta` key `0` holds
//!   hex-encoded JSON (`latestRootBlobId`, `name`, `lastUsedModel`,
//!   `createdAt`); the root blob is protobuf whose field 1 lists message blobs
//!   (AI-SDK-style JSON, including tool results) and field 13 lists summary
//!   archives of older messages. This is the richest source and is preferred.
//! - **Agent transcripts** — `<data>/projects/<slug>/agent-transcripts/<id>/
//!   <id>.jsonl` (`$CURSOR_DATA_DIR` or `~/.cursor`): `{role, message:
//!   {content}}` lines plus `{type: "turn_ended"}`. Lossy (no tool results,
//!   model or usage) and rewritten on compaction; used when no `store.db`
//!   exists for the conversation.
//!
//! Also accepted: `cursor-agent --output-format stream-json` output saved to
//! a `.jsonl` file under a watch root forced to `--source cursor`.
//!
//! Neither store records token usage locally (Cursor bills server-side).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use rusqlite::params;
use serde_json::{json, Map, Value};

use crate::message::{anthropic_blocks, parse_json_arguments, Block, Message, Role};
use crate::sources::{
    any_timestamp, env_path, md5_hex, opencode::open_readonly, summarise_message, AgentSource,
    Enrichment, FileKind, SessionDoc,
};

fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir(home: &Path) -> PathBuf {
    env_path("CURSOR_CONFIG_DIR")
        .or_else(|| {
            env_path("XDG_CONFIG_HOME")
                .map(|x| x.join("cursor"))
                .filter(|p| p.is_dir())
        })
        .unwrap_or_else(|| home.join(".cursor"))
}

pub fn data_dir(home: &Path) -> PathBuf {
    env_path("CURSOR_DATA_DIR").unwrap_or_else(|| home.join(".cursor"))
}

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        config_dir(home).join("chats"),
        config_dir(home).join("acp-sessions"),
        data_dir(home).join("projects"),
    ]
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    let base = name.trim_end_matches("-wal").trim_end_matches("-shm");
    if base == "store.db" {
        return Some(FileKind::Sqlite);
    }
    if name.ends_with(".jsonl") {
        return Some(FileKind::Document);
    }
    None
}

// ---------------------------------------------------------------------------
// Agent transcripts / stream-json documents
// ---------------------------------------------------------------------------

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let stem = path.file_stem()?.to_str()?.to_owned();
    let in_transcripts = path
        .ancestors()
        .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("agent-transcripts"));
    let parent = path.parent()?;
    let session_id = if parent.file_name().and_then(|n| n.to_str()) == Some("subagents") {
        let root = parent.parent()?.file_name()?.to_str()?;
        format!("{root}:{stem}")
    } else {
        stem.clone()
    };
    if in_transcripts && store_db_exists(&stem) {
        // The store.db copy of this conversation is complete; prefer it.
        return Some(Vec::new());
    }
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if in_transcripts {
        if let Some(slug) = path
            .ancestors()
            .find(|a| {
                a.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    == Some("projects")
            })
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            if let Some(cwd) = resolve_slug(slug) {
                ctx.insert("cwd".into(), json!(cwd));
            }
        }
    }
    let records = body
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .map(|mut v| {
            if let Some(o) = v.as_object_mut() {
                if let Some(t) = o.get("metadata").and_then(|m| m.get("overview")).cloned() {
                    ctx.insert("title".into(), t);
                }
                o.insert("_trace".into(), Value::Object(ctx.clone()));
            }
            v
        })
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

fn store_db_exists(agent_id: &str) -> bool {
    let chats = config_dir(&home()).join("chats");
    let Ok(rd) = std::fs::read_dir(chats) else {
        return false;
    };
    rd.flatten()
        .any(|bucket| bucket.path().join(agent_id).join("store.db").is_file())
}

/// Reverse Cursor's lossy project slug (`/home/u/my-app` → `home-u-my-app`)
/// by walking the filesystem: at each `-` choose between a path separator and
/// a literal dash, keeping only prefixes that exist.
pub fn resolve_slug(slug: &str) -> Option<String> {
    static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);
    if let Ok(mut g) = CACHE.lock() {
        if let Some(hit) = g.get_or_insert_with(HashMap::new).get(slug) {
            return hit.clone();
        }
    }
    let parts: Vec<&str> = slug.split('-').filter(|p| !p.is_empty()).collect();
    let roots: Vec<PathBuf> = if cfg!(windows) {
        // `c-Users-me-app` → C:\Users\me\app
        parts
            .first()
            .filter(|d| d.len() == 1)
            .map(|d| vec![PathBuf::from(format!("{d}:\\"))])
            .unwrap_or_default()
    } else {
        vec![PathBuf::from("/")]
    };
    let skip = usize::from(cfg!(windows));
    let found = roots
        .into_iter()
        .find_map(|r| search_slug(&r, &parts[skip.min(parts.len())..], 0))
        .map(|p| p.to_string_lossy().to_string());
    if let Ok(mut g) = CACHE.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(slug.to_owned(), found.clone());
    }
    found
}

fn search_slug(base: &Path, parts: &[&str], depth: usize) -> Option<PathBuf> {
    if parts.is_empty() {
        return base.is_dir().then(|| base.to_path_buf());
    }
    if depth > 64 {
        return None;
    }
    // Try the longest dash-joined component first so `my-app` beats `my/app`.
    for take in (1..=parts.len()).rev() {
        for sep in ["-", ".", "_", " "] {
            let comp = parts[..take].join(sep);
            let cand = base.join(&comp);
            if cand.is_dir() {
                if let Some(p) = search_slug(&cand, &parts[take..], depth + 1) {
                    return Some(p);
                }
            }
            if take == 1 {
                break; // Separators only matter for multi-part components.
            }
        }
    }
    None
}

/// `chats/<md5(cwd)>` → cwd, by hashing the resolved project slugs.
fn cwd_for_bucket(bucket: &str) -> Option<String> {
    let projects = data_dir(&home()).join("projects");
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        let slug = entry.file_name().to_string_lossy().to_string();
        if let Some(path) = resolve_slug(&slug) {
            if md5_hex(&path) == bucket {
                return Some(path);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// store.db
// ---------------------------------------------------------------------------

/// Minimal protobuf wire-format reader: (field number, value) pairs.
enum Pb<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn pb_fields(buf: &[u8]) -> Vec<(u64, Pb<'_>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let Some(key) = read_varint(buf, &mut pos) else {
            break;
        };
        let (field, wire) = (key >> 3, key & 7);
        let val = match wire {
            0 => match read_varint(buf, &mut pos) {
                Some(v) => Pb::Varint(v),
                None => break,
            },
            1 => {
                pos += 8;
                Pb::Fixed
            }
            2 => {
                let Some(len) = read_varint(buf, &mut pos) else {
                    break;
                };
                let end = pos.saturating_add(len as usize);
                if end > buf.len() {
                    break;
                }
                let b = &buf[pos..end];
                pos = end;
                Pb::Bytes(b)
            }
            5 => {
                pos += 4;
                Pb::Fixed
            }
            _ => break,
        };
        out.push((field, val));
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

pub fn read_sqlite(path: &Path, _cursor: &mut Option<String>) -> anyhow::Result<Vec<SessionDoc>> {
    let conn = open_readonly(path)?;
    let meta_hex: String =
        conn.query_row("SELECT value FROM meta WHERE key = '0'", [], |r| r.get(0))?;
    let meta: Value = unhex(meta_hex.trim())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .or_else(|| serde_json::from_str(&meta_hex).ok())
        .unwrap_or(Value::Null);
    let Some(root_id) = meta.get("latestRootBlobId").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    let blob = |id: &str| -> Option<Vec<u8>> {
        conn.query_row("SELECT data FROM blobs WHERE id = ?1", params![id], |r| {
            r.get::<_, Vec<u8>>(0)
        })
        .ok()
    };
    // A bytes field is a 32-byte blob reference when such a blob exists.
    let deref = |b: &[u8]| -> Option<Vec<u8>> {
        if b.len() == 32 {
            if let Some(d) = blob(&hex(b)) {
                return Some(d);
            }
        }
        Some(b.to_vec())
    };
    let Some(root) = blob(root_id) else {
        return Ok(Vec::new());
    };

    let agent_dir = path.parent();
    let session_id = meta
        .get("agentId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            agent_dir
                .and_then(|d| d.file_name())
                .map(|n| n.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "cursor".into());
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(session_id));
    if let Some(n) = meta
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| *n != "New Agent")
    {
        ctx.insert("title".into(), json!(n));
    }
    if let Some(m) = meta.get("lastUsedModel").and_then(Value::as_str) {
        ctx.insert("model".into(), json!(m));
    }
    // ACP sessions carry a meta.json with the cwd; chats use md5(cwd).
    let cwd = agent_dir
        .and_then(|d| std::fs::read_to_string(d.join("meta.json")).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|m| m.get("cwd").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| {
            agent_dir
                .and_then(|d| d.parent())
                .and_then(|b| b.file_name())
                .and_then(|b| cwd_for_bucket(&b.to_string_lossy()))
        });
    if let Some(c) = &cwd {
        ctx.insert("cwd".into(), json!(c));
    }

    let mut message_blobs: Vec<Vec<u8>> = Vec::new();
    let fields = pb_fields(&root);
    // Summarised history first (field 13 archives → their field 1 messages).
    for (f, v) in &fields {
        if let (13, Pb::Bytes(b)) = (f, v) {
            if let Some(archive) = deref(b) {
                for (af, av) in pb_fields(&archive) {
                    if let (1, Pb::Bytes(mb)) = (af, av) {
                        if let Some(m) = deref(mb) {
                            message_blobs.push(m);
                        }
                    }
                }
            }
        }
    }
    let mut started: Option<u64> = None;
    for (f, v) in &fields {
        match (f, v) {
            (1, Pb::Bytes(b)) => {
                if let Some(m) = deref(b) {
                    message_blobs.push(m);
                }
            }
            (26, Pb::Varint(ms)) => started = Some(*ms),
            _ => {}
        }
    }
    let created = started
        .map(|v| json!(v))
        .or_else(|| meta.get("createdAt").cloned())
        .unwrap_or(Value::Null);
    ctx.insert("created".into(), created);

    let records: Vec<Value> = message_blobs
        .iter()
        .filter_map(|b| serde_json::from_slice::<Value>(b).ok())
        .filter(|m| {
            m.pointer("/providerOptions/cursor/isSummary")
                .and_then(Value::as_bool)
                != Some(true)
        })
        .map(|m| json!({ "ai": m, "_trace": ctx }))
        .collect();
    Ok(vec![SessionDoc {
        session_id,
        records,
    }])
}

// ---------------------------------------------------------------------------
// Enrichment
// ---------------------------------------------------------------------------

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId").or_else(|| {
            raw.get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        cwd: cs("cwd").or_else(|| raw.get("cwd").and_then(Value::as_str).map(str::to_owned)),
        title: cs("title"),
        timestamp: any_timestamp(raw.get("timestamp_ms").or_else(|| raw.get("timestamp"))),
        ..Default::default()
    };

    if let Some(ai) = raw.get("ai") {
        return enrich_ai_sdk(ai, e, cs("model"));
    }

    let declared = raw.get("type").and_then(Value::as_str).unwrap_or("");
    if declared == "turn_ended" || declared == "result" {
        e.event_type = "system".into();
        e.turn_end = true;
        e.summary = format!(
            "⚙️  Turn ended ({})",
            raw.get("status")
                .or_else(|| raw.get("subtype"))
                .and_then(Value::as_str)
                .unwrap_or("done")
        );
        return e;
    }
    if declared == "metadata" {
        e.event_type = "system".into();
        e.summary = "⚙️  Transcript metadata".into();
        return e;
    }
    if declared == "system" {
        e.event_type = "system".into();
        e.model = raw.get("model").and_then(Value::as_str).map(str::to_owned);
        e.summary = format!(
            "⚙️  Session start ({})",
            e.model.clone().unwrap_or_default()
        );
        return e;
    }
    if declared == "tool_call" {
        // stream-json: {subtype: started|completed, call_id, tool_call: {...}}
        let tc = raw.get("tool_call").cloned().unwrap_or(Value::Null);
        let (name, body) = tc
            .as_object()
            .and_then(|o| o.iter().next())
            .map(|(k, v)| (k.trim_end_matches("ToolCall").to_owned(), v.clone()))
            .unwrap_or_default();
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(name);
        let id = raw
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if raw.get("subtype").and_then(Value::as_str) == Some("completed") {
            e.event_type = "tool_result".into();
            e.tool_results.push(id.clone());
            e.message = Message::new(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: id,
                    content: Value::String(
                        body.get("result")
                            .map(|r| r.to_string())
                            .unwrap_or_default(),
                    ),
                    is_error: body.pointer("/result/error").is_some(),
                }],
            )
            .non_empty();
            e.summary = format!("📦 {name} result");
        } else {
            e.event_type = "tool_use".into();
            e.tool_uses.push(name.clone());
            e.message = Message::new(
                Role::Assistant,
                vec![Block::ToolUse {
                    id,
                    name: name.clone(),
                    input: body
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| parse_json_arguments(body.get("arguments"))),
                }],
            )
            .non_empty();
            e.summary = format!("🔧 {name}");
        }
        return e;
    }

    // Transcript lines / stream-json user & assistant messages.
    let role = raw
        .get("role")
        .or_else(|| raw.pointer("/message/role"))
        .and_then(Value::as_str)
        .unwrap_or(declared);
    let content = raw
        .pointer("/message/content")
        .or_else(|| raw.get("content"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut blocks = anthropic_blocks(&content);
    for b in &blocks {
        if let Block::ToolUse { name, .. } = b {
            e.tool_uses.push(name.clone());
        }
    }
    match role {
        "user" => {
            for b in &mut blocks {
                if let Block::Text { text } = b {
                    if e.timestamp.is_none() {
                        e.timestamp = between(text, "<timestamp>", "</timestamp>")
                            .and_then(|t| parse_cursor_timestamp(&t));
                    }
                    if let Some(q) = between(text, "<user_query>", "</user_query>") {
                        *text = q;
                    }
                }
            }
            e.event_type = "user".into();
            e.message = Message::new(Role::User, blocks).non_empty();
        }
        "assistant" => {
            e.event_type = "assistant".into();
            e.model =
                cs("model").or_else(|| raw.get("model").and_then(Value::as_str).map(str::to_owned));
            e.message = Message::new(Role::Assistant, blocks).non_empty();
        }
        _ => {
            e.event_type = if role.is_empty() { "unknown" } else { "system" }.into();
        }
    }
    e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
    e
}

/// AI-SDK message from store.db.
fn enrich_ai_sdk(ai: &Value, mut e: Enrichment, model: Option<String>) -> Enrichment {
    let role = ai.get("role").and_then(Value::as_str).unwrap_or("");
    let mut blocks: Vec<Block> = Vec::new();
    match ai.get("content") {
        Some(Value::String(s)) if !s.is_empty() => blocks.push(Block::Text { text: s.clone() }),
        Some(Value::Array(parts)) => {
            for p in parts {
                let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                match p.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" => {
                        let t = s("text");
                        if !t.is_empty() {
                            blocks.push(Block::Text { text: t });
                        }
                    }
                    "reasoning" => {
                        let t = s("text");
                        if !t.is_empty() {
                            blocks.push(Block::Thinking { thinking: t });
                        }
                    }
                    "tool-call" => {
                        e.tool_uses.push(s("toolName"));
                        blocks.push(Block::ToolUse {
                            id: s("toolCallId"),
                            name: s("toolName"),
                            input: p
                                .get("args")
                                .or_else(|| p.get("input"))
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                        });
                    }
                    "tool-result" => {
                        e.tool_results.push(s("toolCallId"));
                        let out = p
                            .get("result")
                            .or_else(|| p.get("output"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let content = match out {
                            Value::String(s) => Value::String(s),
                            Value::Object(ref o) if o.get("value").is_some() => Value::String(
                                o["value"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| o["value"].to_string()),
                            ),
                            other => Value::String(other.to_string()),
                        };
                        blocks.push(Block::ToolResult {
                            tool_use_id: s("toolCallId"),
                            content,
                            is_error: p.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        });
                    }
                    "image" => blocks.push(Block::Image {
                        source: p.get("image").cloned().unwrap_or(Value::Null),
                    }),
                    "file" => blocks.push(Block::Text {
                        text: format!("[file: {}]", s("filename")),
                    }),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    match role {
        "user" | "tool" => {
            let has_text = blocks.iter().any(|b| matches!(b, Block::Text { .. }));
            e.event_type = if has_text { "user" } else { "tool_result" }.into();
            if e.cwd.is_none() {
                let text = Message::new(Role::User, blocks.clone()).plain_text();
                e.cwd = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("Workspace Path:"))
                    .map(|s| s.trim().to_owned());
            }
            e.message = Message::new(Role::User, blocks).non_empty();
        }
        "assistant" => {
            e.event_type = "assistant".into();
            e.model = model;
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
    let _ = AgentSource::Cursor;
    e
}

fn between(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].trim().to_owned())
}

/// `Tuesday, Sep 29, 2026, 9:14 PM (UTC+1)` → RFC 3339.
fn parse_cursor_timestamp(s: &str) -> Option<String> {
    let (dt, tz) = s.rsplit_once(" (UTC")?;
    let tz = tz.trim_end_matches(')');
    let naive = chrono::NaiveDateTime::parse_from_str(dt.trim(), "%A, %b %d, %Y, %I:%M %p").ok()?;
    let (sign, rest) = match tz.chars().next() {
        Some('-') => (-1, &tz[1..]),
        Some('+') => (1, &tz[1..]),
        _ => (1, tz),
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let secs = sign * (h.parse::<i32>().unwrap_or(0) * 3600 + m.parse::<i32>().unwrap_or(0) * 60);
    let off = chrono::FixedOffset::east_opt(secs)?;
    naive
        .and_local_timezone(off)
        .single()
        .map(|d| d.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn pb_bytes(field: u64, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut key = (field << 3) | 2;
        loop {
            let b = (key & 0x7f) as u8;
            key >>= 7;
            if key == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
        let mut len = data.len() as u64;
        loop {
            let b = (len & 0x7f) as u8;
            len >>= 7;
            if len == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn store_db_messages_via_protobuf_root() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("chats/abcd/agent-1");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("meta.json"), r#"{"cwd":"/work/app"}"#).unwrap();
        let db = agent.join("store.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
        let msgs = [
            json!({"role":"user","content":[{"type":"text","text":"fix it"}]}),
            json!({"role":"assistant","content":[{"type":"reasoning","text":"hmm"},{"type":"tool-call","toolCallId":"c1","toolName":"Shell","args":{"command":"ls"}}]}),
            json!({"role":"tool","content":[{"type":"tool-result","toolCallId":"c1","toolName":"Shell","result":"a.rs"}]}),
            json!({"role":"assistant","content":[{"type":"text","text":"Done."}]}),
        ];
        let mut root = Vec::new();
        for (i, m) in msgs.iter().enumerate() {
            let id = [i as u8 + 1; 32];
            conn.execute(
                "INSERT INTO blobs VALUES (?1, ?2)",
                params![hex(&id), serde_json::to_vec(m).unwrap()],
            )
            .unwrap();
            root.extend(pb_bytes(1, &id));
        }
        let root_id = hex(&[9u8; 32]);
        conn.execute("INSERT INTO blobs VALUES (?1, ?2)", params![root_id, root])
            .unwrap();
        let meta = json!({"agentId":"agent-1","latestRootBlobId":root_id,"name":"Fix it","lastUsedModel":"gpt-5"});
        conn.execute(
            "INSERT INTO meta VALUES ('0', ?1)",
            params![hex(meta.to_string().as_bytes())],
        )
        .unwrap();
        drop(conn);

        let docs = read_sqlite(&db, &mut None).unwrap();
        assert_eq!(docs[0].session_id, "agent-1");
        let recs: Vec<Enrichment> = docs[0].records.iter().map(enrich).collect();
        assert_eq!(recs.len(), 4);
        assert_eq!(recs[0].cwd.as_deref(), Some("/work/app"));
        assert_eq!(recs[1].tool_uses, vec!["Shell"]);
        assert_eq!(recs[1].model.as_deref(), Some("gpt-5"));
        assert_eq!(recs[2].event_type, "tool_result");
        assert!(recs[3].turn_end);
        assert_eq!(recs[3].title.as_deref(), Some("Fix it"));
    }

    #[test]
    fn transcript_lines() {
        let body = r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Tuesday, Sep 29, 2026, 9:14 PM (UTC+1)</timestamp>\n<user_query>\nfix the failing test\n</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"Running the test suite first."},{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}
{"type":"turn_ended","status":"success"}
"#;
        let p = Path::new("/nonexistent/.cursor/projects/x/agent-transcripts/abc/abc.jsonl");
        let d = parse_document(p, body).unwrap().remove(0);
        assert_eq!(d.session_id, "abc");
        let u = enrich(&d.records[0]);
        assert_eq!(
            u.message.as_ref().unwrap().plain_text(),
            "fix the failing test"
        );
        assert_eq!(u.timestamp.as_deref(), Some("2026-09-29T21:14:00+01:00"));
        let a = enrich(&d.records[1]);
        assert_eq!(a.tool_uses, vec!["Shell"]);
        assert!(enrich(&d.records[2]).turn_end);
    }

    #[test]
    fn slug_resolution_prefers_existing_dashed_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("my-app/src")).unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let slug = format!(
            "{}-my-app-src",
            root.trim_start_matches('/').replace('/', "-")
        );
        if !cfg!(windows) {
            assert_eq!(
                resolve_slug(&slug).as_deref(),
                Some(dir.path().join("my-app/src").to_string_lossy().as_ref())
            );
        }
    }
}
