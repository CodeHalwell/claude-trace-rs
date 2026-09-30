//! Multi-agent source registry, detection and per-agent adapters.
//!
//! `claude-trace-rs` is a harmonised tracer: it reads the session logs of
//! many coding agents and normalises their very different on-disk formats
//! into one [`crate::event::TraceEvent`] model carrying a canonical
//! [`crate::message::Message`].
//!
//! Each agent has an adapter module that knows:
//! - where the agent keeps its sessions (including the agent's own
//!   environment-variable overrides) — [`AgentSource::candidate_dirs`];
//! - which files are sessions and how they are written — [`classify`]
//!   returns a [`FileKind`] (append-only JSONL, rewritten document, SQLite
//!   database, or multi-file store);
//! - how to turn one raw record into normalised fields — `enrich`.
//!
//! Detection is two-stage: a file under a known agent root is that agent;
//! otherwise (a custom `--watch-root`) its path and then its content are
//! sniffed. Unknown JSONL falls back to the Claude-Code-shaped heuristics,
//! which tolerate missing fields gracefully.

pub mod aider;
pub mod amp;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod continue_dev;
pub mod copilot;
pub mod crush;
pub mod cursor;
pub mod droid;
pub mod gemini;
pub mod goose;
pub mod kimi;
pub mod opencode;
pub mod qwen;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::Message;

/// The coding agent that produced a trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentSource {
    ClaudeCode,
    Codex,
    Gemini,
    Qwen,
    Copilot,
    Cursor,
    Cline,
    RooCode,
    KiloCode,
    OpenCode,
    Crush,
    Goose,
    Aider,
    Continue,
    Kimi,
    Amp,
    Droid,
    /// Anything we couldn't attribute — enriched with the generic
    /// Claude-Code-shaped fallback heuristics.
    Unknown,
}

/// Display metadata for one agent.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AgentSpec {
    pub name: &'static str,
    /// Badge colour for the dashboard.
    pub color: &'static str,
    /// How the agent stores sessions, for the agents overview.
    pub format: &'static str,
    pub homepage: &'static str,
    /// Shell command that resumes a session, with `{id}` (our session id) or
    /// `{uuid}` (the UUID inside it) placeholders. Run in the session's cwd.
    pub resume: Option<&'static str>,
}

