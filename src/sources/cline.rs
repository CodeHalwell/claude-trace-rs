//! Cline, Roo Code and Kilo Code adapter.
//!
//! Three related agents share this module:
//!
//! - **Classic task layout** (Cline, Roo Code, Kilo Code ≤ 5.x): each task is a
//!   directory `tasks/<taskId>/` under the extension's VS Code globalStorage
//!   (`saoudrizwan.claude-dev`, `rooveterinaryinc.roo-cline`,
//!   `kilocode.kilo-code`), under `~/.cline/data/` for Cline's standalone
//!   builds, `~/.vscode-mock/global-storage/` for the Roo CLI, or
//!   `~/.kilocode/cli/global/` for the old Kilo CLI.
//!   `api_conversation_history.json` is an Anthropic Messages array,
//!   rewritten whole on every change; newer Cline adds per-message `ts`,
//!   `modelInfo` and `metrics` (tokens and cost). Roo and old Cline carry no
//!   usage there — it lives in `ui_messages.json`'s `api_req_started`
//!   entries, which are folded in as a session-totals record.
//! - **Cline SDK layout** (Cline 4.x SDK bundle, Cline CLI 3.x):
//!   `~/.cline/data/sessions/<id>/<id>.messages.json` (`{version, messages}`)
//!   beside a `<id>.json` manifest holding cwd, model and title.
//! - **Kilo Code 7.x** is an OpenCode fork: `$XDG_DATA_HOME/kilo/kilo.db`,
//!   read with the OpenCode SQLite reader.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::event::TokenUsage;
use crate::message::{anthropic_blocks, Block, Message, Role};
use crate::sources::{
    any_timestamp, env_path, estimate_cost_for, opencode, summarise_message, truncate,
    xdg_data_home, AgentSource, Enrichment, FileKind, SessionDoc,
};

const API_HISTORY: &str = "api_conversation_history.json";
const UI_MESSAGES: &str = "ui_messages.json";

/// VS Code-family `User/globalStorage` directories on this platform,
/// including forks and remote-server installs.
pub fn vscode_global_storage_roots() -> Vec<PathBuf> {
    let Some(dirs) = directories::BaseDirs::new() else {
        return Vec::new();
    };
    let home = dirs.home_dir().to_path_buf();
    let editors = [
        "Code",
        "Code - Insiders",
        "VSCodium",
        "Cursor",
        "Windsurf",
        "Trae",
        "Kiro",
    ];
    let mut out = Vec::new();
    #[cfg(target_os = "macos")]
    let base = home.join("Library/Application Support");
    #[cfg(target_os = "windows")]
    let base = env_path("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = dirs.config_dir().to_path_buf();
    for e in editors {
        out.push(base.join(e).join("User/globalStorage"));
    }
    for server in [
        ".vscode-server",
        ".cursor-server",
        ".windsurf-server",
        ".vscodium-server",
    ] {
        out.push(home.join(server).join("data/User/globalStorage"));
    }
    out
}

/// Session directories for Cline, Roo Code or Kilo Code.
pub fn default_task_dirs(source: AgentSource) -> Vec<PathBuf> {
    let home = directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let ids: &[&str] = match source {
        AgentSource::RooCode => &[
            "rooveterinaryinc.roo-cline",
            "rooveterinaryinc.roo-code",
            "rooveterinaryinc.roo-code-nightly",
        ],
        AgentSource::KiloCode => &["kilocode.kilo-code"],
        _ => &["saoudrizwan.claude-dev"],
    };
    let mut out: Vec<PathBuf> = vscode_global_storage_roots()
        .into_iter()
        .flat_map(|root| ids.iter().map(move |id| root.join(id).join("tasks")))
        .collect();
    match source {
        AgentSource::RooCode => out.push(home.join(".vscode-mock/global-storage/tasks")),
        AgentSource::KiloCode => {
            out.push(home.join(".kilocode/cli/global/tasks"));
            // Kilo 7.x (OpenCode fork).
            out.push(xdg_data_home(&home).join("kilo"));
        }
        _ => {
            let cline_dir = env_path("CLINE_DIR").unwrap_or_else(|| home.join(".cline"));
            out.push(env_path("CLINE_DATA_DIR").unwrap_or_else(|| cline_dir.join("data")));
        }
    }
    out
}

/// Classic task files. `ui_messages.json` is read as part of its task (for
/// usage totals) rather than as a transcript of its own: it is the UI view
/// of the same conversation, and ingesting it separately would duplicate
/// every turn.
pub fn matches_file(path: &Path) -> bool {
    matches!(path.file_name().and_then(|n| n.to_str()), Some(API_HISTORY)) || is_sdk_messages(path)
}

fn is_sdk_messages(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".messages.json"))
        && path
            .ancestors()
            .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("sessions"))
}

