//! Training-dataset friendly export of Claude Code session events.
//!
//! Six output shapes are supported:
//!
//! * `messages`     — Anthropic Messages API (one JSON object per line, the
//!                    `messages` field holds the role/content turns with all
//!                    content blocks preserved).
//! * `openai`       — OpenAI chat-completion shape with `tool_calls` /
//!                    `tool_call_id` translated from Claude's tool_use /
//!                    tool_result blocks.
//! * `sharegpt`     — `{conversations: [{from, value}]}` (HF / Axolotl /
//!                    Unsloth standard).
//! * `jsonl`        — Raw Claude Code JSONL passthrough. Full fidelity; one
//!                    line per original entry.
//! * `markdown`     — Human-readable transcript (`# User` / `# Assistant` /
//!                    fenced `tool_use` blocks). For review, not training.
//! * `huggingface`  — A directory containing `train.jsonl` + `dataset_info.json`
//!                    + `README.md` so the result is directly usable with
//!                    `datasets.load_dataset("json", data_dir=...)`.

use std::{collections::HashMap, fmt::Write, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{event::TraceEvent, state::SessionStats};

/// Pick the on-the-wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum ExportFormat {
    /// Anthropic Messages API shape — `{messages: [{role, content}]}`.
    Messages,
    /// OpenAI Chat / Tools shape — `{messages: [{role, content, tool_calls}]}`.
    Openai,
    /// ShareGPT — `{conversations: [{from, value}]}`.
    Sharegpt,
    /// Raw Claude Code JSONL passthrough (one line per entry).
    Jsonl,
    /// Human-readable markdown transcript.
    Markdown,
    /// HuggingFace `datasets`-compatible directory layout.
    Huggingface,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "md",
            ExportFormat::Huggingface => "", // it's a directory
            _ => "jsonl",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "text/markdown; charset=utf-8",
            ExportFormat::Jsonl
            | ExportFormat::Messages
            | ExportFormat::Openai
            | ExportFormat::Sharegpt
            | ExportFormat::Huggingface => "application/x-ndjson",
        }
    }
}

/// One session ready for export — every event plus aggregate stats.
pub struct SessionExport<'a> {
    pub stats: &'a SessionStats,
    pub events: &'a [TraceEvent],
}

/// Render a single session to a string in the chosen format.
pub fn render_session(sess: &SessionExport<'_>, format: ExportFormat) -> String {
    match format {
        ExportFormat::Messages => render_messages_line(sess),
        ExportFormat::Openai => render_openai_line(sess),
        ExportFormat::Sharegpt => render_sharegpt_line(sess),
        ExportFormat::Jsonl => render_raw_jsonl(sess),
        ExportFormat::Markdown => render_markdown(sess),
        ExportFormat::Huggingface => render_messages_line(sess),
    }
}

/// Render a multi-session export.  For line-based formats this concatenates
/// one record per session; for markdown it produces a stitched document with
/// `---` separators.
pub fn render_many(sessions: &[SessionExport<'_>], format: ExportFormat) -> String {
    match format {
        ExportFormat::Markdown => sessions
            .iter()
            .map(|s| render_markdown(s))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        ExportFormat::Jsonl => sessions.iter().map(render_raw_jsonl).collect::<String>(),
        _ => sessions
            .iter()
            .map(|s| render_session(s, format))
            .collect::<String>(),
    }
}

// -- Anthropic Messages --------------------------------------------------------

fn render_messages_line(sess: &SessionExport<'_>) -> String {
    let messages: Vec<Value> = sess
        .events
        .iter()
        .filter_map(|ev| match ev.event_type.as_str() {
            "user" => Some(json!({
                "role": "user",
                "content": extract_message_content(&ev.entry),
                "timestamp": ev.timestamp,
            })),
            "assistant" => Some(json!({
                "role": "assistant",
                "content": extract_message_content(&ev.entry),
                "model": ev.model,
                "timestamp": ev.timestamp,
                "usage": ev.usage,
            })),
            _ => None,
        })
        .collect();

    let record = json!({
        "session_id": sess.stats.id,
        "model": sess.stats.model,
        "cwd": sess.stats.cwd,
        "git_branch": sess.stats.git_branch,
        "version": sess.stats.version,
        "title": sess.stats.title,
        "messages": messages,
        "metadata": metadata_object(sess.stats),
    });

    let mut s = serde_json::to_string(&record).unwrap_or_default();
    s.push('\n');
    s
}

// -- OpenAI Chat / Tools -------------------------------------------------------

fn render_openai_line(sess: &SessionExport<'_>) -> String {
    let mut messages: Vec<Value> = Vec::new();
    for ev in sess.events {
        match ev.event_type.as_str() {
            "user" => push_openai_user(&mut messages, &ev.entry),
            "assistant" => push_openai_assistant(&mut messages, ev),
            _ => {}
        }
    }

    let record = json!({
        "session_id": sess.stats.id,
        "model": sess.stats.model,
        "messages": messages,
        "metadata": metadata_object(sess.stats),
    });

    let mut s = serde_json::to_string(&record).unwrap_or_default();
    s.push('\n');
    s
}

fn push_openai_user(messages: &mut Vec<Value>, entry: &Value) {
    let content = entry
        .pointer("/message/content")
        .or_else(|| entry.get("content"));
    let Some(content) = content else { return };

    // Plain-string user message.
    if let Some(s) = content.as_str() {
        messages.push(json!({ "role": "user", "content": s }));
        return;
    }
    // Array of content blocks: text → user message, tool_result → tool messages.
    if let Some(arr) = content.as_array() {
        let mut text_parts: Vec<String> = Vec::new();
        for b in arr {
            let kind = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_owned());
                    }
                }
                "tool_result" => {
                    let id = b.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                    let body = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": body,
                    }));
                }
                _ => {}
            }
        }
        if !text_parts.is_empty() {
            messages.push(json!({ "role": "user", "content": text_parts.join("\n") }));
        }
    }
}

