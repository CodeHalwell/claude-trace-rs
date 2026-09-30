//! Training-dataset friendly export of coding-agent sessions.
//!
//! Every exporter reads the canonical [`Message`] on each event, so all
//! supported agents export identically regardless of their on-disk format.
//!
//! Six output shapes are supported:
//!
//! - `messages` — Anthropic Messages API (one JSON object per line, the
//!   `messages` field holds the role/content turns with all content blocks
//!   preserved).
//! - `openai` — OpenAI chat-completion shape with `tool_calls` / `tool_call_id`
//!   translated from Claude's tool_use / tool_result blocks.
//! - `sharegpt` — `{conversations: [{from, value}]}` (HF / Axolotl / Unsloth
//!   standard).
//! - `jsonl` — Raw passthrough of each agent's original records (tagged with
//!   `source`). Full fidelity; one line per original entry.
//! - `markdown` — Human-readable transcript (`# User` / `# Assistant` / fenced
//!   `tool_use` blocks). For review, not training.
//! - `huggingface` — A directory containing `train.jsonl`, `dataset_info.json`
//!   and a `README.md` so the result is directly usable with
//!   `datasets.load_dataset("json", data_dir=...)`.

use std::{fmt::Write, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    event::{TokenUsage, TraceEvent},
    message::{result_text, Block, Message, Role},
    state::SessionStats,
};

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
    /// Raw agent records passthrough (one line per entry).
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
            // The CLI emits a full directory (train.jsonl + dataset_info.json +
            // README.md) — see `write_huggingface_dir`. The HTTP endpoints emit
            // the `train.jsonl` portion only, so a JSONL extension is what the
            // browser should attach to the download.
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

// -- Transcript assembly -------------------------------------------------------

/// One transcript turn after merging consecutive same-role records.
///
/// Agents split a single API turn across several log records — Claude Code
/// writes one line per content block, Codex writes reasoning, each tool call
/// and the reply as separate items. Training formats expect one message per
/// turn with alternating roles, so consecutive records of the same role are
/// folded together.
struct Turn<'a> {
    role: Role,
    content: Vec<Block>,
    first: &'a TraceEvent,
    model: Option<&'a str>,
    usage: Option<TokenUsage>,
}

fn transcript<'a>(events: &'a [TraceEvent]) -> (Vec<String>, Vec<Turn<'a>>) {
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<Turn<'a>> = Vec::new();
    for ev in events {
        let Some(msg) = &ev.message else { continue };
        if msg.role == Role::System {
            let t = msg.plain_text();
            if !t.trim().is_empty() {
                system.push(t);
            }
            continue;
        }
        // Some agents (Gemini CLI, OpenCode) store a tool's result on the
        // same record as the call. Every training format expects results on
        // the following user turn, so split the record into alternating
        // turns, keeping the blocks in order (call, result, the reply after).
        let mut segments: Vec<(Role, Vec<Block>)> = Vec::new();
        for b in &msg.content {
            let role = if msg.role == Role::Assistant && matches!(b, Block::ToolResult { .. }) {
                Role::User
            } else {
                msg.role
            };
            match segments.last_mut() {
                Some((r, blocks)) if *r == role => blocks.push(b.clone()),
                _ => segments.push((role, vec![b.clone()])),
            }
        }
        let mut usage = ev.usage.clone();
        for (role, blocks) in segments {
            let u = if role == Role::Assistant {
                usage.take()
            } else {
                None
            };
            push_turn(&mut turns, role, blocks, ev, u);
        }
    }
    (system, turns)
}

fn push_turn<'a>(
    turns: &mut Vec<Turn<'a>>,
    role: Role,
    blocks: Vec<Block>,
    ev: &'a TraceEvent,
    usage: Option<TokenUsage>,
) {
    match turns.last_mut() {
        Some(last) if last.role == role => {
            last.content.extend(blocks);
            if role == Role::Assistant && ev.model.is_some() {
                last.model = ev.model.as_deref();
            }
            if let Some(u) = usage {
                let acc = last.usage.get_or_insert_with(TokenUsage::default);
                acc.input += u.input;
                acc.output += u.output;
                acc.cache_read += u.cache_read;
                acc.cache_creation += u.cache_creation;
            }
        }
        _ => turns.push(Turn {
            role,
            content: blocks,
            first: ev,
            model: ev.model.as_deref(),
            usage,
        }),
    }
}

