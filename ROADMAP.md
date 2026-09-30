# Roadmap & improvement ideas

This release made the tracer agent-neutral across seventeen coding agents and
added the **Agent Trace desktop app**. Below is a prioritised list of where
`claude-trace-rs` can go next. Nothing here is committed — it's a menu of
ideas, roughly grouped by theme.

## Just shipped ✅
- **Seventeen agents** from verified on-disk formats: Claude Code, Codex,
  Gemini CLI, Qwen Code, Copilot CLI, Cursor, Cline, Roo Code, Kilo Code,
  OpenCode, Crush, Goose, Aider, Continue, Kimi Code, Amp and Factory Droid,
  with real captured logs as test fixtures.
- **Canonical message model** so transcripts, costs and every export format
  are the same shape whichever agent produced them.
- **Ingestion engine** that tails JSONL, re-parses documents rewritten in
  place, re-queries SQLite stores, upserts changed records, and resumes from
  saved checkpoints after a restart (including files written while stopped).
- **Agent Trace desktop app** (Tauri 2): native window, tray, notifications
  when an agent finishes, daily budget alert, launch at login, native export
  dialogs, attach-to-service mode, and installers built by the release workflow.
- **Pricing table with user overrides** (`pricing.json`), current Claude, GPT,
  Gemini and open-model prices.
- **Agents tab / `agents` command** showing where each agent's logs live and
  what was found; new agents are picked up while running.
- Earlier: persistent SQLite store, full-history search, SQL analytics,
  server-side bookmarks/tags/notes, cross-platform installers.

## Near-term, high-impact
1. **SQLite FTS5 full-text search.** Swap the `LIKE` search for an FTS5 virtual
   table for ranked, much faster search over large histories (with snippets and
   highlight). Bundled SQLite already supports it.
2. **Code-signed desktop builds** (Apple notarisation, Windows Authenticode)
   and an in-app updater, so installs need no security prompts.
3. **MSI / `.pkg` / Homebrew tap / winget** packaging via `cargo-dist` for
   true double-click installers and `brew install` / `winget install`.
4. **Date-range & advanced filters** in the sidebar and analytics (today / 7d /
   30d / custom), plus filter-by-model and filter-by-tool.
5. **Per-project budgets.** The desktop app alerts on a daily total; extend it
   to weekly and per-project limits, shown in the dashboard as well.

## Data & retention
6. **Retention / compaction policy.** Configurable pruning (e.g. keep raw events
   90 days, keep aggregates forever) and a `claude-trace-rs db vacuum` command.
7. **Import existing history** command (`db import`) that backfills the database
   from the JSONL files once, with a progress bar.
8. **Diff / replay.** Step through a session like a debugger; diff two sessions
   or two runs of the same prompt.
9. **Fetched pricing.** Optionally refresh the price table from a published
   source instead of waiting for a release (overrides already work).

## Insight & analysis
10. **Per-session summaries** generated from the transcript (first user prompt,
    files touched, tools used, outcome) for a scannable session list.
11. **Tool-failure analytics.** Surface which tools error most, slowest tool
    calls, and retry loops.
12. **Latency percentiles** (p50/p95/p99) per model and per session, with a
    distribution chart.
13. **Heatmap** of activity by hour/day to see when you (and your agents) work.

## Sharing & integration
14. **Shareable read-only session export to a single self-contained HTML file**
    for posting in PRs or sending to a teammate.
15. **Webhook / MCP endpoint** so other tools can subscribe to live events or
    query the database.
16. **Prometheus `/metrics`** endpoint for users who already run Grafana.

## Quality & polish
17. **Virtualised lists** for the feed and conversation so very long sessions
    stay smooth.
18. **Accessibility pass** (keyboard nav for all controls, ARIA roles, reduced-
    motion support).
19. **Browser settings panel.** The desktop app has one; give the browser
    dashboard the same (theme, default tab, feed cap) persisted to the database.
20. **End-to-end UI tests** with a headless browser in CI (currently run by hand
    with Playwright and, for the desktop app, under Xvfb).
21. **More agents** as they appear: e.g. Windsurf/Cascade and JetBrains Junie
    once their local history formats are stable and documented.
