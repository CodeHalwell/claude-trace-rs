//! Crush (Charm) adapter.
//!
//! Crush keeps one SQLite database per project at `<project>/.crush/crush.db`.
//! The global data dir (`$CRUSH_GLOBAL_DATA`, else `$XDG_DATA_HOME/crush`,
//! else `~/.local/share/crush`, Windows `%LOCALAPPDATA%\crush`) holds a
//! `projects.json` listing every project's data dir — that list is what we
//! watch.
//!
//! `messages.parts` is a JSON array of `{type, data}` parts (text,
//! reasoning, tool_call, tool_result, finish, …); tool results are separate
//! rows with `role = "tool"`. Rows are updated in place while streaming.
//! Crush persists no per-message token usage — only a cumulative session
//! `cost` — so each session gets a trailing cost record.

use std::path::{Path, PathBuf};

use rusqlite::params;
use serde_json::{json, Map, Value};

use crate::message::{parse_json_arguments, Block, Message, Role};
use crate::sources::{
    env_path, epoch_to_rfc3339, opencode::columns, opencode::open_readonly, summarise_message,
    xdg_data_home, Enrichment, FileKind, SessionDoc,
};

pub fn global_dir(home: &Path) -> PathBuf {
    if let Some(p) = env_path("CRUSH_GLOBAL_DATA") {
        return p;
    }
    if cfg!(windows) && std::env::var_os("XDG_DATA_HOME").is_none() {
        if let Some(local) = env_path("LOCALAPPDATA") {
            return local.join("crush");
        }
    }
    xdg_data_home(home).join("crush")
}

/// Every project data dir Crush has used (from `projects.json`).
pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let list = global_dir(home).join("projects.json");
    let Some(v) = std::fs::read_to_string(list)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return Vec::new();
    };
    v.get("projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("data_dir").and_then(Value::as_str))
        .map(PathBuf::from)
        .collect()
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    let base = name.trim_end_matches("-wal").trim_end_matches("-shm");
    (base == "crush.db").then_some(FileKind::Sqlite)
}

