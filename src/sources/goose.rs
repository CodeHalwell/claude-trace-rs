//! Goose (Block) adapter.
//!
//! - **v1.10+**: SQLite `sessions.db` in `<data>/sessions/` — Linux and macOS
//!   `$XDG_DATA_HOME/goose` or `~/.local/share/goose`, Windows
//!   `%APPDATA%\Block\goose\data`; `$GOOSE_PATH_ROOT/data` overrides. Tables
//!   `sessions` (cwd, name, provider/model config, accumulated usage) and
//!   `messages` (`role`, `content_json` block array, `created_timestamp`,
//!   `metadata_json` with per-turn `usage` and `inference.resolvedModel`).
//!   Compaction and `/clear` delete and re-insert a session's messages.
//! - **v1.0–1.9**: `<data>/sessions/<id>.jsonl`, rewritten whole on save;
//!   line 1 is session metadata (no `role`), the rest are messages. Skipped
//!   when a `sessions.db` sits beside them (v1.10 imported them).
//!
//! Tool calls are `toolRequest` blocks on assistant messages and results are
//! `toolResponse` blocks on user messages. Sessions without per-message usage
//! get a trailing totals record from the session's accumulated counters.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{
    env_path, epoch_to_rfc3339, estimate_cost_for, opencode::columns, opencode::open_readonly,
    summarise_message, truncate, xdg_data_home, AgentSource, Enrichment, FileKind, SessionDoc,
};

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    if let Some(root) = env_path("GOOSE_PATH_ROOT") {
        return vec![root.join("data/sessions")];
    }
    let mut v = vec![xdg_data_home(home).join("goose/sessions")];
    if cfg!(windows) {
        if let Some(appdata) = env_path("APPDATA") {
            v.insert(0, appdata.join("Block/goose/data/sessions"));
        }
    }
    v
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    let base = name.trim_end_matches("-wal").trim_end_matches("-shm");
    if base == "sessions.db" {
        return Some(FileKind::Sqlite);
    }
    if name.ends_with(".jsonl") && !path.with_file_name("sessions.db").exists() {
        return Some(FileKind::Document);
    }
    None
}

/// Legacy JSONL session file.
pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let id = path.file_stem()?.to_str()?.to_owned();
    let mut lines = body.lines().filter(|l| !l.trim().is_empty());
    let mut session = json!({ "id": id });
    let mut records = Vec::new();
    if let Some(first) = lines.next() {
        let v: Value = serde_json::from_str(first).ok()?;
        if v.get("role").is_some() {
            records.push(json!({ "message": v }));
        } else {
            session = json!({
                "id": id,
                "working_dir": v.get("working_dir"),
                "name": v.get("description"),
                "accumulated_input_tokens": v.get("accumulated_input_tokens"),
                "accumulated_output_tokens": v.get("accumulated_output_tokens"),
            });
        }
    }
    for l in lines {
        if let Ok(m) = serde_json::from_str::<Value>(l) {
            records.push(json!({ "message": m }));
        }
    }
    Some(vec![finish(id, session, records)])
}

fn finish(id: String, session: Value, mut records: Vec<Value>) -> SessionDoc {
    let has_usage = records
        .iter()
        .any(|r| r.pointer("/message/metadata/usage").is_some());
    if !has_usage {
        let acc = |k: &str| session.get(k).and_then(Value::as_u64).unwrap_or(0);
        if acc("accumulated_input_tokens") + acc("accumulated_output_tokens") > 0 {
            records.push(json!({ "_usage_totals": {
                "input": acc("accumulated_input_tokens"),
                "output": acc("accumulated_output_tokens"),
                "cache_read": acc("accumulated_cache_read_tokens"),
                "cache_write": acc("accumulated_cache_write_tokens"),
                "cost": session.get("accumulated_cost"),
            }}));
        }
    }
    let mut ctx = Map::new();
    ctx.insert("sessionId".into(), json!(id));
    for (from, to) in [
        ("working_dir", "cwd"),
        ("name", "title"),
        ("model", "model"),
    ] {
        if let Some(s) = session
            .get(from)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            ctx.insert(to.into(), json!(s));
        }
    }
    let records = records
        .into_iter()
        .map(|mut r| {
            r["_trace"] = Value::Object(ctx.clone());
            r
        })
        .collect();
    SessionDoc {
        session_id: id,
        records,
    }
}

