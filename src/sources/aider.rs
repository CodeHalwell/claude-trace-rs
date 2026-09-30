//! Aider adapter.
//!
//! Aider appends a Markdown transcript to `.aider.chat.history.md` in the git
//! root of each project it runs in (`--chat-history-file` /
//! `AIDER_CHAT_HISTORY_FILE` move it). There is no central directory, so add
//! your code folders as watch roots (`--watch-root ~/code --source aider`).
//!
//! Grammar (from `aider/io.py`):
//! - `# aider chat started at YYYY-MM-DD HH:MM:SS` starts a session (local
//!   time, no zone). Several sessions share one file.
//! - `#### text` lines are the user's input (one line each).
//! - `> text` lines are tool/info output (model announcements, token
//!   reports, applied edits, commits). Multi-line output keeps the `> ` only
//!   on its first line, and a `Cost:` report can land on an unprefixed
//!   continuation line.
//! - Anything else, after a blank line, is the assistant's reply.
//!
//! The file is append-only but read as a document: each session's records
//! are rebuilt from the text, so a token report arriving after a reply
//! updates that reply's usage in place.

use std::path::Path;

use serde_json::{json, Value};

use crate::event::TokenUsage;
use crate::message::{Block, Message, Role};
use crate::sources::{estimate_cost_for, truncate, AgentSource, Enrichment, SessionDoc};

pub const HISTORY_FILE: &str = ".aider.chat.history.md";
const HEADER: &str = "# aider chat started at ";

pub fn matches_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some(HISTORY_FILE)
}

