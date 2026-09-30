//! OpenCode adapter.
//!
//! OpenCode keeps everything under `$XDG_DATA_HOME/opencode` (default
//! `~/.local/share/opencode` on every OS, including macOS and Windows):
//!
//! - **v1.2+**: a SQLite database `opencode.db` (`opencode-<channel>.db` for
//!   non-release builds, `$OPENCODE_DB` to override). Sessions, messages and
//!   parts are rows whose JSON `data` omits the ids (they are columns); rows
//!   are upserted in place as a turn streams, bumping `time_updated` (ms).
//! - **≤ v1.1**: a multi-file JSON store — `storage/session/<project>/<id>.json`,
//!   `storage/message/<session>/<id>.json`, `storage/part/<message>/<id>.json`
//!   — each rewritten in place.
//!
//! Both are assembled into the same record per message:
//! `{"session": Session.Info, "info": Message.Info, "parts": [Part]}`.
//! Tool calls and their results share one `tool` part; per-step token usage
//! lives on `step-finish` parts (the message's own `tokens` holds only the
//! last step), and `cost` is reported by OpenCode itself.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{
    env_path, epoch_to_rfc3339, estimate_cost_for, summarise_message, truncate, xdg_data_home,
    AgentSource, Enrichment, FileKind, SessionDoc,
};

pub fn data_dir(home: &Path) -> PathBuf {
    xdg_data_home(home).join("opencode")
}

pub fn default_dirs(home: &Path) -> Vec<PathBuf> {
    let mut v = vec![data_dir(home)];
    if let Some(db) = env_path("OPENCODE_DB").filter(|p| p.is_absolute()) {
        if let Some(parent) = db.parent() {
            v.push(parent.to_path_buf());
        }
    }
    v
}

pub fn classify(path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    let base = name
        .trim_end_matches("-wal")
        .trim_end_matches("-shm")
        .trim_end_matches("-journal");
    if base.starts_with("opencode") && base.ends_with(".db") {
        return Some(FileKind::Sqlite);
    }
    if !name.ends_with(".json") {
        return None;
    }
    let comps: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    let n = comps.len();
    if n >= 4 && comps[n - 4] == "storage" && matches!(comps[n - 3], "session" | "message" | "part")
    {
        return Some(FileKind::StoreMember);
    }
    None
}

pub fn skip_dir(dir: &Path) -> bool {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parent = dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    // Parts are reached through their session; snapshots are git objects.
    (name == "part" && parent == "storage")
        || matches!(name, "snapshot" | "log" | "bin" | "tool-output")
}

// ---------------------------------------------------------------------------
// Legacy JSON store
// ---------------------------------------------------------------------------

/// Map a changed store file to its session's unit: `storage/message/<sid>`.
pub fn store_unit(path: &Path) -> Option<PathBuf> {
    let kind_dir = path.parent()?.parent()?;
    let storage = kind_dir.parent()?;
    let kind = kind_dir.file_name()?.to_str()?;
    let sid = match kind {
        "session" => path.file_stem()?.to_str()?.to_owned(),
        "message" => path.parent()?.file_name()?.to_str()?.to_owned(),
        "part" => match std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.get("sessionID")?.as_str().map(str::to_owned))
        {
            Some(sid) => sid,
            // Deleted (or mid-write): the part folder is named after its
            // message, which says which session it belongs to.
            None => session_of_message(storage, path.parent()?.file_name()?.to_str()?)?,
        },
        _ => return None,
    };
    Some(storage.join("message").join(sid))
}

/// The session whose `message/<sid>/` folder holds message `mid`.
fn session_of_message(storage: &Path, mid: &str) -> Option<String> {
    let file = format!("{mid}.json");
    std::fs::read_dir(storage.join("message"))
        .ok()?
        .flatten()
        .find(|d| d.path().join(&file).is_file())
        .map(|d| d.file_name().to_string_lossy().into_owned())
}

pub fn load_store_unit(unit: &Path) -> Option<Vec<SessionDoc>> {
    let sid = unit.file_name()?.to_str()?.to_owned();
    let storage = unit.parent()?.parent()?;
    let session = find_session_file(storage, &sid)
        .and_then(|p| read_json(&p))
        .unwrap_or_else(|| json!({ "id": sid }));

    let mut msg_files: Vec<PathBuf> = std::fs::read_dir(unit)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    msg_files.sort();

    let mut records = Vec::new();
    for mf in msg_files {
        // A file caught mid-rewrite fails to parse: retry on the next change.
        let info = read_json(&mf)?;
        let mid = info
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut part_files: Vec<PathBuf> = std::fs::read_dir(storage.join("part").join(&mid))
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        part_files.sort();
        let parts: Vec<Value> = part_files.iter().filter_map(|p| read_json(p)).collect();
        records.push(json!({ "session": session, "info": info, "parts": parts }));
    }
    Some(vec![SessionDoc {
        session_id: sid,
        records,
    }])
}