fn push_openai_assistant(messages: &mut Vec<Value>, ev: &TraceEvent) {
    let content = ev.entry.pointer("/message/content");
    let Some(content) = content else { return };

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(s) = content.as_str() {
        text_parts.push(s.to_owned());
    } else if let Some(arr) = content.as_array() {
        for b in arr {
            let kind = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_owned());
                    }
                }
                "tool_use" => {
                    let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let args = b.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": args.to_string(),
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut msg = json!({
        "role": "assistant",
        "content": if text_parts.is_empty() { Value::Null } else { Value::String(text_parts.join("\n")) },
    });
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(m) = &ev.model {
        msg["model"] = Value::String(m.clone());
    }
    messages.push(msg);
}

// -- ShareGPT ------------------------------------------------------------------

fn render_sharegpt_line(sess: &SessionExport<'_>) -> String {
    let mut conversations: Vec<Value> = Vec::new();
    for ev in sess.events {
        match ev.event_type.as_str() {
            "user" => {
                if let Some(text) = extract_plain_text(&ev.entry) {
                    if !text.is_empty() {
                        conversations.push(json!({ "from": "human", "value": text }));
                    }
                }
                if let Some(tr) = extract_tool_results_text(&ev.entry) {
                    if !tr.is_empty() {
                        conversations.push(json!({ "from": "tool", "value": tr }));
                    }
                }
            }
            "assistant" => {
                if let Some(text) = extract_plain_text(&ev.entry) {
                    if !text.is_empty() {
                        conversations.push(json!({ "from": "gpt", "value": text }));
                    }
                }
                if let Some(tool_uses) = extract_tool_uses_text(&ev.entry) {
                    if !tool_uses.is_empty() {
                        conversations.push(json!({ "from": "function_call", "value": tool_uses }));
                    }
                }
            }
            _ => {}
        }
    }
    let record = json!({
        "id": sess.stats.id,
        "title": sess.stats.title,
        "model": sess.stats.model,
        "conversations": conversations,
        "metadata": metadata_object(sess.stats),
    });
    let mut s = serde_json::to_string(&record).unwrap_or_default();
    s.push('\n');
    s
}

// -- Raw passthrough -----------------------------------------------------------