#[derive(Debug, PartialEq)]
enum Line<'a> {
    Header(&'a str),
    User(&'a str),
    Tool(&'a str),
    Blank,
    Text(&'a str),
}

fn classify_line(raw: &str) -> Line<'_> {
    let l = raw.trim_end_matches(['\r', '\n']);
    let l = l.strip_suffix("  ").unwrap_or(l);
    if let Some(ts) = l.strip_prefix(HEADER) {
        Line::Header(ts.trim())
    } else if let Some(u) = l.strip_prefix("#### ") {
        Line::User(u)
    } else if l == "####" {
        Line::User("")
    } else if let Some(t) = l.strip_prefix("> ") {
        Line::Tool(t)
    } else if l == ">" {
        Line::Tool("")
    } else if l.trim().is_empty() {
        Line::Blank
    } else {
        Line::Text(l)
    }
}

pub fn parse_document(path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    let default_cwd = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let file_key = short_hash(&path.to_string_lossy());

    let mut docs: Vec<SessionDoc> = Vec::new();
    let mut cur: Option<Builder> = None;
    let mut prev_was_tool = false;

    for raw in body.lines() {
        let line = classify_line(raw);
        if let Line::Header(ts) = line {
            if let Some(b) = cur.take() {
                docs.push(b.finish());
            }
            cur = Some(Builder::new(&file_key, ts, &default_cwd));
            prev_was_tool = false;
            continue;
        }
        let Some(b) = cur.as_mut() else {
            continue; // Preamble before the first header.
        };
        match line {
            Line::User(u) => {
                b.flush_text_only();
                b.push_user(u);
                prev_was_tool = false;
            }
            Line::Tool(t) => {
                b.flush_user();
                b.flush_text_only();
                b.push_tool(t);
                prev_was_tool = true;
            }
            Line::Blank => {
                b.flush_user();
                b.text_blank();
                prev_was_tool = false;
            }
            Line::Text(t) => {
                if prev_was_tool {
                    // Continuation of multi-line tool output (e.g. the
                    // unprefixed `Cost:` line).
                    b.push_tool_continuation(t);
                } else {
                    b.flush_user();
                    b.push_text(t);
                }
            }
            Line::Header(_) => unreachable!(),
        }
    }
    if let Some(b) = cur.take() {
        docs.push(b.finish());
    }
    Some(docs)
}

struct Builder {
    session_id: String,
    timestamp: Option<String>,
    cwd: String,
    model: Option<String>,
    version: Option<String>,
    records: Vec<Value>,
    user_lines: Vec<String>,
    text_lines: Vec<String>,
    tool_lines: Vec<String>,
}

impl Builder {
    fn new(file_key: &str, header_ts: &str, cwd: &str) -> Self {
        let compact: String = header_ts.chars().filter(|c| c.is_ascii_digit()).collect();
        Self {
            session_id: format!("aider-{file_key}-{compact}"),
            timestamp: local_to_rfc3339(header_ts),
            cwd: cwd.to_owned(),
            model: None,
            version: None,
            records: Vec::new(),
            user_lines: Vec::new(),
            text_lines: Vec::new(),
            tool_lines: Vec::new(),
        }
    }

    fn ctx(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "cwd": self.cwd,
            "model": self.model,
            "version": self.version,
            "timestamp": self.timestamp,
        })
    }

    fn push_user(&mut self, u: &str) {
        self.flush_tool();
        self.user_lines.push(u.to_owned());
    }

    fn flush_user(&mut self) {
        if self.user_lines.is_empty() {
            return;
        }
        let text = self.user_lines.join("\n");
        self.user_lines.clear();
        let kind = if text.starts_with('/') {
            "command"
        } else {
            "user"
        };
        let ctx = self.ctx();
        self.records
            .push(json!({ "kind": kind, "text": text, "_trace": ctx }));
    }

    fn push_text(&mut self, t: &str) {
        self.flush_tool();
        self.text_lines.push(t.to_owned());
    }

    fn text_blank(&mut self) {
        self.flush_tool();
        if !self.text_lines.is_empty() {
            self.text_lines.push(String::new());
        }
    }

    fn flush_text(&mut self) {
        self.flush_user();
        self.flush_text_only();
    }

    fn flush_text_only(&mut self) {
        while self.text_lines.last().is_some_and(|l| l.is_empty()) {
            self.text_lines.pop();
        }
        if self.text_lines.is_empty() {
            return;
        }
        let text = self.text_lines.join("\n");
        self.text_lines.clear();
        let ctx = self.ctx();
        self.records
            .push(json!({ "kind": "assistant", "text": text, "_trace": ctx }));
    }

    fn push_tool(&mut self, t: &str) {
        self.flush_user();
        // Metadata announcements update the running context.
        if let Some(v) = t.strip_prefix("Aider v") {
            self.version = Some(v.trim().to_owned());
        }
        for prefix in ["Main model: ", "Model: ", "Models: "] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let name = rest.split(" with ").next().unwrap_or(rest).trim();
                if !name.is_empty() {
                    self.model = Some(name.to_owned());
                }
            }
        }
        if let Some(dir) = t.strip_prefix("Cur working dir: ") {
            self.cwd = dir.trim().to_owned();
        }
        // Each `> ` line is its own record; only unprefixed continuation
        // lines join it.
        self.flush_tool();
        self.tool_lines.push(t.to_owned());
    }

    fn push_tool_continuation(&mut self, t: &str) {
        if let Some(last) = self.tool_lines.last_mut() {
            last.push('\n');
            last.push_str(t);
        } else {
            self.tool_lines.push(t.to_owned());
        }
    }

    fn flush_tool(&mut self) {
        if self.tool_lines.is_empty() {
            return;
        }
        let lines: Vec<String> = std::mem::take(&mut self.tool_lines);
        let joined = lines.join("\n");
        if joined.starts_with("Tokens: ") {
            // Attach usage to the reply it reports on.
            if let Some(report) = parse_tokens(&joined) {
                if let Some(last) = self
                    .records
                    .iter_mut()
                    .rev()
                    .find(|r| r["kind"] == "assistant")
                {
                    last["tokens"] = report;
                    return;
                }
            }
        }
        let ctx = self.ctx();
        self.records
            .push(json!({ "kind": "info", "text": joined, "_trace": ctx }));
    }

    fn finish(mut self) -> SessionDoc {
        self.flush_text();
        self.flush_user();
        self.flush_tool();
        SessionDoc {
            session_id: self.session_id,
            records: self.records,
        }
    }
}

/// `Tokens: 6.2k sent, 1.8k cache write, 2.4k cache hit, 180 received.
/// Cost: $0.0071 message, $0.02 session.` (and older variants).
fn parse_tokens(text: &str) -> Option<Value> {
    let flat = text.replace('\n', " ");
    let body = flat.strip_prefix("Tokens: ")?;
    let (mut sent, mut recv, mut cw, mut ch) = (0u64, 0u64, 0u64, 0u64);
    let token_part = body.split("Cost:").next().unwrap_or(body);
    // Split on ", " — a bare comma is a thousands separator (`12,963 sent`).
    for chunk in token_part.split(", ") {
        let chunk = chunk.trim().trim_end_matches('.');
        let mut it = chunk.splitn(2, ' ');
        let (Some(num), Some(label)) = (it.next(), it.next()) else {
            continue;
        };
        let n = parse_count(num);
        match label.trim() {
            "sent" => sent = n,
            "received" => recv = n,
            "cache write" => cw = n,
            "cache hit" | "cached" => ch = n,
            _ => {}
        }
    }
    if sent == 0 && recv == 0 {
        return None;
    }
    let cost = flat.split("Cost: $").nth(1).and_then(|rest| {
        rest.split_whitespace()
            .next()
            .and_then(|n| n.trim_end_matches(',').parse::<f64>().ok())
    });
    Some(json!({
        "sent": sent, "received": recv, "cache_write": cw, "cache_hit": ch, "cost": cost
    }))
}