pub fn classify(source: AgentSource, path: &Path) -> Option<FileKind> {
    let name = path.file_name()?.to_str()?;
    if source == AgentSource::KiloCode {
        let base = name.trim_end_matches("-wal").trim_end_matches("-shm");
        if base.starts_with("kilo") && base.ends_with(".db") {
            return Some(FileKind::Sqlite);
        }
    }
    if matches_file(path) {
        return Some(FileKind::Document);
    }
    // A usage update in ui_messages.json re-reads its task.
    if name == UI_MESSAGES && path.with_file_name(API_HISTORY).exists() {
        return Some(FileKind::Document);
    }
    None
}

/// `ui_messages.json` belongs to its sibling API history.
pub fn unit_path(path: &Path) -> PathBuf {
    if path.file_name().and_then(|n| n.to_str()) == Some(UI_MESSAGES) {
        return path.with_file_name(API_HISTORY);
    }
    path.to_path_buf()
}

/// Roo and Kilo share Cline's filenames; attribute by path.
pub fn source_for_path(path: &Path) -> AgentSource {
    let p = path.to_string_lossy().to_ascii_lowercase();
    if p.contains("roo-cline") || p.contains("roo-code") || p.contains(".vscode-mock") {
        AgentSource::RooCode
    } else if p.contains("kilocode") || p.contains("/kilo/") {
        AgentSource::KiloCode
    } else {
        AgentSource::Cline
    }
}

/// Parse a task's API history. `source` is the variant already chosen for
/// it (forced by the root, or from the path): Roo and Kilo count usage
/// differently from Cline.
pub fn parse_document(source: AgentSource, path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    if is_sdk_messages(path) {
        return parse_sdk(path, body);
    }
    let arr: Vec<Value> = serde_json::from_str(body).ok()?;
    let task_dir = path.parent()?;
    let session_id = task_dir.file_name()?.to_str()?.to_owned();
    let meta = task_meta(task_dir, &session_id);
    let has_metrics = arr.iter().any(|m| m.get("metrics").is_some());

    let mut records: Vec<Value> = arr
        .into_iter()
        .map(|m| with_ctx(m, &session_id, &meta))
        .collect();
    // Without per-message metrics, fold the UI log's api_req_started totals
    // into one trailing record so the session still reports tokens and cost.
    if !has_metrics {
        if let Some(totals) = ui_usage_totals(&task_dir.join(UI_MESSAGES), source) {
            records.push(with_ctx(
                json!({ "_usage_totals": totals }),
                &session_id,
                &meta,
            ));
        }
    }
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

/// Context recovered from the task's surroundings: cwd, model and title.
fn task_meta(task_dir: &Path, task_id: &str) -> Map<String, Value> {
    let mut meta = Map::new();
    // Roo ≥ 3.49: per-task history_item.json.
    if let Some(v) = read_json(&task_dir.join("history_item.json")) {
        copy_str(&v, "workspace", &mut meta, "cwd");
        copy_str(&v, "task", &mut meta, "title");
    }
    // Cline: <storage>/state/taskHistory.json with cwdOnTaskInitialization.
    if let Some(storage) = task_dir.parent().and_then(|t| t.parent()) {
        if let Some(Value::Array(items)) = read_json(&storage.join("state/taskHistory.json")) {
            if let Some(item) = items
                .iter()
                .find(|i| i.get("id").and_then(Value::as_str) == Some(task_id))
            {
                copy_str(item, "cwdOnTaskInitialization", &mut meta, "cwd");
                copy_str(item, "modelId", &mut meta, "model");
                copy_str(item, "task", &mut meta, "title");
            }
        }
    }
    meta
}

fn copy_str(src: &Value, from: &str, dst: &mut Map<String, Value>, to: &str) {
    if dst.contains_key(to) {
        return;
    }
    if let Some(s) = src
        .get(from)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        dst.insert(to.into(), json!(s));
    }
}