impl AgentSource {
    /// Stable kebab-case identifier stored in the DB and emitted in the API.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentSource::ClaudeCode => "claude-code",
            AgentSource::Codex => "codex",
            AgentSource::Gemini => "gemini",
            AgentSource::Qwen => "qwen",
            AgentSource::Copilot => "copilot",
            AgentSource::Cursor => "cursor",
            AgentSource::Cline => "cline",
            AgentSource::RooCode => "roo-code",
            AgentSource::KiloCode => "kilo-code",
            AgentSource::OpenCode => "opencode",
            AgentSource::Crush => "crush",
            AgentSource::Goose => "goose",
            AgentSource::Aider => "aider",
            AgentSource::Continue => "continue",
            AgentSource::Kimi => "kimi",
            AgentSource::Amp => "amp",
            AgentSource::Droid => "droid",
            AgentSource::Unknown => "unknown",
        }
    }

    /// Parse an identifier (case-insensitive), accepting common aliases.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claude_code" | "claudecode" => {
                Some(AgentSource::ClaudeCode)
            }
            "codex" | "codex-cli" | "openai-codex" => Some(AgentSource::Codex),
            "gemini" | "gemini-cli" | "google-gemini" => Some(AgentSource::Gemini),
            "qwen" | "qwen-code" | "qwen_code" => Some(AgentSource::Qwen),
            "copilot" | "copilot-cli" | "github-copilot" => Some(AgentSource::Copilot),
            "cursor" | "cursor-agent" | "cursor-cli" => Some(AgentSource::Cursor),
            "cline" => Some(AgentSource::Cline),
            "roo-code" | "roo" | "roocode" | "roo-cline" => Some(AgentSource::RooCode),
            "kilo-code" | "kilo" | "kilocode" => Some(AgentSource::KiloCode),
            "opencode" | "open-code" | "sst-opencode" => Some(AgentSource::OpenCode),
            "crush" | "charm-crush" => Some(AgentSource::Crush),
            "goose" | "block-goose" => Some(AgentSource::Goose),
            "aider" => Some(AgentSource::Aider),
            "continue" | "continue-dev" | "continuedev" => Some(AgentSource::Continue),
            "kimi" | "kimi-cli" | "kimi-code" | "moonshot" => Some(AgentSource::Kimi),
            "amp" | "ampcode" | "sourcegraph-amp" => Some(AgentSource::Amp),
            "droid" | "factory" | "factory-droid" => Some(AgentSource::Droid),
            "unknown" => Some(AgentSource::Unknown),
            _ => None,
        }
    }

    pub fn spec(&self) -> AgentSpec {
        let s = |name, color, format, homepage, resume| AgentSpec {
            name,
            color,
            format,
            homepage,
            resume,
        };
        match self {
            AgentSource::ClaudeCode => s(
                "Claude Code",
                "#d97757",
                "JSONL (append-only)",
                "https://claude.com/claude-code",
                Some("claude --resume {id}"),
            ),
            AgentSource::Codex => s(
                "Codex",
                "#10a37f",
                "JSONL rollouts",
                "https://github.com/openai/codex",
                Some("codex resume {uuid}"),
            ),
            AgentSource::Gemini => s(
                "Gemini CLI",
                "#4285f4",
                "JSONL patch log / JSON",
                "https://github.com/google-gemini/gemini-cli",
                Some("gemini --resume {id}"),
            ),
            AgentSource::Qwen => s(
                "Qwen Code",
                "#7c5cff",
                "JSONL (tree)",
                "https://github.com/QwenLM/qwen-code",
                Some("qwen --resume {id}"),
            ),
            AgentSource::Copilot => s(
                "Copilot CLI",
                "#8957e5",
                "JSONL events",
                "https://github.com/github/copilot-cli",
                Some("copilot --resume {id}"),
            ),
            AgentSource::Cursor => s(
                "Cursor",
                "#64748b",
                "JSONL / SQLite",
                "https://cursor.com",
                Some("cursor-agent --resume {id}"),
            ),
            AgentSource::Cline => s(
                "Cline",
                "#0ea5e9",
                "JSON per task",
                "https://github.com/cline/cline",
                None,
            ),
            AgentSource::RooCode => s(
                "Roo Code",
                "#f59e0b",
                "JSON per task",
                "https://github.com/RooCodeInc/Roo-Code",
                None,
            ),
            AgentSource::KiloCode => s(
                "Kilo Code",
                "#e11d48",
                "JSON per task",
                "https://github.com/Kilo-Org/kilocode",
                None,
            ),
            AgentSource::OpenCode => s(
                "OpenCode",
                "#f97316",
                "SQLite / JSON store",
                "https://opencode.ai",
                Some("opencode --session {id}"),
            ),
            AgentSource::Crush => s(
                "Crush",
                "#ec4899",
                "SQLite per project",
                "https://github.com/charmbracelet/crush",
                None,
            ),
            AgentSource::Goose => s(
                "Goose",
                "#22c55e",
                "SQLite / JSONL",
                "https://github.com/block/goose",
                None,
            ),
            AgentSource::Aider => s(
                "Aider",
                "#14b8a6",
                "Markdown chat log",
                "https://aider.chat",
                None,
            ),
            AgentSource::Continue => s(
                "Continue",
                "#6366f1",
                "JSON per session",
                "https://continue.dev",
                None,
            ),
            AgentSource::Kimi => s(
                "Kimi Code",
                "#2563eb",
                "JSONL wire log",
                "https://github.com/MoonshotAI/kimi-code",
                None,
            ),
            AgentSource::Amp => s(
                "Amp",
                "#f43f5e",
                "JSON threads (legacy)",
                "https://ampcode.com",
                Some("amp threads continue {id}"),
            ),
            AgentSource::Droid => s(
                "Factory Droid",
                "#fb923c",
                "JSONL + settings",
                "https://factory.ai",
                Some("droid --resume {id}"),
            ),
            AgentSource::Unknown => s("Unknown", "#9ca3af", "JSONL", "", None),
        }
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &'static str {
        self.spec().name
    }

    /// Model family to assume when a record carries no model name of its own.
    ///
    /// The pricing fallback is Claude Sonnet, which silently misprices other
    /// agents: Codex `token_count` records, for instance, carry usage but no
    /// model (it is stated once on a separate record), so a GPT-5 session
    /// would be billed at Sonnet's rates. `None` keeps the default.
    pub fn default_model_hint(&self) -> Option<&'static str> {
        match self {
            AgentSource::Codex => Some("gpt-5"),
            AgentSource::Copilot => Some("gpt-5"),
            AgentSource::Gemini => Some("gemini-2.5-pro"),
            AgentSource::Qwen => Some("qwen3-coder-plus"),
            AgentSource::Kimi => Some("kimi-k2"),
            _ => None,
        }
    }

    /// Every attributable agent, in display order.
    pub fn all_known() -> &'static [AgentSource] {
        &[
            AgentSource::ClaudeCode,
            AgentSource::Codex,
            AgentSource::Gemini,
            AgentSource::Qwen,
            AgentSource::Copilot,
            AgentSource::Cursor,
            AgentSource::Cline,
            AgentSource::RooCode,
            AgentSource::KiloCode,
            AgentSource::OpenCode,
            AgentSource::Crush,
            AgentSource::Goose,
            AgentSource::Aider,
            AgentSource::Continue,
            AgentSource::Kimi,
            AgentSource::Amp,
            AgentSource::Droid,
        ]
    }

    /// Every directory this agent may keep sessions in on this machine
    /// (existing or not), honouring the agent's own env-var overrides.
    pub fn candidate_dirs(&self) -> Vec<PathBuf> {
        let home = home_dir();
        let mut v = match self {
            AgentSource::ClaudeCode => claude::default_dirs(&home),
            AgentSource::Codex => codex::default_dirs(&home),
            AgentSource::Gemini => gemini::default_dirs(),
            AgentSource::Qwen => qwen::default_dirs(&home),
            AgentSource::Copilot => copilot::default_dirs(&home),
            AgentSource::Cursor => cursor::default_dirs(&home),
            AgentSource::Cline | AgentSource::RooCode | AgentSource::KiloCode => {
                cline::default_task_dirs(*self)
            }
            AgentSource::OpenCode => opencode::default_dirs(&home),
            AgentSource::Crush => crush::default_dirs(&home),
            AgentSource::Goose => goose::default_dirs(&home),
            AgentSource::Aider => Vec::new(),
            AgentSource::Continue => continue_dev::default_dirs(&home),
            AgentSource::Kimi => kimi::default_dirs(&home),
            AgentSource::Amp => amp::default_dirs(&home),
            AgentSource::Droid => droid::default_dirs(&home),
            AgentSource::Unknown => Vec::new(),
        };
        v.dedup();
        v
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve `$VAR` to a path if set and non-empty.
pub(crate) fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Lower-case hex MD5 of a string.
pub(crate) fn md5_hex(s: &str) -> String {
    use md5::{Digest, Md5};
    let d = Md5::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// `file:///home/x/a%20b` → `/home/x/a b` (and `file:///c%3A/x` → `c:/x`).
pub(crate) fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = Vec::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8_lossy(&out).to_string();
    // `/c:/Users/..` → `c:/Users/..` on Windows-style URIs.
    let s = if s.len() > 3 && s.as_bytes()[0] == b'/' && s.as_bytes()[2] == b':' {
        s[1..].to_owned()
    } else {
        s
    };
    Some(s)
}

/// `$XDG_DATA_HOME` or `~/.local/share` — used as-is on every OS by the
/// Node/Go agents that follow XDG regardless of platform.
pub(crate) fn xdg_data_home(home: &Path) -> PathBuf {
    env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"))
}

/// One watch root paired with the agent source it is authoritative for.
#[derive(Debug, Clone)]
pub struct WatchRoot {
    pub path: PathBuf,
    /// Forced source for everything under this root. `None` means
    /// "detect from path/content per file".
    pub source: Option<AgentSource>,
    /// Optional source allow-list for auto-detected files under this root.
    pub allowed_sources: Option<HashSet<AgentSource>>,
}

impl WatchRoot {
    pub fn allows(&self, source: AgentSource) -> bool {
        self.allowed_sources
            .as_ref()
            .map(|allowed| allowed.contains(&source))
            .unwrap_or(true)
    }
}

/// Every known agent directory that currently exists, tagged with its agent
/// so detection is exact rather than sniffed. Nested duplicates (one agent's
/// directory inside another's) keep only the outermost.
pub fn default_roots() -> Vec<WatchRoot> {
    let mut out: Vec<WatchRoot> = Vec::new();
    for src in AgentSource::all_known() {
        for dir in src.candidate_dirs() {
            if dir.is_dir() && !out.iter().any(|r| r.path == dir) {
                out.push(WatchRoot {
                    path: dir,
                    source: Some(*src),
                    allowed_sources: None,
                });
            }
        }
    }
    out
}

/// How a matched file is ingested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Append-only JSON Lines, tailed from the last offset.
    Jsonl,
    /// A whole document (JSON, JSONL patch log, Markdown) re-parsed on change.
    Document,
    /// A SQLite database re-queried on change.
    Sqlite,
    /// One file of a multi-file store; changes rebuild the owning session.
    StoreMember,
}

