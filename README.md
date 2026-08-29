# claude-trace-rs

> Local-first real-time observability dashboard, **persistent trace database**, and training-dataset exporter for terminal coding-agent sessions.

`claude-trace-rs` is a single, cross-platform Rust binary that tails the session logs of **multiple terminal coding agents** — Claude Code, Codex CLI, GitHub Copilot CLI, Kimi Code, Cline, and Cursor Agent — parses the events as they arrive, **persists them to a built-in SQLite database**, surfaces what every session is doing in a clean browser dashboard, and can dump the lot to disk in any of six training-friendly formats.

It is designed for the case where you have **many coding agents running in parallel** (different projects, different worktrees, multiple windows, different tools). Each session's events are clearly separated and **attributed to the agent that produced them**: grouped by project, threaded into a conversation view, broken down into per-session token, cost and tool-usage metrics — stored forever in a local database and exportable as a clean dataset.

## Supported agents

| Agent | Default log location | Format | Status |
| ----- | -------------------- | ------ | ------ |
| **Claude Code** | `~/.claude/projects/**/*.jsonl` | JSONL | ✅ Full |
| **Codex CLI** | `~/.codex/sessions/**/rollout-*.jsonl` | JSONL | ✅ Full |
| **GitHub Copilot CLI** | `~/.copilot/**` | JSONL | ✅ Best-effort |
| **Kimi Code** | `~/.kimi/**` | JSONL (Claude-shaped) | ✅ Full |
| **Cline** (VS Code ext) | `<globalStorage>/saoudrizwan.claude-dev/tasks/**/api_conversation_history.json` | Whole-file JSON array | ✅ Full |
| **Cursor Agent** | `~/.cursor/projects`, `~/.cursor-agent` | JSONL | ✅ Best-effort |

Every known directory that **exists** is watched automatically; point at
anything else with `--watch-root <DIR>` (repeatable) and force an adapter with
`--source <agent>`. Restrict to a subset with `--only claude,codex,…`, or skip
the auto-discovered roots entirely with `--no-default-roots`.

## Highlights

### Harmonised multi-agent tracing
- **One dashboard for every agent.** Claude Code, Codex, Copilot, Kimi, Cline, and Cursor sessions stream into a single unified feed, each tagged with a colour-coded agent badge.
- **Per-agent adapters.** Each agent's on-disk format is normalised into one event model, so cost, tokens, tool calls, and transcripts are comparable across tools.
- **Source filtering everywhere.** Filter the sidebar, search, analytics, and exports by agent.

### Built-in trace database
- **Everything is persisted** to an embedded SQLite database (compiled into the binary via `rusqlite` — nothing to install). Traces survive restarts, machine reboots, and far exceed the in-memory ring buffers.
- **Full history retrieval.** Scroll a session's entire transcript (not just the last few thousand events), paginated straight from the database.
- **Fast search** across every event ever recorded, plus per-session filtering by type and text.
- **Cross-session analytics** computed in SQL: totals, cost-by-model, cost-by-agent, token breakdown, top tools, a 30-day activity timeline.
- **Server-side annotations.** Bookmarks, tags, and notes are stored in the database, so they follow your data instead of a single browser's `localStorage`.

### Real-time dashboard (redesigned)
- **Clean, uncluttered UI** with a calm light/dark theme, a single global search, a project-grouped session navigator, and three focused tabs (Live · Conversation · Analytics).
- **Multi-session sidebar.** Sessions grouped by project (cwd), with a live-activity dot, agent badge, event/cost summary, last-seen time, and one-click bookmarking.
- **Live event feed.** Real-time stream over WebSocket with type/text filters, pause/resume, and a slide-in JSON inspector.
- **Conversation view.** Threaded transcript of user / assistant / tool messages — text, `thinking` blocks, `tool_use` invocations with inputs, `tool_result` payloads, and a **latency badge** (`⚡ 2.4s`) on each assistant turn. Codex `function_call` items render natively.
- **Analytics tab.** Tokens, cache usage, estimated cost (public per-model pricing), top tool calls, cost-by-model, cost-by-agent, and an activity timeline.

### Training-dataset export

Six output formats with full content-block fidelity:

| Format        | Shape                                                     | Best for |
| ------------- | --------------------------------------------------------- | -------- |
| `messages`    | Anthropic Messages JSONL (`{messages:[{role,content}]}`) | Claude fine-tuning, Anthropic SDK |
| `openai`      | OpenAI Chat / Tools (`{messages:[{…,tool_calls}]}`)      | OpenAI / generic LLM fine-tuning |
| `sharegpt`    | `{conversations:[{from,value}]}`                          | HF Datasets, Axolotl, Unsloth |
| `huggingface` | A directory with `train.jsonl` + `dataset_info.json` + `README.md` | `datasets.load_dataset(...)` |
| `jsonl`       | Raw Claude Code passthrough (full fidelity)              | Reprocessing pipelines |
| `markdown`    | Human-readable transcript                                 | Review, sharing |