/// `999`, `6.2k`, `15k`, `12,963` → tokens (the `k` forms are lossy).
fn parse_count(s: &str) -> u64 {
    let s = s.replace(',', "");
    if let Some(k) = s.strip_suffix('k') {
        (k.parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u64
    } else {
        s.parse::<f64>().unwrap_or(0.0) as u64
    }
}

fn local_to_rfc3339(ts: &str) -> Option<String> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|d| d.to_rfc3339())
}

fn short_hash(s: &str) -> String {
    // FNV-1a: stable across runs and platforms (DefaultHasher is not).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", h as u32)
}

/// Split `<think>…</think>`-style reasoning out of a reply.
fn split_reasoning(text: &str) -> (Option<String>, String) {
    for (open, close) in [
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reasoning>", "</reasoning>"),
    ] {
        if let (Some(a), Some(b)) = (text.find(open), text.find(close)) {
            if b > a {
                let thought = text[a + open.len()..b].trim().to_owned();
                let rest = format!("{}{}", &text[..a], &text[b + close.len()..]);
                return (Some(thought), rest.trim().to_owned());
            }
        }
    }
    // Aider's own streamed-reasoning wrapper: <thinking-content-HASH>…</thinking-content-HASH>
    if let Some(a) = text.find("<thinking-content-") {
        if let Some(open_end) = text[a..].find('>') {
            let tag = &text[a + 1..a + open_end];
            let close = format!("</{tag}>");
            if let Some(b) = text.find(&close) {
                let thought = text[a + open_end + 1..b].trim().to_owned();
                let rest = format!("{}{}", &text[..a], &text[b + close.len()..]);
                return (Some(thought), rest.trim().to_owned());
            }
        }
    }
    (None, text.to_owned())
}