/// One session's complete, ordered record list from a document or database.
#[derive(Debug, Clone)]
pub struct SessionDoc {
    pub session_id: String,
    pub records: Vec<Value>,
}

/// Decide whether `path` is a session file and how to read it. With a
/// forced root source only that agent's matcher is consulted; otherwise
/// the path is matched against every agent, falling back to generic JSONL.
pub fn classify(root_source: Option<AgentSource>, path: &Path) -> Option<(AgentSource, FileKind)> {
    if let Some(src) = root_source {
        return classify_as(src, path).map(|k| (src, k));
    }
    // Path-based attribution first (a custom root pointing at a known layout).
    let by_path = detect(None, path, None);
    if by_path != AgentSource::Unknown {
        if let Some(k) = classify_as(by_path, path) {
            return Some((by_path, k));
        }
    }
    // Unique filenames identify an agent anywhere.
    for src in [
        AgentSource::Cline,
        AgentSource::Aider,
        AgentSource::OpenCode,
        AgentSource::Crush,
    ] {
        if let Some(k) = classify_as(src, path) {
            // Roo/Kilo share Cline's filenames; attribute by path when we can.
            let src = if src == AgentSource::Cline {
                cline::source_for_path(path)
            } else {
                src
            };
            return Some((src, k));
        }
    }
    // Gemini CLI's patch logs are JSONL but rewritten through `$set` and
    // `$rewindTo` records, so they must be replayed as documents, never
    // tailed as append-only logs.
    if gemini::is_session_file(path) {
        return Some((AgentSource::Gemini, FileKind::Document));
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("jsonl") => Some((AgentSource::Unknown, FileKind::Jsonl)),
        _ => None,
    }
}