pub fn read_sqlite(path: &Path, cursor: &mut Option<String>) -> anyhow::Result<Vec<SessionDoc>> {
    let conn = open_readonly(path)?;
    // Cursor: "<max message id>|<max sessions.updated_at>".
    let (since, since_updated) = match cursor.as_deref() {
        Some(c) => {
            let (id, updated) = c.split_once('|').unwrap_or((c, ""));
            (
                id.parse::<i64>().unwrap_or(0),
                Some(updated.to_owned()).filter(|u| !u.is_empty()),
            )
        }
        None => (0, None),
    };
    // Message rowids only grow (rewrites re-insert), so the max id is a
    // reliable change marker; session metadata changes bump updated_at.
    let max_id: i64 = conn
        .query_row("SELECT COALESCE(MAX(id), 0) FROM messages", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let scols = columns(&conn, "sessions");
    let has_updated = scols.contains("updated_at");
    let max_updated: Option<String> = if has_updated {
        conn.query_row("SELECT MAX(updated_at) FROM sessions", [], |r| r.get(0))
            .ok()
            .flatten()
    } else {
        None
    };
    // A database restored from a backup or recreated starts below the saved
    // watermark: read it afresh.
    let went_back = max_id < since
        || match (&since_updated, &max_updated) {
            (Some(seen), Some(now)) => conn
                .query_row(
                    "SELECT julianday(?1) < julianday(?2)",
                    params![now, seen],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false),
            _ => false,
        };
    let (since, since_updated) = if went_back {
        (0, None)
    } else {
        (since, since_updated)
    };
    let mut changed: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT DISTINCT session_id FROM messages WHERE id > ?1")?;
        let rows = stmt.query_map(params![since], |r| r.get::<_, String>(0))?;
        changed.extend(rows.flatten());
    }
    if let (true, Some(watermark)) = (has_updated, &since_updated) {
        // Deletions (compaction) insert nothing, so the id check misses
        // them; they bump the session's updated_at. Comparing with the last
        // seen value (not a wall-clock window) also covers downtime.
        if let Ok(mut stmt) =
            conn.prepare("SELECT id FROM sessions WHERE julianday(updated_at) >= julianday(?1)")
        {
            if let Ok(rows) = stmt.query_map(params![watermark], |r| r.get::<_, String>(0)) {
                for id in rows.flatten() {
                    if !changed.contains(&id) {
                        changed.push(id);
                    }
                }
            }
        }
    }
    let mcols = columns(&conn, "messages");
    let col = |cols: &std::collections::BTreeSet<String>, c: &str| {
        if cols.contains(c) {
            c.to_owned()
        } else {
            format!("NULL AS {c}")
        }
    };
    let session_sql = format!(
        "SELECT id, {}, {}, {}, {}, {}, {}, {}, {} FROM sessions WHERE id = ?1",
        col(&scols, "working_dir"),
        col(&scols, "name"),
        col(&scols, "description"),
        col(&scols, "model_config_json"),
        col(&scols, "accumulated_input_tokens"),
        col(&scols, "accumulated_output_tokens"),
        col(&scols, "accumulated_cache_read_tokens"),
        col(&scols, "accumulated_cost"),
    );
    let msg_sql = format!(
        "SELECT role, content_json, created_timestamp, {}, {} FROM messages
         WHERE session_id = ?1 ORDER BY created_timestamp, id",
        col(&mcols, "metadata_json"),
        col(&mcols, "message_id"),
    );
    let mut docs = Vec::new();
    for sid in changed {
        let session: Option<Value> = conn
            .query_row(&session_sql, params![sid], |r| {
                let model_cfg: Option<String> = r.get(4)?;
                let model = model_cfg
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| {
                        v.get("model_name")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
                let name: Option<String> = r.get(2)?;
                let desc: Option<String> = r.get(3)?;
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "working_dir": r.get::<_, Option<String>>(1)?,
                    "name": name.filter(|n| !n.is_empty()).or(desc),
                    "model": model,
                    "accumulated_input_tokens": r.get::<_, Option<i64>>(5)?,
                    "accumulated_output_tokens": r.get::<_, Option<i64>>(6)?,
                    "accumulated_cache_read_tokens": r.get::<_, Option<i64>>(7)?,
                    "accumulated_cost": r.get::<_, Option<f64>>(8)?,
                }))
            })
            .ok();
        let Some(session) = session else { continue };
        let mut stmt = conn.prepare(&msg_sql)?;
        let rows = stmt.query_map(params![sid], |r| {
            Ok(json!({ "message": {
                "role": r.get::<_, String>(0)?,
                "content": serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                "created": r.get::<_, Option<i64>>(2)?,
                "metadata": r.get::<_, Option<String>>(3)?
                    .and_then(|m| serde_json::from_str::<Value>(&m).ok()),
                "id": r.get::<_, Option<String>>(4)?,
            }}))
        })?;
        let records: Vec<Value> = rows.flatten().collect();
        docs.push(finish(sid, session, records));
    }
    *cursor = Some(format!("{max_id}|{}", max_updated.unwrap_or_default()));
    Ok(docs)
}

