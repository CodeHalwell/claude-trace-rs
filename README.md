# claude-trace-rs

> Local-first real-time observability dashboard for Claude Code sessions.

`claude-trace-rs` is a single Rust binary that tails every JSONL session log Claude Code writes to disk, parses the events as they arrive, and serves a built-in browser dashboard that surfaces what every concurrent session is doing — right now.

It is designed for the case where you have **multiple Claude Code instances running in parallel** (different projects, different worktrees, multiple windows). Each session's events are clearly separated: grouped by project, threaded into a conversation view, and broken down into per-session token, cost and tool-usage metrics.

## Features

- **Real-time tail of every session.** Watches `~/.claude/projects/**/*.jsonl` recursively. Picks up sessions that start after the dashboard is launched. Handles partial writes, file truncation and recursion correctly.
- **Multi-session sidebar.** Every concurrent Claude Code window is its own entry, grouped by project (cwd), with a live indicator, last-activity time and an inline sparkline of event rate. Click a session to filter the rest of the UI to it.
- **Live event feed.** Newly observed events stream in with badges per event type, session tag, token / cost columns, type and free-text filters, pause/resume, and JSON inspector.
- **Conversation view.** Renders user / assistant / tool messages chronologically for the selected session — text, `thinking` blocks, `tool_use` invocations with inputs, `tool_result` payloads. Reads like a transcript.
- **Metrics tab.** Tokens in/out, cache-hit rate, estimated USD cost (uses public Claude pricing — Sonnet / Opus / Haiku families), top tool calls bar chart, cost-per-session and tokens-per-session leaderboards, 60-minute event-rate timeline.
- **HTTP + WebSocket API.** Late-joining clients get an automatic snapshot of every known session and the recent event tail, then live deltas.
- **Single-binary, no external services.** Pure Rust. Binds to `127.0.0.1` only. Origin-checked WebSocket. No telemetry.

## Install

### From source (recommended)

```bash
git clone https://github.com/CodeHalwell/claude-trace-rs
cd claude-trace-rs
cargo install --path .
```