### Functional UI

- **Resizable** sidebar + detail panes (widths persisted to `localStorage`).
- **Collapsible** sidebar (`Ctrl/⌘ B`).
- **Multi-select sessions** with checkboxes → bulk export.
- **Bookmarks** + freeform **tags** per session (persisted).
- **Command palette** (`Ctrl/⌘ K`) — fuzzy jump to a session, switch tabs, run actions.
- **Saved filter views** — persist a `(type, search, session, sidebar-search)` combo and recall it.
- **Light / dark theme** toggle.
- **Keyboard shortcuts:** `/` focus search, `esc` clear, `j/k` next/prev event, `f/c/m` switch tabs, `e` export, `b` bookmark, `Space` pause, `?` help.

## Install

`claude-trace-rs` ships as a self-contained binary for **Windows, macOS, and Linux** — no runtime, no system SQLite, nothing else to install.

### One-line installer (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/CodeHalwell/claude-trace-rs/main/scripts/install.sh | sh
```

### One-line installer (Windows, PowerShell)

```powershell
irm https://raw.githubusercontent.com/CodeHalwell/claude-trace-rs/main/scripts/install.ps1 | iex
```

### Download a release

Grab a prebuilt archive (`.tar.gz` / `.zip`) or the Linux `.deb` from the
[Releases page](https://github.com/CodeHalwell/claude-trace-rs/releases), unpack, and put the binary on your `PATH`. Every asset ships with a `.sha256` checksum.

```bash
# Debian / Ubuntu
sudo dpkg -i claude-trace-rs_*_amd64.deb
```

Tagging a release (`git tag v0.3.0 && git push origin v0.3.0`) builds and publishes all of these automatically via the GitHub Actions release workflow.

### From crates.io

```bash
cargo install claude-trace-rs
```

### From source (any platform with Rust)

```bash
git clone https://github.com/CodeHalwell/claude-trace-rs
cd claude-trace-rs
cargo install --path .
```

Installs `claude-trace-rs` into `~/.cargo/bin` (make sure that's on `$PATH`).

## Run it as a background app (no terminal required)

Install it as a per-user background service that starts automatically when you
log in and keeps running with no shell open — using each OS's native mechanism
(systemd user unit on Linux, a LaunchAgent on macOS, a hidden Startup launcher
on Windows). No admin rights needed.

```bash
claude-trace-rs service install            # start now + auto-start at login
claude-trace-rs service install --port 8080 --open
claude-trace-rs service status             # is it installed / running?
claude-trace-rs service uninstall          # stop + remove
```

Then just open <http://127.0.0.1:7779> whenever you want it. On Linux, run
`loginctl enable-linger $USER` once if you want it to keep running while you're
logged out.

## Docker

A multi-arch image is published to the GitHub Container Registry:

```bash
docker run --rm --network host \
  -v "$HOME/.claude/projects:/data/claude" \
  -v "$HOME/.codex/sessions:/data/codex" \
  -v claude-trace-data:/data \
  ghcr.io/codehalwell/claude-trace-rs:latest \
  serve --watch-root /data/claude --watch-root /data/codex
```

The server binds `127.0.0.1` by design, so `--network host` (Linux) is the
simplest way to reach the dashboard from your browser. Mount one host log
directory per agent and pass a matching `--watch-root` for each. The image is
also handy for the offline `export` / `list` subcommands in CI.

### Run without installing

```bash
cargo run --release -- serve --open
```

## Use it

### Live dashboard

```bash
claude-trace-rs                       # serve, default port 7779, all detected agents
claude-trace-rs serve --open          # open browser automatically
claude-trace-rs serve --backfill      # replay everything already on disk
claude-trace-rs serve --only claude,codex   # just these agents
claude-trace-rs serve -w ~/custom/logs --source codex  # custom dir, forced adapter
```

Run as many coding agents as you like — each session shows up in the sidebar, with an agent badge, as it produces its first event. Bookmark the ones you care about, tag them, and the dashboard remembers.

### Export sessions to a training dataset

```bash
# Every session on disk, Anthropic Messages JSONL to stdout
claude-trace-rs export -f messages