fn blocks_of(content: &Value) -> Vec<Block> {
    let mut out = Vec::new();
    for b in content.as_array().into_iter().flatten() {
        match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                // Assistant-only annotations are UI chatter.
                if let Some(t) = b.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    out.push(Block::Text { text: t.to_owned() });
                }
            }
            "thinking" | "reasoning" => {
                if let Some(t) = b
                    .get("thinking")
                    .or_else(|| b.get("text"))
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    out.push(Block::Thinking { thinking: t.to_owned() });
                }
            }
            "toolRequest" | "frontendToolRequest" => {
                let call = b.get("toolCall").cloned().unwrap_or(Value::Null);
                let value = call.get("value").cloned().unwrap_or(Value::Null);
                out.push(Block::ToolUse {
                    id: b.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                    name: value.get("name").and_then(Value::as_str).unwrap_or("").to_owned(),
                    input: value.get("arguments").cloned().unwrap_or_else(|| json!({})),
                });
            }
            "toolResponse" => {
                let res = b.get("toolResult").cloned().unwrap_or(Value::Null);
                let is_error = res.get("status").and_then(Value::as_str) == Some("error")
                    || res.pointer("/value/isError").and_then(Value::as_bool) == Some(true);
                // Older builds: value is a bare Content array; newer: CallToolResult.
                let items = res
                    .pointer("/value/content")
                    .or_else(|| res.get("value"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let text = match &items {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => res
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                };
                out.push(Block::ToolResult {
                    tool_use_id: b.get("id").and_then(Value::as_str).unwrap_or("").to_owned(),
                    content: Value::String(text),
                    is_error,
                });
            }
            "image" => out.push(Block::Image {
                source: json!({"type": "base64", "media_type": b.get("mimeType"), "data": b.get("data")}),
            }),
            _ => {}
        }
    }
    out
}

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title"),
        ..Default::default()
    };
    if let Some(t) = raw.get("_usage_totals") {
        let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
        let u = TokenUsage {
            input: g("input"),
            output: g("output"),
            cache_read: g("cache_read"),
            cache_creation: g("cache_write"),
        };
        e.event_type = "system".into();
        e.model = cs("model");
        match t.get("cost").and_then(Value::as_f64) {
            Some(c) => {
                e.cost_usd = Some(c);
                e.cost_explicit = true;
            }
            None => {
                e.cost_usd = Some(estimate_cost_for(
                    AgentSource::Goose,
                    e.model.as_deref(),
                    &u,
                ))
            }
        }
        e.usage = Some(u);
        e.summary = "⚙️  Session token usage".into();
        return e;
    }
    let msg = raw.get("message").cloned().unwrap_or(Value::Null);
    e.timestamp = msg.get("created").and_then(epoch_to_rfc3339);
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let blocks = blocks_of(msg.get("content").unwrap_or(&Value::Null));
    for b in &blocks {
        match b {
            Block::ToolUse { name, .. } => e.tool_uses.push(name.clone()),
            Block::ToolResult { tool_use_id, .. } => e.tool_results.push(tool_use_id.clone()),
            _ => {}
        }
    }
    let meta = msg.get("metadata").cloned().unwrap_or(Value::Null);
    if role == "assistant" {
        e.event_type = "assistant".into();
        e.model = meta
            .pointer("/inference/resolvedModel")
            .or_else(|| meta.pointer("/inference/requestedModel"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| cs("model"));
        if let Some(u) = meta.get("usage") {
            let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            let (cr, cw) = (g("cacheReadTokens"), g("cacheWriteTokens"));
            let usage = TokenUsage {
                input: g("inputTokens").saturating_sub(cr + cw),
                output: g("outputTokens"),
                cache_read: cr,
                cache_creation: cw,
            };
            match u.get("cost").and_then(Value::as_f64) {
                Some(c) => {
                    e.cost_usd = Some(c);
                    e.cost_explicit =
                        u.get("costSource").and_then(Value::as_str) == Some("provider_reported");
                }
                None => {
                    e.cost_usd = Some(estimate_cost_for(
                        AgentSource::Goose,
                        e.model.as_deref(),
                        &usage,
                    ))
                }
            }
            e.usage = Some(usage);
            e.turn_end = e.tool_uses.is_empty();
        }
        e.message = Message::new(Role::Assistant, blocks).non_empty();
    } else {
        let only_results =
            !blocks.is_empty() && blocks.iter().all(|b| matches!(b, Block::ToolResult { .. }));
        e.event_type = if only_results { "tool_result" } else { "user" }.into();
        e.message = Message::new(Role::User, blocks).non_empty();
    }
    if meta.get("userVisible").and_then(Value::as_bool) == Some(false) && e.event_type == "user" {
        e.event_type = "system".into();
        if let Some(m) = e.message.take() {
            e.message = Message::new(Role::System, m.content).non_empty();
        }
    }
    e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
    if e.summary.len() > 200 {
        e.summary = truncate(&e.summary, 200);
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn sqlite_sessions_and_messages() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT, description TEXT, working_dir TEXT,
              updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, model_config_json TEXT,
              accumulated_input_tokens INTEGER, accumulated_output_tokens INTEGER,
              accumulated_cache_read_tokens INTEGER, accumulated_cost REAL);
            CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT, session_id TEXT,
              role TEXT, content_json TEXT, created_timestamp INTEGER, metadata_json TEXT);
            INSERT INTO sessions (id, name, description, working_dir, model_config_json)
              VALUES ('20260929_3', 'Fix flaky test', '', '/home/u/proj', '{"model_name":"claude-sonnet-4-5"}');
            INSERT INTO messages (message_id, session_id, role, content_json, created_timestamp, metadata_json) VALUES
              ('m1','20260929_3','user','[{"type":"text","text":"why does test_x flake?"}]',1759154590,'{"userVisible":true}'),
              ('m2','20260929_3','assistant','[{"type":"text","text":"Let me run it."},{"type":"toolRequest","id":"toolu_01","toolCall":{"status":"success","value":{"name":"developer__shell","arguments":{"command":"cargo test"}}}}]',1759154597,
               '{"inference":{"resolvedModel":"claude-sonnet-4-5-20250929"},"usage":{"inputTokens":10200,"outputTokens":80,"cacheReadTokens":9000,"cost":0.0071,"costSource":"estimated"}}'),
              ('m3','20260929_3','user','[{"type":"toolResponse","id":"toolu_01","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"test result: ok"}],"isError":false}}}]',1759154605,NULL);
        "#).unwrap();
        drop(conn);
        let mut cursor = None;
        let docs = read_sqlite(&db, &mut cursor).unwrap();
        assert!(cursor.as_deref().unwrap().starts_with("3|"));
        let recs: Vec<Enrichment> = docs[0].records.iter().map(enrich).collect();
        assert_eq!(recs.len(), 3, "per-message usage present, no totals record");
        assert_eq!(recs[0].cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(recs[1].tool_uses, vec!["developer__shell"]);
        assert_eq!(recs[1].model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(recs[1].usage.as_ref().unwrap().input, 1200);
        assert_eq!(recs[2].event_type, "tool_result");
        // Recently updated sessions are re-read (to catch compaction deletes),
        // but unchanged content hashes the same, so nothing is re-emitted.
        let again = read_sqlite(&db, &mut cursor).unwrap();
        assert!(again.is_empty() || again[0].records == docs[0].records);
    }

    #[test]
    fn deletions_while_stopped_are_seen_after_any_downtime() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE sessions (id TEXT PRIMARY KEY, working_dir TEXT, updated_at TIMESTAMP);
            CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT, session_id TEXT,
              role TEXT, content_json TEXT, created_timestamp INTEGER, metadata_json TEXT);
            INSERT INTO sessions VALUES ('s1', '/p', '2026-01-01 00:00:00');
            INSERT INTO messages (message_id, session_id, role, content_json) VALUES
              ('m1','s1','user','[{"type":"text","text":"one"}]'),
              ('m2','s1','assistant','[{"type":"text","text":"two"}]');
        "#).unwrap();
        let mut cursor = None;
        assert_eq!(read_sqlite(&db, &mut cursor).unwrap()[0].records.len(), 2);
        // Compaction a day later (well outside any recent-activity window):
        // a delete and an updated_at bump, but no new message id.
        conn.execute_batch(
            "DELETE FROM messages WHERE message_id = 'm2';
             UPDATE sessions SET updated_at = '2026-01-02 00:00:00' WHERE id = 's1';",
        )
        .unwrap();
        let docs = read_sqlite(&db, &mut cursor).unwrap();
        assert_eq!(docs.len(), 1, "the compacted session was not re-read");
        assert_eq!(docs[0].records.len(), 1);
    }

    #[test]
    fn a_database_restored_from_a_backup_is_read_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let schema = r#"
            CREATE TABLE sessions (id TEXT PRIMARY KEY, working_dir TEXT, updated_at TIMESTAMP);
            CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, message_id TEXT, session_id TEXT,
              role TEXT, content_json TEXT, created_timestamp INTEGER, metadata_json TEXT);"#;
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(schema).unwrap();
        conn.execute_batch(
            r#"INSERT INTO sessions VALUES ('s1', '/p', '2026-01-02 00:00:00');
               INSERT INTO messages (message_id, session_id, role, content_json) VALUES
                 ('m1','s1','user','[{"type":"text","text":"one"}]'),
                 ('m2','s1','assistant','[{"type":"text","text":"two"}]'),
                 ('m3','s1','user','[{"type":"text","text":"three"}]');"#,
        )
        .unwrap();
        drop(conn);
        let mut cursor = None;
        read_sqlite(&db, &mut cursor).unwrap();
        // Replaced by an older copy: fewer rows, an earlier update time.
        std::fs::remove_file(&db).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(schema).unwrap();
        conn.execute_batch(
            r#"INSERT INTO sessions VALUES ('s1', '/p', '2026-01-01 00:00:00');
               INSERT INTO messages (message_id, session_id, role, content_json) VALUES
                 ('m1','s1','user','[{"type":"text","text":"one"}]');"#,
        )
        .unwrap();
        drop(conn);
        let docs = read_sqlite(&db, &mut cursor).unwrap();
        assert_eq!(docs.len(), 1, "the replaced database was not re-read");
        assert_eq!(docs[0].records.len(), 1);
        assert!(cursor.as_deref().unwrap().starts_with("1|"));
    }

    #[test]
    fn legacy_jsonl_with_totals() {
        let body = r#"{"working_dir":"/home/u/proj","description":"Fix flaky test","accumulated_input_tokens":39000,"accumulated_output_tokens":1211}
{"id":"msg_1","role":"user","created":1759154590,"content":[{"type":"text","text":"why?"}],"metadata":{"userVisible":true,"agentVisible":true}}
{"id":"msg_3","role":"user","created":1759154605,"content":[{"type":"toolResponse","id":"t","toolResult":{"status":"success","value":[{"type":"text","text":"ok"}]}}]}
"#;
        let d = parse_document(
            Path::new("/nonexistent/sessions/20250101_120000.jsonl"),
            body,
        )
        .unwrap()
        .remove(0);
        assert_eq!(d.session_id, "20250101_120000");
        assert_eq!(d.records.len(), 3);
        let r = enrich(&d.records[1]);
        let (_, c, _) = r.message.as_ref().unwrap().tool_results().next().unwrap();
        assert_eq!(c, "ok");
        let t = enrich(&d.records[2]);
        assert_eq!(t.usage.as_ref().unwrap().input, 39000);
    }
}