pub fn read_sqlite(path: &Path, cursor: &mut Option<String>) -> anyhow::Result<Vec<SessionDoc>> {
    let conn = open_readonly(path)?;
    let since: i64 = cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(i64::MIN);
    // The project directory is the parent of `.crush`.
    let cwd = path
        .parent()
        .filter(|d| d.file_name().and_then(|n| n.to_str()) == Some(".crush"))
        .and_then(|d| d.parent())
        .map(|p| p.to_string_lossy().to_string());
    let mut changed: Vec<(String, i64)> = Vec::new();
    {
        // Second resolution: re-read the boundary second (`>=`).
        let mut stmt = conn.prepare(
            "SELECT session_id, MAX(updated_at) FROM messages WHERE updated_at >= ?1 GROUP BY session_id",
        )?;
        let rows = stmt.query_map(params![since], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        changed.extend(rows.flatten());
    }
    let mcols = columns(&conn, "messages");
    let provider = if mcols.contains("provider") {
        "provider"
    } else {
        "NULL AS provider"
    };
    let mut max_seen = since;
    let mut docs = Vec::new();
    for (sid, t) in changed {
        max_seen = max_seen.max(t);
        let session = conn
            .query_row(
                "SELECT title, cost, parent_session_id, created_at FROM sessions WHERE id = ?1",
                params![sid],
                |r| {
                    Ok(json!({
                        "title": r.get::<_, Option<String>>(0)?,
                        "cost": r.get::<_, Option<f64>>(1)?,
                        "parent": r.get::<_, Option<String>>(2)?,
                        "created": r.get::<_, Option<i64>>(3)?,
                    }))
                },
            )
            .unwrap_or(Value::Null);
        let mut ctx = Map::new();
        ctx.insert("sessionId".into(), json!(sid));
        if let Some(c) = &cwd {
            ctx.insert("cwd".into(), json!(c));
        }
        if let Some(t) = session
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            ctx.insert("title".into(), json!(t));
        }
        let sql = format!(
            "SELECT id, role, parts, model, {provider}, created_at, finished_at FROM messages
             WHERE session_id = ?1 ORDER BY created_at, rowid"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![sid], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "role": r.get::<_, String>(1)?,
                "parts": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
                "model": r.get::<_, Option<String>>(3)?,
                "provider": r.get::<_, Option<String>>(4)?,
                "created_at": r.get::<_, Option<i64>>(5)?,
                "finished_at": r.get::<_, Option<i64>>(6)?,
            }))
        })?;
        let mut records: Vec<Value> = rows
            .flatten()
            .map(|m| json!({ "message": m, "_trace": ctx }))
            .collect();
        if let Some(cost) = session
            .get("cost")
            .and_then(Value::as_f64)
            .filter(|c| *c > 0.0)
        {
            records.push(json!({ "_session_cost": cost, "_trace": ctx }));
        }
        docs.push(SessionDoc {
            session_id: sid,
            records,
        });
    }
    if max_seen > i64::MIN {
        *cursor = Some(max_seen.to_string());
    }
    Ok(docs)
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
    if let Some(c) = raw.get("_session_cost").and_then(Value::as_f64) {
        e.event_type = "system".into();
        e.cost_usd = Some(c);
        e.cost_explicit = true;
        e.summary = format!("⚙️  Session cost ${c:.4}");
        return e;
    }
    let msg = raw.get("message").cloned().unwrap_or(Value::Null);
    e.timestamp = msg.get("created_at").and_then(epoch_to_rfc3339);
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let mut blocks = Vec::new();
    let mut finish: Option<String> = None;
    for p in msg
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let d = p.get("data").cloned().unwrap_or(Value::Null);
        let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        match p.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if d.get("hidden").and_then(Value::as_bool) != Some(true) && !s("text").is_empty() {
                    blocks.push(Block::Text { text: s("text") });
                }
            }
            "reasoning" => {
                if !s("thinking").is_empty() {
                    blocks.push(Block::Thinking {
                        thinking: s("thinking"),
                    });
                }
            }
            "tool_call" => {
                e.tool_uses.push(s("name"));
                blocks.push(Block::ToolUse {
                    id: s("id"),
                    name: s("name"),
                    input: parse_json_arguments(d.get("input")),
                });
            }
            "tool_result" => {
                e.tool_results.push(s("tool_call_id"));
                blocks.push(Block::ToolResult {
                    tool_use_id: s("tool_call_id"),
                    content: Value::String(s("content")),
                    is_error: d.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                });
            }
            "shell_command" => blocks.push(Block::Text {
                text: format!("$ {}\n{}", s("command"), s("output")),
            }),
            "image_url" => blocks.push(Block::Image {
                source: json!({"type": "url", "url": d.get("url")}),
            }),
            "finish" => finish = Some(s("reason")),
            _ => {}
        }
    }
    match role {
        "assistant" => {
            e.event_type = "assistant".into();
            e.model = msg
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned);
            e.turn_end = finish.as_deref() == Some("end_turn");
            e.message = Message::new(Role::Assistant, blocks).non_empty();
        }
        "tool" => {
            e.event_type = "tool_result".into();
            e.message = Message::new(Role::User, blocks).non_empty();
        }
        "system" => {
            e.event_type = "system".into();
            e.message = Message::new(Role::System, blocks).non_empty();
        }
        _ => {
            e.event_type = "user".into();
            e.message = Message::new(Role::User, blocks).non_empty();
        }
    }
    e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn project_db() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("proj/.crush");
        std::fs::create_dir_all(&data).unwrap();
        let db = data.join("crush.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(r#"
            CREATE TABLE sessions (id TEXT PRIMARY KEY, parent_session_id TEXT, title TEXT, cost REAL, created_at INTEGER, updated_at INTEGER);
            CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT, parts TEXT, model TEXT, provider TEXT,
              created_at INTEGER, updated_at INTEGER, finished_at INTEGER);
            INSERT INTO sessions VALUES ('s1', NULL, 'Fix flaky test', 0.0831, 1759154590, 1759154650);
            INSERT INTO messages VALUES ('m1','s1','user','[{"type":"text","data":{"text":"why does test_x flake?"}},{"type":"finish","data":{"reason":"stop","time":0}}]','',NULL,1759154590,1759154590,NULL);
            INSERT INTO messages VALUES ('m2','s1','assistant','[{"type":"reasoning","data":{"thinking":"look"}},{"type":"text","data":{"text":"Let me look."}},{"type":"tool_call","data":{"id":"toolu_01","name":"view","input":"{\"file_path\":\"x_test.go\"}","finished":true}},{"type":"finish","data":{"reason":"tool_use","time":1759154597}}]','claude-sonnet-4-5','anthropic',1759154591,1759154597,1759154597);
            INSERT INTO messages VALUES ('m3','s1','tool','[{"type":"tool_result","data":{"tool_call_id":"toolu_01","name":"view","content":"<file>…</file>","is_error":false}}]','',NULL,1759154598,1759154598,NULL);
        "#).unwrap();
        drop(conn);
        let mut cursor = None;
        let docs = read_sqlite(&db, &mut cursor).unwrap();
        let recs: Vec<Enrichment> = docs[0].records.iter().map(enrich).collect();
        assert_eq!(recs.len(), 4);
        assert_eq!(
            recs[0].cwd.as_deref(),
            Some(dir.path().join("proj").to_string_lossy().as_ref())
        );
        assert_eq!(recs[1].tool_uses, vec!["view"]);
        assert_eq!(recs[1].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(recs[2].tool_results, vec!["toolu_01"]);
        assert_eq!(recs[3].cost_usd, Some(0.0831));
        assert_eq!(cursor.as_deref(), Some("1759154598"));
    }
}