# A HuggingFace-loadable dataset directory
claude-trace-rs export -f huggingface -o ~/datasets/my-claude-runs

# Just two sessions, OpenAI Chat/Tools format, into a file
claude-trace-rs export -f openai \
  --session 92072ce0-b5ca-444b-a0b1-5f67327392e3,abc12345-... \
  -o ./training.jsonl

# Markdown transcript for one session
claude-trace-rs export -f markdown --session <UUID> -o run.md

# Filter out tiny sessions
claude-trace-rs export -f messages --min-events 10 -o decent.jsonl
```

Load a HuggingFace export:

```python
import os
from datasets import load_dataset
# datasets does not expand `~`, so do it ourselves.
ds = load_dataset("json", data_files={
    "train": os.path.expanduser("~/datasets/my-claude-runs/train.jsonl")
})
print(ds["train"][0]["messages"][:3])
```

### List sessions as JSON

```bash
claude-trace-rs list | jq '.[] | {id, cwd, event_count, cost_usd}'
```

### From the dashboard

- Click **⤓ Export** in the header → modal with format picker + live preview.
- Or click **☑ Select** in the sidebar to enter multi-select mode, tick sessions, then **Export…**.
- Or open the conversation view for a single session and use **⤓ Export this session**.

## CLI reference

```
Usage: claude-trace-rs [OPTIONS] [COMMAND]

Commands:
  serve    Run the live dashboard server (default)
  export   Export one or more sessions to disk in a training-friendly format
  list     Print every session discovered on disk as JSON
  service  Install/manage a background service (install | uninstall | status)

Global options:
  -w, --watch-root <DIR>   Where to read agent trace files from. Repeatable.
                           [env: CLAUDE_TRACE_WATCH_ROOT]
                           (default: every known agent log dir that exists)
      --source <AGENT>     Force the adapter for --watch-root dirs:
                           claude | codex | copilot | kimi | cline | cursor
      --only <AGENTS>      Restrict tracing to these agents (comma-separated)
      --no-default-roots   Don't auto-add known agent log directories

serve:
  -p, --port <PORT>            HTTP/WS port [env: CLAUDE_TRACE_PORT, default: 7779]
      --channel-capacity <N>   Per-subscriber broadcast buffer [default: 1024]
      --backfill               Replay every event already on disk
      --open                   Open the dashboard URL in a browser
      --db <PATH>              SQLite database file [env: CLAUDE_TRACE_DB,
                               default: platform data dir, see below]

export:
  -f, --format <FMT>        messages | openai | sharegpt | jsonl | markdown | huggingface
                            [default: messages]
  -o, --out <PATH>          Output file (or directory for --format huggingface). Use '-' for stdout.
      --session <IDS>       Comma-separated list of session IDs (default: all)
      --from <AGENTS>       Only export sessions from these agents (comma-separated)
      --min-events <N>      Skip sessions with fewer events than this [default: 1]
```

## HTTP API

All endpoints are localhost-only. Cross-origin requests are rejected with `403`.

| Endpoint                                       | Description                                |
| ---------------------------------------------- | ------------------------------------------ |
| `GET /health`                                  | Liveness + counts                          |
| `GET /api/sessions`                            | Every in-memory session with aggregates    |
| `GET /api/sessions/:id`                        | One session's aggregates                   |
| `GET /api/sessions/:id/events?limit`           | Buffered recent events                     |
| `GET /api/sessions/:id/export?format=…`        | Download one session (any of the 6 formats)|
| `GET /api/export?format=…&sessions=id1,id2`    | Bulk export (omit `sessions` for all)      |
| `GET /api/snapshot?events=N`                   | Sessions + last N global events            |
| `WS /ws`                                       | Live snapshot + event stream               |
| **Database-backed (persistent across restarts):** | |
| `GET /api/db/sessions?search&project&source&bookmarked&sort&limit` | All persisted sessions, filtered/sorted |
| `GET /api/db/projects`                         | Distinct projects with session counts      |
| `GET /api/db/sources`                          | Per-agent session/event/cost rollups       |
| `GET /api/db/sessions/:id/events?type&search&limit&offset` | Paginated full session history  |
| `GET /api/db/search?q=…&limit&source`          | Full-text-ish search across all events     |
| `GET /api/db/stats`                            | Cross-session analytics rollups            |
| `GET /api/db/sessions/:id/meta`                | Read bookmark / tags / notes               |
| `POST /api/db/sessions/:id/meta`               | Persist bookmark / tags / notes            |

```bash
curl -OJ "http://127.0.0.1:7779/api/sessions/$SID/export?format=huggingface"
```

## How it works

```
~/.claude/projects/…  ~/.codex/sessions/…  ~/.copilot/…  Cline tasks/ …
              │                 │                │              │
              └─────────────────┴───────┬────────┴──────────────┘
                                        ▼
              notify (inotify / kqueue / FSEvents) ──▶ tailing line reader
                                        │
                                        ▼
                    source detection (sources/mod.rs: path → content sniff)
                                        │
                                        ▼
                    per-agent adapter (sources/{claude,codex,copilot,
                                              kimi,cline,cursor}.rs)
                                        │
                                        ▼
                          normalised TraceEvent (event.rs)
                                        │
              ┌─────────────────────────┼───────────────────────────┐
              ▼                     ▼            ▼                  ▼
    broadcast::channel       SessionStore   SQLite database    export module
              │             (in-memory       (db.rs, on disk)    (export.rs)
              ▼              live feed)            │                  ▼
   WebSocket subscribers          │       history / search /     CLI / HTTP
        (Live tab)                ▼       analytics / pagination   download
                          REST snapshot   (Conversation, Analytics,
                                           global search tabs)
