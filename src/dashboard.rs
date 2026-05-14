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
  :root {
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

  /* Top bar */
  header {
    background: linear-gradient(180deg, var(--bg2) 0%, var(--bg) 100%);
    border-bottom: 1px solid var(--border);
    padding: 10px 16px;
    display: flex;
    align-items: center;
    gap: 14px;
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
  header h1 .logo {
    font-size: 18px;
  }
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
    width: 7px;
    height: 7px;
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
  .controls button {
    background: var(--bg3);
    border: 1px solid var(--border);
    color: var(--text);
    padding: 4px 10px;
    border-radius: 6px;
    cursor: pointer;
    font-size: 12px;
  }
  .controls button:hover { background: var(--bg4); border-color: var(--accent); }
  .controls button.active { background: var(--accent-bg); border-color: var(--accent); color: var(--accent); }

  /* Main layout */
  .main { display: flex; flex: 1; overflow: hidden; }
  aside {
    width: 280px;
    flex-shrink: 0;
    background: var(--bg2);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
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
  }
  .session-item:hover { background: var(--bg3); }
  .session-item.active { border-left-color: var(--accent); background: var(--bg3); }
  .session-item .row1 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 4px;
  }
  .session-item .live {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--green);
    box-shadow: 0 0 6px var(--green);
    flex-shrink: 0;
    display: none;
  }
  .session-item.live .live { display: inline-block; }
  .session-item .sid {
    font-family: 'SF Mono', monospace;
    font-size: 11px;
    color: var(--accent);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
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
  }
  .session-item .meta .cost { color: var(--orange); }
  .session-item .meta .branch { color: var(--purple); }
  .spark { height: 18px; margin-top: 4px; }
  .spark path.area { fill: rgba(88, 166, 255, 0.22); }
  .spark path.line { fill: none; stroke: var(--accent); stroke-width: 1.2; }

  /* Content */
  .content { flex: 1; display: flex; flex-direction: column; overflow: hidden; }
  .tabs {
    display: flex;
    background: var(--bg2);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    padding-left: 8px;
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

  /* Feed tab */
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
  .feed-container { display: flex; flex: 1; overflow: hidden; }
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
  .event-row .tokens {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--text-dim);
    flex-shrink: 0;
  }
  .event-row .cost {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--orange);
    flex-shrink: 0;
  }
  .event-row .ts {
    font-family: 'SF Mono', monospace;
    font-size: 10px;
    color: var(--text-dim);
    flex-shrink: 0;
  }

  /* Detail panel */
  #detail {
    width: 380px;
    flex-shrink: 0;
    border-left: 1px solid var(--border);
    background: var(--bg2);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
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

  /* Conversation tab */
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

  /* Metrics tab */
  #metrics { flex: 1; overflow-y: auto; padding: 16px; }
  .metric-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
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
  .metric-card .sub {
    font-size: 11px;
    color: var(--text-dim);
    margin-top: 4px;
  }

  .chart-row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 16px; }
  @media (max-width: 1100px) { .chart-row { grid-template-columns: 1fr; } }
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

  /* Empty/loading state */
  #no-events {
    padding: 60px 32px;
    text-align: center;
    color: var(--text-muted);
    font-size: 14px;
  }
  #no-events .hint { font-size: 12px; margin-top: 8px; color: var(--text-dim); }

  /* Stats bar */
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

  /* Scrollbars (webkit) */
  ::-webkit-scrollbar { width: 9px; height: 9px; }
  ::-webkit-scrollbar-track { background: transparent; }
  ::-webkit-scrollbar-thumb { background: var(--bg4); border-radius: 4px; }
  ::-webkit-scrollbar-thumb:hover { background: var(--border-strong); }

  .clear-filter-btn {
    font-size: 10px;
    padding: 2px 8px;
    border-radius: 8px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-muted);
    cursor: pointer;
  }
  .clear-filter-btn:hover { color: var(--red); border-color: var(--red); }