fn render_raw_jsonl(sess: &SessionExport<'_>) -> String {
    let mut out = String::with_capacity(sess.events.len() * 256);
    for ev in sess.events {
        if let Ok(s) = serde_json::to_string(&ev.entry) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

// -- Markdown ------------------------------------------------------------------

fn render_markdown(sess: &SessionExport<'_>) -> String {
    let mut out = String::with_capacity(4096);
    let title = sess
        .stats
        .title
        .clone()
        .unwrap_or_else(|| format!("Session {}", sess.stats.id));
    let _ = writeln!(out, "# {}", title);
    let _ = writeln!(out, "");
    let _ = writeln!(out, "- **ID:** `{}`", sess.stats.id);
    if let Some(m) = &sess.stats.model {
        let _ = writeln!(out, "- **Model:** {}", m);
    }
    if let Some(c) = &sess.stats.cwd {
        let _ = writeln!(out, "- **CWD:** `{}`", c);
    }
    if let Some(b) = &sess.stats.git_branch {
        let _ = writeln!(out, "- **Branch:** `{}`", b);
    }
    let _ = writeln!(
        out,
        "- **Events:** {} · **Cost (est.):** ${:.4} · **Tokens out:** {}",
        sess.stats.event_count, sess.stats.cost_usd, sess.stats.output_tokens
    );
    let _ = writeln!(out, "");

    for ev in sess.events {
        match ev.event_type.as_str() {
            "user" => {
                let _ = writeln!(out, "## 👤 User");
                let _ = writeln!(out, "");
                write_content_md(&mut out, ev.entry.pointer("/message/content").or_else(|| ev.entry.get("content")));
                let _ = writeln!(out, "");
            }
            "assistant" => {
                let _ = writeln!(out, "## 🤖 Assistant");
                if let Some(t) = &ev.timestamp {
                    let _ = writeln!(out, "*{}*", t);
                }
                let _ = writeln!(out, "");
                write_content_md(&mut out, ev.entry.pointer("/message/content"));
                let _ = writeln!(out, "");
            }
            "summary" => {
                if let Some(s) = ev.entry.get("summary").and_then(|v| v.as_str()) {
                    let _ = writeln!(out, "> **Summary:** {}", s);
                    let _ = writeln!(out, "");
                }
            }
            _ => {}
        }
    }
    out
}

fn write_content_md(out: &mut String, content: Option<&Value>) {
    let Some(content) = content else { return };
    if let Some(s) = content.as_str() {
        out.push_str(s);
        out.push('\n');
        return;
    }
    if let Some(arr) = content.as_array() {
        for b in arr {
            let kind = b.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                        out.push_str("\n\n");
                    }
                }
                "thinking" => {
                    let t = b
                        .get("thinking")
                        .or_else(|| b.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let _ = writeln!(out, "<details><summary>💭 Thinking</summary>\n\n```\n{}\n```\n\n</details>", t);
                }
                "tool_use" => {
                    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let input = serde_json::to_string_pretty(b.get("input").unwrap_or(&Value::Null))
                        .unwrap_or_default();
                    let _ = writeln!(out, "**🔧 Tool: `{}`**\n\n```json\n{}\n```\n", name, input);
                }
                "tool_result" => {
                    let id = b.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
                    let body = match b.get("content") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
                        None => String::new(),
                    };
                    let _ = writeln!(out, "**📦 Tool result** (`{}`)\n\n```\n{}\n```\n", id, body);
                }
                _ => {}
            }
        }
    }
}

// -- Helpers -------------------------------------------------------------------

fn extract_message_content(entry: &Value) -> Value {
    if let Some(c) = entry.pointer("/message/content") {
        return c.clone();
    }
    if let Some(c) = entry.get("content") {
        return c.clone();
    }
    Value::Null
}

fn extract_plain_text(entry: &Value) -> Option<String> {
    let c = entry
        .pointer("/message/content")
        .or_else(|| entry.get("content"))?;
    if let Some(s) = c.as_str() {
        return Some(s.to_owned());
    }
    if let Some(arr) = c.as_array() {
        let mut parts = Vec::new();
        for b in arr {
            if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    parts.push(t.to_owned());
                }
            }
        }
        return Some(parts.join("\n"));
    }
    None
}

fn extract_tool_uses_text(entry: &Value) -> Option<String> {
    let arr = entry.pointer("/message/content")?.as_array()?;
    let mut out = Vec::new();
    for b in arr {
        if b.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let input = b.get("input").cloned().unwrap_or(Value::Null);
            out.push(
                json!({ "name": name, "arguments": input }).to_string(),
            );
        }
    }
    Some(out.join("\n"))
}

fn extract_tool_results_text(entry: &Value) -> Option<String> {
    let c = entry
        .pointer("/message/content")
        .or_else(|| entry.get("content"))?;
    let arr = c.as_array()?;
    let mut parts = Vec::new();
    for b in arr {
        if b.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
            let body = match b.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            parts.push(body);
        }
    }
    Some(parts.join("\n"))
}