This places a `claude-trace-rs` binary in `~/.cargo/bin` (make sure that's on your `PATH`).

### One-off run without installing

```bash
cargo run --release -- --open
```

## Run

```bash
# Most common: open the dashboard in your browser, watch the default Claude Code dir.
claude-trace-rs --open

# Replay everything already on disk, then tail for new events.
claude-trace-rs --open --backfill

# Watch a non-standard location, custom port.
claude-trace-rs -w /path/to/sessions -p 7700 --open
```

Then start Claude Code as usual in any number of windows / projects. Each session shows up in the sidebar as it produces its first event.

The dashboard is at `http://127.0.0.1:7779/` (or whichever `--port` you chose).

## CLI

```
Usage: claude-trace-rs [OPTIONS]

Options:
  -w, --watch-root <WATCH_ROOT>            Root directory to watch for Claude Code JSONL session files
                                           [env: CLAUDE_TRACE_WATCH_ROOT=] [default: ~/.claude/projects]
  -p, --port <PORT>                        TCP port to bind the HTTP and WebSocket server to
                                           [env: CLAUDE_TRACE_PORT=] [default: 7779]
      --channel-capacity <N>               Broadcast channel capacity (events buffered per subscriber)
                                           [default: 1024]
      --backfill                           Replay every event already on disk into the in-memory store
                                           [env: CLAUDE_TRACE_BACKFILL=]
      --open                               Open the dashboard URL in the default browser at startup
                                           [env: CLAUDE_TRACE_OPEN=]
  -h, --help                               Print help
  -V, --version                            Print version
```

All flags are also settable via environment variables (e.g. `CLAUDE_TRACE_PORT=7700 claude-trace-rs`).

## HTTP API

The same data the dashboard renders is available as JSON:

| Endpoint                             | Description                                                                    |
| ------------------------------------ | ------------------------------------------------------------------------------ |
| `GET /health`                        | Liveness + counts                                                              |
| `GET /api/sessions`                  | Every known session with aggregate stats                                       |
| `GET /api/sessions/:id`              | One session's aggregate stats                                                  |
| `GET /api/sessions/:id/events?limit` | Buffered recent events for a session (newest `limit`)                          |
| `GET /api/snapshot?events=N`         | Snapshot of all sessions + last N global events                                |
| `WS /ws`                             | Connection-time snapshot, then live `TraceEvent` deltas                        |

Example:

```bash
curl -s http://127.0.0.1:7779/api/sessions | jq '.sessions[] | {id, cwd, event_count, cost_usd}'
```

### WebSocket event shape

Each event broadcast on `/ws` looks like this:

```json
{
  "session_id": "92072ce0-b5ca-444b-a0b1-5f67327392e3",
  "line_index": 14,
  "event_type": "assistant",
  "summary": "🤖 Working on the dashboard · 🔧 Read",
  "observed_at": "2026-05-14T05:39:11.480Z",
  "timestamp": "2026-05-14T05:39:11.300Z",
  "cwd": "/home/user/claude-trace-rs",
  "git_branch": "main",
  "version": "1.0.30",
  "model": "claude-opus-4-7",
  "tool_uses": ["Read"],
  "tool_results": [],
  "usage": { "input": 6, "output": 161, "cache_read": 0, "cache_creation": 25667 },
  "cost_usd": 0.4828,
  "cost_estimated": true,
  "entry": { /* raw JSONL line */ }
}
```

## How it works

```
~/.claude/projects/<project>/<session-uuid>.jsonl
              │
              ▼
   notify (inotify / kqueue / FSEvents) ──▶ tailing line reader
                                                │
                                                ▼
                                  parser + enricher (event.rs)
                                                │
                                                ▼
                            broadcast::channel ──▶  WebSocket subscribers
                                                │
                                                ▼
                            SessionStore (state.rs)
                                                │
                                                ▼
                                  HTTP /api/*  &  /ws snapshot
```

- The watcher seeds existing files to EOF (or replays from byte 0 with `--backfill`), then tails new appended lines.
- Each parsed line is enriched with `session_id` (taken from the entry's `sessionId` when present, so two concurrent processes that share a filename collision still get separated correctly), tool names extracted from embedded `tool_use` blocks, token usage, and an estimated cost based on the model.
- Events are simultaneously broadcast to live WebSocket subscribers **and** ingested into an in-memory `SessionStore` so newly opened dashboard tabs get an immediate snapshot.
- Per-session ring buffers retain up to 5,000 recent events each (configurable in `state.rs`).

## Concurrency and session separation

Each Claude Code instance writes to its own JSONL file (`<sessionId>.jsonl`) inside its project directory. `claude-trace-rs` keys all aggregates on the `sessionId` field carried in every entry — not the filename — so even if you reuse paths or rotate files, distinct sessions stay distinct.

The dashboard sidebar always shows every session it has ever seen during this run, sorted by most-recent activity, with a green "live" dot for anything that has produced an event in the last 10 seconds. Clicking a session filters the feed, conversation, and metrics views to it.

## Cost estimation

If a JSONL entry includes a `costUSD` field (older Claude Code builds did), we use it as-is. Otherwise we estimate from the model and the token-usage fields using public list pricing for the Claude 4-series families (Opus / Sonnet / Haiku). The dashboard tags estimated costs with `(est.)`. Pricing constants live in `src/event.rs::pricing_for` — adjust if your contract differs.

## Security

- Binds **only** to `127.0.0.1` — never to all interfaces.
- WebSocket upgrades are rejected when the `Origin` header is set to anything other than `http(s)://127.0.0.1` or `http(s)://localhost`, defending against cross-site WebSocket hijacking from a browser visiting an unrelated site.
- CORS is permissive on REST endpoints because they only serve data already available locally on disk. Tighten via the source if you proxy this externally.

## Development

```bash
cargo test          # full unit-test suite
cargo run --release -- --open --backfill
cargo build --release
```

Project layout:

```
src/
  main.rs        CLI, tilde expansion, browser-open, wiring
  event.rs       TraceEvent + enrichment (model, tools, usage, cost)
  state.rs       SessionStore — aggregates, ring buffers
  watcher.rs     filesystem tail, partial-write safety, truncation reset
  server.rs      axum router, REST endpoints, WebSocket handler
  dashboard.rs   built-in single-page HTML + JS UI
```

## License

MIT.