fn find_session_file(storage: &Path, sid: &str) -> Option<PathBuf> {
    let dir = storage.join("session");
    for proj in std::fs::read_dir(dir).ok()?.flatten() {
        let p = proj.path().join(format!("{sid}.json"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

// ---------------------------------------------------------------------------
// SQLite store
// ---------------------------------------------------------------------------

pub(crate) fn open_readonly(path: &Path) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_millis(2000))?;
    Ok(conn)
}

pub(crate) fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

pub(crate) fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            out.extend(rows.flatten());
        }
    }
    out
}

/// Read every session touched since `cursor` (max `time_updated`, ms).
pub fn read_sqlite(path: &Path, cursor: &mut Option<String>) -> anyhow::Result<Vec<SessionDoc>> {
    let conn = open_readonly(path)?;
    if !has_table(&conn, "session") || !has_table(&conn, "message") {
        return Ok(Vec::new());
    }
    let since: i64 = cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(i64::MIN);
    let mut changed: HashMap<String, i64> = HashMap::new();
    let mut note = |sid: String, t: i64| {
        let e = changed.entry(sid).or_insert(t);
        *e = (*e).max(t);
    };
    // `>=`: an update landing in the same millisecond as the last one we saw
    // must not be missed; re-reading a session is harmless.
    for sql in [
        "SELECT id, time_updated FROM session WHERE time_updated >= ?1",
        "SELECT session_id, MAX(time_updated) FROM message WHERE time_updated >= ?1 GROUP BY session_id",
        "SELECT m.session_id, MAX(p.time_updated) FROM part p JOIN message m ON m.id = p.message_id
         WHERE p.time_updated >= ?1 GROUP BY m.session_id",
    ] {
        if let Ok(mut stmt) = conn.prepare(sql) {
            let rows = stmt.query_map(params![since], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0)))
            })?;
            for (sid, t) in rows.flatten() {
                note(sid, t);
            }
        }
    }
    let mut max_seen = since;
    let mut docs = Vec::new();
    for (sid, t) in changed {
        max_seen = max_seen.max(t);
        if let Some(doc) = load_session(&conn, &sid)? {
            docs.push(doc);
        }
    }
    if max_seen > i64::MIN {
        *cursor = Some(max_seen.to_string());
    }
    Ok(docs)
}