pub fn enrich(raw: &Value) -> Enrichment {
    let ctx = raw.get("_trace").cloned().unwrap_or(Value::Null);
    let cs = |k: &str| ctx.get(k).and_then(Value::as_str).map(str::to_owned);
    let text = raw
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mut e = Enrichment {
        session_id: cs("sessionId"),
        cwd: cs("cwd"),
        version: cs("version"),
        timestamp: cs("timestamp"),
        ..Default::default()
    };
    match raw.get("kind").and_then(Value::as_str).unwrap_or("") {
        "user" => {
            e.event_type = "user".into();
            e.message = Message::text(Role::User, text.clone()).non_empty();
            e.summary = format!("👤 {}", truncate(&text, 120));
        }
        "command" => {
            e.event_type = "system".into();
            e.summary = format!("⌨️  {}", truncate(&text, 110));
        }
        "assistant" => {
            e.event_type = "assistant".into();
            e.model = cs("model");
            let (thought, reply) = split_reasoning(&text);
            let mut blocks = Vec::new();
            if let Some(t) = thought.filter(|t| !t.is_empty()) {
                blocks.push(Block::Thinking { thinking: t });
            }
            if !reply.is_empty() {
                blocks.push(Block::Text {
                    text: reply.clone(),
                });
            }
            e.message = Message::new(Role::Assistant, blocks).non_empty();
            e.summary = format!("🤖 {}", truncate(&reply, 110));
            if let Some(t) = raw.get("tokens") {
                let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
                // `sent` already includes cache writes; cache hits are extra.
                let (cw, ch) = (g("cache_write"), g("cache_hit"));
                let u = TokenUsage {
                    input: g("sent").saturating_sub(cw),
                    output: g("received"),
                    cache_read: ch,
                    cache_creation: cw,
                };
                match t.get("cost").and_then(Value::as_f64) {
                    Some(c) => {
                        e.cost_usd = Some(c);
                        e.cost_explicit = true;
                    }
                    None => {
                        e.cost_usd = Some(estimate_cost_for(
                            AgentSource::Aider,
                            e.model.as_deref(),
                            &u,
                        ))
                    }
                }
                e.usage = Some(u);
                e.turn_end = true;
            }
        }
        _ => {
            e.event_type = "system".into();
            let first = text.lines().next().unwrap_or("");
            if let Some(file) = first.strip_prefix("Applied edit to ") {
                e.event_type = "tool_use".into();
                e.tool_uses.push("edit".into());
                e.summary = format!("✏️  Edited {}", truncate(file, 100));
            } else if first.starts_with("Commit ") {
                e.event_type = "tool_use".into();
                e.tool_uses.push("git_commit".into());
                e.summary = format!("📌 {}", truncate(first, 110));
            } else {
                e.summary = format!("ℹ️  {}", truncate(first, 110));
            }
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "
# aider chat started at 2026-09-28 14:03:11

> /home/dan/.local/bin/aider --model sonnet
> Aider v0.86.2
> Main model: anthropic/claude-sonnet-4-5-20250929 with diff edit format, prompt cache, infinite output
> Git repo: .git with 212 files

#### /add src/parser.rs
> Added src/parser.rs to the chat

#### make parse_line return a Result
#### instead of panicking

src/parser.rs
```rust
<<<<<<< SEARCH
pub fn parse_line(s: &str) -> Event {
=======
pub fn parse_line(s: &str) -> Result<Event, serde_json::Error> {
>>>>>>> REPLACE
```

> Tokens: 6.2k sent, 1.8k cache write, 2.4k cache hit, 180 received.
Cost: $0.0071 message, $0.0071 session.
> Applied edit to src/parser.rs
> Commit 3f2a9c1 refactor: Return Result from parse_line

# aider chat started at 2026-09-29 09:41:52

> Aider v0.86.2
> Model: ollama_chat/qwen3-coder with whole edit format

#### why does tail_file re-read the whole file?

<think>look at seek</think>
It calls `seek(SeekFrom::Start(0))` on every event.

> Tokens: 3,900 sent, 142 received. Cost: $0.02 request, $0.05 session.
";

    fn docs() -> Vec<SessionDoc> {
        parse_document(Path::new("/home/dan/proj/.aider.chat.history.md"), LOG).unwrap()
    }

    #[test]
    fn splits_sessions_and_attributes_usage() {
        let d = docs();
        assert_eq!(d.len(), 2);
        assert!(d[0].session_id.starts_with("aider-"));
        assert_ne!(d[0].session_id, d[1].session_id);

        let recs: Vec<Enrichment> = d[0].records.iter().map(enrich).collect();
        let user = recs.iter().find(|e| e.event_type == "user").unwrap();
        assert_eq!(
            user.message.as_ref().unwrap().plain_text(),
            "make parse_line return a Result\ninstead of panicking"
        );
        let reply = recs.iter().find(|e| e.event_type == "assistant").unwrap();
        assert_eq!(
            reply.model.as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250929")
        );
        assert_eq!(reply.version.as_deref(), Some("0.86.2"));
        assert_eq!(reply.cwd.as_deref(), Some("/home/dan/proj"));
        let u = reply.usage.as_ref().unwrap();
        assert_eq!(u.input, 6200 - 1800);
        assert_eq!(u.cache_creation, 1800);
        assert_eq!(u.cache_read, 2400);
        assert_eq!(u.output, 180);
        assert_eq!(reply.cost_usd, Some(0.0071));
        assert!(reply.turn_end);
        assert!(recs.iter().any(|e| e.tool_uses == vec!["edit"]));
        assert!(recs.iter().any(|e| e.tool_uses == vec!["git_commit"]));
        assert!(reply
            .message
            .as_ref()
            .unwrap()
            .plain_text()
            .contains("SEARCH"));
    }

    #[test]
    fn old_cost_wording_and_reasoning_tags() {
        let d = docs();
        let recs: Vec<Enrichment> = d[1].records.iter().map(enrich).collect();
        let reply = recs.iter().find(|e| e.event_type == "assistant").unwrap();
        assert_eq!(reply.model.as_deref(), Some("ollama_chat/qwen3-coder"));
        assert_eq!(reply.usage.as_ref().unwrap().input, 3900);
        assert_eq!(reply.cost_usd, Some(0.02));
        let m = reply.message.as_ref().unwrap();
        assert!(
            matches!(&m.content[0], Block::Thinking { thinking } if thinking == "look at seek")
        );
        assert!(!m.plain_text().contains("<think>"));
    }

    #[test]
    fn session_ids_are_stable() {
        assert_eq!(docs()[0].session_id, docs()[0].session_id);
        assert_eq!(parse_count("6.2k"), 6200);
        assert_eq!(parse_count("12,963"), 12963);
        assert_eq!(parse_count("999"), 999);
    }
}