fn classify_as(src: AgentSource, path: &Path) -> Option<FileKind> {
    match src {
        AgentSource::ClaudeCode | AgentSource::Unknown => {
            (path.extension().and_then(|e| e.to_str()) == Some("jsonl")).then_some(FileKind::Jsonl)
        }
        AgentSource::Codex => codex::classify(path),
        AgentSource::Gemini => gemini::matches_file(path).then_some(FileKind::Document),
        AgentSource::Qwen => qwen::classify(path),
        AgentSource::Copilot => copilot::classify(path),
        AgentSource::Cursor => cursor::classify(path),
        AgentSource::Cline | AgentSource::RooCode | AgentSource::KiloCode => {
            cline::classify(src, path)
        }
        AgentSource::OpenCode => opencode::classify(path),
        AgentSource::Crush => crush::classify(path),
        AgentSource::Goose => goose::classify(path),
        AgentSource::Aider => aider::matches_file(path).then_some(FileKind::Document),
        AgentSource::Continue => continue_dev::classify(path),
        AgentSource::Kimi => kimi::classify(path),
        AgentSource::Amp => amp::classify(path),
        AgentSource::Droid => droid::classify(path),
    }
}

/// Directories not worth descending into while seeding.
pub fn skip_dir(root_source: Option<AgentSource>, dir: &Path) -> bool {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(
        name,
        "node_modules" | ".git" | "target" | ".venv" | "venv" | "__pycache__" | ".cache" | ".npm"
    ) {
        return true;
    }
    match root_source {
        Some(AgentSource::Gemini) => gemini::skip_dir(dir),
        Some(AgentSource::OpenCode) => opencode::skip_dir(dir),
        Some(AgentSource::Codex) => codex::skip_dir(dir),
        _ => false,
    }
}

