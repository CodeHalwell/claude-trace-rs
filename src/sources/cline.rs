//! Cline adapter.
//!
//! Cline is a VS Code extension (`saoudrizwan.claude-dev`). It stores each
//! task under the editor's globalStorage directory:
//!
//! ```text
//! <globalStorage>/saoudrizwan.claude-dev/tasks/<taskId>/
//!     api_conversation_history.json   — Anthropic Messages array (the API log)
//!     ui_messages.json                — UI messages ("say"/"ask" records)
//! ```
//!
//! These are whole-file JSON **arrays**, not JSONL, so the loader/watcher
//! special-case Cline files: the array is parsed once and each element is
//! ingested with a synthetic line index (its position in the array). Re-reads
//! are harmless because persistence is keyed on `(session_id, line_index)`.
//!
//! The API conversation history uses Anthropic message shapes
//! (`{role, content:[{type:text|tool_use|tool_result,…}]}`), so enrichment
//! closely follows the Claude Code adapter, plus Cline's `say`/`ask` UI
//! records from `ui_messages.json`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::event::TokenUsage;
use crate::sources::{estimate_cost, truncate, Enrichment};

/// Cline task directories for the current platform, rooted at the VS Code
/// globalStorage directory. We cover stable VS Code plus the common forks
/// (Insiders, VSCodium, Cursor — Cline can be installed in any of them).
pub fn default_task_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(dirs) = directories::BaseDirs::new() else {
        return out;
    };
    #[allow(unused_variables)]
    let home = dirs.home_dir();

    #[cfg(target_os = "macos")]
    let roots: Vec<PathBuf> = vec![
        home.join("Library/Application Support/Code/User/globalStorage"),
        home.join("Library/Application Support/Code - Insiders/User/globalStorage"),
        home.join("Library/Application Support/VSCodium/User/globalStorage"),
        home.join("Library/Application Support/Cursor/User/globalStorage"),
    ];
    #[cfg(target_os = "windows")]
    let roots: Vec<PathBuf> = {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        vec![
            PathBuf::from(&appdata).join("Code/User/globalStorage"),
            PathBuf::from(&appdata).join("Code - Insiders/User/globalStorage"),
            PathBuf::from(&appdata).join("VSCodium/User/globalStorage"),
            PathBuf::from(&appdata).join("Cursor/User/globalStorage"),
        ]
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let roots: Vec<PathBuf> = {
        let config = dirs.config_dir().to_path_buf();
        vec![
            config.join("Code/User/globalStorage"),
            config.join("Code - Insiders/User/globalStorage"),
            config.join("VSCodium/User/globalStorage"),
            config.join("Cursor/User/globalStorage"),
        ]
    };

    for r in roots {
        let d = r.join("saoudrizwan.claude-dev/tasks");
        if d.is_dir() {
            out.push(d);
        }
    }
    out
}

/// File matcher for Cline tasks: the API conversation history. This is a
/// whole-file JSON array (not JSONL), so matching files are ingested via the
/// whole-file path in the loader/watcher.
///
/// `ui_messages.json` is deliberately **not** matched. It is the UI-layer view
/// of the very same conversation, so ingesting both would record every Cline
/// turn twice. Worse, both files sit in `tasks/<taskId>/` and so share a
/// session id and a 0-based index space: their records collide on the
/// `(session_id, line_index)` key, which means double-counted events without a
/// database and silently dropped events with one (whichever file `read_dir`
/// happened to yield second lost its records). The API history is the
/// authoritative record — it alone carries token usage and cost — so it is the
/// one we ingest.
pub fn matches_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("api_conversation_history.json")
    )
}

/// Normalise one Cline record (one element of either array file).
pub fn enrich(raw: &Value) -> Enrichment {
    // ui_messages.json entries: {"ts":…,"type":"say"|"ask","say":"text",…}
    if raw.get("ts").is_some() && (raw.get("say").is_some() || raw.get("ask").is_some()) {
        return enrich_ui_message(raw);
    }
    enrich_api_message(raw)
}

