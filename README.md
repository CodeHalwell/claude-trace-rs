# claude-trace-rs

> Local-first real-time observability dashboard, **persistent trace database**, and training-dataset exporter for coding-agent sessions — as a CLI, a background service or a **native desktop app**.

`claude-trace-rs` is a cross-platform Rust tool that reads the session logs of **the coding agents you already use** — Claude Code, Codex, Gemini CLI, Qwen Code, GitHub Copilot CLI, Cursor, Cline, Roo Code, Kilo Code, OpenCode, Crush, Goose, Aider, Continue, Kimi Code, Amp and Factory Droid — normalises them into one conversation model, **persists them to a built-in SQLite database**, shows what every session is doing in a live dashboard, and exports the lot as clean training data.

It is designed for the case where you have **many coding agents running in parallel** (different projects, different worktrees, different tools). Each session is attributed to the agent that produced it, grouped by project, threaded into a conversation view and broken down into token, cost and tool-usage metrics — stored locally and exportable as a dataset.

Two ways to run it:

- **Agent Trace desktop app** (Windows, macOS, Linux) — the dashboard in its own window with a tray icon, a notification when an agent finishes and is waiting on you, a daily budget alert, launch at login and native save dialogs. See [Desktop app](#desktop-app).
- **`claude-trace-rs` CLI** — a single binary that serves the same dashboard in your browser, runs as a background service, and exports datasets from the command line.

## Supported agents

| Agent | Default log location | Format | Notes |
| ----- | -------------------- | ------ | ----- |
| **Claude Code** | `~/.claude/projects` (`$CLAUDE_CONFIG_DIR`) | JSONL | Sub-agent transcripts kept as their own sessions; usage counted once per API response |
| **Codex CLI** | `~/.codex/sessions` (`$CODEX_HOME`) | JSONL rollouts | Current and pre-0.32 formats, paginated history, token-usage records |
| **Gemini CLI** | `~/.gemini/tmp/<project>/chats` | JSONL patch log, legacy JSON | Rewinds and in-place updates applied |
| **Qwen Code** | `~/.qwen/projects/<project>/chats` | JSONL (tree) | |
| **GitHub Copilot CLI** | `~/.copilot/session-state` | JSONL events | Token usage from the session summary |
| **Cursor** | `~/.cursor/chats`, `~/.cursor/projects` | SQLite store, JSONL transcripts | |
| **Cline** / **Roo Code** / **Kilo Code** | VS Code `globalStorage/<extension>/tasks`, `~/.cline/data` | JSON per task | VS Code, Insiders, VSCodium, Cursor and Windsurf storage |
| **OpenCode** | `~/.local/share/opencode` | SQLite, legacy JSON store | Kilo Code 7's CLI database too |
| **Crush** | `<project>/.crush/crush.db` | SQLite | Point `--watch-root` at your projects |
| **Goose** | `~/.local/share/goose/sessions` | SQLite, legacy JSONL | |
| **Aider** | `<project>/.aider.chat.history.md` | Markdown log | Point `--watch-root` at your projects |
| **Continue** | `~/.continue/sessions` | JSON per session | |
| **Kimi Code** | `~/.kimi-code/sessions`, `~/.kimi/sessions` | JSONL wire log | |
| **Amp** | `~/.local/share/amp/threads` | JSON threads | Older local threads only; current Amp keeps threads server-side |
| **Factory Droid** | `~/.factory/sessions` | JSONL + settings sidecar | |

Paths are the Linux/macOS defaults; Windows and macOS application-support locations and the agents' own environment variables are honoured too. `claude-trace-rs agents` shows exactly where each agent's logs are expected on your machine and which were found.

Every known directory that **exists** is watched automatically, and agents installed while the tracer is running are picked up within 30 seconds. Point at anything else with `--watch-root <DIR>` (repeatable) and force an adapter with `--source <id>`; restrict to a subset with `--only claude-code,codex,…`, or skip the auto-discovered roots with `--no-default-roots`.

## Highlights

### Harmonised multi-agent tracing
- **One dashboard for every agent.** Sessions from every supported agent stream into a single feed, each tagged with a colour-coded agent badge.
- **One conversation model.** Each adapter turns its agent's records into the same Anthropic-style messages (text, thinking, tool use, tool result), so transcripts, costs and exports are comparable across tools.
- **Handles how agents actually write.** Append-only logs are tailed; files rewritten in place (Gemini, Copilot, Cline) are re-read and diffed; SQLite stores (OpenCode, Cursor, Crush, Goose) are re-queried. Changed records are updated, not duplicated, and a restart catches up on anything written while the tracer was stopped.
- **Source filtering everywhere.** Filter the sidebar, search, analytics, and exports by agent.

### Built-in trace database
- **Everything is persisted** to an embedded SQLite database (compiled into the binary via `rusqlite` — nothing to install). Traces survive restarts, machine reboots, and far exceed the in-memory ring buffers.
- **Full history retrieval.** Scroll a session's entire transcript (not just the last few thousand events), paginated straight from the database.
- **Fast search** across every event ever recorded, plus per-session filtering by type and text.
- **Cross-session analytics** computed in SQL: totals, cost-by-model, cost-by-agent, token breakdown, top tools, a 30-day activity timeline.
- **Server-side annotations.** Bookmarks, tags, and notes are stored in the database, so they follow your data instead of a single browser's `localStorage`.

### Real-time dashboard (redesigned)
- **Clean, uncluttered UI** with a calm light/dark theme, a single global search, a project-grouped session navigator, and four focused tabs (Live · Conversation · Analytics · Agents).
- **Multi-session sidebar.** Sessions grouped by project (cwd), with a live-activity dot, agent badge, event/cost summary, last-seen time, and one-click bookmarking.
- **Live event feed.** Real-time stream over WebSocket with type/text filters, pause/resume, and a slide-in JSON inspector.
- **Conversation view.** Threaded transcript of user / assistant / tool messages — text, `thinking` blocks, `tool_use` invocations with inputs, `tool_result` payloads, and a **latency badge** (`⚡ 2.4s`) on each assistant turn. Codex `function_call` items render natively.
- **Analytics tab.** Tokens, cache usage, estimated cost (public per-model pricing), top tool calls, cost-by-model, cost-by-agent, and an activity timeline.
- **Agents tab.** Every supported agent, where its logs live, whether they were found and how many sessions each has produced.
- **Resume in one click.** Copy the command that reopens a session in its own agent (`claude --resume …`, `codex resume …`, `gemini --resume …`).

### Training-dataset export

Six output formats with full content-block fidelity. Consecutive records from the same turn are merged, tool results are placed in the following user turn, and system/context messages are kept separately, so every format has properly alternating roles whichever agent produced the session:

| Format        | Shape                                                     | Best for |
| ------------- | --------------------------------------------------------- | -------- |
| `messages`    | Anthropic Messages JSONL (`{messages:[{role,content}]}`) | Claude fine-tuning, Anthropic SDK |
| `openai`      | OpenAI Chat / Tools (`{messages:[{…,tool_calls}]}`)      | OpenAI / generic LLM fine-tuning |
| `sharegpt`    | `{conversations:[{from,value}]}`                          | HF Datasets, Axolotl, Unsloth |
| `huggingface` | A directory with `train.jsonl` + `dataset_info.json` + `README.md` | `datasets.load_dataset(...)` |
| `jsonl`       | Raw agent records, one per line (full fidelity)           | Reprocessing pipelines |
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

`claude-trace-rs` ships as a self-contained binary for **Windows, macOS, and Linux** — no runtime, no system SQLite, nothing else to install. The desktop app is a separate download; see [Desktop app](#desktop-app).

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

## Desktop app

**Agent Trace** is the same dashboard in a native window (Tauri — the system webview, not a bundled browser), with the things a browser tab cannot do:

- **Notifications when an agent finishes** and is waiting on you, with the project, turn duration, cost and your prompt. Agents that mark the end of a turn (Claude Code, Codex, Gemini, Copilot, Droid, …) notify immediately; for the rest, a configurable quiet period is used.
- **Tray icon** showing active sessions and today's spend. Closing the window keeps tracing in the background.
- **Daily budget alert** — one notification when today's estimated spend crosses a limit you set.
- **Launch at login**, start hidden in the tray, single instance.
- **Native save dialogs** for exports, including a real HuggingFace dataset folder (`train.jsonl`, `dataset_info.json`, dataset card).
- **Open a session's project folder**, and copy the command that resumes it in its agent.
- A **Settings** panel (⚙ in the header): notifications, idle threshold, budget, window behaviour, port, agent filter and extra folders.

### Install

Download the installer for your platform from the [Releases page](https://github.com/CodeHalwell/claude-trace-rs/releases):

| Platform | File |
| -------- | ---- |
| Windows  | `Agent.Trace_<version>_x64-setup.exe` (or the `.msi`) |
| macOS    | `Agent.Trace_<version>_aarch64.dmg` (Apple silicon) / `_x64.dmg` (Intel) |
| Linux    | `.deb`, `.rpm` or `.AppImage` |

The builds are not code-signed yet, so the first launch needs one extra step: on macOS, right-click the app and choose **Open**; on Windows, choose **More info → Run anyway** in SmartScreen.

### How it runs

On start-up the app looks for a claude-trace-rs server on its port (7779 by default). If the background service (`claude-trace-rs service install`) or a `claude-trace-rs serve` is already running there, the app **attaches** to it — one tracer, one database. The server has to prove it is yours first (an HMAC of a random challenge, keyed by a per-user `server.key` in the data directory); anything else on the port is ignored. Otherwise it starts its own tracer in-process, sharing the CLI's database, so history recorded by either is visible in both. The first run imports everything already on disk; later runs catch up from where they left off.

| | Linux | macOS | Windows |
| - | ----- | ----- | ------- |
| Settings | `~/.config/io.github.codehalwell.agent-trace/settings.json` | `~/Library/Application Support/io.github.codehalwell.agent-trace/` | `%APPDATA%\io.github.codehalwell.agent-trace\` |
| Log file | `~/.local/share/io.github.codehalwell.agent-trace/logs/` | `~/Library/Logs/io.github.codehalwell.agent-trace/` | `%LOCALAPPDATA%\io.github.codehalwell.agent-trace\logs\` |

### Build it yourself

```bash
# Linux only: the webview and tray libraries
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev

cargo run -p agent-trace-desktop                  # development build
cd desktop && npx @tauri-apps/cli@2 build         # installers in target/release/bundle/
```

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
  agents   Show every supported agent, where its logs live, and whether they were found
  service  Install/manage a background service (install | uninstall | status)

Global options:
  -w, --watch-root <DIR>   Where to read agent trace files from. Repeatable.
                           [env: CLAUDE_TRACE_WATCH_ROOT]
                           (default: every known agent log dir that exists)
      --source <ID>        Force the adapter for --watch-root dirs (ids from `agents`):
                           claude-code | codex | gemini | qwen | copilot | cursor |
                           cline | roo-code | kilo-code | opencode | crush | goose |
                           aider | continue | kimi | amp | droid
      --only <IDS>         Restrict tracing to these agents (comma-separated)
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

agents:
      --json                Machine-readable output
```

Without `--backfill`, `serve` resumes each file from where the previous run left off (checkpoints live in the database) and reads files written while it was stopped in full, so nothing is missed or counted twice across restarts.

### Pricing

Costs are estimated from token counts when an agent does not report them, using public list prices for the Claude, GPT/o-series, Gemini, Qwen, Kimi, DeepSeek, GLM and Grok families. They are for spotting trends, not billing. To correct a price or add a model, create `pricing.json` in the config directory (`~/.config/claude-trace-rs/` on Linux, `~/Library/Application Support/rs.claude-trace.claude-trace-rs/` on macOS, `%APPDATA%\claude-trace\claude-trace-rs\config\` on Windows):

```json
[
  { "match": "my-finetune", "input": 1.0, "output": 4.0, "cache_read": 0.1, "cache_write": 1.25 }
]
```

Prices are USD per million tokens; `match` is a case-insensitive substring of the model name, and overrides win over the built-in table.

## HTTP API

All endpoints are loopback-only. Requests whose `Host` is not a loopback name, and cross-origin requests from anything but `http(s)://127.0.0.1` / `localhost` / `[::1]`, are rejected with `403`.

| Endpoint                                       | Description                                |
| ---------------------------------------------- | ------------------------------------------ |
| `GET /health`                                  | Liveness, version and counts               |
| `GET /api/agents`                              | Supported agents, their log folders, what was found and session counts |
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
| `GET /api/db/cost?since=<RFC 3339>`            | Estimated spend since a point in time      |
| `GET /api/db/sessions/:id/meta`                | Read bookmark / tags / notes               |
| `POST /api/db/sessions/:id/meta`               | Persist bookmark / tags / notes            |

Events carry a `message` field — the record as an agent-neutral `{role, content: [blocks]}` message — alongside the raw `entry`, so API clients can read any agent's transcript the same way.

```bash
curl -OJ "http://127.0.0.1:7779/api/sessions/$SID/export?format=huggingface"
```

## How it works

```
 agent logs: JSONL · rewritten JSON documents · SQLite stores · Markdown
        │
        ▼
 notify (inotify / kqueue / FSEvents) + 300 ms debounce
        │
        ▼
 ingest engine (ingest.rs)
   • JSONL        → tail from a saved byte offset
   • documents    → re-parse, diff records by hash, upsert / remove
   • SQLite       → re-query with a per-adapter cursor
   • multi-file   → map a file to its session unit and rebuild it
        │
        ▼
 per-agent adapter (sources/*.rs) ──▶ TraceEvent + canonical Message
        │                                (message.rs: role + text / thinking /
        │                                 tool_use / tool_result / image blocks)
        ▼
 ┌──────────────┬─────────────────┬───────────────────┬──────────────┐
 ▼              ▼                 ▼                   ▼              ▼
broadcast    SessionStore      SQLite database     export.rs     desktop app
channel      (live aggregates) (events, sessions,  (6 formats)   (notifications,
 │                              checkpoints)                       tray, budget)
 ▼
WebSocket → dashboard
```

### Where data is stored

The database lives in your platform's data directory (override with `--db` or `CLAUDE_TRACE_DB`):

| OS      | Default path                                                   |
| ------- | ------------------------------------------------------------- |
| Linux   | `~/.local/share/claude-trace-rs/trace.db`                     |
| macOS   | `~/Library/Application Support/claude-trace-rs/trace.db`      |
| Windows | `%APPDATA%\claude-trace-rs\data\trace.db`                     |

Events are keyed by `(session_id, line_index)`: a record that changes (a document rewritten in place, a database row updated) replaces its earlier version and the session totals are corrected, so restarts and `--backfill` never double-count. Nothing ever leaves your machine.

- Each file is attributed to an **agent source** (its location first, then content sniffing) and normalised by that agent's adapter.
- Session ids come from the records themselves where present, so concurrent agents writing into the same directory stay separate. Sub-agent transcripts get their own ids.
- Usage that an agent repeats across records (Claude Code writes one line per content block, each carrying the whole response's usage; Codex reports running totals) is counted once.
- A bounded in-memory `SessionStore` keeps per-session aggregates and a ring buffer of recent events for instant dashboard snapshots; full history is read from the database.
- The `export` and `list` subcommands read the log folders once with the same engine and exit.

## Security

- Binds **only** to `127.0.0.1` — never to all interfaces.
- Requests are rejected with `403` unless the `Host` header is a loopback name (DNS-rebinding defence), and WebSocket upgrades and `/api/*` requests are rejected when the `Origin` is anything other than exactly `http(s)://127.0.0.1` / `localhost` / `[::1]` (with any port). No-Origin requests (curl, server-to-server) pass through.
- CORS allow-origin is the same exact loopback check, not `Any`.
- The desktop app grants its dashboard only the app's own commands, only on the port its tracer uses, and only attaches to a server that proves it holds your per-user key; links leave the app for your browser, and "open folder" never runs a file.
- No telemetry, no outbound calls.

## Development

```bash
cargo test -p claude-trace-rs                     # CLI + library
cargo test -p agent-trace-desktop                 # desktop app (needs the webview libraries on Linux)
cargo run --release -- serve --open --backfill
cargo run -p agent-trace-desktop
```

The repository is a Cargo workspace. `cargo build` / `cargo install --path .` build only the CLI; the desktop app is opt-in.

```
src/
  main.rs         CLI: serve / export / list / agents / service
  lib.rs          library root shared with the desktop app
  runtime.rs      start database + watcher + server in one call; root resolution
  message.rs      canonical agent-neutral message model
  ingest.rs       ingestion engine: JSONL tail, documents, SQLite, multi-file stores
  sources/        agent registry, detection and one adapter per agent
                  (claude, codex, gemini, qwen, copilot, cursor, cline, opencode,
                   crush, goose, aider, continue_dev, kimi, amp, droid)
  pricing.rs      per-model price table + user overrides
  event.rs        TraceEvent — the normalised transport object
  state.rs        SessionStore — live aggregates + ring buffers, DB write-through
  db.rs           SQLite: events, sessions, annotations, checkpoints, analytics
  watcher.rs      filesystem watching + new-agent discovery
  loader.rs       one-shot ingestion for export / list
  export.rs       Anthropic / OpenAI / ShareGPT / raw / Markdown / HuggingFace
  server.rs       axum router, REST + DB + export endpoints, WebSocket
  dashboard.rs    built-in single-page UI
tests/fixtures/   real (sanitised) agent logs used by the tests
desktop/          Agent Trace (Tauri 2): tray, notifications, settings, native dialogs
```

See [`ROADMAP.md`](ROADMAP.md) for planned improvements and ideas.

## License

MIT.