fn load_session(conn: &Connection, sid: &str) -> anyhow::Result<Option<SessionDoc>> {
    let cols = columns(conn, "session");
    let opt = |c: &str| {
        if cols.contains(c) {
            c.to_owned()
        } else {
            format!("NULL AS {c}")
        }
    };
    let sql = format!(
        "SELECT id, {}, {}, directory, title, version, time_created, time_updated FROM session WHERE id = ?1",
        opt("project_id"),
        opt("parent_id")
    );
    let session: Option<Value> = conn
        .query_row(&sql, params![sid], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "projectID": r.get::<_, Option<String>>(1)?,
                "parentID": r.get::<_, Option<String>>(2)?,
                "directory": r.get::<_, Option<String>>(3)?,
                "title": r.get::<_, Option<String>>(4)?,
                "version": r.get::<_, Option<String>>(5)?,
                "time": {
                    "created": r.get::<_, Option<i64>>(6)?,
                    "updated": r.get::<_, Option<i64>>(7)?,
                },
            }))
        })
        .optional()?;
    let Some(session) = session else {
        return Ok(None);
    };

    let mut parts_by_msg: HashMap<String, Vec<Value>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT p.id, p.message_id, p.data FROM part p JOIN message m ON m.id = p.message_id
             WHERE m.session_id = ?1 ORDER BY p.message_id, p.id",
        )?;
        let rows = stmt.query_map(params![sid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for (pid, mid, data) in rows.flatten() {
            let mut v: Value = serde_json::from_str(&data).unwrap_or_else(|_| json!({}));
            if let Some(o) = v.as_object_mut() {
                o.insert("id".into(), json!(pid));
                o.insert("messageID".into(), json!(mid));
                o.insert("sessionID".into(), json!(sid));
            }
            parts_by_msg.entry(mid).or_default().push(v);
        }
    }
    let mut stmt = conn
        .prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created, id")?;
    let rows = stmt.query_map(params![sid], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut records = Vec::new();
    for (mid, data) in rows.flatten() {
        let mut info: Value = serde_json::from_str(&data).unwrap_or_else(|_| json!({}));
        if let Some(o) = info.as_object_mut() {
            o.insert("id".into(), json!(mid));
            o.insert("sessionID".into(), json!(sid));
        }
        let parts = parts_by_msg.remove(&mid).unwrap_or_default();
        records.push(json!({ "session": session, "info": info, "parts": parts }));
    }
    Ok(Some(SessionDoc {
        session_id: sid.to_owned(),
        records,
    }))
}

// ---------------------------------------------------------------------------
// Enrichment
// ---------------------------------------------------------------------------

pub fn enrich(raw: &Value) -> Enrichment {
    let session = raw.get("session").cloned().unwrap_or(Value::Null);
    let info = raw.get("info").cloned().unwrap_or(Value::Null);
    let parts: Vec<Value> = raw
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let role = info.get("role").and_then(Value::as_str).unwrap_or("");
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);

    let mut e = Enrichment {
        session_id: s(&info, "sessionID").or_else(|| s(&session, "id")),
        timestamp: info.pointer("/time/created").and_then(epoch_to_rfc3339),
        cwd: info
            .pointer("/path/cwd")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| s(&session, "directory").filter(|d| !d.is_empty())),
        version: s(&session, "version"),
        title: s(&session, "title")
            .filter(|t| !t.starts_with("New session - ") && !t.starts_with("Child session - ")),
        ..Default::default()
    };

    let mut blocks: Vec<Block> = Vec::new();
    let mut step_usage = TokenUsage::default();
    let mut steps = 0;
    for p in &parts {
        let kind = p.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "text" => {
                if p.get("ignored").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if let Some(t) = p
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    blocks.push(Block::Text { text: t.to_owned() });
                }
            }
            "reasoning" => {
                if let Some(t) = p
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    blocks.push(Block::Thinking {
                        thinking: t.to_owned(),
                    });
                }
            }
            "file" => {
                let mime = p.get("mime").and_then(Value::as_str).unwrap_or("");
                let name = p
                    .get("filename")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                if mime.starts_with("image/") {
                    blocks.push(Block::Image {
                        source: json!({ "type": "url", "url": p.get("url"), "media_type": mime }),
                    });
                } else {
                    blocks.push(Block::Text {
                        text: format!("[file: {name}]"),
                    });
                }
            }
            "tool" => {
                let id = p
                    .get("callID")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let name = p
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let state = p.get("state").cloned().unwrap_or(Value::Null);
                e.tool_uses.push(name.clone());
                blocks.push(Block::ToolUse {
                    id: id.clone(),
                    name,
                    input: state.get("input").cloned().unwrap_or_else(|| json!({})),
                });
                match state.get("status").and_then(Value::as_str) {
                    Some("completed") => {
                        e.tool_results.push(id.clone());
                        blocks.push(Block::ToolResult {
                            tool_use_id: id,
                            content: state.get("output").cloned().unwrap_or(Value::Null),
                            is_error: false,
                        });
                    }
                    Some("error") => {
                        e.tool_results.push(id.clone());
                        blocks.push(Block::ToolResult {
                            tool_use_id: id,
                            content: state.get("error").cloned().unwrap_or(Value::Null),
                            is_error: true,
                        });
                    }
                    _ => {}
                }
            }
            "subtask" => {
                if let Some(t) = p.get("prompt").and_then(Value::as_str) {
                    blocks.push(Block::Text {
                        text: format!("[subtask] {t}"),
                    });
                }
            }
            "step-finish" => {
                if let Some(t) = p.get("tokens") {
                    steps += 1;
                    add_tokens(&mut step_usage, t);
                }
            }
            _ => {}
        }
    }

    if role == "assistant" {
        e.event_type = "assistant".into();
        e.model = s(&info, "modelID");
        let usage = if steps > 0 {
            Some(step_usage)
        } else {
            info.get("tokens").map(|t| {
                let mut u = TokenUsage::default();
                add_tokens(&mut u, t);
                u
            })
        };
        e.usage = usage.filter(|u| u.input + u.output + u.cache_read + u.cache_creation > 0);
        match info.get("cost").and_then(Value::as_f64) {
            Some(c) => {
                e.cost_usd = Some(c);
                e.cost_explicit = true;
            }
            None => {
                if let Some(u) = &e.usage {
                    e.cost_usd = Some(estimate_cost_for(
                        AgentSource::OpenCode,
                        e.model.as_deref(),
                        u,
                    ));
                }
            }
        }
        let finish = info.get("finish").and_then(Value::as_str).unwrap_or("");
        let completed = info.pointer("/time/completed").is_some();
        e.turn_end = completed && !matches!(finish, "tool-calls" | "tool_calls" | "unknown" | "");
        e.message = Message::new(Role::Assistant, blocks).non_empty();
        e.summary = match info.pointer("/error/name").and_then(Value::as_str) {
            Some(err) => {
                let msg = info
                    .pointer("/error/data/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                e.event_type = "error".into();
                format!("⛔ {err}: {}", truncate(msg, 90))
            }
            None => summarise_message("assistant", e.message.as_ref(), &e.tool_uses),
        };
        if info.get("summary").and_then(Value::as_bool) == Some(true) {
            e.event_type = "summary".into();
            e.summary = format!(
                "📝 Summary: {}",
                truncate(
                    &e.message
                        .as_ref()
                        .map(|m| m.plain_text())
                        .unwrap_or_default(),
                    100
                )
            );
        }
    } else {
        e.event_type = if role.is_empty() { "unknown" } else { "user" }.into();
        e.model = info
            .pointer("/model/modelID")
            .and_then(Value::as_str)
            .map(str::to_owned);
        e.message = Message::new(Role::User, blocks).non_empty();
        e.summary = summarise_message("user", e.message.as_ref(), &[]);
    }
    e
}