/// The unit a changed file belongs to for documents and databases: the file
/// itself, except that a SQLite `-wal`/`-shm` maps to its database.
pub fn unit_path(source: AgentSource, kind: FileKind, path: &Path) -> PathBuf {
    if kind == FileKind::Document
        && matches!(
            source,
            AgentSource::Cline
                | AgentSource::RooCode
                | AgentSource::KiloCode
                | AgentSource::Unknown
        )
    {
        return cline::unit_path(path);
    }
    if kind == FileKind::Document && source == AgentSource::Droid {
        return droid::unit_path(path);
    }
    if kind == FileKind::Sqlite {
        let s = path.to_string_lossy();
        for suffix in ["-wal", "-shm", "-journal"] {
            if let Some(base) = s.strip_suffix(suffix) {
                return PathBuf::from(base);
            }
        }
    }
    path.to_path_buf()
}

/// For a multi-file store member, the unit (session) it belongs to.
pub fn store_unit(source: AgentSource, path: &Path) -> Option<PathBuf> {
    match source {
        AgentSource::OpenCode => opencode::store_unit(path),
        _ => None,
    }
}

pub fn load_store_unit(source: AgentSource, unit: &Path) -> Option<Vec<SessionDoc>> {
    match source {
        AgentSource::OpenCode => opencode::load_store_unit(unit),
        _ => None,
    }
}

/// Parse a whole document into sessions. `None` means "not parseable right
/// now" (typically caught mid-write); retry on the next change.
pub fn parse_document(source: AgentSource, path: &Path, body: &str) -> Option<Vec<SessionDoc>> {
    match source {
        AgentSource::Gemini => gemini::parse_document(path, body, false),
        AgentSource::Qwen => qwen::parse_document(path, body),
        AgentSource::Cline | AgentSource::RooCode | AgentSource::KiloCode => {
            cline::parse_document(path, body)
        }
        AgentSource::Aider => aider::parse_document(path, body),
        AgentSource::Continue => continue_dev::parse_document(path, body),
        AgentSource::Goose => goose::parse_document(path, body),
        AgentSource::Copilot => copilot::parse_document(path, body),
        AgentSource::Cursor => cursor::parse_document(path, body),
        AgentSource::Kimi => kimi::parse_document(path, body),
        AgentSource::Amp => amp::parse_document(path, body),
        AgentSource::Droid => droid::parse_document(path, body),
        _ => None,
    }
}

/// Read the sessions of a SQLite store that changed since `cursor`,
/// advancing it.
pub fn read_sqlite(
    source: AgentSource,
    path: &Path,
    cursor: &mut Option<String>,
) -> anyhow::Result<Vec<SessionDoc>> {
    match source {
        AgentSource::OpenCode | AgentSource::KiloCode => opencode::read_sqlite(path, cursor),
        AgentSource::Crush => crush::read_sqlite(path, cursor),
        AgentSource::Goose => goose::read_sqlite(path, cursor),
        AgentSource::Cursor => cursor::read_sqlite(path, cursor),
        _ => Ok(Vec::new()),
    }
}

/// Per-file stateful annotation of JSONL records, applied in file order
/// before enrichment (e.g. carrying Codex's model onto usage records).
pub fn annotate(source: AgentSource, carry: &mut serde_json::Map<String, Value>, rec: &mut Value) {
    if source == AgentSource::Codex {
        codex::annotate(carry, rec);
    }
}

/// Identify the agent of an unattributed JSON document by its shape.
pub fn sniff_document(path: &Path, body: &str) -> AgentSource {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if cline::matches_file(path) {
        return cline::source_for_path(path);
    }
    if aider::matches_file(path) {
        return AgentSource::Aider;
    }
    let first = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let v: Option<Value> = serde_json::from_str(body)
        .ok()
        .or_else(|| serde_json::from_str(first).ok());
    let Some(v) = v else {
        return AgentSource::Unknown;
    };
    if v.get("sessionId").is_some() && v.get("projectHash").is_some() {
        return AgentSource::Gemini;
    }
    if v.get("sessionId").is_some() && v.get("history").is_some() {
        return AgentSource::Continue;
    }
    if name.ends_with(".jsonl") && v.get("working_dir").is_some() {
        return AgentSource::Goose;
    }
    AgentSource::Unknown
}