fn system_value(system: &[String]) -> Value {
    if system.is_empty() {
        Value::Null
    } else {
        Value::String(system.join("\n\n"))
    }
}

// -- Anthropic Messages --------------------------------------------------------

fn render_messages_line(sess: &SessionExport<'_>) -> String {
    let (system, turns) = transcript(sess.events);
    let messages: Vec<Value> = turns
        .iter()
        .map(|t| {
            let mut m = json!({
                "role": t.role.as_str(),
                "content": t.content,
                "timestamp": t.first.timestamp,
            });
            if t.role == Role::Assistant {
                m["model"] = json!(t.model);
                m["usage"] = json!(t.usage);
            }
            m
        })
        .collect();

    let record = json!({
        "session_id": sess.stats.id,
        "source": sess.stats.source,
        "model": sess.stats.model,
        "cwd": sess.stats.cwd,
        "git_branch": sess.stats.git_branch,
        "version": sess.stats.version,
        "title": sess.stats.title,
        "system": system_value(&system),
        "messages": messages,
        "metadata": metadata_object(sess.stats),
    });

    let mut s = serde_json::to_string(&record).unwrap_or_default();
    s.push('\n');
    s
}

// -- OpenAI Chat / Tools -------------------------------------------------------

fn render_openai_line(sess: &SessionExport<'_>) -> String {
    let (system, turns) = transcript(sess.events);
    let mut messages: Vec<Value> = Vec::new();
    if !system.is_empty() {
        messages.push(json!({ "role": "system", "content": system.join("\n\n") }));
    }
    for t in &turns {
        match t.role {
            Role::User => push_openai_user(&mut messages, &t.content),
            Role::Assistant => push_openai_assistant(&mut messages, t),
            Role::System => {}
        }
    }

    let record = json!({
        "session_id": sess.stats.id,
        "source": sess.stats.source,
        "model": sess.stats.model,
        "messages": messages,
        "metadata": metadata_object(sess.stats),
    });

    let mut s = serde_json::to_string(&record).unwrap_or_default();
    s.push('\n');
    s
}

/// Tool results become `tool` messages (they must directly follow the
/// assistant's `tool_calls`); remaining text becomes one user message.
fn push_openai_user(messages: &mut Vec<Value>, content: &[Block]) {
    let mut text_parts: Vec<&str> = Vec::new();
    let mut images: Vec<&Value> = Vec::new();
    for b in content {
        match b {
            Block::ToolResult {
                tool_use_id,
                content,
                ..
            } => messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": result_text(content),
            })),
            Block::Text { text } => text_parts.push(text),
            Block::Image { source } => images.push(source),
            _ => {}
        }
    }
    let image_parts: Vec<Value> = images
        .iter()
        .filter_map(|src| image_url(src))
        .map(|url| json!({ "type": "image_url", "image_url": { "url": url } }))
        .collect();
    if !image_parts.is_empty() {
        // Multimodal: OpenAI content parts, text first.
        let mut parts: Vec<Value> = Vec::new();
        if !text_parts.is_empty() {
            parts.push(json!({ "type": "text", "text": text_parts.join("\n") }));
        }
        parts.extend(image_parts);
        messages.push(json!({ "role": "user", "content": parts }));
    } else if !text_parts.is_empty() {
        messages.push(json!({ "role": "user", "content": text_parts.join("\n") }));
    } else if !images.is_empty() {
        // Images whose data the agent did not log.
        messages.push(json!({ "role": "user", "content": "[image]" }));
    }
}

/// An image block's source as a URL OpenAI accepts: an http(s) or data URL.
/// Sources come in the shapes the agents log them: Anthropic
/// `{type: base64, media_type, data}` / `{type: url, url}`, OpenAI
/// `{url}` or a bare string, Gemini `{mimeType, data}`.
fn image_url(source: &Value) -> Option<String> {
    if let Some(s) = source.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    if let Some(url) = source.get("url").and_then(Value::as_str) {
        return (!url.is_empty()).then(|| url.to_owned());
    }
    let data = source.get("data").and_then(Value::as_str)?;
    if data.is_empty() {
        return None;
    }
    let media = ["media_type", "mimeType", "mime_type"]
        .iter()
        .find_map(|k| source.get(*k).and_then(Value::as_str))
        .unwrap_or("image/png");
    Some(format!("data:{media};base64,{data}"))
}

