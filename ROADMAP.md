# Roadmap & improvement ideas

This release added three big things: a **built-in SQLite trace database**, a
**redesigned dashboard**, and **cross-platform installers**. Below is a
prioritised list of where `claude-trace-rs` can go next. Nothing here is
committed — it's a menu of ideas, roughly grouped by theme.

## Just shipped ✅
- Persistent, embedded SQLite store for every event (survives restarts).
- Full-history retrieval, pagination, and search backed by the database.
- Cross-session analytics computed in SQL (cost-by-model, top tools, timeline).
- Server-side bookmarks / tags / notes (no longer trapped in `localStorage`).
- Clean, decluttered UI: global search, project-grouped navigator, 3 focused tabs.
- One-line installers for macOS/Linux/Windows, `.deb`, and a tagged-release CI.
- **Harmonised multi-agent tracing** — Claude Code, Codex CLI, Copilot CLI,
  Kimi Code, Cline, and Cursor Agent adapters feeding one dashboard/database/
  exporter, with per-agent badges, filters, and cost analytics.

## Near-term, high-impact
1. **SQLite FTS5 full-text search.** Swap the `LIKE` search for an FTS5 virtual
   table for ranked, much faster search over large histories (with snippets and
   highlight). Bundled SQLite already supports it.
2. **Native desktop app (Tauri).** Wrap the existing UI in a real application
   window with an icon, system tray, and "launch at login," so it stops being
   "a server you open in a browser." Keeps the same Rust backend.
3. **MSI / `.pkg` / Homebrew tap / winget** packaging via `cargo-dist` for
   true double-click installers and `brew install` / `winget install`.
4. **Date-range & advanced filters** in the sidebar and analytics (today / 7d /
   30d / custom), plus filter-by-model and filter-by-tool.
5. **Cost budgets & alerts.** Set a daily/weekly spend or token budget and get a
   visual warning (and optional desktop notification) when a project crosses it.

## Data & retention
6. **Retention / compaction policy.** Configurable pruning (e.g. keep raw events
   90 days, keep aggregates forever) and a `claude-trace-rs db vacuum` command.
7. **Import existing history** command (`db import`) that backfills the database
   from the JSONL files once, with a progress bar.
8. **Diff / replay.** Step through a session like a debugger; diff two sessions
   or two runs of the same prompt.
9. **Authoritative pricing.** Ship a versioned pricing table (and let users
   override it) so cost is accurate as model prices change.

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
19. **Settings panel** in the UI (theme, default tab, feed cap, polling cadence)
    persisted to the database.
20. **End-to-end UI tests** with a headless browser in CI.