/// Session id for records that carry none, derived from the file path.
/// Generic stems (`events.jsonl`, `context.jsonl`, …) use their directory.
pub fn session_id_for_path(source: AgentSource, path: &Path) -> String {
    if source == AgentSource::Copilot {
        return copilot::session_id_for_path(path);
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let generic = matches!(
        stem,
        "events"
            | "context"
            | "history"
            | "wire"
            | "messages"
            | "session"
            | "transcript"
            | "chat"
            | "log"
            | "api_conversation_history"
    );
    if generic {
        if let Some(parent) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        {
            return parent.to_owned();
        }
    }
    stem.to_owned()
}

/// Detect the agent source for a file. The root's forced source wins; then
/// well-known path fragments; then content sniffing of `sniff`.
pub fn detect(root_source: Option<AgentSource>, path: &Path, sniff: Option<&Value>) -> AgentSource {
    if let Some(s) = root_source {
        if s != AgentSource::Unknown {
            return s;
        }
    }
    let p = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let has = |frag: &str| p.contains(frag);
    if has("/.claude/projects/") || has("/claude/projects/") {
        return AgentSource::ClaudeCode;
    }
    if has("/.codex/") || codex::looks_like_rollout(path) {
        return AgentSource::Codex;
    }
    if has("/.gemini/tmp/") {
        return AgentSource::Gemini;
    }
    if has("/.qwen/") {
        return AgentSource::Qwen;
    }
    if has("/.copilot/") {
        return AgentSource::Copilot;
    }
    if has("rooveterinaryinc.roo-cline") {
        return AgentSource::RooCode;
    }
    if has("kilocode.kilo-code") {
        return AgentSource::KiloCode;
    }
    if has("saoudrizwan.claude-dev") || has("/.cline/") {
        return AgentSource::Cline;
    }
    if has("/opencode/storage/") || has("/opencode/opencode") {
        return AgentSource::OpenCode;
    }
    if has("/.crush/") {
        return AgentSource::Crush;
    }
    if has("/goose/sessions/") {
        return AgentSource::Goose;
    }
    if has("/.continue/sessions/") {
        return AgentSource::Continue;
    }
    if has("/.kimi/") || has("/.kimi-code/") {
        return AgentSource::Kimi;
    }
    if has("/amp/threads/") {
        return AgentSource::Amp;
    }
    if has("/.factory/sessions/") {
        return AgentSource::Droid;
    }
    if has("/.cursor/") || has("/.cursor-agent/") {
        return AgentSource::Cursor;
    }
    if let Some(v) = sniff {
        return sniff_source(v);
    }
    AgentSource::Unknown
}

/// Identify an agent from the shape of a single JSONL record.
pub fn sniff_source(v: &Value) -> AgentSource {
    if let Some(t) = v.get("type").and_then(Value::as_str) {
        // Codex rollouts.
        if matches!(
            t,
            "session_meta" | "turn_context" | "response_item" | "event_msg" | "compacted"
        ) {
            return AgentSource::Codex;
        }
        // Copilot CLI dotted event names.
        if t.contains('.') && v.get("data").is_some() {
            return AgentSource::Copilot;
        }
    }
    // Cline UI messages.
    if v.get("ts").is_some() && (v.get("say").is_some() || v.get("ask").is_some()) {
        return AgentSource::Cline;
    }
    // Qwen Code: Claude-like envelope, Gemini-style parts.
    if v.get("sessionId").is_some()
        && v.get("uuid").is_some()
        && v.pointer("/message/parts").is_some()
    {
        return AgentSource::Qwen;
    }
    if v.get("sessionId").is_some() {
        return AgentSource::ClaudeCode;
    }
    AgentSource::Unknown
}

/// The normalised fields every adapter produces from one raw record.
#[derive(Debug, Default)]
pub struct Enrichment {
    pub event_type: String,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub model: Option<String>,
    pub tool_uses: Vec<String>,
    pub tool_results: Vec<String>,
    pub usage: Option<crate::event::TokenUsage>,
    /// Cost in USD — reported by the agent or estimated.
    pub cost_usd: Option<f64>,
    /// True when `cost_usd` came from the agent rather than our estimate.
    pub cost_explicit: bool,
    pub summary: String,
    /// The record as a transcript message, when it is part of the dialogue.
    pub message: Option<Message>,
    pub title: Option<String>,
    pub turn_end: bool,
    /// Identity of the API response this usage belongs to; records sharing
    /// a key repeat the same usage and must be counted once.
    pub usage_key: Option<String>,
}

/// Dispatch to the adapter for `source`.
pub fn enrich(source: AgentSource, raw: &Value) -> Enrichment {
    match source {
        AgentSource::ClaudeCode | AgentSource::Unknown => claude::enrich(raw),
        AgentSource::Codex => codex::enrich(raw),
        AgentSource::Gemini => gemini::enrich(raw),
        AgentSource::Qwen => qwen::enrich(raw),
        AgentSource::Copilot => copilot::enrich(raw),
        AgentSource::Cursor => cursor::enrich(raw),
        AgentSource::Cline | AgentSource::RooCode | AgentSource::KiloCode => {
            cline::enrich(raw, source)
        }
        AgentSource::OpenCode => opencode::enrich(raw),
        AgentSource::Crush => crush::enrich(raw),
        AgentSource::Goose => goose::enrich(raw),
        AgentSource::Aider => aider::enrich(raw),
        AgentSource::Continue => continue_dev::enrich(raw),
        AgentSource::Kimi => kimi::enrich(raw),
        AgentSource::Amp => amp::enrich(raw),
        AgentSource::Droid => droid::enrich(raw),
    }
}

// ---------------------------------------------------------------------------
// Pricing (kept here for adapters; the table lives in `crate::pricing`)
// ---------------------------------------------------------------------------

pub use crate::pricing::{pricing_for, Pricing};

/// Estimated USD cost for `source`, assuming that agent's usual model family
/// when the record itself names no model.
pub fn estimate_cost_for(
    source: AgentSource,
    model: Option<&str>,
    u: &crate::event::TokenUsage,
) -> f64 {
    estimate_cost(model.or_else(|| source.default_model_hint()), u)
}

/// Estimated USD cost for a token usage breakdown.
pub fn estimate_cost(model: Option<&str>, u: &crate::event::TokenUsage) -> f64 {
    pricing_for(model).cost(u)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Recursively gather human-readable text from an entry for search. Indexes
/// every string value (so tool inputs like paths and commands are
/// searchable) except a blacklist of identifiers and binary payloads.
pub fn collect_text(val: &Value, out: &mut String) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if matches!(
                    k.as_str(),
                    "uuid"
                        | "parentUuid"
                        | "id"
                        | "tool_use_id"
                        | "sessionId"
                        | "session_id"
                        | "signature"
                        | "thoughtSignature"
                        | "timestamp"
                        | "call_id"
                        | "encrypted_content"
                        | "data"
                        | "_trace"
                ) {
                    continue;
                }
                if let Some(s) = v.as_str() {
                    out.push(' ');
                    out.push_str(s);
                } else {
                    collect_text(v, out);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_text(v, out);
            }
        }
        _ => {}
    }
}