fn push_openai_assistant(messages: &mut Vec<Value>, t: &Turn<'_>) {
    let mut text_parts: Vec<&str> = Vec::new();
    let mut reasoning: Vec<&str> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for b in &t.content {
        match b {
            Block::Text { text } => text_parts.push(text),
            Block::Thinking { thinking } => reasoning.push(thinking),
            Block::ToolUse { id, name, input } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() },
            })),
            _ => {}
        }
    }
    let mut msg = json!({
        "role": "assistant",
        "content": if text_parts.is_empty() { Value::Null } else { Value::String(text_parts.join("\n")) },
    });
    if !reasoning.is_empty() {
        // The `reasoning_content` convention (DeepSeek, Qwen, Kimi, vLLM).
        msg["reasoning_content"] = Value::String(reasoning.join("\n"));
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    if let Some(m) = t.model {
        msg["model"] = Value::String(m.to_owned());
    }
    messages.push(msg);
}

// -- ShareGPT ------------------------------------------------------------------

fn render_sharegpt_line(sess: &SessionExport<'_>) -> String {
    let (system, turns) = transcript(sess.events);
    let mut conversations: Vec<Value> = Vec::new();
    for t in &turns {
        let text = Message::new(t.role, t.content.clone()).plain_text();
        match t.role {
            Role::User => {
                let results: Vec<String> = t
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolResult { content, .. } => Some(result_text(content)),
                        _ => None,
                    })
                    .collect();
                if !results.is_empty() {
                    conversations.push(json!({ "from": "tool", "value": results.join("\n") }));
                }
                if !text.is_empty() {
                    conversations.push(json!({ "from": "human", "value": text }));
                }
            }
            Role::Assistant => {
                if !text.is_empty() {
                    conversations.push(json!({ "from": "gpt", "value": text }));
                }
                let calls: Vec<String> = t
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolUse { name, input, .. } => {
                            Some(json!({ "name": name, "arguments": input }).to_string())
                        }
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    conversations
                        .push(json!({ "from": "function_call", "value": calls.join("\n") }));
                }
            }
            Role::System => {}
        }
    }
    let record = json!({
        "id": sess.stats.id,
        "source": sess.stats.source,
        "title": sess.stats.title,
        "model": sess.stats.model,
        "system": system_value(&system),
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
        // Attach the source so mixed-agent datasets stay attributable even
        // when the raw record itself has no agent-identifying field. Prefer
        // the event's own attribution; fall back to the session's.
        let mut entry = ev.entry.clone();
        let src = if ev.source.is_empty() || ev.source == "unknown" {
            sess.stats.source.as_str()
        } else {
            ev.source.as_str()
        };
        if let Some(obj) = entry.as_object_mut() {
            obj.entry("source".to_owned())
                .or_insert_with(|| Value::String(src.to_owned()));
        }
        if let Ok(s) = serde_json::to_string(&entry) {
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
        .or_else(|| sess.stats.first_prompt.clone())
        .unwrap_or_else(|| format!("Session {}", sess.stats.id));
    let _ = writeln!(out, "# {}", title);
    let _ = writeln!(out);
    let _ = writeln!(out, "- **ID:** `{}`", sess.stats.id);
    let _ = writeln!(
        out,
        "- **Agent:** {}",
        crate::sources::AgentSource::parse(&sess.stats.source)
            .map(|s| s.display_name())
            .unwrap_or(&sess.stats.source)
    );
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
    let _ = writeln!(out);

    let (system, turns) = transcript(sess.events);
    if !system.is_empty() {
        let _ = writeln!(
            out,
            "<details><summary>⚙️ System / instructions</summary>\n\n```\n{}\n```\n\n</details>\n",
            system.join("\n\n")
        );
    }
    for t in &turns {
        let only_results = t
            .content
            .iter()
            .all(|b| matches!(b, Block::ToolResult { .. }));
        match t.role {
            Role::User if only_results => {}
            Role::User => {
                let _ = writeln!(out, "## 👤 User\n");
            }
            _ => {
                let _ = writeln!(out, "## 🤖 Assistant");
                if let Some(ts) = &t.first.timestamp {
                    let _ = writeln!(out, "*{}*", ts);
                }
                let _ = writeln!(out);
            }
        }
        write_blocks_md(&mut out, &t.content);
    }
    out
}

fn write_blocks_md(out: &mut String, blocks: &[Block]) {
    for b in blocks {
        match b {
            Block::Text { text } => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            Block::Thinking { thinking } => {
                let _ = writeln!(
                    out,
                    "<details><summary>💭 Thinking</summary>\n\n```\n{}\n```\n\n</details>\n",
                    thinking
                );
            }
            Block::ToolUse { name, input, .. } => {
                let input = serde_json::to_string_pretty(input).unwrap_or_default();
                let _ = writeln!(out, "**🔧 Tool: `{}`**\n\n```json\n{}\n```\n", name, input);
            }
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let label = if *is_error {
                    "⛔ Tool error"
                } else {
                    "📦 Tool result"
                };
                let _ = writeln!(
                    out,
                    "**{}** (`{}`)\n\n```\n{}\n```\n",
                    label,
                    tool_use_id,
                    result_text(content)
                );
            }
            Block::Image { .. } => {
                out.push_str("*[image]*\n\n");
            }
        }
    }
}

fn metadata_object(s: &SessionStats) -> Value {
    json!({
        "source": s.source,
        "first_prompt": s.first_prompt,
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
    let body = render_many(sessions, ExportFormat::Messages);
    let totals = sessions.iter().fold(HfTotals::default(), |mut t, s| {
        t.sessions += 1;
        t.events += s.stats.event_count as u64;
        t.cost_usd += s.stats.cost_usd;
        t.input_tokens += s.stats.input_tokens;
        t.output_tokens += s.stats.output_tokens;
        t
    });
    write_hf_files(out_dir, &body, &totals)
}

/// Same as [`write_huggingface_dir`], but from an already-rendered
/// `messages` JSONL body (what the HTTP export endpoints return). Totals for
/// the dataset card are read back from each record's `metadata`.
pub fn write_huggingface_from_jsonl(out_dir: &Path, body: &str) -> std::io::Result<()> {
    let mut t = HfTotals::default();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let m = &v["metadata"];
        let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
        t.sessions += 1;
        t.events += n("event_count");
        t.input_tokens += n("input_tokens");
        t.output_tokens += n("output_tokens");
        t.cost_usd += m.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    }
    write_hf_files(out_dir, body, &t)
}

#[derive(Default)]
struct HfTotals {
    sessions: u64,
    events: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

fn write_hf_files(out_dir: &Path, body: &str, t: &HfTotals) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join("train.jsonl"), body)?;

    // We deliberately omit a `features` block: each record's `content` field
    // can be either a string OR a heterogeneous array of `text` / `thinking` /
    // `tool_use` / `tool_result` / `image` blocks (each with their own
    // sub-schema), and records also carry top-level `cwd`, `git_branch`,
    // `version`, and a free-form `metadata` object. Pinning a narrow schema
    // would lie about the data; letting `datasets` infer types from the JSONL
    // is both accurate and what `load_dataset("json", ...)` does by default.
    let info = json!({
        "description": "Coding-agent session traces exported by claude-trace-rs. \
            Each record is one session from one agent (see `source`); \
            `messages` is an Anthropic-shape list whose `content` is an array \
            of content blocks (text / thinking / tool_use / tool_result / \
            image). Additional top-level fields: source, cwd, git_branch, \
            version, title, system, metadata.",
        "citation": "",
        "homepage": "https://github.com/CodeHalwell/claude-trace-rs",
        "license": "user-defined",
        "splits": {
            "train": { "name": "train", "num_examples": t.sessions }
        }
    });
    std::fs::write(
        out_dir.join("dataset_info.json"),
        serde_json::to_vec_pretty(&info)?,
    )?;

    let card = format!(
        "---\nlicense: other\ntask_categories:\n  - conversational\n  - text-generation\nlanguage:\n  - en\nsize_categories:\n  - n<1K\npretty_name: \"Coding Agent Sessions\"\n---\n\n# Coding-agent session dataset\n\nGenerated by [`claude-trace-rs`](https://github.com/CodeHalwell/claude-trace-rs).\n\n- Sessions: **{sessions}**\n- Total events: **{events}**\n- Aggregate input tokens: **{tin}**\n- Aggregate output tokens: **{tout}**\n- Estimated cost: **${cost:.2}**\n\n## Schema\n\nEach line of `train.jsonl` is one coding-agent session (Claude Code, Codex, Gemini CLI, …). Top-level fields:\n\n| Field         | Type   | Notes |\n| ------------- | ------ | ----- |\n| `session_id`  | string | Stable session identifier from the agent |\n| `source`      | string | Agent that produced the session (`claude-code`, `codex`, `gemini`, …) |\n| `model`       | string\\|null | e.g. `claude-opus-4-7` |\n| `cwd`         | string\\|null | Working directory the run started in |\n| `git_branch`  | string\\|null | Git branch when known |\n| `version`     | string\\|null | Agent CLI version |\n| `system`      | string\\|null | System / developer instructions recorded in the session |\n| `title`       | string\\|null | AI-assigned title if present |\n| `messages`    | array  | Anthropic-shape messages — see below |\n| `metadata`    | object | Aggregates: `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `cost_usd`, `first_seen`, `last_seen`, `event_count`, `tool_counts` |\n\n`messages[*].content` is an array of content blocks. Each block has a `type` field; possible values:\n\n- `text` — `text: string`\n- `thinking` — `thinking: string` (extended-thinking output)\n- `tool_use` — `id`, `name`, `input`\n- `tool_result` — `tool_use_id`, `content` (string or array)\n- `image` — `source`\n\nExample:\n\n```json\n{{\n  \"session_id\": \"…\",\n  \"model\": \"claude-opus-4-7\",\n  \"messages\": [\n    {{\"role\": \"user\", \"content\": [{{\"type\":\"text\",\"text\":\"…\"}}]}},\n    {{\"role\": \"assistant\", \"content\": [\n      {{\"type\":\"text\",\"text\":\"…\"}},\n      {{\"type\":\"tool_use\",\"id\":\"toolu_…\",\"name\":\"Read\",\"input\":{{}}}}\n    ]}},\n    {{\"role\": \"user\", \"content\": [\n      {{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_…\",\"content\":\"…\"}}\n    ]}}\n  ],\n  \"metadata\": {{ \"cost_usd\": 0.42, \"input_tokens\": 1234, \"output_tokens\": 567 }}\n}}\n```\n\n## Load\n\n```python\nimport os\nfrom datasets import load_dataset\nds = load_dataset(\"json\", data_files={{\n    \"train\": os.path.expanduser(\"train.jsonl\")\n}})\nprint(ds[\"train\"][0][\"messages\"][:3])\n```\n",
        sessions = t.sessions,
        events = t.events,
        tin = t.input_tokens,
        tout = t.output_tokens,
        cost = t.cost_usd,
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
        TraceEvent::from_raw(
            "sid",
            0,
            json!({ "type": t, "sessionId": "sid", "message": raw }),
        )
    }

    #[test]
    fn messages_format_includes_tool_use_blocks() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hello" })),
            ev(
                "assistant",
                json!({
                    "model": "claude-opus-4-7",
                    "content": [
                        { "type": "text", "text": "hi" },
                        { "type": "tool_use", "id": "t1", "name": "Read", "input": { "path": "/x" } }
                    ]
                }),
            ),
            ev(
                "user",
                json!({
                    "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "file body" }]
                }),
            ),
        ];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Messages,
        );
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
    fn embedded_tool_results_keep_their_place_in_the_conversation() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hello" })),
            ev(
                "assistant",
                json!({
                    "content": [
                        { "type": "tool_use", "id": "t1", "name": "Read", "input": {} },
                        { "type": "tool_result", "tool_use_id": "t1", "content": "one" },
                        { "type": "tool_use", "id": "t2", "name": "Read", "input": {} },
                        { "type": "tool_result", "tool_use_id": "t2", "content": "two" },
                        { "type": "text", "text": "done" }
                    ]
                }),
            ),
        ];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Messages,
        );
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let shape: Vec<(String, String)> = parsed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                let types: Vec<&str> = m["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b["type"].as_str().unwrap())
                    .collect();
                (m["role"].as_str().unwrap().to_owned(), types.join(","))
            })
            .collect();
        let expect = [
            ("user", "text"),
            ("assistant", "tool_use"),
            ("user", "tool_result"),
            ("assistant", "tool_use"),
            ("user", "tool_result"),
            ("assistant", "text"),
        ];
        assert_eq!(
            shape,
            expect
                .iter()
                .map(|(r, t)| (r.to_string(), t.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn openai_format_translates_tool_calls() {
        let s = stats();
        let events = vec![
            ev("user", json!({ "content": "hello" })),
            ev(
                "assistant",
                json!({
                    "model": "claude-opus-4-7",
                    "content": [
                        { "type": "text", "text": "calling tool" },
                        { "type": "tool_use", "id": "t1", "name": "Read", "input": { "path": "/x" } }
                    ]
                }),
            ),
            ev(
                "user",
                json!({
                    "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }]
                }),
            ),
        ];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Openai,
        );
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
            ev(
                "assistant",
                json!({
                    "content": [{ "type": "text", "text": "hello back" }]
                }),
            ),
        ];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Sharegpt,
        );
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
            ev(
                "assistant",
                json!({
                    "content": [{ "type": "text", "text": "back" }]
                }),
            ),
        ];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Markdown,
        );
        assert!(out.contains("# Test session"));
        assert!(out.contains("## 👤 User"));
        assert!(out.contains("## 🤖 Assistant"));
    }

    #[test]
    fn raw_jsonl_preserves_full_entries() {
        let s = stats();
        let events = vec![ev("user", json!({ "content": "hi" }))];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Jsonl,
        );
        assert!(out.contains("\"sessionId\":\"sid\""));
        assert!(out.contains("\"type\":\"user\""));
    }

    #[test]
    fn raw_jsonl_tags_source_for_mixed_agents() {
        let s = stats();
        let events = vec![ev("user", json!({ "content": "hi" }))];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Jsonl,
        );
        // The raw record gains a top-level "source" for attribution.
        let line: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(line["source"], "claude-code");
    }

    #[test]
    fn messages_record_carries_source() {
        let mut s = stats();
        s.source = "codex".to_owned();
        let events = vec![ev("user", json!({ "content": "hi" }))];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Messages,
        );
        let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed["source"], "codex");
        assert_eq!(parsed["metadata"]["source"], "codex");
    }

    #[test]
    fn render_many_concatenates_sessions() {
        let s1 = stats();
        let mut s2 = stats();
        s2.id = "other".to_owned();
        let e1 = vec![ev("user", json!({ "content": "a" }))];
        let e2 = vec![ev("user", json!({ "content": "b" }))];
        let sessions = vec![
            SessionExport {
                stats: &s1,
                events: &e1,
            },
            SessionExport {
                stats: &s2,
                events: &e2,
            },
        ];
        let out = render_many(&sessions, ExportFormat::Messages);
        assert_eq!(out.lines().count(), 2);
    }
    /// A Codex rollout session: content lives under `/payload/content` with
    /// Responses-API block types, and tool calls are standalone records. Before
    /// normalisation every exporter silently produced empty output for these.
    fn codex_session() -> (SessionStats, Vec<TraceEvent>) {
        let mut st = stats();
        st.source = "codex".to_owned();
        let mk = |raw: serde_json::Value| {
            TraceEvent::from_raw_as("s", 0, raw, crate::sources::AgentSource::Codex)
        };
        let events = vec![
            mk(json!({"type":"response_item","payload":{
                "type":"message","role":"user",
                "content":[{"type":"input_text","text":"list the files"}]}})),
            mk(json!({"type":"response_item","payload":{
                "type":"function_call","name":"shell","call_id":"c1",
                "arguments":"{\"command\":\"ls\"}"}})),
            mk(json!({"type":"response_item","payload":{
                "type":"function_call_output","call_id":"c1","output":"Cargo.toml"}})),
            mk(json!({"type":"response_item","payload":{
                "type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"There is one file."}]}})),
        ];
        (st, events)
    }

    #[test]
    fn codex_messages_export_carries_content() {
        let (st, events) = codex_session();
        let out = render_session(
            &SessionExport {
                stats: &st,
                events: &events,
            },
            ExportFormat::Messages,
        );
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4, "all four turns exported: {msgs:?}");
        assert_eq!(msgs[0]["content"][0]["text"], "list the files");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["name"], "shell");
        // Codex serialises arguments as a JSON string; it is re-parsed.
        assert_eq!(msgs[1]["content"][0]["input"]["command"], "ls");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[3]["content"][0]["text"], "There is one file.");
    }

    #[test]
    fn codex_openai_export_carries_content() {
        let (st, events) = codex_session();
        let out = render_session(
            &SessionExport {
                stats: &st,
                events: &events,
            },
            ExportFormat::Openai,
        );
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "list the files");
        let call = msgs
            .iter()
            .find(|m| m.get("tool_calls").is_some())
            .expect("a tool_calls message");
        assert_eq!(call["tool_calls"][0]["function"]["name"], "shell");
        assert!(
            msgs.iter().any(|m| m["content"] == "There is one file."),
            "assistant text missing: {msgs:?}"
        );
    }

    #[test]
    fn codex_sharegpt_and_markdown_exports_are_not_empty() {
        let (st, events) = codex_session();
        let sg = render_session(
            &SessionExport {
                stats: &st,
                events: &events,
            },
            ExportFormat::Sharegpt,
        );
        let v: serde_json::Value = serde_json::from_str(sg.trim()).unwrap();
        let conv = v["conversations"].as_array().unwrap();
        assert!(
            conv.iter().any(|c| c["value"] == "list the files"),
            "human turn missing: {conv:?}"
        );
        assert!(
            conv.iter().any(|c| c["from"] == "function_call"),
            "tool call missing: {conv:?}"
        );

        let md = render_session(
            &SessionExport {
                stats: &st,
                events: &events,
            },
            ExportFormat::Markdown,
        );
        assert!(md.contains("list the files"), "markdown missing user text");
        assert!(md.contains("There is one file."), "markdown missing reply");
    }

    #[test]
    fn huggingface_dir_from_rendered_jsonl() {
        let s = stats();
        let events = vec![ev("user", json!({ "content": "hello" }))];
        let body = render_many(
            &[SessionExport {
                stats: &s,
                events: &events,
            }],
            ExportFormat::Huggingface,
        );
        let dir = tempfile::tempdir().unwrap();
        write_huggingface_from_jsonl(dir.path(), &body).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("train.jsonl")).unwrap(),
            body
        );
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("dataset_info.json")).unwrap())
                .unwrap();
        assert_eq!(info["splits"]["train"]["num_examples"], 1);
        let card = std::fs::read_to_string(dir.path().join("README.md")).unwrap();
        assert!(card.contains("Sessions: **1**"), "{card}");
        assert!(card.contains("input tokens: **100**"), "{card}");
    }

    #[test]
    fn openai_export_keeps_images_as_content_parts() {
        let s = stats();
        let events = vec![ev(
            "user",
            json!({ "content": [
                { "type": "text", "text": "what is this?" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": "QUJD" } },
                { "type": "image", "source": { "type": "url", "url": "https://example.com/a.png" } }
            ] }),
        )];
        let out = render_session(
            &SessionExport {
                stats: &s,
                events: &events,
            },
            ExportFormat::Openai,
        );
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let user = v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap();
        let parts = user["content"].as_array().expect("content parts");
        assert_eq!(parts[0], json!({ "type": "text", "text": "what is this?" }));
        assert_eq!(parts[1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
        assert_eq!(parts[2]["image_url"]["url"], "https://example.com/a.png");
        assert_eq!(
            image_url(&json!({ "mimeType": "image/webp", "data": "eA==" })).as_deref(),
            Some("data:image/webp;base64,eA==")
        );
        assert_eq!(image_url(&json!(null)), None);
    }
}