</style>
</head>
<body>
<header>
  <h1><span class="logo">🔍</span> Claude Trace</h1>
  <span id="status" class="pill disconnected"><span class="dot"></span> <span id="status-text">Connecting…</span></span>
  <span class="pill" id="watch-root-pill" title="Watch root">—</span>
  <span class="spacer"></span>
  <span class="header-stat"><span><b id="hdr-sessions">0</b> sessions</span></span>
  <span class="header-stat"><span><b id="hdr-events">0</b> events</span></span>
  <span class="header-stat"><span><b id="hdr-cost">$0.0000</b> est. cost</span></span>
  <div class="controls">
    <button id="pause-btn" title="Pause/resume live updates">⏸ Pause</button>
    <label style="display:flex;align-items:center;gap:4px;font-size:12px;color:var(--text-muted);cursor:pointer">
      <input type="checkbox" id="scroll-lock" checked> Auto-scroll
    </label>
  </div>
</header>
<div class="main">
  <aside>
    <h2>Sessions <span class="count" id="session-count">0</span></h2>
    <input id="session-search" placeholder="Filter sessions…">
    <div id="session-list"><div style="padding:16px 12px;font-size:12px;color:var(--text-muted)">No sessions yet. Run Claude Code to populate.</div></div>
  </aside>
  <div class="content">
    <div class="tabs">
      <div class="tab active" data-tab="feed">Live Feed</div>
      <div class="tab" data-tab="conversation">Conversation</div>
      <div class="tab" data-tab="metrics">Metrics</div>
    </div>

    <!-- Feed tab -->
    <div class="tab-body active" data-tab-body="feed">
      <div class="toolbar">
        <button class="type-btn active" data-type="all">All</button>
        <button class="type-btn" data-type="user">👤 User</button>
        <button class="type-btn" data-type="assistant">🤖 Assistant</button>
        <button class="type-btn" data-type="tool_use">🔧 Tool Use</button>
        <button class="type-btn" data-type="tool_result">📦 Tool Result</button>
        <button class="type-btn" data-type="system">⚙️ System</button>
        <input id="search" type="text" placeholder="Search events…">
        <button class="clear-filter-btn" id="clear-session-filter" style="display:none">✕ Session filter</button>
      </div>
      <div class="feed-container">
        <div id="feed"><div id="no-events">Waiting for events…<div class="hint">Run Claude Code (or use --backfill) and they'll appear here.</div></div></div>
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

    <!-- Conversation tab -->
    <div class="tab-body" data-tab-body="conversation">
      <div id="conversation"><div id="conversation-empty">Select a session to view the conversation.<div class="hint">Conversation view renders user / assistant / tool messages threaded chronologically.</div></div></div>
    </div>

    <!-- Metrics tab -->
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