/// Trim a string to `max_len` chars, collapsing newlines, char-boundary safe.
pub fn truncate(s: &str, max_len: usize) -> String {
    let s = s.trim().replace('\n', " ");
    if s.chars().count() <= max_len {
        s
    } else {
        let end = s
            .char_indices()
            .nth(max_len)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    }
}

/// Epoch seconds or milliseconds (heuristically) → RFC 3339.
pub fn epoch_to_rfc3339(v: &Value) -> Option<String> {
    let n = v.as_f64()?;
    let ms = if n > 1e11 { n } else { n * 1000.0 };
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|d| d.to_rfc3339())
}

/// Read a timestamp that may be RFC 3339 text or an epoch number.
pub fn any_timestamp(v: Option<&Value>) -> Option<String> {
    let v = v?;
    v.as_str()
        .map(str::to_owned)
        .or_else(|| epoch_to_rfc3339(v))
}

/// Default one-line summary for a canonical message.
pub fn summarise_message(event_type: &str, msg: Option<&Message>, tools: &[String]) -> String {
    let text = msg.map(|m| m.plain_text()).unwrap_or_default();
    let preview = truncate(&text, 110);
    match event_type {
        "user" if preview.is_empty() => {
            let n = msg.map(|m| m.tool_results().count()).unwrap_or(0);
            if n > 0 {
                format!("📦 Tool result ×{n}")
            } else {
                "👤 User".into()
            }
        }
        "user" => format!("👤 {preview}"),
        "assistant" => {
            let t = if tools.is_empty() {
                String::new()
            } else {
                format!(" · 🔧 {}", tools.join(", "))
            };
            if !preview.is_empty() {
                format!("🤖 {preview}{t}")
            } else if !tools.is_empty() {
                format!("🔧 {}", tools.join(", "))
            } else if msg.is_some_and(|m| {
                m.content
                    .iter()
                    .any(|b| matches!(b, crate::message::Block::Thinking { .. }))
            }) {
                "💭 Thinking".into()
            } else {
                "🤖 Assistant".into()
            }
        }
        "tool_use" => format!("🔧 {}", tools.join(", ")),
        "tool_result" => "📦 Tool result".into(),
        "system" => format!("⚙️  {preview}"),
        other => format!("❓ {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gemini_patch_logs_are_documents_under_any_root() {
        let p = Path::new("/data/exported/app/chats/session-2026-09-29T14-03-4f1c9a2e.jsonl");
        assert_eq!(
            classify(None, p),
            Some((AgentSource::Gemini, FileKind::Document))
        );
        let other = Path::new("/data/exported/app/chats/4f1c9a2e.jsonl");
        assert_ne!(
            classify(None, other).map(|c| c.0),
            Some(AgentSource::Gemini)
        );
        assert_eq!(
            classify(None, Path::new("/data/x/abc.jsonl")),
            Some((AgentSource::Unknown, FileKind::Jsonl))
        );
    }

    #[test]
    fn source_ids_roundtrip() {
        for s in AgentSource::all_known() {
            assert_eq!(AgentSource::parse(s.as_str()), Some(*s), "{s:?}");
            assert!(!s.spec().name.is_empty());
        }
        assert_eq!(AgentSource::parse("claude"), Some(AgentSource::ClaudeCode));
        assert_eq!(AgentSource::parse("CODEX"), Some(AgentSource::Codex));
        assert_eq!(AgentSource::parse("roo"), Some(AgentSource::RooCode));
        assert_eq!(AgentSource::parse("nope"), None);
    }

    #[test]
    fn sniffing() {
        assert_eq!(
            sniff_source(&json!({"timestamp":"t","type":"turn_context","payload":{}})),
            AgentSource::Codex
        );
        assert_eq!(
            sniff_source(&json!({"type":"user","sessionId":"abc","content":"hi"})),
            AgentSource::ClaudeCode
        );
        assert_eq!(
            sniff_source(&json!({"ts":123,"type":"say","say":"text","text":"hello"})),
            AgentSource::Cline
        );
        assert_eq!(
            sniff_source(
                &json!({"uuid":"u","sessionId":"s","type":"user","message":{"role":"user","parts":[{"text":"x"}]}})
            ),
            AgentSource::Qwen
        );
    }

    #[test]
    fn path_detection() {
        let p = Path::new("/tmp/whatever/rollout-1.jsonl");
        assert_eq!(
            detect(Some(AgentSource::Codex), p, None),
            AgentSource::Codex
        );
        let c = Path::new("/home/me/.claude/projects/proj/s.jsonl");
        assert_eq!(detect(None, c, None), AgentSource::ClaudeCode);
        let x = Path::new("/home/me/.codex/sessions/2026/01/01/rollout-x.jsonl");
        assert_eq!(detect(None, x, None), AgentSource::Codex);
        let r = Path::new("/c/User/globalStorage/rooveterinaryinc.roo-cline/tasks/t/api_conversation_history.json");
        assert_eq!(detect(None, r, None), AgentSource::RooCode);
    }

    #[test]
    fn classify_by_source() {
        let jsonl = Path::new("s.jsonl");
        let cline = Path::new("/x/tasks/t1/api_conversation_history.json");
        assert_eq!(
            classify(Some(AgentSource::ClaudeCode), jsonl),
            Some((AgentSource::ClaudeCode, FileKind::Jsonl))
        );
        assert_eq!(classify(Some(AgentSource::ClaudeCode), cline), None);
        assert_eq!(
            classify(Some(AgentSource::Cline), cline),
            Some((AgentSource::Cline, FileKind::Document))
        );
        assert_eq!(classify(Some(AgentSource::Cline), jsonl), None);
        // Auto-detect roots admit JSONL and every agent's unique filenames.
        assert_eq!(
            classify(None, jsonl),
            Some((AgentSource::Unknown, FileKind::Jsonl))
        );
        assert_eq!(
            classify(None, cline),
            Some((AgentSource::Cline, FileKind::Document))
        );
        assert_eq!(classify(None, Path::new("/x/package.json")), None);
    }

    #[test]
    fn generic_stems_use_parent_dir() {
        assert_eq!(
            session_id_for_path(AgentSource::Unknown, Path::new("/a/sess-42/events.jsonl")),
            "sess-42"
        );
        assert_eq!(
            session_id_for_path(AgentSource::ClaudeCode, Path::new("/a/p/abc-123.jsonl")),
            "abc-123"
        );
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("short", 10), "short");
    }
}