```

### Where data is stored

The database lives in your platform's data directory (override with `--db` or `CLAUDE_TRACE_DB`):

| OS      | Default path                                                   |
| ------- | ------------------------------------------------------------- |
| Linux   | `~/.local/share/claude-trace-rs/trace.db`                     |
| macOS   | `~/Library/Application Support/claude-trace-rs/trace.db`      |
| Windows | `%APPDATA%\claude-trace-rs\data\trace.db`                     |

Events are de-duplicated by `(session_id, line_index)`, so restarting the watcher or running with `--backfill` never double-counts. Nothing ever leaves your machine.

- Each parsed record is attributed to an **agent source** (path-based first, then content-sniffing) and normalised by that agent's adapter — so Claude Code, Codex, Copilot, Kimi, Cline, and Cursor sessions all land in one model.
- Each parsed line is enriched with a `session_id` taken from the entry's session field where present — so two concurrent agent processes that happen to write to the same path stay cleanly separated. Cline sessions are keyed on their task directory: only
  `api_conversation_history.json` is read, since `ui_messages.json` is the
  UI-layer view of the same conversation and ingesting both would record every
  turn twice under the same session and event key.
- Tool names are extracted from embedded tool-call blocks; tokens from input/output/cache fields; cost estimated per-model (per-agent pricing families) where the agent doesn't report an explicit cost.
- A bounded in-memory `SessionStore` retains per-session aggregates plus a 5,000-event ring buffer per session so reconnecting dashboards (and the live API) get an instant snapshot.
- The `export` CLI subcommand sidesteps the watcher entirely — it walks the watch root once via `loader.rs`, then emits the chosen format and exits.

## Security

- Binds **only** to `127.0.0.1` — never to all interfaces.
- WebSocket upgrades and `/api/*` requests are rejected with `403` when the `Origin` header is anything other than `http(s)://127.0.0.1` / `localhost` / `[::1]`. No-Origin requests (curl, server-to-server) pass through.
- CORS allow-origin is a localhost predicate, not `Any`.
- No telemetry, no outbound calls.

## Development

```bash
cargo test
cargo run --release -- serve --open --backfill
cargo install --path .
```

Project layout:

```
src/
  main.rs        CLI subcommands (serve/export/list), watch-root resolution, DB bootstrap
  sources/       multi-agent detection + adapters
    mod.rs       AgentSource enum, detection, pricing table, shared helpers
    claude.rs    Claude Code adapter (+ generic fallback for unknown formats)
    codex.rs     Codex CLI rollout adapter
    copilot.rs   GitHub Copilot CLI adapter
    kimi.rs      Kimi Code adapter
    cline.rs     Cline (VS Code) adapter — whole-file JSON arrays
    cursor.rs    Cursor Agent adapter
  event.rs       TraceEvent — normalised transport object, source-tagged
  state.rs       SessionStore — in-memory aggregates + ring buffers, DB write-through
  db.rs          embedded SQLite store: events, sessions, annotations, analytics
  watcher.rs     multi-root filesystem tail; partial-write + truncation safety
  loader.rs      one-shot multi-root ingestion for offline CLI export
  export.rs      Anthropic / OpenAI / ShareGPT / Raw / Markdown / HuggingFace
  server.rs      axum router, REST + DB + export endpoints, WebSocket handler
  dashboard.rs   built-in single-page HTML + JS UI (Live / Conversation / Analytics)
```

See [`ROADMAP.md`](ROADMAP.md) for planned improvements and ideas.

## License

MIT.