/// API conversation history entries are Anthropic messages:
/// `{role:"user"|"assistant", content: string | [blocks]}`.
fn enrich_api_message(raw: &Value) -> Enrichment {
    let role = raw.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let event_type = if role == "user" { "user" } else { "assistant" }.to_owned();

    let model = raw.get("model").and_then(|v| v.as_str()).map(str::to_owned);
    let (tool_uses, tool_results) = extract_content_kinds(raw);
    let usage = extract_usage(raw);
    let cost_usd = raw
        .get("cost")
        .or_else(|| raw.get("costUSD"))
        .and_then(|v| v.as_f64())
        .or_else(|| usage.as_ref().map(|u| estimate_cost(model.as_deref(), u)));

    let preview = text_preview(raw, 120);
    let summary = if event_type == "user" {
        if preview.is_empty() {
            if !tool_results.is_empty() {
                format!("📦 Tool result ×{}", tool_results.len())
            } else {
                "👤 User".to_owned()
            }
        } else {
            format!("👤 {preview}")
        }
    } else if !tool_uses.is_empty() {
        let tools = tool_uses.join(", ");
        if preview.is_empty() {
            format!("🔧 {tools}")
        } else {
            format!("🤖 {preview} · 🔧 {tools}")
        }
    } else if !preview.is_empty() {
        format!("🤖 {preview}")
    } else {
        "🤖 Assistant".to_owned()
    };

    Enrichment {
        event_type,
        session_id: None, // filled from the task directory name by the loader
        timestamp: None,
        cwd: None,
        git_branch: None,
        version: None,
        model,
        tool_uses,
        tool_results,
        usage,
        cost_usd,
        summary,
    }
}

/// UI messages: `say` records are assistant→user output, `ask` records are
/// prompts/confirmations shown to the user.
fn enrich_ui_message(raw: &Value) -> Enrichment {
    let kind = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let sub = raw
        .get("say")
        .or_else(|| raw.get("ask"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let text = raw.get("text").and_then(|v| v.as_str()).unwrap_or("");

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
        if let Some(t) = raw.get("tool").and_then(|v| v.as_str()) {
            tool_uses.push(t.to_owned());
        }
    }

    Enrichment {
        event_type: event_type.to_owned(),
        session_id: None,
        timestamp: raw
            .get("ts")
            .and_then(|v| v.as_i64())
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.to_rfc3339())),
        cwd: None,
        git_branch: None,
        version: None,
        model: raw.get("model").and_then(|v| v.as_str()).map(str::to_owned),
        tool_uses,
        tool_results: Vec::new(),
        usage: None,
        cost_usd: None,
        summary: format!("{emoji} {}", truncate(text, 100)),
    }
}

fn extract_content_kinds(val: &Value) -> (Vec<String>, Vec<String>) {
    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();
    if let Some(arr) = val.get("content").and_then(|c| c.as_array()) {
        for block in arr {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("tool_use") => {
                    if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                        tool_uses.push(name.to_owned());
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(|v| v.as_str()) {
                        tool_results.push(id.to_owned());
                    }
                }
                _ => {}
            }
        }
    }
    (tool_uses, tool_results)
}

fn extract_usage(val: &Value) -> Option<TokenUsage> {
    let usage = val.get("usage")?;
    let input = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
        return None;
    }
    Some(TokenUsage {
        input,
        output,
        cache_read,
        cache_creation,
    })
}

fn text_preview(val: &Value, max_len: usize) -> String {
    match val.get("content") {
        Some(Value::String(s)) => truncate(s, max_len),
        Some(Value::Array(arr)) => {
            for b in arr {
                if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        return truncate(t, max_len);
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_matcher() {
        assert!(matches_file(Path::new("/x/api_conversation_history.json")));
        // ui_messages.json is the UI view of the same conversation: ingesting
        // it too would duplicate every turn and collide on (session_id,
        // line_index) with the API history.
        assert!(!matches_file(Path::new("/x/ui_messages.json")));
        assert!(!matches_file(Path::new("/x/other.json")));
        assert!(!matches_file(Path::new("/x/s.jsonl")));
    }

    #[test]
    fn api_message_user() {
        let e = enrich(&json!({"role":"user","content":"fix the tests"}));
        assert_eq!(e.event_type, "user");
        assert!(e.summary.contains("fix the tests"));
    }

    #[test]
    fn api_message_assistant_tool_use() {
        let e = enrich(&json!({
            "role":"assistant",
            "content":[{"type":"tool_use","name":"read_file","id":"t1","input":{}}],
            "usage":{"input_tokens":10,"output_tokens":5}
        }));
        assert_eq!(e.event_type, "assistant");
        assert_eq!(e.tool_uses, vec!["read_file"]);
        assert!(e.usage.is_some());
    }

    #[test]
    fn ui_message_say_text() {
        let e = enrich(
            &json!({"ts":1750000000000i64,"type":"say","say":"text","text":"working on it"}),
        );
        assert_eq!(e.event_type, "assistant");
        assert!(e.summary.contains("working on it"));
        assert!(e.timestamp.is_some());
    }

    #[test]
    fn ui_message_command_is_tool_use() {
        let e = enrich(&json!({"ts":1,"type":"say","say":"command","text":"ls -la"}));
        assert_eq!(e.event_type, "tool_use");
        assert_eq!(e.tool_uses, vec!["execute_command"]);
    }

    #[test]
    fn ui_message_ask_is_user() {
        let e = enrich(&json!({"ts":1,"type":"ask","ask":"followup","text":"proceed?"}));
        assert_eq!(e.event_type, "user");
    }
}