fn metadata_object(s: &SessionStats) -> Value {
    json!({
        "input_tokens": s.input_tokens,
        "output_tokens": s.output_tokens,
        "cache_read_tokens": s.cache_read_tokens,
        "cache_creation_tokens": s.cache_creation_tokens,
        "cost_usd": s.cost_usd,
        "first_seen": s.first_seen,
        "last_seen": s.last_seen,
        "event_count": s.event_count,
        "user_count": s.user_count,
        "assistant_count": s.assistant_count,
        "tool_use_count": s.tool_use_count,
        "tool_result_count": s.tool_result_count,
        "tool_counts": s.tool_counts,
    })
}

// -- HuggingFace dataset directory --------------------------------------------

/// Write a HuggingFace `datasets`-compatible directory at `out_dir` containing
/// `train.jsonl`, `dataset_info.json` and a basic `README.md` dataset card.
pub fn write_huggingface_dir(
    out_dir: &Path,
    sessions: &[SessionExport<'_>],
) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let train_path = out_dir.join("train.jsonl");
    let body = render_many(sessions, ExportFormat::Messages);
    std::fs::write(&train_path, body)?;

    let totals = sessions.iter().fold(
        HashMap::<&'static str, f64>::new(),
        |mut acc, s| {
            *acc.entry("sessions").or_insert(0.0) += 1.0;
            *acc.entry("events").or_insert(0.0) += s.stats.event_count as f64;
            *acc.entry("cost_usd").or_insert(0.0) += s.stats.cost_usd;
            *acc.entry("input_tokens").or_insert(0.0) += s.stats.input_tokens as f64;
            *acc.entry("output_tokens").or_insert(0.0) += s.stats.output_tokens as f64;
            acc
        },
    );

    let info = json!({
        "description": "Claude Code session traces exported by claude-trace-rs.",
        "citation": "",
        "homepage": "https://github.com/CodeHalwell/claude-trace-rs",
        "license": "user-defined",
        "features": {
            "session_id": { "dtype": "string", "_type": "Value" },
            "model": { "dtype": "string", "_type": "Value" },
            "title": { "dtype": "string", "_type": "Value" },
            "messages": {
                "feature": {
                    "role": { "dtype": "string", "_type": "Value" },
                    "content": { "_type": "Sequence", "feature": {
                        "type": { "dtype": "string", "_type": "Value" },
                        "text": { "dtype": "string", "_type": "Value" }
                    }}
                },
                "_type": "Sequence"
            }
        },
        "splits": {
            "train": { "name": "train", "num_examples": totals.get("sessions").copied().unwrap_or(0.0) as u64 }
        }
    });
    std::fs::write(out_dir.join("dataset_info.json"), serde_json::to_vec_pretty(&info)?)?;

    let card = format!(
        "---\nlicense: other\ntask_categories:\n  - conversational\n  - text-generation\nlanguage:\n  - en\nsize_categories:\n  - n<1K\npretty_name: \"Claude Code Sessions\"\n---\n\n# Claude Code session dataset\n\nGenerated by [`claude-trace-rs`](https://github.com/CodeHalwell/claude-trace-rs).\n\n- Sessions: **{sessions}**\n- Total events: **{events}**\n- Aggregate input tokens: **{tin}**\n- Aggregate output tokens: **{tout}**\n- Estimated cost: **${cost:.2}**\n\nEach line of `train.jsonl` is one Claude Code session in Anthropic Messages\nformat:\n\n```json\n{{\n  \"session_id\": \"…\",\n  \"model\": \"claude-opus-4-7\",\n  \"messages\": [\n    {{\"role\": \"user\", \"content\": \"…\"}},\n    {{\"role\": \"assistant\", \"content\": [{{\"type\":\"text\",\"text\":\"…\"}}, {{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{{…}}}}]}},\n    {{\"role\": \"user\", \"content\": [{{\"type\":\"tool_result\",\"tool_use_id\":\"…\",\"content\":\"…\"}}]}}\n  ],\n  \"metadata\": {{ \"cost_usd\": 0.42, \"input_tokens\": 1234, \"output_tokens\": 567 }}\n}}\n```\n\nLoad with:\n\n```python\nfrom datasets import load_dataset\nds = load_dataset(\"json\", data_files={{\"train\": \"train.jsonl\"}})\n```\n",
        sessions = totals.get("sessions").copied().unwrap_or(0.0) as u64,
        events = totals.get("events").copied().unwrap_or(0.0) as u64,
        tin = totals.get("input_tokens").copied().unwrap_or(0.0) as u64,
        tout = totals.get("output_tokens").copied().unwrap_or(0.0) as u64,
        cost = totals.get("cost_usd").copied().unwrap_or(0.0),
    );
    std::fs::write(out_dir.join("README.md"), card)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionStats;
    use serde_json::json;

    fn stats() -> SessionStats {
        SessionStats {
            id: "sid".to_owned(),
            cwd: Some("/tmp/proj".to_owned()),
            git_branch: Some("main".to_owned()),
            model: Some("claude-opus-4-7".to_owned()),
            event_count: 3,
            user_count: 1,
            assistant_count: 1,
            cost_usd: 0.5,
            input_tokens: 100,
            output_tokens: 50,
            title: Some("Test session".to_owned()),
            ..Default::default()
        }
    }

    fn ev(t: &str, raw: serde_json::Value) -> TraceEvent {
        TraceEvent::from_raw("sid", 0, json!({ "type": t, "sessionId": "sid", "message": raw }))
    }

    #[test]
    fn messages_format_includes_tool_use_blocks() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hello" })),
            ev("assistant", json!({
                "model": "claude-opus-4-7",
                "content": [
                    { "type": "text", "text": "hi" },
                    { "type": "tool_use", "id": "t1", "name": "Read", "input": { "path": "/x" } }
                ]
            })),
            ev("user", json!({
                "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "file body" }]
            })),
        ];
        let out = render_session(&SessionExport { stats: &s, events: &events }, ExportFormat::Messages);
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["session_id"], "sid");
        let msgs = parsed["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        let asst_content = msgs[1]["content"].as_array().unwrap();
        assert!(asst_content.iter().any(|b| b["type"] == "tool_use"));
        assert_eq!(msgs[2]["role"], "user");
        let user_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(user_content[0]["type"], "tool_result");
    }

    #[test]
    fn openai_format_translates_tool_calls() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hello" })),
            ev("assistant", json!({
                "model": "claude-opus-4-7",
                "content": [
                    { "type": "text", "text": "calling tool" },
                    { "type": "tool_use", "id": "t1", "name": "Read", "input": { "path": "/x" } }
                ]
            })),
            ev("user", json!({
                "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }]
            })),
        ];
        let out = render_session(&SessionExport { stats: &s, events: &events }, ExportFormat::Openai);
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let msgs = parsed["messages"].as_array().unwrap();
        // user, assistant (with tool_calls), tool result
        let asst = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        let tcs = asst["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["function"]["name"], "Read");
        let tool = msgs.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tool["tool_call_id"], "t1");
        assert_eq!(tool["content"], "ok");
    }

    #[test]
    fn sharegpt_format_basic() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hi" })),
            ev("assistant", json!({
                "content": [{ "type": "text", "text": "hello back" }]
            })),
        ];
        let out = render_session(&SessionExport { stats: &s, events: &events }, ExportFormat::Sharegpt);
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let conv = parsed["conversations"].as_array().unwrap();
        assert_eq!(conv.len(), 2);
        assert_eq!(conv[0]["from"], "human");
        assert_eq!(conv[1]["from"], "gpt");
    }

    #[test]
    fn markdown_format_renders_sections() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hi" })),
            ev("assistant", json!({
                "content": [{ "type": "text", "text": "back" }]
            })),
        ];
        let out = render_session(&SessionExport { stats: &s, events: &events }, ExportFormat::Markdown);
        assert!(out.contains("# Test session"));
        assert!(out.contains("## 👤 User"));
        assert!(out.contains("## 🤖 Assistant"));
    }

    #[test]
    fn raw_jsonl_preserves_full_entries() {
        let s = stats();
        let events = vec![ev("user", json!({ "content": "hi" }))];
        let out = render_session(&SessionExport { stats: &s, events: &events }, ExportFormat::Jsonl);
        assert!(out.contains("\"sessionId\":\"sid\""));
        assert!(out.contains("\"type\":\"user\""));
    }

    #[test]
    fn render_many_concatenates_sessions() {
        let s1 = stats();
        let mut s2 = stats();
        s2.id = "other".to_owned();
        let e1 = vec![ev("user", json!({ "content": "a" }))];
        let e2 = vec![ev("user", json!({ "content": "b" }))];
        let sessions = vec![
            SessionExport { stats: &s1, events: &e1 },
            SessionExport { stats: &s2, events: &e2 },
        ];
        let out = render_many(&sessions, ExportFormat::Messages);
        assert_eq!(out.lines().count(), 2);
    }
}