/// OpenCode `tokens`: `input` already excludes cache reads/writes.
fn add_tokens(u: &mut TokenUsage, t: &Value) {
    let g = |p: &str| t.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    u.input += g("/input");
    u.output += g("/output") + g("/reasoning");
    u.cache_read += g("/cache/read");
    u.cache_creation += g("/cache/write");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_record() -> Value {
        json!({
            "session": {"id":"ses_1","directory":"/home/u/proj","title":"Fix flaky test","version":"1.18.33"},
            "info": {"id":"msg_2","sessionID":"ses_1","role":"assistant",
                "time":{"created":1759154601000i64,"completed":1759154609876i64},
                "modelID":"claude-sonnet-4-5","providerID":"anthropic",
                "path":{"cwd":"/home/u/proj","root":"/home/u/proj"},
                "cost":0.0123,
                "tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},
                "finish":"tool-calls"},
            "parts": [
                {"type":"reasoning","text":"check tests"},
                {"type":"text","text":"Running them."},
                {"type":"tool","callID":"toolu_01","tool":"bash",
                 "state":{"status":"completed","input":{"command":"cargo test"},"output":"ok. 12 passed"}},
                {"type":"step-finish","cost":0.006,"tokens":{"input":12,"output":345,"reasoning":5,"cache":{"read":14000,"write":877}}},
                {"type":"step-finish","cost":0.006,"tokens":{"input":8,"output":20,"reasoning":0,"cache":{"read":14100,"write":0}}}
            ]
        })
    }

    #[test]
    fn assistant_message_with_tool_and_step_usage() {
        let e = enrich(&assistant_record());
        assert_eq!(e.event_type, "assistant");
        assert_eq!(e.session_id.as_deref(), Some("ses_1"));
        assert_eq!(e.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(e.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(e.tool_uses, vec!["bash"]);
        let u = e.usage.unwrap();
        assert_eq!(u.input, 20);
        assert_eq!(u.output, 370);
        assert_eq!(u.cache_read, 28100);
        assert_eq!(u.cache_creation, 877);
        assert_eq!(e.cost_usd, Some(0.0123));
        assert!(e.cost_explicit);
        assert!(!e.turn_end);
        let m = e.message.unwrap();
        assert!(m
            .tool_results()
            .any(|(id, c, _)| id == "toolu_01" && c == "ok. 12 passed"));
        assert_eq!(e.title.as_deref(), Some("Fix flaky test"));
    }

    #[test]
    fn user_message_and_default_title_ignored() {
        let e = enrich(&json!({
            "session": {"id":"ses_1","title":"New session - 2026-01-01T00:00:00Z"},
            "info": {"id":"msg_1","sessionID":"ses_1","role":"user","time":{"created":1759154590000i64}},
            "parts": [{"type":"text","text":"why does test_x flake?"},{"type":"text","text":"x","ignored":true}]
        }));
        assert_eq!(e.event_type, "user");
        assert_eq!(e.message.unwrap().plain_text(), "why does test_x flake?");
        assert!(e.title.is_none());
        assert!(e.timestamp.unwrap().starts_with("2025-09-29"));
    }

    #[test]
    fn sqlite_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id text PRIMARY KEY, project_id text, parent_id text, slug text,
                directory text, title text, version text, time_created integer, time_updated integer);
             CREATE TABLE message (id text PRIMARY KEY, session_id text, time_created integer,
                time_updated integer, data text);
             CREATE TABLE part (id text PRIMARY KEY, message_id text, session_id text,
                time_created integer, time_updated integer, data text);
             INSERT INTO session VALUES ('ses_1','p','',NULL,'/home/u/proj','Fix it','1.18.33',1000,1000);
             INSERT INTO message VALUES ('msg_1','ses_1',1001,1001,'{\"role\":\"user\",\"time\":{\"created\":1001}}');
             INSERT INTO part VALUES ('prt_1','msg_1','ses_1',1001,1001,'{\"type\":\"text\",\"text\":\"hello\"}');
             INSERT INTO message VALUES ('msg_2','ses_1',1002,1005,'{\"role\":\"assistant\",\"modelID\":\"gpt-5\",\"cost\":0.01,\"time\":{\"created\":1002,\"completed\":1005},\"finish\":\"stop\"}');
             INSERT INTO part VALUES ('prt_2','msg_2','ses_1',1002,1005,'{\"type\":\"text\",\"text\":\"hi there\"}');",
        )
        .unwrap();
        drop(conn);

        let mut cursor = None;
        let docs = read_sqlite(&db, &mut cursor).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].records.len(), 2);
        assert_eq!(cursor.as_deref(), Some("1005"));
        let a = enrich(&docs[0].records[1]);
        assert!(a.turn_end);
        assert_eq!(a.message.unwrap().plain_text(), "hi there");

        // Nothing changed since the cursor except the boundary session.
        let again = read_sqlite(&db, &mut cursor).unwrap();
        assert_eq!(again.len(), 1, "boundary row re-read (>=) but harmless");
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE message SET time_updated = 900 WHERE id='msg_2'", [])
            .unwrap();
        conn.execute("UPDATE part SET time_updated = 900", [])
            .unwrap();
        conn.execute("UPDATE session SET time_updated = 900", [])
            .unwrap();
        conn.execute("UPDATE message SET time_updated = 900", [])
            .unwrap();
        drop(conn);
        assert!(read_sqlite(&db, &mut cursor).unwrap().is_empty());
    }

    #[test]
    fn legacy_json_store() {
        let dir = tempfile::tempdir().unwrap();
        let st = dir.path().join("storage");
        std::fs::create_dir_all(st.join("session/proj1")).unwrap();
        std::fs::create_dir_all(st.join("message/ses_1")).unwrap();
        std::fs::create_dir_all(st.join("part/msg_1")).unwrap();
        std::fs::write(
            st.join("session/proj1/ses_1.json"),
            r#"{"id":"ses_1","directory":"/p","title":"T","version":"1.1.0"}"#,
        )
        .unwrap();
        std::fs::write(
            st.join("message/ses_1/msg_1.json"),
            r#"{"id":"msg_1","sessionID":"ses_1","role":"user","time":{"created":1}}"#,
        )
        .unwrap();
        let part = st.join("part/msg_1/prt_1.json");
        std::fs::write(
            &part,
            r#"{"id":"prt_1","sessionID":"ses_1","messageID":"msg_1","type":"text","text":"hey"}"#,
        )
        .unwrap();

        assert_eq!(classify(&part), Some(FileKind::StoreMember));
        let unit = store_unit(&part).unwrap();
        assert_eq!(unit, st.join("message/ses_1"));
        assert_eq!(
            store_unit(&st.join("session/proj1/ses_1.json")).unwrap(),
            unit
        );
        let docs = load_store_unit(&unit).unwrap();
        assert_eq!(docs[0].session_id, "ses_1");
        // A deleted part still resolves through its message.
        let parked = dir.path().join("prt_1.json");
        std::fs::rename(&part, &parked).unwrap();
        assert_eq!(store_unit(&part).unwrap(), unit);
        std::fs::rename(&parked, &part).unwrap();
        assert_eq!(
            enrich(&docs[0].records[0]).message.unwrap().plain_text(),
            "hey"
        );
        assert_eq!(
            classify(Path::new("/d/opencode/opencode.db-wal")),
            Some(FileKind::Sqlite)
        );
    }
}
