/// Returns the built-in dashboard HTML page as a static string.
pub fn dashboard_html(port: u16) -> String {
    DASHBOARD_HTML.replace("__PORT__", &port.to_string())
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Claude Trace · Session Insights</title>
<style>
  /* ============================================================
     Themes — toggle via data-theme on <html>.
     ============================================================ */
  :root[data-theme="dark"], :root {
    --bg: #0d1117;
    --bg2: #161b22;
    --bg3: #21262d;
    --bg4: #2d333b;
    --border: #30363d;
    --border-strong: #444c56;
    --text: #c9d1d9;
    --text-muted: #8b949e;
    --text-dim: #6e7681;
    --accent: #58a6ff;
    --accent-bg: #1c3461;
    --green: #3fb950;
    --green-bg: #143a14;
    --yellow: #d29922;
    --yellow-bg: #3b2700;
    --red: #f85149;
    --orange: #e3b341;
    --purple: #bc8cff;
    --purple-bg: #2d1f3d;
    --pink: #f778ba;
    --gold: #f0c674;
    --shadow: 0 2px 8px rgba(0,0,0,.4);
  }
  :root[data-theme="light"] {
    --bg: #ffffff;
    --bg2: #f6f8fa;
    --bg3: #eaeef2;
    --bg4: #d0d7de;
    --border: #d0d7de;
    --border-strong: #afb8c1;
    --text: #1f2328;
    --text-muted: #59636e;
    --text-dim: #818b98;
    --accent: #0969da;
    --accent-bg: #ddf4ff;
    --green: #1a7f37;
    --green-bg: #dafbe1;
    --yellow: #9a6700;
    --yellow-bg: #fff8c5;
    --red: #cf222e;
    --orange: #bc4c00;
    --purple: #8250df;
    --purple-bg: #fbefff;
    --pink: #bf3989;
    --gold: #bf8700;
    --shadow: 0 2px 8px rgba(0,0,0,.08);
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { height: 100%; }
  body {
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
    font-size: 13px;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  code, pre, .mono { font-family: 'SF Mono', Consolas, Menlo, monospace; }

  /* ------- Top bar ----------------------------------------------------- */
  header {
    background: var(--bg2);
    border-bottom: 1px solid var(--border);
    padding: 10px 16px;
    display: flex;
    align-items: center;
    gap: 12px;
    flex-shrink: 0;
  }
  header h1 {
    font-size: 15px;
    font-weight: 600;
    color: var(--accent);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  header h1 .logo { font-size: 18px; }
  .pill {
    font-size: 11px;
    padding: 3px 9px;
    border-radius: 12px;
    background: var(--bg3);
    color: var(--text-muted);
    border: 1px solid var(--border);
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .pill.connected { color: var(--green); border-color: var(--green); }
  .pill.disconnected { color: var(--red); border-color: var(--red); }
  .pill.connected .dot { background: var(--green); box-shadow: 0 0 8px var(--green); }
  .pill.disconnected .dot { background: var(--red); }
  .pill .dot {
    width: 7px; height: 7px;
    border-radius: 50%;
    background: var(--text-muted);
    display: inline-block;
  }
  .spacer { flex: 1; }
  .header-stat {
    font-size: 11px;
    color: var(--text-muted);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    line-height: 1.2;
  }
  .header-stat b { color: var(--text); font-size: 13px; font-weight: 600; }
  .controls { display: flex; gap: 6px; align-items: center; }
  .btn {
    background: var(--bg3);
    border: 1px solid var(--border);
    color: var(--text);
    padding: 5px 10px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 12px;
    transition: border-color 80ms ease, background 80ms ease;
  }
  .btn:hover { background: var(--bg4); border-color: var(--accent); }
  .btn.primary { background: var(--accent); color: white; border-color: var(--accent); }
  .btn.primary:hover { filter: brightness(1.1); }
  .btn.active { background: var(--accent-bg); border-color: var(--accent); color: var(--accent); }
  .btn.icon { padding: 4px 7px; }

  /* ------- Layout ------------------------------------------------------ */
  .main { display: flex; flex: 1; overflow: hidden; }
  aside {
    width: 300px;
    flex-shrink: 0;
    background: var(--bg2);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    transition: width 200ms ease;
  }
  aside.collapsed { width: 0; border-right: none; }
  .resize-handle {
    width: 4px;
    flex-shrink: 0;
    background: transparent;
    cursor: col-resize;
    margin: 0 -2px;
    z-index: 10;
  }
  .resize-handle:hover, .resize-handle.dragging { background: var(--accent); }

  aside h2 {
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    padding: 10px 12px 4px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  aside h2 .count { color: var(--text-dim); font-weight: 400; }
  .sidebar-toolbar {
    display: flex;
    gap: 4px;
    padding: 0 12px 6px;
  }
  .sidebar-toolbar .btn { font-size: 11px; padding: 3px 7px; flex-shrink: 0; }
  #session-search {
    margin: 0 12px 6px;
    background: var(--bg3);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    padding: 4px 8px;
    font-size: 12px;
    outline: none;
  }
  #session-search:focus { border-color: var(--accent); }
  #session-list { overflow-y: auto; flex: 1; padding: 2px 0 8px; }
  .bulk-bar {
    display: none;
    background: var(--accent-bg);
    border-top: 1px solid var(--accent);
    padding: 8px 10px;
    flex-direction: column;
    gap: 6px;
  }
  .bulk-bar.visible { display: flex; }
  .bulk-bar .row { display: flex; gap: 4px; align-items: center; }
  .bulk-bar .row b { color: var(--accent); font-size: 12px; }
  .bulk-bar .row .btn { font-size: 11px; padding: 3px 8px; }

  .project-group { border-top: 1px solid var(--border); }
  .project-group:first-child { border-top: none; }
  .project-header {
    padding: 6px 12px 4px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-dim);
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--bg);
  }
  .project-header .pname {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-weight: 500;
  }

  .session-item {
    padding: 7px 12px 8px;
    cursor: pointer;
    border-left: 3px solid transparent;
    position: relative;
    user-select: none;
  }
  .session-item:hover { background: var(--bg3); }
  .session-item.active { border-left-color: var(--accent); background: var(--bg3); }
  .session-item .row1 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 4px;
  }
  .session-item .check {
    width: 13px; height: 13px;
    accent-color: var(--accent);
    flex-shrink: 0;
    display: none;
  }
  .session-item .live {
    width: 6px; height: 6px;
    border-radius: 50%;
    background: var(--green);
    box-shadow: 0 0 6px var(--green);
    flex-shrink: 0;
    display: none;
  }
  .session-item.live .live { display: inline-block; }
  body.select-mode .session-item .check { display: inline-block; }
  .session-item .sid {
    font-family: 'SF Mono', monospace;
    font-size: 11px;
    color: var(--accent);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .session-item .star {
    cursor: pointer;
    color: var(--text-dim);
    flex-shrink: 0;
    font-size: 14px;
    line-height: 1;
  }
  .session-item .star.on { color: var(--gold); }
  .session-item .star:hover { color: var(--gold); }
  .session-item .title {
    font-size: 12px;
    color: var(--text);
    margin-bottom: 3px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .session-item .meta {
    font-size: 10.5px;
    color: var(--text-dim);
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }
  .session-item .meta .cost { color: var(--orange); }
  .session-item .meta .branch { color: var(--purple); }
  .session-item .tags {
    display: flex;
    gap: 3px;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .session-item .tag {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: 8px;
    background: var(--purple-bg);
    color: var(--purple);
    cursor: default;
  }
  .spark { height: 18px; margin-top: 4px; }
  .spark path.area { fill: rgba(88, 166, 255, 0.22); }
  .spark path.line { fill: none; stroke: var(--accent); stroke-width: 1.2; }

  /* ------- Content ----------------------------------------------------- */
  .content { flex: 1; display: flex; flex-direction: column; overflow: hidden; min-width: 0; }
  .tabs {
    display: flex;
    background: var(--bg2);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    padding-left: 8px;
    align-items: center;
  }
  .tab {
    padding: 9px 14px;
    cursor: pointer;
    font-size: 12.5px;
    color: var(--text-muted);
    border-bottom: 2px solid transparent;
    user-select: none;
  }
  .tab:hover { color: var(--text); }
  .tab.active { color: var(--accent); border-bottom-color: var(--accent); }
  .tab-body { flex: 1; overflow: hidden; display: none; }
  .tab-body.active { display: flex; flex-direction: column; }

  /* ------- Feed -------------------------------------------------------- */
  .toolbar {
    display: flex;
    gap: 6px;
    padding: 8px 12px;
    background: var(--bg2);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    flex-wrap: wrap;
    align-items: center;
  }
  .type-btn {
    font-size: 11px;
    padding: 3px 9px;
    border-radius: 12px;
    border: 1px solid var(--border);
    background: var(--bg3);
    color: var(--text-muted);
    cursor: pointer;
  }
  .type-btn:hover { color: var(--text); border-color: var(--border-strong); }
  .type-btn.active { border-color: var(--accent); color: var(--accent); background: var(--accent-bg); }
  #search {
    flex: 1;
    min-width: 200px;
    background: var(--bg3);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text);
    padding: 5px 10px;
    font-size: 12.5px;
    outline: none;
  }
  #search:focus { border-color: var(--accent); }
  .filter-chip {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 8px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-muted);
    cursor: pointer;
    display: none;
  }
  .filter-chip.visible { display: inline-flex; }
  .filter-chip:hover { color: var(--red); border-color: var(--red); }
  .feed-container { display: flex; flex: 1; overflow: hidden; min-height: 0; }
  #feed { flex: 1; overflow-y: auto; padding: 4px 0; }
  .event-row {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 6px 12px;
    cursor: pointer;
    border-left: 3px solid transparent;
    font-size: 13px;
  }
  .event-row:hover { background: var(--bg3); }
  .event-row.selected { background: var(--bg3); border-left-color: var(--accent); }
  .event-row .session-tag {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--text-dim);
    background: var(--bg3);
    border: 1px solid var(--border);
    padding: 1px 5px;
    border-radius: 8px;
    flex-shrink: 0;
    cursor: pointer;
  }
  .event-row .session-tag:hover { color: var(--accent); border-color: var(--accent); }
  .event-row .idx {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--text-dim);
    min-width: 36px;
    text-align: right;
    flex-shrink: 0;
  }
  .event-row .badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 10px;
    flex-shrink: 0;
    font-weight: 500;
  }
  .badge-user { background: #1c3461; color: #79c0ff; }
  .badge-assistant { background: var(--green-bg); color: var(--green); }
  .badge-tool_use { background: #3b2700; color: var(--orange); }
  .badge-tool_result { background: #2d1f00; color: var(--yellow); }
  .badge-system { background: var(--purple-bg); color: var(--purple); }
  .badge-summary { background: #1c4361; color: #79c0ff; }
  .badge-attachment, .badge-ai-title, .badge-queue-operation, .badge-last-prompt {
    background: var(--bg3);
    color: var(--text-muted);
  }
  .badge-unknown { background: var(--bg3); color: var(--text-muted); }
  .event-row .summary {
    color: var(--text);
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .event-row .tokens { font-family: 'SF Mono', monospace; font-size: 10px; color: var(--text-dim); flex-shrink: 0; }
  .event-row .cost { font-family: 'SF Mono', monospace; font-size: 10px; color: var(--orange); flex-shrink: 0; }
  .event-row .ts { font-family: 'SF Mono', monospace; font-size: 10px; color: var(--text-dim); flex-shrink: 0; }

  /* ------- Detail pane (resizable) ------------------------------------ */
  #detail {
    width: 380px;
    flex-shrink: 0;
    border-left: 1px solid var(--border);
    background: var(--bg2);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  #detail.collapsed { width: 0; border-left: none; }
  #detail-header {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 11.5px;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  #detail-header .copy-btn {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 8px;
    background: var(--bg3);
    border: 1px solid var(--border);
    color: var(--text-muted);
    cursor: pointer;
    text-transform: none;
    letter-spacing: 0;
  }
  #detail-header .copy-btn:hover { color: var(--accent); border-color: var(--accent); }
  #detail-meta {
    padding: 10px 12px;
    font-size: 12px;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  #detail-meta .row { display: flex; gap: 6px; margin-bottom: 3px; align-items: baseline; }
  #detail-meta .k { color: var(--text-dim); min-width: 56px; }
  #detail-meta .v { color: var(--text); word-break: break-all; }
  #detail-meta .v.accent { color: var(--accent); }
  #detail-meta .v.purple { color: var(--purple); }
  #detail-meta .v.orange { color: var(--orange); }
  #detail-json {
    flex: 1;
    overflow: auto;
    padding: 12px;
    font-family: 'SF Mono', monospace;
    font-size: 11.5px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--text);
  }

  /* ------- Conversation ----------------------------------------------- */
  #conversation-empty, #metrics-empty {
    padding: 60px 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 14px;
  }
  #conversation-empty .hint { font-size: 12px; margin-top: 8px; color: var(--text-dim); }
  #conversation {
    flex: 1;
    overflow-y: auto;
    padding: 16px 24px;
  }
  .conv-toolbar {
    padding: 8px 24px;
    background: var(--bg2);
    border-bottom: 1px solid var(--border);
    display: flex;
    gap: 6px;
    align-items: center;
    flex-shrink: 0;
  }
  .conv-toolbar .title {
    font-size: 12px;
    color: var(--text-muted);
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .conv-msg {
    margin-bottom: 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    background: var(--bg2);
  }
  .conv-msg .ch {
    padding: 6px 12px;
    font-size: 11px;
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  .conv-msg .ch .role {
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 10.5px;
  }
  .conv-msg.user .ch .role { color: #79c0ff; }
  .conv-msg.assistant .ch .role { color: var(--green); }
  .conv-msg.system .ch .role { color: var(--purple); }
  .conv-msg .ch .meta { color: var(--text-dim); }
  .conv-msg .ch .latency {
    background: var(--bg3);
    color: var(--accent);
    padding: 1px 6px;
    border-radius: 8px;
    font-family: 'SF Mono', monospace;
    font-size: 10px;
  }
  .conv-msg .body { padding: 12px; font-size: 13px; line-height: 1.55; }
  .conv-msg .body .text { white-space: pre-wrap; word-break: break-word; }
  .conv-msg .body .tool {
    margin-top: 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .conv-msg .body .tool .head {
    padding: 6px 10px;
    font-size: 11px;
    color: var(--orange);
    border-bottom: 1px solid var(--border);
    background: rgba(227, 179, 65, 0.08);
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .conv-msg .body .tool .head .name { font-weight: 600; }
  .conv-msg .body .tool .head .id {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--text-dim);
  }
  .conv-msg .body .tool pre {
    margin: 0;
    padding: 8px 10px;
    font-family: 'SF Mono', monospace;
    font-size: 11px;
    color: var(--text-muted);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 240px;
    overflow: auto;
  }
  .conv-msg .body .thinking {
    margin-top: 8px;
    padding: 8px 10px;
    border-left: 3px solid var(--purple);
    background: rgba(188, 140, 255, 0.06);
    font-size: 12px;
    color: var(--text-muted);
    white-space: pre-wrap;
    font-style: italic;
  }
  .conv-msg.tool-result {
    border-color: rgba(227, 179, 65, 0.4);
  }
  .conv-msg.tool-result .ch .role { color: var(--yellow); }

  /* ------- Metrics ---------------------------------------------------- */
  #metrics { flex: 1; overflow-y: auto; padding: 16px; }
  .metric-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 12px;
    margin-bottom: 16px;
  }
  .metric-card {
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px 16px;
  }
  .metric-card .label {
    font-size: 11px;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 6px;
  }
  .metric-card .value {
    font-size: 22px;
    font-weight: 600;
    color: var(--text);
    font-family: 'SF Mono', monospace;
  }
  .metric-card .value.green { color: var(--green); }
  .metric-card .value.orange { color: var(--orange); }
  .metric-card .value.purple { color: var(--purple); }
  .metric-card .value.accent { color: var(--accent); }
  .metric-card .sub { font-size: 11px; color: var(--text-dim); margin-top: 4px; }

  .chart-row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 16px; }
  @media (max-width: 1200px) { .chart-row { grid-template-columns: 1fr; } }
  .chart-card {
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 14px 16px;
  }
  .chart-card h3 {
    font-size: 12px;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 12px;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .bar-list { display: flex; flex-direction: column; gap: 6px; }
  .bar {
    display: grid;
    grid-template-columns: 110px 1fr 60px;
    gap: 10px;
    align-items: center;
    font-size: 12px;
  }
  .bar .name { color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bar .track {
    background: var(--bg3);
    height: 14px;
    border-radius: 7px;
    overflow: hidden;
    position: relative;
  }
  .bar .fill {
    height: 100%;
    background: linear-gradient(90deg, var(--accent) 0%, var(--purple) 100%);
    border-radius: 7px;
  }
  .bar .val {
    font-family: 'SF Mono', monospace;
    font-size: 11px;
    color: var(--text-muted);
    text-align: right;
  }

  .timeline-chart { width: 100%; height: 200px; }
  .timeline-chart .axis line, .timeline-chart .axis path { stroke: var(--border); fill: none; }
  .timeline-chart .axis text { fill: var(--text-dim); font-size: 10px; }
  .timeline-chart .grid line { stroke: var(--bg3); }
  .timeline-chart path.area { fill: rgba(88, 166, 255, 0.18); }
  .timeline-chart path.line { fill: none; stroke: var(--accent); stroke-width: 1.5; }

  /* ------- Empty / loading ------------------------------------------- */
  #no-events {
    padding: 60px 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 14px;
  }
  #no-events .hint { font-size: 12px; margin-top: 8px; color: var(--text-dim); }

  /* ------- Stats bar -------------------------------------------------- */
  .statsbar {
    background: var(--bg2);
    border-top: 1px solid var(--border);
    padding: 6px 16px;
    display: flex;
    gap: 20px;
    font-size: 11px;
    color: var(--text-muted);
    flex-shrink: 0;
    overflow-x: auto;
  }
  .statsbar span { white-space: nowrap; }
  .statsbar span b { color: var(--text); font-weight: 600; }
  .statsbar .green b { color: var(--green); }
  .statsbar .orange b { color: var(--orange); }
  .statsbar .purple b { color: var(--purple); }

  /* ------- Modals + command palette ---------------------------------- */
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,.5);
    display: none;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal-backdrop.visible { display: flex; }
  .modal {
    background: var(--bg2);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: var(--shadow);
    max-width: 560px;
    width: calc(100% - 40px);
    max-height: calc(100% - 80px);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .modal.large { max-width: 720px; }
  .modal header {
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg3);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .modal header h3 {
    font-size: 14px;
    font-weight: 600;
    color: var(--text);
    flex: 1;
  }
  .modal .body {
    padding: 14px 16px;
    overflow: auto;
    flex: 1;
  }
  .modal .footer {
    padding: 10px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg);
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    align-items: center;
  }
  .form-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 12px;
  }
  .form-field label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .form-field select, .form-field input[type="text"], .form-field textarea {
    background: var(--bg);
    border: 1px solid var(--border);
    color: var(--text);
    padding: 6px 10px;
    font-size: 13px;
    border-radius: 6px;
    outline: none;
    font-family: inherit;
  }
  .form-field textarea { font-family: 'SF Mono', monospace; min-height: 80px; resize: vertical; }
  .form-field select:focus, .form-field input:focus, .form-field textarea:focus { border-color: var(--accent); }
  .format-options {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .format-options label {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .format-options label:hover { border-color: var(--accent); }
  .format-options input { position: absolute; opacity: 0; pointer-events: none; }
  .format-options label.selected { border-color: var(--accent); background: var(--accent-bg); }
  .format-options .name { font-weight: 600; font-size: 12px; color: var(--text); }
  .format-options .desc { font-size: 11px; color: var(--text-dim); }

  /* Command palette */
  .palette-input {
    width: 100%;
    background: var(--bg);
    border: none;
    color: var(--text);
    padding: 16px;
    font-size: 14px;
    outline: none;
    border-bottom: 1px solid var(--border);
  }
  .palette-list { max-height: 380px; overflow-y: auto; }
  .palette-item {
    padding: 10px 16px;
    cursor: pointer;
    display: flex;
    gap: 10px;
    align-items: center;
    border-bottom: 1px solid var(--border);
  }
  .palette-item.active { background: var(--accent-bg); }
  .palette-item .icon { color: var(--accent); }
  .palette-item .title { flex: 1; color: var(--text); }
  .palette-item .sub { font-size: 11px; color: var(--text-dim); }

  /* Keyboard help */
  .kbd-grid { display: grid; grid-template-columns: 130px 1fr; gap: 6px 16px; }
  .kbd { font-family: 'SF Mono', monospace; font-size: 11px; background: var(--bg3); border: 1px solid var(--border); border-radius: 4px; padding: 1px 6px; }
  .kbd-grid .desc { font-size: 12px; color: var(--text-muted); }

  /* Toast */
  #toast {
    position: fixed;
    bottom: 60px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--bg3);
    color: var(--text);
    border: 1px solid var(--accent);
    padding: 8px 14px;
    border-radius: 8px;
    box-shadow: var(--shadow);
    font-size: 12.5px;
    z-index: 200;
    opacity: 0;
    pointer-events: none;
    transition: opacity 200ms ease, transform 200ms ease;
  }
  #toast.visible { opacity: 1; transform: translateX(-50%) translateY(0); }

  /* Scrollbars */
  ::-webkit-scrollbar { width: 9px; height: 9px; }
  ::-webkit-scrollbar-track { background: transparent; }
  ::-webkit-scrollbar-thumb { background: var(--bg4); border-radius: 4px; }
  ::-webkit-scrollbar-thumb:hover { background: var(--border-strong); }

  /* Inline tag editor */
  .tag-editor {
    display: flex;
    gap: 4px;
    align-items: center;
    flex-wrap: wrap;
    margin-top: 4px;
  }
  .tag-editor input {
    background: transparent;
    border: 1px dashed var(--border);
    color: var(--text-muted);
    padding: 1px 5px;
    border-radius: 8px;
    font-size: 10px;
    outline: none;
    width: 70px;
  }
  .tag-editor input:focus { border-color: var(--accent); border-style: solid; }
</style>
</head>
<body>
<header>
  <button class="btn icon" id="toggle-sidebar" title="Toggle sidebar (Ctrl/⌘+B)">☰</button>
  <h1><span class="logo">🔍</span> Claude Trace</h1>
  <span id="status" class="pill disconnected"><span class="dot"></span> <span id="status-text">Connecting…</span></span>
  <span class="pill" id="watch-root-pill" title="Watch root">—</span>
  <span class="spacer"></span>
  <span class="header-stat"><span><b id="hdr-sessions">0</b> sessions</span></span>
  <span class="header-stat"><span><b id="hdr-events">0</b> events</span></span>
  <span class="header-stat"><span><b id="hdr-cost">$0.0000</b> est.</span></span>
  <div class="controls">
    <button class="btn icon" id="palette-btn" title="Command palette (Ctrl/⌘+K)">⌘K</button>
    <button class="btn icon" id="theme-btn" title="Toggle theme">🌓</button>
    <button class="btn icon" id="help-btn" title="Help (?)">?</button>
    <button class="btn" id="export-all-btn" title="Export all sessions (E)">⤓ Export</button>
    <button class="btn" id="pause-btn" title="Pause live updates (Space)">⏸ Pause</button>
    <label style="display:flex;align-items:center;gap:4px;font-size:12px;color:var(--text-muted);cursor:pointer">
      <input type="checkbox" id="scroll-lock" checked> Auto-scroll
    </label>
  </div>
</header>
<div class="main">
  <aside id="sidebar">
    <h2>Sessions <span class="count" id="session-count">0</span></h2>
    <div class="sidebar-toolbar">
      <button class="btn" id="select-mode-btn" title="Multi-select (Shift+S)">☑ Select</button>
      <button class="btn" id="show-bookmarked-btn" title="Show bookmarked only">★ Pinned</button>
      <button class="btn" id="clear-bookmarks-btn" title="Clear bookmarks" style="display:none">✕</button>
    </div>
    <input id="session-search" placeholder="Filter by id / project / tag…">
    <div id="session-list"><div style="padding:16px 12px;font-size:12px;color:var(--text-muted)">No sessions yet. Run Claude Code to populate.</div></div>
    <div class="bulk-bar" id="bulk-bar">
      <div class="row"><b id="bulk-count">0 selected</b></div>
      <div class="row">
        <button class="btn" id="bulk-select-all">All</button>
        <button class="btn" id="bulk-select-none">None</button>
        <button class="btn primary" id="bulk-export-btn">Export…</button>
      </div>
    </div>
  </aside>
  <div class="resize-handle" id="sidebar-resize" data-target="sidebar"></div>
  <div class="content">
    <div class="tabs">
      <div class="tab active" data-tab="feed">Live Feed</div>
      <div class="tab" data-tab="conversation">Conversation</div>
      <div class="tab" data-tab="metrics">Metrics</div>
    </div>

    <!-- Feed -->
    <div class="tab-body active" data-tab-body="feed">
      <div class="toolbar">
        <button class="type-btn active" data-type="all">All</button>
        <button class="type-btn" data-type="user">👤 User</button>
        <button class="type-btn" data-type="assistant">🤖 Assistant</button>
        <button class="type-btn" data-type="tool_use">🔧 Tool Use</button>
        <button class="type-btn" data-type="tool_result">📦 Tool Result</button>
        <button class="type-btn" data-type="system">⚙️ System</button>
        <input id="search" type="text" placeholder="Search events… (press /)">
        <button class="filter-chip" id="clear-session-filter">✕ session</button>
        <button class="btn" id="save-view-btn" title="Save current filters as a view">💾</button>
        <select class="btn" id="saved-views" style="font-size:11px"><option value="">— Saved views —</option></select>
      </div>
      <div class="feed-container">
        <div id="feed"><div id="no-events">Waiting for events…<div class="hint">Run Claude Code (or use --backfill) and they'll appear here.</div></div></div>
        <div class="resize-handle" id="detail-resize" data-target="detail"></div>
        <div id="detail">
          <div id="detail-header">
            <span>Event Detail</span>
            <button class="copy-btn" id="copy-json">Copy JSON</button>
          </div>
          <div id="detail-meta">Select an event to inspect.</div>
          <pre id="detail-json"></pre>
        </div>
      </div>
    </div>

    <!-- Conversation -->
    <div class="tab-body" data-tab-body="conversation">
      <div class="conv-toolbar" id="conv-toolbar" style="display:none">
        <span class="title" id="conv-title"></span>
        <button class="btn" id="conv-export-btn">⤓ Export this session</button>
        <button class="btn" id="conv-bookmark-btn">★</button>
      </div>
      <div id="conversation"><div id="conversation-empty">Select a session to view the conversation.<div class="hint">Conversation view threads user / assistant / tool messages chronologically with latency badges.</div></div></div>
    </div>

    <!-- Metrics -->
    <div class="tab-body" data-tab-body="metrics">
      <div id="metrics"></div>
    </div>
  </div>
</div>
<div class="statsbar">
  <span>Events: <b id="stat-events">0</b></span>
  <span>User: <b id="stat-user">0</b></span>
  <span class="green">Assistant: <b id="stat-asst">0</b></span>
  <span class="orange">Tool calls: <b id="stat-tools">0</b></span>
  <span>Tokens in: <b id="stat-tok-in">0</b></span>
  <span>Tokens out: <b id="stat-tok-out">0</b></span>
  <span class="purple">Cache read: <b id="stat-cache-r">0</b></span>
  <span class="purple">Cache write: <b id="stat-cache-w">0</b></span>
  <span class="orange">Est. cost: <b id="stat-cost">$0.0000</b></span>
</div>

<!-- Export modal -->
<div class="modal-backdrop" id="export-modal">
  <div class="modal">
    <header>
      <h3>Export <span id="export-scope" style="color:var(--accent)">all sessions</span></h3>
      <button class="btn icon" data-close="export-modal">✕</button>
    </header>
    <div class="body">
      <div class="form-field">
        <label>Format</label>
        <div class="format-options" id="export-format-options">
          <label class="selected"><input type="radio" name="export-format" value="messages" checked><span class="name">Anthropic Messages</span><span class="desc">JSONL · drop-in for Claude fine-tuning</span></label>
          <label><input type="radio" name="export-format" value="openai"><span class="name">OpenAI Chat / Tools</span><span class="desc">JSONL · tool_calls translated</span></label>
          <label><input type="radio" name="export-format" value="sharegpt"><span class="name">ShareGPT</span><span class="desc">JSONL · HF / Axolotl / Unsloth</span></label>
          <label><input type="radio" name="export-format" value="huggingface"><span class="name">HuggingFace dataset</span><span class="desc">JSONL · datasets.load_dataset shape</span></label>
          <label><input type="radio" name="export-format" value="jsonl"><span class="name">Raw JSONL</span><span class="desc">Full Claude Code passthrough</span></label>
          <label><input type="radio" name="export-format" value="markdown"><span class="name">Markdown</span><span class="desc">Human-readable transcript</span></label>
        </div>
      </div>
      <div id="export-preview" style="font-family:'SF Mono',monospace;font-size:11px;color:var(--text-dim);max-height:160px;overflow:auto;background:var(--bg);border:1px solid var(--border);border-radius:6px;padding:10px;white-space:pre-wrap"></div>
    </div>
    <div class="footer">
      <button class="btn" data-close="export-modal">Cancel</button>
      <button class="btn primary" id="export-download-btn">⤓ Download</button>
    </div>
  </div>
</div>

<!-- Help modal -->
<div class="modal-backdrop" id="help-modal">
  <div class="modal">
    <header><h3>Keyboard shortcuts &amp; tips</h3><button class="btn icon" data-close="help-modal">✕</button></header>
    <div class="body">
      <div class="kbd-grid">
        <div><span class="kbd">/</span></div><div class="desc">Focus event search</div>
        <div><span class="kbd">Esc</span></div><div class="desc">Clear filters / close modal</div>
        <div><span class="kbd">j</span> <span class="kbd">k</span></div><div class="desc">Next / previous event</div>
        <div><span class="kbd">f</span> <span class="kbd">c</span> <span class="kbd">m</span></div><div class="desc">Feed / Conversation / Metrics tab</div>
        <div><span class="kbd">e</span></div><div class="desc">Open export dialog</div>
        <div><span class="kbd">b</span></div><div class="desc">Toggle bookmark on current session</div>
        <div><span class="kbd">Shift</span>+<span class="kbd">S</span></div><div class="desc">Toggle multi-select mode</div>
        <div><span class="kbd">Ctrl</span>/<span class="kbd">⌘</span>+<span class="kbd">K</span></div><div class="desc">Open command palette</div>
        <div><span class="kbd">Ctrl</span>/<span class="kbd">⌘</span>+<span class="kbd">B</span></div><div class="desc">Toggle sidebar</div>
        <div><span class="kbd">Space</span></div><div class="desc">Pause / resume live updates</div>
        <div><span class="kbd">?</span></div><div class="desc">Show this help</div>
      </div>
      <div style="margin-top:14px;padding-top:12px;border-top:1px solid var(--border)">
        <div style="font-size:11px;text-transform:uppercase;letter-spacing:0.05em;color:var(--text-muted);margin-bottom:8px">Mouse tips</div>
        <div class="kbd-grid">
          <div class="desc"><b>Sidebar star</b></div><div class="desc">Click to bookmark / unbookmark a session</div>
          <div class="desc"><b>Tag input</b></div><div class="desc">Type a tag, comma-separated for multiple, press <span class="kbd">Enter</span></div>
          <div class="desc"><b>Tag</b></div><div class="desc">Double-click an existing tag to remove it</div>
          <div class="desc"><b>Session chip in feed</b></div><div class="desc">Click to filter the feed to that session (click again to clear)</div>
          <div class="desc"><b>Pane edges</b></div><div class="desc">Drag the thin vertical bars to resize the sidebar / detail pane</div>
        </div>
      </div>
    </div>
  </div>
</div>

<!-- Command palette -->
<div class="modal-backdrop" id="palette-modal">
  <div class="modal large" style="display:flex;flex-direction:column">
    <input class="palette-input" id="palette-input" placeholder="Search sessions, actions, projects…">
    <div class="palette-list" id="palette-list"></div>
  </div>
</div>

<div id="toast"></div>

<script>
(function() {
  const WS_URL = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`;
  const STORAGE = {
    BOOKMARKS: 'claudeTrace.bookmarks',
    TAGS: 'claudeTrace.tags',
    THEME: 'claudeTrace.theme',
    VIEWS: 'claudeTrace.views',
    SIDEBAR_W: 'claudeTrace.sidebarWidth',
    DETAIL_W: 'claudeTrace.detailWidth',
  };
  const EVENT_CAP = 50000;

  // ---- State ------------------------------------------------------------
  let ws, reconnectTimer;
  const sessions = new Map();
  let allEvents = [];
  const sessionEvents = new Map();
  const seenKeys = new Set();

  let activeSession = null;
  let activeType = 'all';
  let searchQuery = '';
  let sessionFilterQuery = '';
  let selectedIdx = null;
  let activeTab = 'feed';
  let paused = false;
  let pendingBuffer = [];
  let watchRoot = '';
  let bookmarks = new Set(load(STORAGE.BOOKMARKS) || []);
  let tags = load(STORAGE.TAGS) || {};
  let savedViews = load(STORAGE.VIEWS) || [];
  let showBookmarkedOnly = false;
  let selectMode = false;
  let selectedSet = new Set();

  // Theme
  const savedTheme = load(STORAGE.THEME) || 'dark';
  document.documentElement.setAttribute('data-theme', savedTheme);

  const $ = (s, root = document) => root.querySelector(s);
  const $$ = (s, root = document) => Array.from(root.querySelectorAll(s));

  function load(key) {
    try { return JSON.parse(localStorage.getItem(key)); } catch (e) { return null; }
  }
  function save(key, val) {
    try { localStorage.setItem(key, JSON.stringify(val)); } catch (e) {}
  }

  function fmtCost(c) { return '$' + (c || 0).toFixed(4); }
  function fmtNum(n) { return (n || 0).toLocaleString(); }
  function escHtml(s) {
    return String(s ?? '')
      .replace(/&/g, '&amp;').replace(/</g, '&lt;')
      .replace(/>/g, '&gt;').replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }
  function shortId(id) {
    if (!id) return '';
    if (id.length <= 14) return id;
    return id.slice(0, 8) + '…' + id.slice(-4);
  }
  function projectName(cwd) {
    if (!cwd) return '(unknown project)';
    const parts = cwd.split(/[/\\]/).filter(Boolean);
    return parts[parts.length - 1] || cwd;
  }
  function howLongAgo(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    const diff = (Date.now() - d.getTime()) / 1000;
    if (diff < 5) return 'now';
    if (diff < 60) return Math.floor(diff) + 's ago';
    if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
    if (diff < 86400) return Math.floor(diff / 3600) + 'h ago';
    return Math.floor(diff / 86400) + 'd ago';
  }
  function isLive(iso) {
    if (!iso) return false;
    return (Date.now() - new Date(iso).getTime()) < 10_000;
  }
  function copy(text) {
    navigator.clipboard?.writeText(text).catch(() => {});
  }
  function toast(msg, ms = 2200) {
    const t = $('#toast');
    t.textContent = msg;
    t.classList.add('visible');
    clearTimeout(toast._timer);
    toast._timer = setTimeout(() => t.classList.remove('visible'), ms);
  }

  // ---- WebSocket lifecycle ---------------------------------------------
  function connect() {
    ws = new WebSocket(WS_URL);
    ws.onopen = () => {
      $('#status').className = 'pill connected';
      $('#status-text').textContent = 'Connected';
      clearTimeout(reconnectTimer);
    };
    ws.onclose = () => {
      $('#status').className = 'pill disconnected';
      $('#status-text').textContent = 'Disconnected · retrying…';
      reconnectTimer = setTimeout(connect, 1500);
    };
    ws.onerror = () => { try { ws.close(); } catch (e) {} };
    ws.onmessage = (e) => {
      let msg;
      try { msg = JSON.parse(e.data); } catch (err) { return; }
      if (msg.type === 'connected') {
        watchRoot = msg.watch_root || '';
        $('#watch-root-pill').textContent = '📁 ' + (watchRoot || '~/.claude/projects');
        $('#watch-root-pill').title = watchRoot;
        return;
      }
      if (msg.type === 'snapshot') {
        handleSnapshot(msg);
        return;
      }
      handleEvent(msg);
    };
  }

  function eventKey(ev) { return ev.session_id + ':' + ev.line_index; }
  function pushSessionEvent(ev) {
    let arr = sessionEvents.get(ev.session_id);
    if (!arr) { arr = []; sessionEvents.set(ev.session_id, arr); }
    arr.push(ev);
  }

  function handleSnapshot(snap) {
    sessions.clear();
    allEvents = [];
    sessionEvents.clear();
    seenKeys.clear();
    selectedIdx = null;
    (snap.sessions || []).forEach(s => sessions.set(s.id, s));
    (snap.events || []).forEach(e => {
      const k = eventKey(e);
      if (seenKeys.has(k)) return;
      seenKeys.add(k);
      e._idx = allEvents.length;
      allEvents.push(e);
      pushSessionEvent(e);
    });
    rerenderAll();
  }

  function handleEvent(ev) {
    const key = eventKey(ev);
    if (seenKeys.has(key)) return;
    seenKeys.add(key);
    if (paused) {
      pendingBuffer.push(ev);
      $('#pause-btn').textContent = `▶ Resume (${pendingBuffer.length})`;
      return;
    }
    ingestEvent(ev);
    incrementalRender(ev);
  }

  function ingestEvent(ev) {
    ev._idx = allEvents.length;
    allEvents.push(ev);
    pushSessionEvent(ev);
    if (allEvents.length > EVENT_CAP) {
      const drop = Math.floor(EVENT_CAP / 10);
      const dropped = allEvents.splice(0, drop);
      for (let i = 0; i < allEvents.length; i++) allEvents[i]._idx = i;
      dropped.forEach(d => seenKeys.delete(eventKey(d)));
      sessionEvents.clear();
      for (const e of allEvents) pushSessionEvent(e);
      if (selectedIdx !== null) {
        selectedIdx -= drop;
        if (selectedIdx < 0) selectedIdx = null;
      }
      if (activeTab === 'feed') renderFeed();
    }

    let s = sessions.get(ev.session_id);
    if (!s) {
      s = { id: ev.session_id, cwd: ev.cwd, git_branch: ev.git_branch, version: ev.version, model: ev.model,
        first_seen: ev.observed_at, last_seen: ev.observed_at, last_entry_timestamp: ev.timestamp,
        event_count: 0, user_count: 0, assistant_count: 0, tool_use_count: 0, tool_result_count: 0, system_count: 0,
        input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 0,
        cost_usd: 0, tool_counts: {}, title: null };
      sessions.set(ev.session_id, s);
    }
    s.last_seen = ev.observed_at;
    if (ev.timestamp) s.last_entry_timestamp = ev.timestamp;
    if (!s.cwd && ev.cwd) s.cwd = ev.cwd;
    if (ev.git_branch) s.git_branch = ev.git_branch;
    if (ev.version) s.version = ev.version;
    if (ev.model) s.model = ev.model;
    s.event_count++;
    if (ev.event_type === 'user') s.user_count++;
    else if (ev.event_type === 'assistant') s.assistant_count++;
    else if (ev.event_type === 'tool_use') s.tool_use_count++;
    else if (ev.event_type === 'tool_result') s.tool_result_count++;
    else if (ev.event_type === 'system') s.system_count++;
    (ev.tool_uses || []).forEach(name => {
      s.tool_use_count++;
      s.tool_counts[name] = (s.tool_counts[name] || 0) + 1;
    });
    s.tool_result_count += (ev.tool_results || []).length;
    if (ev.usage) {
      s.input_tokens += ev.usage.input || 0;
      s.output_tokens += ev.usage.output || 0;
      s.cache_read_tokens += ev.usage.cache_read || 0;
      s.cache_creation_tokens += ev.usage.cache_creation || 0;
    }
    s.cost_usd += ev.cost_usd || 0;
    if (ev.event_type === 'ai-title') {
      const t = ev.entry && ev.entry.aiTitle;
      if (t) s.title = t;
    }
  }

  function incrementalRender(ev) {
    updateHeaderStats();
    updateFooterStats();
    renderSessions();
    if (activeTab === 'feed' && matchesFilter(ev)) appendEventToFeed(ev);
    else if (activeTab === 'conversation' && activeSession === ev.session_id) renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  }

  function rerenderAll() {
    updateHeaderStats();
    updateFooterStats();
    renderSessions();
    if (activeTab === 'feed') renderFeed();
    else if (activeTab === 'conversation') renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  }

  // ---- Aggregates ------------------------------------------------------
  function aggregate() {
    const t = { events: 0, user: 0, asst: 0, tools: 0, tokIn: 0, tokOut: 0, cacheR: 0, cacheW: 0, cost: 0 };
    for (const s of sessions.values()) {
      t.events += s.event_count; t.user += s.user_count; t.asst += s.assistant_count; t.tools += s.tool_use_count;
      t.tokIn += s.input_tokens; t.tokOut += s.output_tokens;
      t.cacheR += s.cache_read_tokens; t.cacheW += s.cache_creation_tokens;
      t.cost += s.cost_usd;
    }
    return t;
  }
  function updateHeaderStats() {
    $('#hdr-sessions').textContent = sessions.size.toString();
    const t = aggregate();
    $('#hdr-events').textContent = fmtNum(t.events);
    $('#hdr-cost').textContent = fmtCost(t.cost);
  }
  function updateFooterStats() {
    const t = aggregate();
    $('#stat-events').textContent = fmtNum(t.events);
    $('#stat-user').textContent = fmtNum(t.user);
    $('#stat-asst').textContent = fmtNum(t.asst);
    $('#stat-tools').textContent = fmtNum(t.tools);
    $('#stat-tok-in').textContent = fmtNum(t.tokIn);
    $('#stat-tok-out').textContent = fmtNum(t.tokOut);
    $('#stat-cache-r').textContent = fmtNum(t.cacheR);
    $('#stat-cache-w').textContent = fmtNum(t.cacheW);
    $('#stat-cost').textContent = fmtCost(t.cost);
  }

  // ---- Bookmarks + tags ------------------------------------------------
  function toggleBookmark(sid) {
    if (bookmarks.has(sid)) bookmarks.delete(sid); else bookmarks.add(sid);
    save(STORAGE.BOOKMARKS, Array.from(bookmarks));
    renderSessions();
    if (activeTab === 'conversation') renderConversation();
  }
  function setTagInput(sid, val) {
    const arr = (val || '').split(',').map(s => s.trim()).filter(Boolean);
    if (arr.length) tags[sid] = arr; else delete tags[sid];
    save(STORAGE.TAGS, tags);
    renderSessions();
  }

  // ---- Sessions sidebar ------------------------------------------------
  function sparkPath(values, w, h) {
    if (!values.length) return { line: '', area: '' };
    const max = Math.max(...values, 1);
    const dx = w / Math.max(values.length - 1, 1);
    let line = '';
    values.forEach((v, i) => {
      const x = i * dx;
      const y = h - (v / max) * (h - 2) - 1;
      line += (i === 0 ? 'M' : 'L') + x.toFixed(2) + ',' + y.toFixed(2) + ' ';
    });
    const area = line + `L${w},${h} L0,${h} Z`;
    return { line: line.trim(), area: area.trim() };
  }

  function eventBucketsFor(sessionId, buckets = 24) {
    const bucketArr = new Array(buckets).fill(0);
    const sessEvents = sessionEvents.get(sessionId);
    if (!sessEvents || !sessEvents.length) return bucketArr;
    const first = new Date(sessEvents[0].observed_at).getTime();
    const last = new Date(sessEvents[sessEvents.length - 1].observed_at).getTime();
    const span = Math.max(last - first, 1);
    sessEvents.forEach(e => {
      const t = new Date(e.observed_at).getTime();
      const b = Math.min(buckets - 1, Math.floor(((t - first) / span) * buckets));
      bucketArr[b]++;
    });
    return bucketArr;
  }

  function renderSessions() {
    const list = $('#session-list');
    const q = sessionFilterQuery.toLowerCase();
    let arr = Array.from(sessions.values()).filter(s => {
      if (showBookmarkedOnly && !bookmarks.has(s.id)) return false;
      if (!q) return true;
      const hay = (s.id + ' ' + (s.cwd||'') + ' ' + (s.title||'') + ' ' + (s.git_branch||'') + ' ' + (tags[s.id]||[]).join(' ')).toLowerCase();
      return hay.includes(q);
    });
    // Bookmarked first, then by last activity.
    arr.sort((a, b) => {
      const ab = bookmarks.has(a.id) ? 1 : 0;
      const bb = bookmarks.has(b.id) ? 1 : 0;
      if (ab !== bb) return bb - ab;
      return (b.last_seen || '').localeCompare(a.last_seen || '');
    });
    $('#session-count').textContent = arr.length;

    if (!arr.length) {
      list.innerHTML = '<div style="padding:16px 12px;font-size:12px;color:var(--text-muted)">No sessions ' + (q || showBookmarkedOnly ? 'match.' : 'yet.') + '</div>';
      return;
    }

    const groups = new Map();
    for (const s of arr) {
      const key = s.cwd || '__unknown__';
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(s);
    }

    let html = '';
    for (const [cwd, gList] of groups) {
      const pname = cwd === '__unknown__' ? '(unknown project)' : projectName(cwd);
      html += `<div class="project-group">
        <div class="project-header" title="${escHtml(cwd === '__unknown__' ? '' : cwd)}">
          <span>📁</span><span class="pname">${escHtml(pname)}</span><span class="count">${gList.length}</span>
        </div>`;
      for (const s of gList) {
        const active = s.id === activeSession ? ' active' : '';
        const live = isLive(s.last_seen) ? ' live' : '';
        const isBkmk = bookmarks.has(s.id);
        const isSel = selectedSet.has(s.id);
        const buckets = eventBucketsFor(s.id, 24);
        const sp = sparkPath(buckets, 240, 18);
        const branchPart = s.git_branch ? `<span class="branch">⎇ ${escHtml(s.git_branch)}</span>` : '';
        const sessTags = (tags[s.id] || []).map(t => `<span class="tag">${escHtml(t)}</span>`).join('');
        html += `<div class="session-item${active}${live}" data-id="${escHtml(s.id)}">
          <div class="row1">
            <input type="checkbox" class="check" ${isSel ? 'checked' : ''} data-id="${escHtml(s.id)}">
            <span class="live"></span>
            <span class="sid" title="${escHtml(s.id)}">${escHtml(shortId(s.id))}</span>
            <span class="star ${isBkmk ? 'on' : ''}" data-bkmk="${escHtml(s.id)}" title="Bookmark">${isBkmk ? '★' : '☆'}</span>
            <span style="font-size:10px;color:var(--text-dim)">${escHtml(howLongAgo(s.last_seen))}</span>
          </div>
          ${s.title ? `<div class="title" title="${escHtml(s.title)}">${escHtml(s.title)}</div>` : ''}
          <div class="meta">
            <span>${s.event_count} ev</span>
            <span class="cost">${fmtCost(s.cost_usd)}</span>
            ${branchPart}
          </div>
          <div class="tags" data-tag-row="${escHtml(s.id)}">${sessTags}
            <input data-tag-input="${escHtml(s.id)}" placeholder="+ tag">
          </div>
          <svg class="spark" viewBox="0 0 240 18" preserveAspectRatio="none">
            <path class="area" d="${sp.area}"/>
            <path class="line" d="${sp.line}"/>
          </svg>
        </div>`;
      }
      html += '</div>';
    }
    list.innerHTML = html;

    list.querySelectorAll('.session-item').forEach(el => {
      el.addEventListener('click', (e) => {
        if (e.target.matches('.check, .star, input, .tag')) return;
        const id = el.dataset.id;
        if (selectMode) {
          toggleSelected(id);
          renderSessions();
          return;
        }
        activeSession = (activeSession === id) ? null : id;
        updateSessionFilterChip();
        renderSessions();
        if (activeTab === 'feed') renderFeed();
        else if (activeTab === 'conversation') renderConversation();
        else if (activeTab === 'metrics') renderMetrics();
      });
    });
    list.querySelectorAll('.star').forEach(el => {
      el.addEventListener('click', (e) => {
        e.stopPropagation();
        toggleBookmark(el.dataset.bkmk);
      });
    });
    list.querySelectorAll('.check').forEach(el => {
      el.addEventListener('click', (e) => e.stopPropagation());
      el.addEventListener('change', () => {
        const id = el.dataset.id;
        if (el.checked) selectedSet.add(id); else selectedSet.delete(id);
        updateBulkBar();
      });
    });
    list.querySelectorAll('[data-tag-input]').forEach(el => {
      el.value = '';
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          const id = el.dataset.tagInput;
          const existing = tags[id] || [];
          const newOnes = el.value.split(',').map(s => s.trim()).filter(Boolean);
          const merged = Array.from(new Set([...existing, ...newOnes]));
          setTagInput(id, merged.join(','));
        }
      });
    });
    list.querySelectorAll('.tag').forEach(el => {
      el.addEventListener('dblclick', (e) => {
        const row = e.target.closest('.session-item');
        if (!row) return;
        const id = row.dataset.id;
        const existing = tags[id] || [];
        const toRemove = e.target.textContent.trim();
        setTagInput(id, existing.filter(t => t !== toRemove).join(','));
      });
    });
  }

  function toggleSelected(id) {
    if (selectedSet.has(id)) selectedSet.delete(id); else selectedSet.add(id);
    updateBulkBar();
  }
  function updateBulkBar() {
    $('#bulk-bar').classList.toggle('visible', selectMode);
    $('#bulk-count').textContent = `${selectedSet.size} selected`;
  }
  function updateSessionFilterChip() {
    const chip = $('#clear-session-filter');
    chip.classList.toggle('visible', !!activeSession);
    chip.textContent = activeSession ? `✕ ${shortId(activeSession)}` : '';
  }

  // ---- Feed ------------------------------------------------------------
  function matchesFilter(ev) {
    if (activeType !== 'all') {
      if (activeType === 'tool_use') {
        if (!(ev.event_type === 'tool_use' || (ev.tool_uses && ev.tool_uses.length))) return false;
      } else if (activeType === 'tool_result') {
        if (!(ev.event_type === 'tool_result' || (ev.tool_results && ev.tool_results.length))) return false;
      } else if (ev.event_type !== activeType) return false;
    }
    if (activeSession && ev.session_id !== activeSession) return false;
    if (searchQuery) {
      const hay = (ev.summary + ' ' + ev.session_id + ' ' + (ev.tool_uses || []).join(' ') + ' ' + JSON.stringify(ev.entry)).toLowerCase();
      if (!hay.includes(searchQuery.toLowerCase())) return false;
    }
    return true;
  }

  function eventRowHtml(ev) {
    const etype = ev.event_type || 'unknown';
    const safeEtype = etype.replace(/[^a-z0-9_]/gi, '_');
    const ts = ev.observed_at ? ev.observed_at.slice(11, 19) : '';
    const selected = ev._idx === selectedIdx ? ' selected' : '';
    let tokens = '';
    if (ev.usage) {
      const ti = ev.usage.input + (ev.usage.cache_read || 0) + (ev.usage.cache_creation || 0);
      const to = ev.usage.output || 0;
      if (ti || to) tokens = `${ti.toLocaleString()}↑ ${to.toLocaleString()}↓`;
    }
    const cost = ev.cost_usd ? fmtCost(ev.cost_usd) : '';
    return `<div class="event-row${selected}" data-idx="${ev._idx}">
      <span class="session-tag" data-sid="${escHtml(ev.session_id)}" title="${escHtml(ev.session_id)}">${escHtml(shortId(ev.session_id))}</span>
      <span class="idx">${ev.line_index}</span>
      <span class="badge badge-${safeEtype}">${escHtml(etype)}</span>
      <span class="summary">${escHtml(ev.summary || '')}</span>
      <span class="tokens">${escHtml(tokens)}</span>
      <span class="cost">${escHtml(cost)}</span>
      <span class="ts">${escHtml(ts)}</span>
    </div>`;
  }

  function renderFeed() {
    const feed = $('#feed');
    const visible = allEvents.filter(matchesFilter);
    if (!visible.length) {
      feed.innerHTML = '<div id="no-events">No matching events.<div class="hint">Try clearing filters or waiting for Claude Code to produce more events.</div></div>';
      return;
    }
    feed.innerHTML = visible.map(eventRowHtml).join('');
    attachFeedHandlers(feed);
    if ($('#scroll-lock').checked) feed.scrollTop = feed.scrollHeight;
  }

  function attachFeedHandlers(feed) {
    feed.querySelectorAll('.event-row').forEach(el => {
      el.addEventListener('click', () => {
        selectedIdx = parseInt(el.dataset.idx, 10);
        feed.querySelectorAll('.event-row.selected').forEach(n => n.classList.remove('selected'));
        el.classList.add('selected');
        showDetail(allEvents[selectedIdx]);
      });
    });
    feed.querySelectorAll('.session-tag').forEach(el => {
      el.addEventListener('click', (e) => {
        e.stopPropagation();
        const sid = el.dataset.sid;
        activeSession = activeSession === sid ? null : sid;
        updateSessionFilterChip();
        renderSessions();
        renderFeed();
      });
    });
  }

  function appendEventToFeed(ev) {
    const feed = $('#feed');
    const placeholder = feed.querySelector('#no-events');
    if (placeholder) feed.innerHTML = '';
    const atBottom = feed.scrollHeight - feed.scrollTop <= feed.clientHeight + 40;
    feed.insertAdjacentHTML('beforeend', eventRowHtml(ev));
    const newRow = feed.lastElementChild;
    if (newRow) {
      newRow.addEventListener('click', () => {
        selectedIdx = parseInt(newRow.dataset.idx, 10);
        feed.querySelectorAll('.event-row.selected').forEach(n => n.classList.remove('selected'));
        newRow.classList.add('selected');
        showDetail(allEvents[selectedIdx]);
      });
      const tag = newRow.querySelector('.session-tag');
      if (tag) tag.addEventListener('click', (e) => {
        e.stopPropagation();
        const sid = tag.dataset.sid;
        activeSession = activeSession === sid ? null : sid;
        updateSessionFilterChip();
        renderSessions();
        renderFeed();
      });
    }
    if ($('#scroll-lock').checked && atBottom) feed.scrollTop = feed.scrollHeight;
  }

  function showDetail(ev) {
    if (!ev) return;
    const meta = $('#detail-meta');
    const json = $('#detail-json');
    const usage = ev.usage || {};
    const totIn = (usage.input || 0) + (usage.cache_read || 0) + (usage.cache_creation || 0);
    meta.innerHTML = `
      <div class="row"><span class="k">Session</span><span class="v accent">${escHtml(ev.session_id)}</span></div>
      <div class="row"><span class="k">Type</span><span class="v">${escHtml(ev.event_type)}</span></div>
      <div class="row"><span class="k">Line</span><span class="v">${ev.line_index}</span></div>
      ${ev.timestamp ? `<div class="row"><span class="k">Time</span><span class="v">${escHtml(ev.timestamp)}</span></div>` : ''}
      <div class="row"><span class="k">Observed</span><span class="v">${escHtml(ev.observed_at)}</span></div>
      ${ev.model ? `<div class="row"><span class="k">Model</span><span class="v purple">${escHtml(ev.model)}</span></div>` : ''}
      ${ev.cwd ? `<div class="row"><span class="k">CWD</span><span class="v">${escHtml(ev.cwd)}</span></div>` : ''}
      ${ev.git_branch ? `<div class="row"><span class="k">Branch</span><span class="v">${escHtml(ev.git_branch)}</span></div>` : ''}
      ${(ev.tool_uses && ev.tool_uses.length) ? `<div class="row"><span class="k">Tools</span><span class="v">${escHtml(ev.tool_uses.join(', '))}</span></div>` : ''}
      ${(totIn || usage.output) ? `<div class="row"><span class="k">Tokens</span><span class="v">${fmtNum(totIn)}↑ ${fmtNum(usage.output)}↓ <span style="color:var(--text-dim)">(in: ${fmtNum(usage.input)}, cache R: ${fmtNum(usage.cache_read)}, cache W: ${fmtNum(usage.cache_creation)})</span></span></div>` : ''}
      ${ev.cost_usd ? `<div class="row"><span class="k">Cost</span><span class="v orange">${fmtCost(ev.cost_usd)}${ev.cost_estimated ? ' (est.)' : ''}</span></div>` : ''}
    `;
    json.textContent = JSON.stringify(ev.entry, null, 2);
  }

  // ---- Conversation ----------------------------------------------------
  function renderConversation() {
    const root = $('#conversation');
    const tb = $('#conv-toolbar');
    if (!activeSession) {
      tb.style.display = 'none';
      root.innerHTML = '<div id="conversation-empty">Select a session to view the conversation.<div class="hint">Conversation view threads user / assistant / tool messages chronologically with latency badges.</div></div>';
      return;
    }
    const s = sessions.get(activeSession);
    const evs = sessionEvents.get(activeSession) || [];
    tb.style.display = 'flex';
    $('#conv-title').textContent = (s && (s.title || shortId(s.id))) || shortId(activeSession);
    $('#conv-bookmark-btn').textContent = bookmarks.has(activeSession) ? '★ Pinned' : '☆ Pin';
    if (!evs.length) {
      root.innerHTML = '<div id="conversation-empty">No events yet for this session.</div>';
      return;
    }
    // Pre-compute latencies (assistant timestamp - preceding user timestamp).
    const latencies = computeLatencies(evs);
    const html = evs.map((ev, i) => renderConvMessage(ev, latencies[i])).filter(Boolean).join('');
    root.innerHTML = html || '<div id="conversation-empty">Nothing to render for this session.</div>';
  }

  function computeLatencies(evs) {
    const lats = new Array(evs.length).fill(null);
    let lastUserT = null;
    for (let i = 0; i < evs.length; i++) {
      const e = evs[i];
      const t = e.timestamp ? new Date(e.timestamp).getTime() : new Date(e.observed_at).getTime();
      if (e.event_type === 'user') {
        lastUserT = t;
      } else if (e.event_type === 'assistant' && lastUserT) {
        lats[i] = t - lastUserT;
        lastUserT = null; // assign each latency once
      }
    }
    return lats;
  }
  function fmtLatency(ms) {
    if (ms == null) return '';
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60_000) return `${(ms/1000).toFixed(1)}s`;
    return `${Math.floor(ms/60_000)}m ${Math.round((ms%60_000)/1000)}s`;
  }

  function renderConvMessage(ev, latencyMs) {
    const t = ev.event_type;
    if (t === 'user') {
      const content = ev.entry && (ev.entry.message?.content ?? ev.entry.content);
      const parts = renderContentParts(content);
      if (parts.onlyToolResult) return renderToolResultCard(ev, content);
      return `<div class="conv-msg user">
        <div class="ch"><span class="role">User</span><span class="meta">${escHtml(ev.timestamp || ev.observed_at)}</span></div>
        <div class="body">${parts.html}</div>
      </div>`;
    }
    if (t === 'assistant') {
      const content = ev.entry && ev.entry.message?.content;
      const parts = renderContentParts(content);
      const usage = ev.usage || {};
      const tok = (usage.output || usage.input) ? ` · ${fmtNum(usage.input + (usage.cache_read||0) + (usage.cache_creation||0))}↑ ${fmtNum(usage.output)}↓` : '';
      const cost = ev.cost_usd ? ' · ' + fmtCost(ev.cost_usd) : '';
      const lat = latencyMs ? `<span class="latency">⚡ ${fmtLatency(latencyMs)}</span>` : '';
      return `<div class="conv-msg assistant">
        <div class="ch"><span class="role">Assistant</span>${ev.model ? `<span class="meta">${escHtml(ev.model)}</span>` : ''}${lat}<span class="meta">${escHtml(ev.timestamp || ev.observed_at)}${tok}${cost}</span></div>
        <div class="body">${parts.html || '<span style="color:var(--text-dim)">(no visible content)</span>'}</div>
      </div>`;
    }
    if (t === 'system') {
      const txt = (ev.entry && (ev.entry.content || ev.entry.text)) || '';
      return `<div class="conv-msg system">
        <div class="ch"><span class="role">System</span><span class="meta">${escHtml(ev.timestamp || ev.observed_at)}</span></div>
        <div class="body"><div class="text">${escHtml(typeof txt === 'string' ? txt : JSON.stringify(txt, null, 2))}</div></div>
      </div>`;
    }
    if (t === 'summary') {
      const s = ev.entry?.summary || '';
      return `<div class="conv-msg system">
        <div class="ch"><span class="role">Summary</span><span class="meta">${escHtml(ev.timestamp || ev.observed_at)}</span></div>
        <div class="body"><div class="text">${escHtml(s)}</div></div>
      </div>`;
    }
    return '';
  }

  function renderContentParts(content) {
    if (content == null) return { html: '', onlyToolResult: false };
    if (typeof content === 'string') return { html: `<div class="text">${escHtml(content)}</div>`, onlyToolResult: false };
    if (!Array.isArray(content)) return { html: `<pre style="white-space:pre-wrap">${escHtml(JSON.stringify(content, null, 2))}</pre>`, onlyToolResult: false };
    let html = '';
    let onlyToolResult = true;
    for (const block of content) {
      const bt = block?.type;
      if (bt !== 'tool_result') onlyToolResult = false;
      switch (bt) {
        case 'text': html += `<div class="text">${escHtml(block.text || '')}</div>`; break;
        case 'thinking': html += `<div class="thinking">${escHtml(block.thinking || block.text || '')}</div>`; break;
        case 'tool_use': {
          const input = block.input ? JSON.stringify(block.input, null, 2) : '';
          html += `<div class="tool"><div class="head"><span>🔧</span><span class="name">${escHtml(block.name || '?')}</span><span class="id">${escHtml(block.id || '')}</span></div>${input ? `<pre>${escHtml(input)}</pre>` : ''}</div>`;
          break;
        }
        case 'tool_result': {
          const body = typeof block.content === 'string' ? block.content : JSON.stringify(block.content, null, 2);
          html += `<div class="tool"><div class="head"><span>📦</span><span class="name">tool_result</span><span class="id">${escHtml(block.tool_use_id || '')}</span></div><pre>${escHtml(body || '')}</pre></div>`;
          break;
        }
        case 'image': html += `<div style="color:var(--text-dim);font-size:11px">[image]</div>`; break;
        default: html += `<pre style="color:var(--text-dim);font-size:11px;white-space:pre-wrap">${escHtml(JSON.stringify(block, null, 2))}</pre>`;
      }
    }
    return { html, onlyToolResult };
  }

  function renderToolResultCard(ev, content) {
    const items = (content || []).filter(b => b.type === 'tool_result');
    const body = items.map(b => {
      const text = typeof b.content === 'string' ? b.content : JSON.stringify(b.content, null, 2);
      return `<div class="tool"><div class="head"><span>📦</span><span class="name">tool_result</span><span class="id">${escHtml(b.tool_use_id || '')}</span></div><pre>${escHtml(text || '')}</pre></div>`;
    }).join('');
    return `<div class="conv-msg tool-result">
      <div class="ch"><span class="role">Tool Result</span><span class="meta">${escHtml(ev.timestamp || ev.observed_at)}</span></div>
      <div class="body">${body}</div>
    </div>`;
  }

  // ---- Metrics ---------------------------------------------------------
  function renderMetrics() {
    const root = $('#metrics');
    if (!sessions.size) {
      root.innerHTML = '<div id="metrics-empty">No data yet. Run Claude Code to populate the metrics.</div>';
      return;
    }
    const sessList = activeSession
      ? [sessions.get(activeSession)].filter(Boolean)
      : Array.from(sessions.values());

    const totals = sessList.reduce((acc, s) => {
      acc.events += s.event_count; acc.user += s.user_count; acc.asst += s.assistant_count;
      acc.tools += s.tool_use_count; acc.toolResults += s.tool_result_count;
      acc.tokIn += s.input_tokens; acc.tokOut += s.output_tokens;
      acc.cacheR += s.cache_read_tokens; acc.cacheW += s.cache_creation_tokens;
      acc.cost += s.cost_usd;
      return acc;
    }, { events:0,user:0,asst:0,tools:0,toolResults:0,tokIn:0,tokOut:0,cacheR:0,cacheW:0,cost:0 });

    const allInRead = totals.tokIn + totals.cacheR + totals.cacheW;
    const cacheHit = allInRead > 0 ? ((totals.cacheR / allInRead) * 100).toFixed(1) : '0.0';

    const toolCounts = {};
    sessList.forEach(s => { for (const [k, v] of Object.entries(s.tool_counts || {})) toolCounts[k] = (toolCounts[k] || 0) + v; });
    const toolEntries = Object.entries(toolCounts).sort((a,b) => b[1]-a[1]).slice(0, 15);
    const maxTool = toolEntries.length ? toolEntries[0][1] : 1;

    const sessCost = sessList.map(s => ({ id: s.id, title: s.title, cost: s.cost_usd, events: s.event_count }))
      .sort((a,b) => b.cost - a.cost).slice(0, 10);
    const maxCost = sessCost.length ? Math.max(...sessCost.map(s => s.cost), 0.0001) : 0.0001;

    const now = Date.now();
    const buckets = new Array(60).fill(0);
    const events = activeSession ? (sessionEvents.get(activeSession) || []) : allEvents;
    events.forEach(e => {
      const t = new Date(e.observed_at).getTime();
      const ageMin = (now - t) / 60_000;
      if (ageMin >= 0 && ageMin < 60) buckets[59 - Math.floor(ageMin)]++;
    });
    const totalInWindow = buckets.reduce((a,b)=>a+b, 0);

    // Latency stats (assistant - preceding user, across sessions)
    const lats = [];
    for (const sid of (activeSession ? [activeSession] : sessionEvents.keys())) {
      const evs = sessionEvents.get(sid) || [];
      const l = computeLatencies(evs);
      for (const x of l) if (x != null) lats.push(x);
    }
    lats.sort((a,b) => a-b);
    const p50 = lats.length ? lats[Math.floor(lats.length*0.5)] : 0;
    const p95 = lats.length ? lats[Math.floor(lats.length*0.95)] : 0;

    root.innerHTML = `
      <div class="metric-grid">
        <div class="metric-card"><div class="label">Sessions</div><div class="value accent">${sessList.length}</div><div class="sub">${activeSession ? 'filtered to active' : 'across all observed sessions'}</div></div>
        <div class="metric-card"><div class="label">Events</div><div class="value">${fmtNum(totals.events)}</div><div class="sub">${fmtNum(totals.user)} user · ${fmtNum(totals.asst)} assistant · ${fmtNum(totals.tools)} tool calls</div></div>
        <div class="metric-card"><div class="label">Est. cost</div><div class="value orange">${fmtCost(totals.cost)}</div><div class="sub">approximate — public list pricing</div></div>
        <div class="metric-card"><div class="label">Tokens out</div><div class="value green">${fmtNum(totals.tokOut)}</div><div class="sub">${fmtNum(totals.tokIn)} input · ${fmtNum(totals.cacheR + totals.cacheW)} cache</div></div>
        <div class="metric-card"><div class="label">Cache hit</div><div class="value purple">${cacheHit}%</div><div class="sub">${fmtNum(totals.cacheR)} of ${fmtNum(allInRead)} input</div></div>
        <div class="metric-card"><div class="label">Latency</div><div class="value">${fmtLatency(p50)}</div><div class="sub">median · p95 ${fmtLatency(p95)} · n=${lats.length}</div></div>
        <div class="metric-card"><div class="label">Events / hr</div><div class="value">${fmtNum(totalInWindow)}</div><div class="sub">over the last hour</div></div>
      </div>

      <div class="chart-row">
        <div class="chart-card">
          <h3>Events over last hour <span style="color:var(--text-dim);font-size:11px;font-weight:400">${activeSession ? 'session ' + escHtml(shortId(activeSession)) : 'all sessions'}</span></h3>
          ${timelineChartSvg(buckets)}
        </div>
        <div class="chart-card">
          <h3>Top tool calls</h3>
          ${toolEntries.length ? '<div class="bar-list">' + toolEntries.map(([name, count]) =>
            `<div class="bar"><span class="name" title="${escHtml(name)}">${escHtml(name)}</span>
              <div class="track"><div class="fill" style="width:${(count/maxTool*100).toFixed(1)}%"></div></div>
              <span class="val">${count}</span></div>`).join('') + '</div>' : '<div style="color:var(--text-dim);font-size:12px">No tool calls yet.</div>'}
        </div>
      </div>

      <div class="chart-row">
        <div class="chart-card">
          <h3>Cost by session (top 10)</h3>
          ${sessCost.length ? '<div class="bar-list">' + sessCost.map(s =>
            `<div class="bar"><span class="name" title="${escHtml(s.id)}">${escHtml(s.title || shortId(s.id))}</span>
              <div class="track"><div class="fill" style="width:${(s.cost/maxCost*100).toFixed(1)}%"></div></div>
              <span class="val">${fmtCost(s.cost)}</span></div>`).join('') + '</div>' : '<div style="color:var(--text-dim);font-size:12px">No cost data yet.</div>'}
        </div>
        <div class="chart-card">
          <h3>Tokens by session (top 10)</h3>
          ${(() => {
            const arr = sessList.slice().sort((a,b)=>(b.output_tokens||0)-(a.output_tokens||0)).slice(0,10);
            if (!arr.length) return '<div style="color:var(--text-dim);font-size:12px">No token data yet.</div>';
            const max = Math.max(...arr.map(s=>s.output_tokens||0), 1);
            return '<div class="bar-list">' + arr.map(s =>
              `<div class="bar"><span class="name" title="${escHtml(s.id)}">${escHtml(s.title || shortId(s.id))}</span>
                <div class="track"><div class="fill" style="width:${((s.output_tokens||0)/max*100).toFixed(1)}%;background:linear-gradient(90deg,var(--green),var(--accent))"></div></div>
                <span class="val">${fmtNum(s.output_tokens)}↓</span></div>`).join('') + '</div>';
          })()}
        </div>
      </div>
    `;
  }

  function timelineChartSvg(buckets) {
    const w = 480, h = 200, padL = 36, padB = 22, padT = 8, padR = 8;
    const max = Math.max(...buckets, 1);
    const innerW = w - padL - padR;
    const innerH = h - padT - padB;
    const dx = innerW / Math.max(buckets.length - 1, 1);
    let line = '';
    buckets.forEach((v, i) => {
      const x = padL + i * dx;
      const y = padT + innerH - (v / max) * innerH;
      line += (i === 0 ? 'M' : 'L') + x.toFixed(1) + ',' + y.toFixed(1) + ' ';
    });
    const area = line + `L${(padL+innerW).toFixed(1)},${(padT+innerH).toFixed(1)} L${padL},${(padT+innerH).toFixed(1)} Z`;
    let gridY = '', yLabels = '';
    for (let i = 0; i <= 4; i++) {
      const y = padT + (innerH * i / 4);
      gridY += `<line x1="${padL}" y1="${y}" x2="${padL+innerW}" y2="${y}"/>`;
      const val = Math.round(max * (1 - i / 4));
      yLabels += `<text x="${padL - 6}" y="${y + 3}" text-anchor="end">${val}</text>`;
    }
    let xLabels = '';
    [0, 15, 30, 45, 59].forEach(i => {
      const x = padL + i * dx;
      const min = 60 - i;
      xLabels += `<text x="${x}" y="${h - 6}" text-anchor="middle">${min}m</text>`;
    });
    return `<svg class="timeline-chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
      <g class="grid">${gridY}</g>
      <path class="area" d="${area}"/>
      <path class="line" d="${line}"/>
      <g class="axis">
        <line x1="${padL}" y1="${padT}" x2="${padL}" y2="${padT+innerH}"/>
        <line x1="${padL}" y1="${padT+innerH}" x2="${padL+innerW}" y2="${padT+innerH}"/>
        ${yLabels}${xLabels}
      </g>
    </svg>`;
  }

  // ---- Export ----------------------------------------------------------
  function openExportModal(scope) {
    // scope: { kind: 'single', id } | { kind: 'all' } | { kind: 'bulk' }
    const m = $('#export-modal');
    m._scope = scope;
    if (scope.kind === 'single') $('#export-scope').textContent = `session ${shortId(scope.id)}`;
    else if (scope.kind === 'bulk') $('#export-scope').textContent = `${scope.ids.length} selected session${scope.ids.length === 1 ? '' : 's'}`;
    else $('#export-scope').textContent = 'all sessions';
    m.classList.add('visible');
    refreshExportPreview();
  }
  function closeModal(id) { $('#'+id).classList.remove('visible'); }
  function currentExportFormat() {
    const r = $('input[name="export-format"]:checked');
    return r ? r.value : 'messages';
  }
  // Cancel any preview fetch that's still in flight when a new one starts —
  // otherwise switching formats rapidly can stack downloads of (potentially
  // large) full exports in flight.
  let previewAbort = null;
  const PREVIEW_LIMIT = 2000;

  async function refreshExportPreview() {
    const fmt = currentExportFormat();
    const scope = $('#export-modal')._scope;
    const url = exportUrl(scope, fmt);
    const pre = $('#export-preview');
    pre.textContent = `GET ${url}\n\n(loading preview…)`;
    if (previewAbort) { try { previewAbort.abort(); } catch (e) {} }
    previewAbort = new AbortController();
    const ctrl = previewAbort;
    try {
      const res = await fetch(url, {
        headers: { 'Accept': 'application/x-ndjson' },
        signal: ctrl.signal,
      });
      if (!res.ok) throw new Error('HTTP ' + res.status);
      // Stream the response body and stop reading once we've buffered enough
      // for the preview. For large exports this means we never pull more than
      // a few KB across the network, and we never allocate the full payload
      // in JS memory.
      if (!res.body || !res.body.getReader) {
        const text = await res.text();
        if (ctrl.signal.aborted) return;
        pre.textContent = text.slice(0, PREVIEW_LIMIT) + (text.length > PREVIEW_LIMIT ? '\n…' : '');
        return;
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder('utf-8', { fatal: false });
      let acc = '';
      let truncated = false;
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        acc += decoder.decode(value, { stream: true });
        if (acc.length >= PREVIEW_LIMIT) {
          truncated = true;
          acc = acc.slice(0, PREVIEW_LIMIT);
          try { await reader.cancel(); } catch (e) {}
          ctrl.abort();
          break;
        }
      }
      acc += decoder.decode();
      if (ctrl.signal.aborted && !truncated) return;
      pre.textContent = acc + (truncated ? '\n…' : '');
    } catch (e) {
      if (e.name === 'AbortError') return; // superseded by a newer preview
      pre.textContent = 'Preview failed: ' + e.message;
    }
  }
  function exportUrl(scope, fmt) {
    if (scope.kind === 'single') return `/api/sessions/${encodeURIComponent(scope.id)}/export?format=${fmt}`;
    if (scope.kind === 'bulk') return `/api/export?format=${fmt}&sessions=${encodeURIComponent(scope.ids.join(','))}`;
    return `/api/export?format=${fmt}`;
  }
  function downloadExport() {
    const scope = $('#export-modal')._scope;
    const fmt = currentExportFormat();
    const url = exportUrl(scope, fmt);
    // For huggingface, we receive a JSONL of records — adjust filename hint.
    const a = document.createElement('a');
    a.href = url;
    a.download = '';
    document.body.appendChild(a);
    a.click();
    a.remove();
    toast('Downloading…');
    closeModal('export-modal');
  }

  // ---- Saved views -----------------------------------------------------
  function renderSavedViewsSelect() {
    const sel = $('#saved-views');
    sel.innerHTML = '<option value="">— Saved views —</option>'
      + savedViews.map((v, i) => `<option value="${i}">${escHtml(v.name)}</option>`).join('');
  }
  function applySavedView(idx) {
    const v = savedViews[idx]; if (!v) return;
    activeType = v.type || 'all';
    searchQuery = v.search || '';
    activeSession = v.session || null;
    sessionFilterQuery = v.sessionFilter || '';
    $$('.type-btn').forEach(b => b.classList.toggle('active', b.dataset.type === activeType));
    $('#search').value = searchQuery;
    $('#session-search').value = sessionFilterQuery;
    updateSessionFilterChip();
    renderSessions();
    renderFeed();
    toast(`Loaded view "${v.name}"`);
  }
  function saveCurrentView() {
    const name = prompt('Name this view:', 'My view');
    if (!name) return;
    savedViews.push({
      name, type: activeType, search: searchQuery,
      session: activeSession, sessionFilter: sessionFilterQuery,
    });
    save(STORAGE.VIEWS, savedViews);
    renderSavedViewsSelect();
    toast(`Saved "${name}"`);
  }

  // ---- Command palette -------------------------------------------------
  let paletteItems = [];
  let paletteIdx = 0;
  function openPalette() {
    paletteItems = buildPaletteItems('');
    paletteIdx = 0;
    renderPalette();
    $('#palette-modal').classList.add('visible');
    setTimeout(() => $('#palette-input').focus(), 50);
  }
  function buildPaletteItems(query) {
    const q = query.toLowerCase();
    const items = [];
    const cmds = [
      { icon: '⤓', title: 'Export all sessions', action: () => openExportModal({ kind: 'all' }) },
      { icon: '⏸', title: paused ? 'Resume live updates' : 'Pause live updates', action: () => togglePause() },
      { icon: '🌓', title: 'Toggle theme', action: () => toggleTheme() },
      { icon: '?', title: 'Show keyboard shortcuts', action: () => $('#help-modal').classList.add('visible') },
      { icon: '📁', title: 'Toggle sidebar', action: () => toggleSidebar() },
      { icon: '☑', title: 'Toggle multi-select mode', action: () => toggleSelectMode() },
      { icon: '#', title: 'Go to Live Feed', action: () => switchTab('feed') },
      { icon: '#', title: 'Go to Conversation', action: () => switchTab('conversation') },
      { icon: '#', title: 'Go to Metrics', action: () => switchTab('metrics') },
    ];
    for (const c of cmds) if (!q || c.title.toLowerCase().includes(q)) items.push({ kind: 'cmd', ...c });
    for (const s of sessions.values()) {
      const hay = (s.id + ' ' + (s.title||'') + ' ' + (s.cwd||'')).toLowerCase();
      if (!q || hay.includes(q)) {
        items.push({
          kind: 'session',
          icon: bookmarks.has(s.id) ? '★' : '◆',
          title: s.title || shortId(s.id),
          sub: (s.cwd || '') + ' · ' + s.event_count + ' events · ' + fmtCost(s.cost_usd),
          action: () => { activeSession = s.id; updateSessionFilterChip(); renderSessions(); switchTab('conversation'); },
        });
        if (items.length > 60) break;
      }
    }
    return items;
  }
  function renderPalette() {
    const list = $('#palette-list');
    list.innerHTML = paletteItems.map((it, i) =>
      `<div class="palette-item${i === paletteIdx ? ' active' : ''}" data-i="${i}">
        <span class="icon">${escHtml(it.icon)}</span>
        <span class="title">${escHtml(it.title)}</span>
        ${it.sub ? `<span class="sub">${escHtml(it.sub)}</span>` : ''}
      </div>`).join('');
    list.querySelectorAll('.palette-item').forEach(el => {
      el.addEventListener('click', () => {
        const i = parseInt(el.dataset.i, 10);
        const it = paletteItems[i]; if (!it) return;
        $('#palette-modal').classList.remove('visible');
        it.action();
      });
    });
    const active = list.querySelector('.palette-item.active');
    if (active) active.scrollIntoView({ block: 'nearest' });
  }

  // ---- Tab switching ---------------------------------------------------
  function switchTab(name) {
    $$('.tab').forEach(x => x.classList.toggle('active', x.dataset.tab === name));
    $$('.tab-body').forEach(x => x.classList.toggle('active', x.dataset.tabBody === name));
    activeTab = name;
    if (name === 'feed') renderFeed();
    else if (name === 'conversation') renderConversation();
    else if (name === 'metrics') renderMetrics();
  }

  function togglePause() {
    paused = !paused;
    if (!paused) {
      const buf = pendingBuffer; pendingBuffer = [];
      buf.forEach(ev => ingestEvent(ev));
      rerenderAll();
      $('#pause-btn').textContent = '⏸ Pause';
      $('#pause-btn').classList.remove('active');
    } else {
      $('#pause-btn').textContent = '▶ Resume (0)';
      $('#pause-btn').classList.add('active');
    }
  }
  function toggleTheme() {
    const cur = document.documentElement.getAttribute('data-theme') === 'light' ? 'dark' : 'light';
    document.documentElement.setAttribute('data-theme', cur);
    save(STORAGE.THEME, cur);
  }
  function toggleSidebar() {
    $('#sidebar').classList.toggle('collapsed');
  }
  function toggleSelectMode() {
    selectMode = !selectMode;
    document.body.classList.toggle('select-mode', selectMode);
    $('#select-mode-btn').classList.toggle('active', selectMode);
    if (!selectMode) selectedSet.clear();
    updateBulkBar();
    renderSessions();
  }

  // ---- Resize handles --------------------------------------------------
  function setupResize(handle, target, key) {
    let dragging = false, startX, startW;
    handle.addEventListener('mousedown', (e) => {
      dragging = true;
      startX = e.clientX;
      startW = target.offsetWidth;
      handle.classList.add('dragging');
      e.preventDefault();
    });
    window.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      let w;
      if (target.id === 'sidebar') w = Math.max(180, Math.min(600, startW + (e.clientX - startX)));
      else w = Math.max(220, Math.min(900, startW - (e.clientX - startX)));
      target.style.width = w + 'px';
    });
    window.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      handle.classList.remove('dragging');
      save(key, target.offsetWidth);
    });
  }

  // ---- Wiring ----------------------------------------------------------
  $$('.tab').forEach(t => t.addEventListener('click', () => switchTab(t.dataset.tab)));

  $$('.type-btn').forEach(btn => btn.addEventListener('click', () => {
    activeType = btn.dataset.type;
    $$('.type-btn').forEach(b => b.classList.toggle('active', b === btn));
    renderFeed();
  }));

  $('#search').addEventListener('input', e => { searchQuery = e.target.value; renderFeed(); });
  $('#session-search').addEventListener('input', e => { sessionFilterQuery = e.target.value; renderSessions(); });
  $('#clear-session-filter').addEventListener('click', () => {
    activeSession = null;
    updateSessionFilterChip();
    renderSessions();
    if (activeTab === 'feed') renderFeed();
    else if (activeTab === 'conversation') renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  });
  $('#pause-btn').addEventListener('click', togglePause);
  $('#theme-btn').addEventListener('click', toggleTheme);
  $('#toggle-sidebar').addEventListener('click', toggleSidebar);
  $('#help-btn').addEventListener('click', () => $('#help-modal').classList.add('visible'));
  $('#palette-btn').addEventListener('click', openPalette);
  $('#export-all-btn').addEventListener('click', () => openExportModal({ kind: 'all' }));
  $('#conv-export-btn').addEventListener('click', () => {
    if (activeSession) openExportModal({ kind: 'single', id: activeSession });
  });
  $('#conv-bookmark-btn').addEventListener('click', () => {
    if (activeSession) toggleBookmark(activeSession);
  });
  $('#select-mode-btn').addEventListener('click', toggleSelectMode);
  $('#show-bookmarked-btn').addEventListener('click', () => {
    showBookmarkedOnly = !showBookmarkedOnly;
    $('#show-bookmarked-btn').classList.toggle('active', showBookmarkedOnly);
    renderSessions();
  });
  $('#bulk-select-all').addEventListener('click', () => {
    for (const s of sessions.values()) selectedSet.add(s.id);
    updateBulkBar(); renderSessions();
  });
  $('#bulk-select-none').addEventListener('click', () => {
    selectedSet.clear(); updateBulkBar(); renderSessions();
  });
  $('#bulk-export-btn').addEventListener('click', () => {
    if (!selectedSet.size) { toast('Select at least one session first'); return; }
    openExportModal({ kind: 'bulk', ids: Array.from(selectedSet) });
  });

  // Modal close buttons
  $$('[data-close]').forEach(b => b.addEventListener('click', () => closeModal(b.dataset.close)));
  $$('.modal-backdrop').forEach(bd => bd.addEventListener('click', (e) => {
    if (e.target === bd) bd.classList.remove('visible');
  }));

  // Export modal format radios + preview
  $('#export-format-options').addEventListener('change', () => {
    $$('#export-format-options label').forEach(l => l.classList.toggle('selected', l.querySelector('input').checked));
    refreshExportPreview();
  });
  $('#export-download-btn').addEventListener('click', downloadExport);

  // Copy detail JSON
  $('#copy-json').addEventListener('click', () => {
    if (selectedIdx !== null && allEvents[selectedIdx]) {
      copy(JSON.stringify(allEvents[selectedIdx].entry, null, 2));
      $('#copy-json').textContent = '✓ Copied';
      setTimeout(() => { $('#copy-json').textContent = 'Copy JSON'; }, 1200);
    }
  });

  // Saved views
  $('#save-view-btn').addEventListener('click', saveCurrentView);
  $('#saved-views').addEventListener('change', (e) => {
    if (e.target.value !== '') applySavedView(parseInt(e.target.value, 10));
    e.target.value = '';
  });
  renderSavedViewsSelect();

  // Palette input handlers
  $('#palette-input').addEventListener('input', (e) => {
    paletteItems = buildPaletteItems(e.target.value);
    paletteIdx = 0;
    renderPalette();
  });
  $('#palette-input').addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { paletteIdx = Math.min(paletteItems.length-1, paletteIdx+1); renderPalette(); e.preventDefault(); }
    else if (e.key === 'ArrowUp') { paletteIdx = Math.max(0, paletteIdx-1); renderPalette(); e.preventDefault(); }
    else if (e.key === 'Enter') {
      const it = paletteItems[paletteIdx];
      if (it) { $('#palette-modal').classList.remove('visible'); it.action(); }
      e.preventDefault();
    } else if (e.key === 'Escape') {
      $('#palette-modal').classList.remove('visible');
    }
  });

  // Global keyboard shortcuts
  document.addEventListener('keydown', (e) => {
    const target = e.target;
    const inField = target.matches('input, textarea, select');
    const cmd = e.ctrlKey || e.metaKey;
    // Modal close
    if (e.key === 'Escape') {
      const open = $$('.modal-backdrop.visible');
      if (open.length) { open.forEach(o => o.classList.remove('visible')); return; }
      if (inField) target.blur();
      // also clear searchQuery if focused
      if (searchQuery) { searchQuery = ''; $('#search').value = ''; renderFeed(); }
      return;
    }
    // Ctrl/Cmd combos
    if (cmd && e.key.toLowerCase() === 'k') { e.preventDefault(); openPalette(); return; }
    if (cmd && e.key.toLowerCase() === 'b') { e.preventDefault(); toggleSidebar(); return; }
    if (inField) return;
    if (e.key === '/') { e.preventDefault(); $('#search').focus(); }
    else if (e.key === '?') { e.preventDefault(); $('#help-modal').classList.add('visible'); }
    else if (e.key === ' ') { e.preventDefault(); togglePause(); }
    else if (e.key === 'f') switchTab('feed');
    else if (e.key === 'c') switchTab('conversation');
    else if (e.key === 'm') switchTab('metrics');
    else if (e.key === 'e') openExportModal(activeSession ? { kind: 'single', id: activeSession } : { kind: 'all' });
    else if (e.key === 'b' && activeSession) { toggleBookmark(activeSession); toast(bookmarks.has(activeSession) ? 'Bookmarked' : 'Removed bookmark'); }
    else if (e.key === 'S' && e.shiftKey) toggleSelectMode();
    else if (e.key === 'j' || e.key === 'k') {
      const dir = e.key === 'j' ? 1 : -1;
      const visible = allEvents.filter(matchesFilter);
      if (!visible.length) return;
      let cur = visible.findIndex(v => v._idx === selectedIdx);
      cur = Math.max(0, Math.min(visible.length-1, cur + dir));
      if (cur < 0) cur = 0;
      const ev = visible[cur]; if (!ev) return;
      selectedIdx = ev._idx;
      switchTab('feed');
      const row = $(`.event-row[data-idx="${ev._idx}"]`);
      if (row) {
        $$('.event-row.selected').forEach(n => n.classList.remove('selected'));
        row.classList.add('selected');
        row.scrollIntoView({ block: 'nearest' });
      }
      showDetail(ev);
    }
  });

  // Resize handles
  setupResize($('#sidebar-resize'), $('#sidebar'), STORAGE.SIDEBAR_W);
  setupResize($('#detail-resize'), $('#detail'), STORAGE.DETAIL_W);
  // Restore pane widths from storage
  const sw = load(STORAGE.SIDEBAR_W); if (sw) $('#sidebar').style.width = sw + 'px';
  const dw = load(STORAGE.DETAIL_W); if (dw) $('#detail').style.width = dw + 'px';

  // Periodic refresh so "live" dots and "Xs ago" stay current.
  setInterval(() => { if (sessions.size) renderSessions(); }, 5000);

  connect();
})();
</script>
</body>
</html>"##;