fn with_ctx(mut m: Value, session_id: &str, meta: &Map<String, Value>) -> Value {
    if let Some(obj) = m.as_object_mut() {
        let mut ctx = meta.clone();
        ctx.insert("sessionId".into(), json!(session_id));
        obj.insert("_trace".into(), Value::Object(ctx));
    }
    m
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Sum `api_req_started` usage from `ui_messages.json`.
fn ui_usage_totals(path: &Path, source: AgentSource) -> Option<Value> {
    let Value::Array(msgs) = read_json(path)? else {
        return None;
    };
    let (mut tin, mut tout, mut cw, mut cr, mut cost, mut n) = (0u64, 0u64, 0u64, 0u64, 0f64, 0);
    for m in &msgs {
        if m.get("say").and_then(Value::as_str) != Some("api_req_started") {
            continue;
        }
        let Some(info) = m
            .get("text")
            .and_then(Value::as_str)
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
        else {
            continue;
        };
        let g = |k: &str| info.get(k).and_then(Value::as_u64).unwrap_or(0);
        let (i, w, r) = (g("tokensIn"), g("cacheWrites"), g("cacheReads"));
        // Roo and Kilo report tokensIn *including* cache; Cline excludes it.
        tin += if source == AgentSource::Cline {
            i
        } else {
            i.saturating_sub(w + r)
        };
        tout += g("tokensOut");
        cw += w;
        cr += r;
        cost += info.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
        n += 1;
    }
    (n > 0).then(|| {
        json!({"input": tin, "output": tout, "cache_write": cw, "cache_read": cr, "cost": cost, "requests": n})
    })
}

/// Cline SDK `<id>.messages.json` plus its manifest.
fn parse_sdk(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let msgs = match &v {
        Value::Array(a) => a.clone(),
        other => other.get("messages")?.as_array()?.clone(),
    };
    let dir = path.parent()?;
    let root_id = dir.file_name()?.to_str()?.to_owned();
    let stem = path
        .file_name()?
        .to_str()?
        .trim_end_matches(".messages.json")
        .to_owned();
    // Subagent/team transcripts share the root's directory.
    let session_id = if stem == root_id {
        root_id.clone()
    } else {
        format!("{root_id}:{stem}")
    };
    let mut meta = Map::new();
    if let Some(manifest) = read_json(&dir.join(format!("{root_id}.json"))) {
        copy_str(&manifest, "cwd", &mut meta, "cwd");
        copy_str(&manifest, "model", &mut meta, "model");
        if let Some(t) = manifest.pointer("/metadata/title").and_then(Value::as_str) {
            meta.insert("title".into(), json!(t));
        }
    }
    let records = msgs
        .into_iter()
        .map(|m| with_ctx(m, &session_id, &meta))
        .collect();
    Some(vec![SessionDoc {
        session_id,
        records,
    }])
}

// ---------------------------------------------------------------------------
// Enrichment
// ---------------------------------------------------------------------------

pub fn enrich(raw: &Value, source: AgentSource) -> Enrichment {
    // Kilo 7.x rows are OpenCode-shaped.
    if raw.get("info").is_some() && raw.get("parts").is_some() {
        return opencode::enrich(raw);
    }
    // Stand-alone ui_messages.json records (older ingests, custom roots).
    if raw.get("ts").is_some() && (raw.get("say").is_some() || raw.get("ask").is_some()) {
        return enrich_ui_message(raw);
    }
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        title: cs("title").map(|t| truncate(&t, 120)),
        timestamp: any_timestamp(raw.get("ts")),
        ..Default::default()
    };

    if let Some(t) = raw.get("_usage_totals") {
        let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
        e.event_type = "system".into();
        e.model = cs("model");
        e.usage = Some(TokenUsage {
            input: g("input"),
            output: g("output"),
            cache_read: g("cache_read"),
            cache_creation: g("cache_write"),
        });
        e.cost_usd = t.get("cost").and_then(Value::as_f64);
        e.cost_explicit = true;
        e.summary = format!("⚙️  Usage across {} API request(s)", g("requests"));
        return e;
    }

    let role = raw.get("role").and_then(Value::as_str).unwrap_or("");
    let content = raw.get("content").cloned().unwrap_or(Value::Null);
    let mut blocks = anthropic_blocks(&content);
    // SDK/Roo extras: reasoning blocks, SDK images `{data, mediaType}`.
    if let Value::Array(arr) = &content {
        for b in arr {
            match b.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    let text = b
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| {
                            b.get("summary").and_then(Value::as_array).map(|s| {
                                s.iter()
                                    .filter_map(|x| x.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                        })
                        .unwrap_or_default();
                    if !text.is_empty() {
                        blocks.push(Block::Thinking { thinking: text });
                    }
                }
                Some("file") => {
                    if let Some(p) = b.get("path").and_then(Value::as_str) {
                        blocks.push(Block::Text {
                            text: format!("[file: {p}]"),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(r) = raw.get("reasoning_content").and_then(Value::as_str) {
        blocks.insert(
            0,
            Block::Thinking {
                thinking: r.to_owned(),
            },
        );
    }
    for b in &blocks {
        match b {
            Block::ToolUse { name, .. } => e.tool_uses.push(name.clone()),
            Block::ToolResult { tool_use_id, .. } => e.tool_results.push(tool_use_id.clone()),
            _ => {}
        }
    }

    // cwd / model from Roo/Cline environment details on user turns.
    if role == "user" {
        let text = Message::new(Role::User, blocks.clone()).plain_text();
        if e.cwd.is_none() {
            e.cwd = env_detail_cwd(&text);
        }
        if let Some(m) = between(&text, "<model>", "</model>") {
            e.model = Some(m);
        }
    }

    e.model = raw
        .pointer("/modelInfo/modelId")
        .or_else(|| raw.pointer("/modelInfo/id"))
        .or_else(|| raw.get("model"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(e.model)
        .or_else(|| (role == "assistant").then(|| cs("model")).flatten());

    if let Some(m) = raw.get("metrics") {
        let g = |p: &str| m.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        let u = if m.get("inputTokens").is_some() {
            // SDK metrics: inputTokens includes cache tokens.
            let (r, w) = (g("/cacheReadTokens"), g("/cacheWriteTokens"));
            TokenUsage {
                input: g("/inputTokens").saturating_sub(r + w),
                output: g("/outputTokens"),
                cache_read: r,
                cache_creation: w,
            }
        } else {
            // Classic: prompt excludes cache; `cached` lumps reads and writes.
            TokenUsage {
                input: g("/tokens/prompt"),
                output: g("/tokens/completion"),
                cache_read: g("/tokens/cached"),
                cache_creation: 0,
            }
        };
        if u.input + u.output + u.cache_read + u.cache_creation > 0 {
            e.usage = Some(u);
        }
        if let Some(c) = m.get("cost").and_then(Value::as_f64) {
            e.cost_usd = Some(c);
            e.cost_explicit = true;
        }
    } else if let Some(u) = raw.get("usage") {
        // Anthropic-style usage (older custom exports).
        let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let usage = TokenUsage {
            input: g("input_tokens"),
            output: g("output_tokens"),
            cache_read: g("cache_read_input_tokens"),
            cache_creation: g("cache_creation_input_tokens"),
        };
        if usage.input + usage.output + usage.cache_read + usage.cache_creation > 0 {
            e.usage = Some(usage);
        }
    }
    if e.cost_usd.is_none() {
        if let Some(u) = &e.usage {
            e.cost_usd = Some(estimate_cost_for(source, e.model.as_deref(), u));
        }
    }

    let msg_role = if role == "user" {
        Role::User
    } else {
        Role::Assistant
    };
    e.event_type = if role == "user" { "user" } else { "assistant" }.into();
    e.message = Message::new(msg_role, blocks).non_empty();
    e.summary = summarise_message(&e.event_type, e.message.as_ref(), &e.tool_uses);
    e.turn_end = role == "assistant"
        && e.tool_uses
            .iter()
            .any(|t| t == "attempt_completion" || t == "ask_followup_question");
    e
}

fn env_detail_cwd(text: &str) -> Option<String> {
    for marker in [
        "Current Working Directory (",
        "Current Workspace Directory (",
    ] {
        if let Some(start) = text.find(marker) {
            let rest = &text[start + marker.len()..];
            if let Some(end) = rest.find(')') {
                let p = rest[..end].trim();
                if !p.is_empty() {
                    return Some(p.to_owned());
                }
            }
        }
    }
    None
}

fn between(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    let s = text[start..end].trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// `ui_messages.json` records: `say` is agent output, `ask` prompts the user.
fn enrich_ui_message(raw: &Value) -> Enrichment {
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
    let sub = raw
        .get("say")
        .or_else(|| raw.get("ask"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let text = raw.get("text").and_then(Value::as_str).unwrap_or("");
    let (event_type, emoji) = match (kind, sub) {
        (_, "user_feedback") | ("ask", _) => ("user", "👤"),
        ("say", "text") => ("assistant", "🤖"),
        ("say", "completion_result") => ("assistant", "✅"),
        ("say", "api_req_started") => ("system", "⚙️"),
        ("say", "command" | "command_output") => ("tool_use", "🔧"),
        ("say", "tool" | "use_mcp_server" | "mcp_server_request_started") => ("tool_use", "🔧"),
        ("say", "error") => ("system", "⛔"),
        _ => ("system", "⚙️"),
    };
    let mut tool_uses = Vec::new();
    if sub == "command" {
        tool_uses.push("execute_command".to_owned());
    } else if matches!(sub, "tool" | "use_mcp_server") {
        if let Some(t) = raw.get("tool").and_then(Value::as_str) {
            tool_uses.push(t.to_owned());
        }
    }
    Enrichment {
        event_type: event_type.to_owned(),
        timestamp: any_timestamp(raw.get("ts")),
        model: raw.get("model").and_then(Value::as_str).map(str::to_owned),
        tool_uses,
        summary: format!("{emoji} {}", truncate(text, 100)),
        turn_end: sub == "completion_result",
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_matcher() {
        assert!(matches_file(Path::new(
            "/x/tasks/1/api_conversation_history.json"
        )));
        assert!(!matches_file(Path::new("/x/tasks/1/ui_messages.json")));
        assert!(!matches_file(Path::new(
            "/x/tasks/1/api_conversation_history.json.tmp.1.2.json"
        )));
        assert!(matches_file(Path::new(
            "/h/.cline/data/sessions/17_ab/17_ab.messages.json"
        )));
        assert!(!matches_file(Path::new("/x/other.json")));
    }

    #[test]
    fn classic_task_with_metrics_and_task_history_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("saoudrizwan.claude-dev");
        let task = storage.join("tasks/1790604003");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::create_dir_all(storage.join("state")).unwrap();
        std::fs::write(
            storage.join("state/taskHistory.json"),
            r#"[{"id":"1790604003","task":"Add a --since flag","cwdOnTaskInitialization":"/home/dan/proj","modelId":"claude-sonnet-4-5"}]"#,
        )
        .unwrap();
        let body = r#"[{"role":"user","content":[{"type":"text","text":"<task>\nAdd a --since flag\n</task>"}],"ts":1790604003140},
 {"role":"assistant","content":[{"type":"text","text":"Reading."},{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"src/cli.rs"}}],"modelInfo":{"modelId":"claude-sonnet-4-5-20250929","providerId":"anthropic","mode":"act"},"metrics":{"tokens":{"prompt":223,"completion":212,"cached":14650},"cost":0.0583},"ts":1790604009871}]"#;
        let path = task.join(API_HISTORY);
        std::fs::write(&path, body).unwrap();
        let docs = parse_document(AgentSource::Cline, &path, body).unwrap();
        assert_eq!(docs[0].session_id, "1790604003");
        assert_eq!(docs[0].records.len(), 2);
        let a = enrich(&docs[0].records[1], AgentSource::Cline);
        assert_eq!(a.cwd.as_deref(), Some("/home/dan/proj"));
        assert_eq!(a.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(a.cost_usd, Some(0.0583));
        assert!(a.cost_explicit);
        assert_eq!(a.tool_uses, vec!["read_file"]);
        assert_eq!(a.title.as_deref(), Some("Add a --since flag"));
        assert!(a.timestamp.is_some());
    }

    #[test]
    fn roo_usage_from_ui_messages_and_env_details() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("rooveterinaryinc.roo-cline/tasks/t9");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join(UI_MESSAGES),
            r#"[{"ts":1,"type":"say","say":"api_req_started","text":"{\"tokensIn\":1500,\"tokensOut\":100,\"cacheWrites\":200,\"cacheReads\":1000,\"cost\":0.02}"}]"#,
        )
        .unwrap();
        let body = r#"[{"role":"user","content":[{"type":"text","text":"<user_message>hi</user_message>\n<environment_details>\n# Current Workspace Directory (/work/app) Files\n<model>gpt-5</model>\n</environment_details>"}],"ts":1},{"role":"assistant","content":"hello","ts":2}]"#;
        let path = task.join(API_HISTORY);
        std::fs::write(&path, body).unwrap();
        assert_eq!(source_for_path(&path), AgentSource::RooCode);
        let docs = parse_document(AgentSource::RooCode, &path, body).unwrap();
        assert_eq!(docs[0].records.len(), 3, "usage totals appended");
        let u = enrich(&docs[0].records[0], AgentSource::RooCode);
        assert_eq!(u.cwd.as_deref(), Some("/work/app"));
        assert_eq!(u.model.as_deref(), Some("gpt-5"));
        let totals = enrich(&docs[0].records[2], AgentSource::RooCode);
        let usage = totals.usage.unwrap();
        assert_eq!(usage.input, 300, "Roo tokensIn includes cache");
        assert_eq!(usage.cache_read, 1000);
        assert_eq!(totals.cost_usd, Some(0.02));
        assert_eq!(unit_path(&task.join(UI_MESSAGES)), path);
    }

    #[test]
    fn sdk_session_layout() {
        let dir = tempfile::tempdir().unwrap();
        let sdir = dir.path().join(".cline/data/sessions/1790_ab12c");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            sdir.join("1790_ab12c.json"),
            r#"{"version":1,"session_id":"1790_ab12c","cwd":"/home/dan/proj","model":"claude-sonnet-4-5","metadata":{"title":"Add a flag"}}"#,
        )
        .unwrap();
        let body = r#"{"version":1,"sessionId":"1790_ab12c","messages":[
          {"id":"m1","role":"user","content":[{"type":"text","text":"Add a --since flag"}],"ts":1790604000200},
          {"id":"m2","role":"assistant","content":[{"type":"text","text":"Reading."},{"type":"tool_use","id":"toolu_01","name":"read_files","input":{"paths":["src/cli.rs"]}}],"ts":1790604009871,
           "modelInfo":{"id":"claude-sonnet-4-5","provider":"anthropic"},
           "metrics":{"inputTokens":14873,"outputTokens":212,"cacheReadTokens":0,"cacheWriteTokens":14650,"cost":0.0583}},
          {"id":"m3","role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","name":"read_files","content":"fn main(){}"}],"ts":1790604010100}]}"#;
        let path = sdir.join("1790_ab12c.messages.json");
        let docs = parse_document(AgentSource::Cline, &path, body).unwrap();
        assert_eq!(docs[0].session_id, "1790_ab12c");
        let a = enrich(&docs[0].records[1], AgentSource::Cline);
        let u = a.usage.unwrap();
        assert_eq!(u.input, 223);
        assert_eq!(u.cache_creation, 14650);
        assert_eq!(a.cwd.as_deref(), Some("/home/dan/proj"));
        let r = enrich(&docs[0].records[2], AgentSource::Cline);
        assert_eq!(r.tool_results, vec!["toolu_01"]);
        // A team/subagent transcript in the same directory is its own session.
        let child =
            parse_document(AgentSource::Cline, &sdir.join("agent7.messages.json"), body).unwrap();
        assert_eq!(child[0].session_id, "1790_ab12c:agent7");
    }

    #[test]
    fn ui_message_records_still_enrich() {
        let e = enrich(
            &json!({"ts":1750000000000i64,"type":"say","say":"text","text":"working on it"}),
            AgentSource::Cline,
        );
        assert_eq!(e.event_type, "assistant");
        assert!(e.summary.contains("working on it"));
        let c = enrich(
            &json!({"ts":1,"type":"say","say":"command","text":"ls -la"}),
            AgentSource::Cline,
        );
        assert_eq!(c.tool_uses, vec!["execute_command"]);
    }
}
