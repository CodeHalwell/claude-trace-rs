# claude-trace-rs

> Local-first real-time observability dashboard **and training-dataset exporter** for Claude Code sessions.

`claude-trace-rs` is a single Rust binary that tails every JSONL session log Claude Code writes to disk, parses the events as they arrive, surfaces what every concurrent session is doing in a rich browser dashboard, and can dump the lot to disk in any of six training-friendly formats.

It is designed for the case where you have **multiple Claude Code instances running in parallel** (different projects, different worktrees, multiple windows). Each session's events are clearly separated: grouped by project, threaded into a conversation view, broken down into per-session token, cost and tool-usage metrics — and exportable as a clean dataset.

## Highlights

### Real-time dashboard
- **Multi-session sidebar.** Every concurrent Claude Code window is its own entry, grouped by project (cwd), with a live-activity dot, a per-session sparkline of event rate, the last-seen time, and per-session bookmarks + freeform tags persisted in `localStorage`.
- **Live event feed.** Stream of newly observed events with badges per event type, session tag, token/cost columns, type and free-text filters, pause/resume, JSON inspector, and saved filter views.
- **Conversation view.** Threaded transcript of user / assistant / tool messages for the selected session — text, `thinking` blocks, `tool_use` invocations with inputs, `tool_result` payloads, **latency badge** (`⚡ 2.4s`) on each assistant turn relative to the preceding user message.
- **Metrics tab.** Tokens, cache-hit rate, estimated cost (uses public Claude pricing), top tool calls, cost / tokens leaderboards, 60-minute event-rate timeline, p50/p95 latency.

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

### From source

```bash
git clone https://github.com/CodeHalwell/claude-trace-rs
cd claude-trace-rs
cargo install --path .
```

Installs `claude-trace-rs` into `~/.cargo/bin` (make sure that's on `$PATH`).

### Run without installing

```bash
cargo run --release -- --open
```

## Use it

### Live dashboard

```bash
claude-trace-rs                       # serve, default port 7779
claude-trace-rs serve --open          # open browser automatically
claude-trace-rs serve --backfill      # replay everything already on disk
```

Open as many Claude Code windows as you like — each session shows up in the sidebar as it produces its first event. Bookmark the ones you care about, tag them, and the dashboard remembers.

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
from datasets import load_dataset
ds = load_dataset("json", data_files={"train": "~/datasets/my-claude-runs/train.jsonl"})
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
  serve   Run the live dashboard server (default)
  export  Export one or more sessions to disk in a training-friendly format
  list    Print every session discovered on disk as JSON

Global options:
  -w, --watch-root <DIR>   Where to read JSONL session files from
                           [env: CLAUDE_TRACE_WATCH_ROOT, default: ~/.claude/projects]

serve:
  -p, --port <PORT>            HTTP/WS port [env: CLAUDE_TRACE_PORT, default: 7779]
      --channel-capacity <N>   Per-subscriber broadcast buffer [default: 1024]
      --backfill               Replay every event already on disk
      --open                   Open the dashboard URL in a browser

export:
  -f, --format <FMT>        messages | openai | sharegpt | jsonl | markdown | huggingface
                            [default: messages]
  -o, --out <PATH>          Output file (or directory for --format huggingface). Use '-' for stdout.
      --session <IDS>       Comma-separated list of session IDs (default: all)
      --min-events <N>      Skip sessions with fewer events than this [default: 1]
```

## HTTP API

All endpoints are localhost-only. Cross-origin requests are rejected with `403`.

| Endpoint                                       | Description                                |
| ---------------------------------------------- | ------------------------------------------ |
| `GET /health`                                  | Liveness + counts                          |
| `GET /api/sessions`                            | Every known session with aggregates        |
| `GET /api/sessions/:id`                        | One session's aggregates                   |
| `GET /api/sessions/:id/events?limit`           | Buffered recent events                     |
| `GET /api/sessions/:id/export?format=…`        | Download one session (any of the 6 formats)|
| `GET /api/export?format=…&sessions=id1,id2`    | Bulk export (omit `sessions` for all)      |
| `GET /api/snapshot?events=N`                   | Sessions + last N global events            |
| `WS /ws`                                       | Live snapshot + event stream               |

```bash
curl -OJ "http://127.0.0.1:7779/api/sessions/$SID/export?format=huggingface"
```

## How it works

```
~/.claude/projects/<project>/<sessionId>.jsonl
              │
              ▼
   notify (inotify / kqueue / FSEvents) ──▶ tailing line reader
                                                │
                                                ▼
                                  parser + enricher (event.rs)
                                                │
                          ┌─────────────────────┼─────────────────────┐
                          ▼                     ▼                     ▼
                broadcast::channel       SessionStore        export module
                          │             (state.rs)             (export.rs)
                          ▼                     ▼                     ▼
              WebSocket subscribers      REST snapshot            CLI / HTTP
                                                                  download
```

- Each parsed line is enriched with `session_id` taken from the entry's `sessionId` field — so two concurrent Claude Code processes that happen to write to the same path stay cleanly separated.
- Tool names are extracted from embedded `tool_use` content blocks; tokens from input/output/cache fields; cost estimated per-model where Claude Code doesn't include `costUSD`.
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
  main.rs        CLI subcommands (serve/export/list), tilde expansion, browser launch
  event.rs       TraceEvent + enrichment (model, tools, usage, cost)
  state.rs       SessionStore — aggregates + ring buffers
  watcher.rs     filesystem tail; partial-write + truncation safety
  loader.rs      one-shot directory ingestion for offline CLI export
  export.rs      Anthropic / OpenAI / ShareGPT / Raw / Markdown / HuggingFace
  server.rs      axum router, REST + export endpoints, WebSocket handler
  dashboard.rs   built-in single-page HTML + JS UI
```

## License

MIT.