<script>
(function() {
  const WS_URL = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`;
  let ws, reconnectTimer;
  /** Map<sessionId, SessionInfo> — authoritative aggregates from server. */
  const sessions = new Map();
  /** All events received so far, capped. */
  let allEvents = [];
  /** Per-session events index keyed on session_id (kept in sync with allEvents). */
  const sessionEvents = new Map();
  /** Dedupe key set: `${session_id}:${line_index}` for every ingested event. */
  const seenKeys = new Set();
  const EVENT_CAP = 50000;

  let activeSession = null;
  let activeType = 'all';
  let searchQuery = '';
  let sessionFilterQuery = '';
  let selectedIdx = null;
  let activeTab = 'feed';
  let paused = false;
  let pendingBuffer = [];
  let watchRoot = '';

  const $ = (s) => document.querySelector(s);
  const $$ = (s) => document.querySelectorAll(s);

  function fmtCost(c) { return '$' + (c || 0).toFixed(4); }
  function fmtNum(n) { return (n || 0).toLocaleString(); }
  function escHtml(s) {
    return String(s ?? '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
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
    if (diff < 0) return 'now';
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

  // ---- WebSocket lifecycle ------------------------------------------------
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

  function handleSnapshot(snap) {
    // Snapshots are authoritative — drop any state we may have built up from
    // a previous connection so reconnects don't accumulate duplicate events.
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

  function eventKey(ev) {
    return ev.session_id + ':' + ev.line_index;
  }

  function pushSessionEvent(ev) {
    let arr = sessionEvents.get(ev.session_id);
    if (!arr) { arr = []; sessionEvents.set(ev.session_id, arr); }
    arr.push(ev);
  }

  function handleEvent(ev) {
    // Dedupe events that appear in both the connection-time snapshot and the
    // live stream (the server subscribes before snapshotting to avoid races,
    // which can produce a brief overlap).
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
      // Drop oldest 10% to keep memory bounded.
      const drop = Math.floor(EVENT_CAP / 10);
      const dropped = allEvents.splice(0, drop);
      // Reindex remaining events.
      for (let i = 0; i < allEvents.length; i++) allEvents[i]._idx = i;
      // Clear dedupe keys for dropped events so the set doesn't grow unbounded.
      dropped.forEach(d => seenKeys.delete(eventKey(d)));
      // Rebuild the per-session index from the surviving events.
      sessionEvents.clear();
      for (const e of allEvents) pushSessionEvent(e);
      // Clamp / clear the selection so we never click an invalid index.
      if (selectedIdx !== null) {
        selectedIdx -= drop;
        if (selectedIdx < 0) selectedIdx = null;
      }
      // The DOM still references old data-idx values — re-render the feed so
      // every row is in sync with the new indices.
      if (activeTab === 'feed') renderFeed();
    }
    // Update / create session aggregate.
    let s = sessions.get(ev.session_id);
    if (!s) {
      s = {
        id: ev.session_id,
        cwd: ev.cwd, git_branch: ev.git_branch, version: ev.version, model: ev.model,
        first_seen: ev.observed_at, last_seen: ev.observed_at,
        last_entry_timestamp: ev.timestamp,
        event_count: 0, user_count: 0, assistant_count: 0,
        tool_use_count: 0, tool_result_count: 0, system_count: 0,
        input_tokens: 0, output_tokens: 0,
        cache_read_tokens: 0, cache_creation_tokens: 0,
        cost_usd: 0, tool_counts: {}, title: null,
      };
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
    if (activeTab === 'feed') {
      if (matchesFilter(ev)) appendEventToFeed(ev);
    } else if (activeTab === 'conversation' && activeSession === ev.session_id) {
      renderConversation();
    } else if (activeTab === 'metrics') {
      renderMetrics();
    }
  }

  function rerenderAll() {
    updateHeaderStats();
    updateFooterStats();
    renderSessions();
    if (activeTab === 'feed') renderFeed();
    else if (activeTab === 'conversation') renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  }

  // ---- Header / footer aggregates ----------------------------------------
  function aggregate() {
    const totals = {
      events: 0, user: 0, asst: 0, tools: 0,
      tokIn: 0, tokOut: 0, cacheR: 0, cacheW: 0, cost: 0,
    };
    for (const s of sessions.values()) {
      totals.events += s.event_count;
      totals.user += s.user_count;
      totals.asst += s.assistant_count;
      totals.tools += s.tool_use_count;
      totals.tokIn += s.input_tokens;
      totals.tokOut += s.output_tokens;
      totals.cacheR += s.cache_read_tokens;
      totals.cacheW += s.cache_creation_tokens;
      totals.cost += s.cost_usd;
    }
    return totals;
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

  // ---- Sessions sidebar ---------------------------------------------------
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
    const arr = Array.from(sessions.values())
      .filter(s => {
        if (!q) return true;
        return (s.id || '').toLowerCase().includes(q) ||
          (s.cwd || '').toLowerCase().includes(q) ||
          (s.title || '').toLowerCase().includes(q) ||
          (s.git_branch || '').toLowerCase().includes(q);
      })
      .sort((a, b) => (b.last_seen || '').localeCompare(a.last_seen || ''));

    $('#session-count').textContent = arr.length;

    if (!arr.length) {
      list.innerHTML = '<div style="padding:16px 12px;font-size:12px;color:var(--text-muted)">No sessions ' + (q ? 'match.' : 'yet.') + '</div>';
      return;
    }

    // Group by cwd (project).
    const groups = new Map();
    for (const s of arr) {
      const key = s.cwd || '__unknown__';
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(s);
    }

    let html = '';
    for (const [cwd, list] of groups) {
      const pname = cwd === '__unknown__' ? '(unknown project)' : projectName(cwd);
      html += `<div class="project-group">
        <div class="project-header" title="${escHtml(cwd === '__unknown__' ? '' : cwd)}">
          <span>📁</span><span class="pname">${escHtml(pname)}</span><span class="count">${list.length}</span>
        </div>`;
      for (const s of list) {
        const active = s.id === activeSession ? ' active' : '';
        const live = isLive(s.last_seen) ? ' live' : '';
        const title = s.title ? s.title : (s.event_count + ' events · ' + howLongAgo(s.last_seen));
        const buckets = eventBucketsFor(s.id, 24);
        const sp = sparkPath(buckets, 240, 18);
        const branchPart = s.git_branch ? `<span class="branch">⎇ ${escHtml(s.git_branch)}</span>` : '';
        html += `<div class="session-item${active}${live}" data-id="${escHtml(s.id)}">
          <div class="row1">
            <span class="live"></span>
            <span class="sid" title="${escHtml(s.id)}">${escHtml(shortId(s.id))}</span>
            <span style="font-size:10px;color:var(--text-dim)">${escHtml(howLongAgo(s.last_seen))}</span>
          </div>
          ${s.title ? `<div class="title" title="${escHtml(s.title)}">${escHtml(s.title)}</div>` : ''}
          <div class="meta">
            <span>${s.event_count} ev</span>
            <span class="cost">${fmtCost(s.cost_usd)}</span>
            ${branchPart}
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
      el.addEventListener('click', () => {
        const id = el.dataset.id;
        activeSession = (activeSession === id) ? null : id;
        $('#clear-session-filter').style.display = activeSession ? 'inline-block' : 'none';
        $('#clear-session-filter').textContent = activeSession ? `✕ ${shortId(activeSession)}` : '';
        renderSessions();
        if (activeTab === 'feed') renderFeed();
        else if (activeTab === 'conversation') renderConversation();
        else if (activeTab === 'metrics') renderMetrics();
      });
    });
  }

  // ---- Feed ---------------------------------------------------------------
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
        $('#clear-session-filter').style.display = activeSession ? 'inline-block' : 'none';
        $('#clear-session-filter').textContent = activeSession ? `✕ ${shortId(activeSession)}` : '';
        renderSessions();
        renderFeed();
      });
    });
  }

  function appendEventToFeed(ev) {
    const feed = $('#feed');
    const placeholder = feed.querySelector('#no-events');
    if (placeholder) {
      feed.innerHTML = '';
    }
    const atBottom = feed.scrollHeight - feed.scrollTop <= feed.clientHeight + 40;
    feed.insertAdjacentHTML('beforeend', eventRowHtml(ev));
    // Re-attach handler only for the new row.
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
        $('#clear-session-filter').style.display = activeSession ? 'inline-block' : 'none';
        $('#clear-session-filter').textContent = activeSession ? `✕ ${shortId(activeSession)}` : '';
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

  // ---- Conversation -------------------------------------------------------
  function renderConversation() {
    const root = $('#conversation');
    if (!activeSession) {
      root.innerHTML = '<div id="conversation-empty">Select a session to view the conversation.<div class="hint">Conversation view renders user / assistant / tool messages threaded chronologically.</div></div>';
      return;
    }
    const evs = sessionEvents.get(activeSession) || [];
    if (!evs.length) {
      root.innerHTML = '<div id="conversation-empty">No events yet for this session.</div>';
      return;
    }
    const html = evs.map(renderConvMessage).filter(Boolean).join('');
    root.innerHTML = html || '<div id="conversation-empty">Nothing to render for this session.</div>';
  }

  function renderConvMessage(ev) {
    const t = ev.event_type;
    if (t === 'user') {
      const content = ev.entry && (ev.entry.message?.content ?? ev.entry.content);
      const parts = renderContentParts(content);
      // If only tool_results, render as tool-result card.
      if (parts.onlyToolResult) {
        return renderToolResultCard(ev, content);
      }
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
      return `<div class="conv-msg assistant">
        <div class="ch"><span class="role">Assistant</span>${ev.model ? `<span class="meta">${escHtml(ev.model)}</span>` : ''}<span class="meta">${escHtml(ev.timestamp || ev.observed_at)}${tok}${cost}</span></div>
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
    return ''; // skip ai-title, queue-operation, attachment, etc.
  }

  function renderContentParts(content) {
    if (content == null) return { html: '', onlyToolResult: false };
    if (typeof content === 'string') {
      return { html: `<div class="text">${escHtml(content)}</div>`, onlyToolResult: false };
    }
    if (!Array.isArray(content)) {
      return { html: `<pre style="white-space:pre-wrap">${escHtml(JSON.stringify(content, null, 2))}</pre>`, onlyToolResult: false };
    }
    let html = '';
    let onlyToolResult = true;
    for (const block of content) {
      const bt = block?.type;
      if (bt !== 'tool_result') onlyToolResult = false;
      switch (bt) {
        case 'text':
          html += `<div class="text">${escHtml(block.text || '')}</div>`;
          break;
        case 'thinking':
          html += `<div class="thinking">${escHtml(block.thinking || block.text || '')}</div>`;
          break;
        case 'tool_use': {
          const input = block.input ? JSON.stringify(block.input, null, 2) : '';
          html += `<div class="tool">
            <div class="head"><span>🔧</span><span class="name">${escHtml(block.name || '?')}</span><span class="id">${escHtml(block.id || '')}</span></div>
            ${input ? `<pre>${escHtml(input)}</pre>` : ''}
          </div>`;
          break;
        }
        case 'tool_result': {
          const body = typeof block.content === 'string'
            ? block.content
            : JSON.stringify(block.content, null, 2);
          html += `<div class="tool">
            <div class="head"><span>📦</span><span class="name">tool_result</span><span class="id">${escHtml(block.tool_use_id || '')}</span></div>
            <pre>${escHtml(body || '')}</pre>
          </div>`;
          break;
        }
        case 'image':
          html += `<div style="color:var(--text-dim);font-size:11px">[image]</div>`;
          break;
        default:
          html += `<pre style="color:var(--text-dim);font-size:11px;white-space:pre-wrap">${escHtml(JSON.stringify(block, null, 2))}</pre>`;
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

  // ---- Metrics ------------------------------------------------------------
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
      acc.events += s.event_count;
      acc.user += s.user_count;
      acc.asst += s.assistant_count;
      acc.tools += s.tool_use_count;
      acc.toolResults += s.tool_result_count;
      acc.tokIn += s.input_tokens;
      acc.tokOut += s.output_tokens;
      acc.cacheR += s.cache_read_tokens;
      acc.cacheW += s.cache_creation_tokens;
      acc.cost += s.cost_usd;
      return acc;
    }, { events:0,user:0,asst:0,tools:0,toolResults:0,tokIn:0,tokOut:0,cacheR:0,cacheW:0,cost:0 });

    // Cache hit rate.
    const cachedIn = totals.cacheR;
    const allInRead = totals.tokIn + totals.cacheR + totals.cacheW;
    const cacheHit = allInRead > 0 ? ((totals.cacheR / allInRead) * 100).toFixed(1) : '0.0';

    // Tool counts aggregate.
    const toolCounts = {};
    sessList.forEach(s => {
      for (const [k, v] of Object.entries(s.tool_counts || {})) {
        toolCounts[k] = (toolCounts[k] || 0) + v;
      }
    });
    const toolEntries = Object.entries(toolCounts).sort((a,b) => b[1]-a[1]).slice(0, 15);
    const maxTool = toolEntries.length ? toolEntries[0][1] : 1;

    // Cost by session.
    const sessCost = sessList
      .map(s => ({ id: s.id, title: s.title, cost: s.cost_usd, events: s.event_count }))
      .sort((a,b) => b.cost - a.cost)
      .slice(0, 10);
    const maxCost = sessCost.length ? Math.max(...sessCost.map(s => s.cost), 0.0001) : 0.0001;

    // Events timeline (60-minute window, 1-minute buckets).
    const now = Date.now();
    const windowMs = 60 * 60 * 1000;
    const buckets = new Array(60).fill(0);
    const events = activeSession
      ? (sessionEvents.get(activeSession) || [])
      : allEvents;
    events.forEach(e => {
      const t = new Date(e.observed_at).getTime();
      const ageMin = (now - t) / 60_000;
      if (ageMin >= 0 && ageMin < 60) {
        buckets[59 - Math.floor(ageMin)]++;
      }
    });
    const totalInWindow = buckets.reduce((a,b)=>a+b, 0);

    root.innerHTML = `
      <div class="metric-grid">
        <div class="metric-card"><div class="label">Sessions</div><div class="value accent">${sessList.length}</div><div class="sub">${activeSession ? 'filtered to active session' : 'across all observed sessions'}</div></div>
        <div class="metric-card"><div class="label">Events</div><div class="value">${fmtNum(totals.events)}</div><div class="sub">${fmtNum(totals.user)} user · ${fmtNum(totals.asst)} assistant · ${fmtNum(totals.tools)} tool calls</div></div>
        <div class="metric-card"><div class="label">Est. cost</div><div class="value orange">${fmtCost(totals.cost)}</div><div class="sub">approximate — based on public list pricing</div></div>
        <div class="metric-card"><div class="label">Tokens out</div><div class="value green">${fmtNum(totals.tokOut)}</div><div class="sub">${fmtNum(totals.tokIn)} input · ${fmtNum(totals.cacheR + totals.cacheW)} cache</div></div>
        <div class="metric-card"><div class="label">Cache hit rate</div><div class="value purple">${cacheHit}%</div><div class="sub">${fmtNum(totals.cacheR)} cache-read of ${fmtNum(allInRead)} input</div></div>
        <div class="metric-card"><div class="label">Events / min (60m)</div><div class="value">${fmtNum(totalInWindow)}</div><div class="sub">over the last hour</div></div>
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

    // 5 y-ticks
    const ticks = 4;
    let gridY = '';
    let yLabels = '';
    for (let i = 0; i <= ticks; i++) {
      const y = padT + (innerH * i / ticks);
      gridY += `<line x1="${padL}" y1="${y}" x2="${padL+innerW}" y2="${y}"/>`;
      const val = Math.round(max * (1 - i / ticks));
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

  // ---- Wiring -------------------------------------------------------------
  $$('.tab').forEach(t => t.addEventListener('click', () => {
    $$('.tab').forEach(x => x.classList.remove('active'));
    $$('.tab-body').forEach(x => x.classList.remove('active'));
    t.classList.add('active');
    activeTab = t.dataset.tab;
    document.querySelector(`.tab-body[data-tab-body="${activeTab}"]`).classList.add('active');
    if (activeTab === 'feed') renderFeed();
    else if (activeTab === 'conversation') renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  }));

  $$('.type-btn').forEach(btn => btn.addEventListener('click', () => {
    activeType = btn.dataset.type;
    $$('.type-btn').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    renderFeed();
  }));

  $('#search').addEventListener('input', e => {
    searchQuery = e.target.value;
    renderFeed();
  });

  $('#session-search').addEventListener('input', e => {
    sessionFilterQuery = e.target.value;
    renderSessions();
  });

  $('#clear-session-filter').addEventListener('click', () => {
    activeSession = null;
    $('#clear-session-filter').style.display = 'none';
    renderSessions();
    if (activeTab === 'feed') renderFeed();
    else if (activeTab === 'conversation') renderConversation();
    else if (activeTab === 'metrics') renderMetrics();
  });

  $('#pause-btn').addEventListener('click', () => {
    paused = !paused;
    if (!paused) {
      const buf = pendingBuffer;
      pendingBuffer = [];
      buf.forEach(ev => { ingestEvent(ev); });
      rerenderAll();
      $('#pause-btn').textContent = '⏸ Pause';
      $('#pause-btn').classList.remove('active');
    } else {
      $('#pause-btn').textContent = '▶ Resume (0)';
      $('#pause-btn').classList.add('active');
    }
  });

  $('#copy-json').addEventListener('click', () => {
    if (selectedIdx !== null && allEvents[selectedIdx]) {
      copy(JSON.stringify(allEvents[selectedIdx].entry, null, 2));
      $('#copy-json').textContent = '✓ Copied';
      setTimeout(() => { $('#copy-json').textContent = 'Copy JSON'; }, 1200);
    }
  });

  // Periodic refresh of sidebar so "live" dots and "Xs ago" stay current.
  setInterval(() => {
    if (sessions.size) renderSessions();
  }, 5000);

  connect();
})();
</script>
</body>
</html>"##;
