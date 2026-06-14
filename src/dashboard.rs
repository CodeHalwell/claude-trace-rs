/// Returns the built-in dashboard HTML page.
///
/// The page is a single self-contained document (no external assets) so it can
/// be served straight from the binary. `__PORT__` is left for compatibility but
/// the client derives its WebSocket URL from `location.host`, so the dashboard
/// works behind any port or host mapping.
pub fn dashboard_html(port: u16) -> String {
    DASHBOARD_HTML.replace("__PORT__", &port.to_string())
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en" data-theme="dark">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Claude Trace</title>
<style>
  :root[data-theme="dark"]{
    --bg:#0b0e14; --surface:#11151c; --surface-2:#161b24; --surface-3:#1d2430;
    --border:#232b38; --border-soft:#1a212c;
    --text:#e6edf3; --muted:#9aa7b8; --dim:#67768c;
    --accent:#6ea8fe; --accent-soft:#16243d; --accent-strong:#3b82f6;
    --green:#3fb950; --green-soft:#0f2a16;
    --amber:#e3b341; --amber-soft:#2c2410;
    --red:#f85149; --purple:#bc8cff; --pink:#f778ba; --cyan:#56d4dd;
    --shadow:0 10px 30px rgba(0,0,0,.45);
  }
  :root[data-theme="light"]{
    --bg:#f6f8fb; --surface:#ffffff; --surface-2:#f2f5f9; --surface-3:#e9eef5;
    --border:#dde3ec; --border-soft:#e8edf3;
    --text:#10151c; --muted:#5a6675; --dim:#8a96a6;
    --accent:#2563eb; --accent-soft:#e6efff; --accent-strong:#1d4ed8;
    --green:#15803d; --green-soft:#dcfce7;
    --amber:#b45309; --amber-soft:#fef3c7;
    --red:#dc2626; --purple:#7c3aed; --pink:#db2777; --cyan:#0e7490;
    --shadow:0 12px 32px rgba(20,40,80,.12);
  }
  *{box-sizing:border-box}
  html,body{height:100%;margin:0}
  body{
    font-family:ui-sans-serif,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;
    background:var(--bg); color:var(--text); font-size:13.5px; line-height:1.5;
    -webkit-font-smoothing:antialiased;
  }
  ::-webkit-scrollbar{width:10px;height:10px}
  ::-webkit-scrollbar-thumb{background:var(--surface-3);border-radius:8px}
  ::-webkit-scrollbar-thumb:hover{background:var(--border)}
  ::-webkit-scrollbar-track{background:transparent}
  a{color:var(--accent);text-decoration:none}
  button{font-family:inherit;cursor:pointer}
  code,pre,.mono{font-family:ui-monospace,SFMono-Regular,"SF Mono",Menlo,Consolas,monospace}

  /* ---------- App shell ---------- */
  .app{display:grid;grid-template-rows:auto 1fr;height:100vh}
  .topbar{
    display:flex;align-items:center;gap:14px;padding:10px 16px;
    background:var(--surface);border-bottom:1px solid var(--border);
  }
  .brand{display:flex;align-items:center;gap:10px;font-weight:700;letter-spacing:.2px}
  .brand .logo{
    width:26px;height:26px;border-radius:8px;display:grid;place-items:center;
    background:linear-gradient(135deg,var(--accent-strong),var(--purple));color:#fff;font-size:15px;
  }
  .brand small{font-weight:500;color:var(--dim)}
  .topbar .search{flex:1;max-width:520px;position:relative}
  .topbar .search input{
    width:100%;padding:8px 12px 8px 32px;border-radius:9px;border:1px solid var(--border);
    background:var(--surface-2);color:var(--text);outline:none;font-size:13px;
  }
  .topbar .search input:focus{border-color:var(--accent);background:var(--surface)}
  .topbar .search .icon{position:absolute;left:10px;top:50%;transform:translateY(-50%);color:var(--dim)}
  .spacer{flex:1}
  .status{display:flex;align-items:center;gap:7px;font-size:12px;color:var(--muted);
    padding:5px 10px;border:1px solid var(--border);border-radius:20px;background:var(--surface-2)}
  .dot{width:8px;height:8px;border-radius:50%;background:var(--dim)}
  .dot.on{background:var(--green);box-shadow:0 0 0 3px var(--green-soft)}
  .dot.off{background:var(--red)}
  .btn{
    display:inline-flex;align-items:center;gap:7px;padding:7px 12px;border-radius:9px;
    border:1px solid var(--border);background:var(--surface-2);color:var(--text);font-size:12.5px;font-weight:600;
  }
  .btn:hover{border-color:var(--accent);color:var(--accent)}
  .btn.primary{background:var(--accent-strong);border-color:var(--accent-strong);color:#fff}
  .btn.primary:hover{filter:brightness(1.08);color:#fff}
  .icon-btn{width:34px;height:34px;display:grid;place-items:center;border-radius:9px;
    border:1px solid var(--border);background:var(--surface-2);color:var(--muted);font-size:15px}
  .icon-btn:hover{color:var(--accent);border-color:var(--accent)}

  .body{display:grid;grid-template-columns:300px 1fr;min-height:0}
  .body.collapsed{grid-template-columns:0 1fr}

  /* ---------- Sidebar ---------- */
  .sidebar{
    background:var(--surface);border-right:1px solid var(--border);
    display:flex;flex-direction:column;min-height:0;overflow:hidden;
  }
  .sidebar .controls{padding:12px;border-bottom:1px solid var(--border);display:flex;flex-direction:column;gap:9px}
  .field{position:relative}
  .field input,.field select{
    width:100%;padding:7px 10px;border-radius:8px;border:1px solid var(--border);
    background:var(--surface-2);color:var(--text);font-size:12.5px;outline:none;
  }
  .field input:focus,.field select:focus{border-color:var(--accent)}
  .row{display:flex;gap:8px;align-items:center}
  .toggle{display:flex;align-items:center;gap:6px;font-size:12px;color:var(--muted);cursor:pointer;user-select:none}
  .toggle input{accent-color:var(--accent-strong)}
  .sidebar .list{flex:1;overflow-y:auto;padding:8px}
  .group-label{display:flex;align-items:center;gap:6px;padding:8px 8px 4px;font-size:11px;font-weight:700;
    text-transform:uppercase;letter-spacing:.6px;color:var(--dim);cursor:pointer}
  .group-label .count{margin-left:auto;font-weight:600;color:var(--dim)}
  .session{
    display:block;padding:9px 10px;border-radius:10px;margin-bottom:3px;cursor:pointer;border:1px solid transparent;
  }
  .session:hover{background:var(--surface-2)}
  .session.active{background:var(--accent-soft);border-color:var(--accent)}
  .session .line1{display:flex;align-items:center;gap:7px}
  .session .title{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;flex:1}
  .session .star{color:var(--dim);font-size:13px}
  .session .star.on{color:var(--amber)}
  .session .line2{display:flex;align-items:center;gap:8px;margin-top:3px;font-size:11px;color:var(--muted)}
  .session .pill{padding:1px 6px;border-radius:6px;background:var(--surface-3);color:var(--muted);font-size:10.5px}
  .session .meta-right{margin-left:auto;display:flex;align-items:center;gap:6px}
  .empty{padding:30px 16px;text-align:center;color:var(--dim);font-size:12.5px}

  /* ---------- Main ---------- */
  .main{display:flex;flex-direction:column;min-width:0;min-height:0;background:var(--bg)}
  .session-head{padding:12px 18px;border-bottom:1px solid var(--border);background:var(--surface)}
  .session-head .h1{display:flex;align-items:center;gap:10px}
  .session-head h2{margin:0;font-size:15px;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .session-head .sub{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px;font-size:11.5px;color:var(--muted)}
  .tag-chip{display:inline-flex;align-items:center;gap:4px;padding:2px 8px;border-radius:12px;
    background:var(--surface-3);color:var(--muted);font-size:11px}
  .tag-chip .x{cursor:pointer;color:var(--dim)}
  .tag-chip .x:hover{color:var(--red)}
  .stat-strip{display:flex;flex-wrap:wrap;gap:18px;margin-top:10px}
  .stat{display:flex;flex-direction:column}
  .stat .v{font-size:15px;font-weight:700}
  .stat .k{font-size:10.5px;text-transform:uppercase;letter-spacing:.5px;color:var(--dim)}

  .tabs{display:flex;gap:2px;padding:0 14px;border-bottom:1px solid var(--border);background:var(--surface)}
  .tab{padding:11px 14px;font-size:13px;font-weight:600;color:var(--muted);border-bottom:2px solid transparent}
  .tab:hover{color:var(--text)}
  .tab.active{color:var(--accent);border-bottom-color:var(--accent)}
  .tab .badge{margin-left:6px;font-size:10px;padding:1px 6px;border-radius:10px;background:var(--surface-3);color:var(--muted)}

  .panel{flex:1;overflow-y:auto;min-height:0;padding:14px 16px}
  .panel-toolbar{display:flex;align-items:center;gap:9px;margin-bottom:12px;flex-wrap:wrap}
  .panel-toolbar select,.panel-toolbar input{
    padding:6px 9px;border-radius:8px;border:1px solid var(--border);background:var(--surface);color:var(--text);font-size:12px;outline:none}
  .panel-toolbar .grow{flex:1;min-width:120px}

  /* ---------- Event feed ---------- */
  .event{
    display:grid;grid-template-columns:64px 84px 1fr auto;gap:10px;align-items:center;
    padding:8px 10px;border:1px solid var(--border-soft);border-radius:9px;margin-bottom:5px;
    background:var(--surface);cursor:pointer;
  }
  .event:hover{border-color:var(--accent)}
  .event .time{font-size:11px;color:var(--dim)}
  .badge-type{font-size:10.5px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;
    padding:2px 7px;border-radius:6px;text-align:center}
  .t-user{background:var(--accent-soft);color:var(--accent)}
  .t-assistant{background:var(--green-soft);color:var(--green)}
  .t-tool_use,.t-tool_result{background:var(--amber-soft);color:var(--amber)}
  .t-system,.t-summary{background:var(--surface-3);color:var(--muted)}
  .event .summary{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .event .nums{font-size:11px;color:var(--dim);text-align:right;white-space:nowrap}
  .sess-ref{font-size:10.5px;color:var(--accent);background:var(--accent-soft);padding:1px 6px;border-radius:6px}

  /* ---------- Conversation ---------- */
  .msg{margin-bottom:12px;max-width:920px}
  .msg .who{display:flex;align-items:center;gap:8px;margin-bottom:5px;font-size:11.5px;font-weight:700;color:var(--muted)}
  .msg .who .role{padding:2px 8px;border-radius:6px}
  .msg .bubble{border:1px solid var(--border);border-radius:12px;padding:11px 13px;background:var(--surface)}
  .msg.user .bubble{background:var(--accent-soft);border-color:transparent}
  .msg .bubble pre{white-space:pre-wrap;word-break:break-word;margin:0;font-size:12.5px;line-height:1.55}
  .think{border-left:2px solid var(--purple);padding:4px 0 4px 10px;margin:6px 0;color:var(--muted);font-style:italic;font-size:12px}
  .toolcard{border:1px dashed var(--border);border-radius:9px;padding:8px 10px;margin-top:8px;background:var(--surface-2)}
  .toolcard .h{font-size:11.5px;font-weight:700;color:var(--amber);margin-bottom:5px}
  .toolcard pre{white-space:pre-wrap;word-break:break-word;margin:0;font-size:11.5px;color:var(--muted);max-height:240px;overflow:auto}
  .lat{font-size:10.5px;color:var(--dim);font-weight:600}
  .load-more{margin:8px auto;display:block}

  /* ---------- Analytics ---------- */
  .cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:12px;margin-bottom:18px}
  .card{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:14px}
  .card .k{font-size:11px;text-transform:uppercase;letter-spacing:.5px;color:var(--dim)}
  .card .v{font-size:24px;font-weight:800;margin-top:4px}
  .card .sub{font-size:11px;color:var(--muted);margin-top:3px}
  .grid2{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:14px}
  .block{background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:14px}
  .block h3{margin:0 0 12px;font-size:13px}
  .bar-row{display:grid;grid-template-columns:130px 1fr 60px;gap:10px;align-items:center;margin-bottom:7px;font-size:12px}
  .bar-row .name{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;color:var(--muted)}
  .bar{height:9px;border-radius:6px;background:linear-gradient(90deg,var(--accent-strong),var(--purple))}
  .bar-row .val{text-align:right;color:var(--muted);font-variant-numeric:tabular-nums}
  .timeline{display:flex;align-items:flex-end;gap:3px;height:120px;padding-top:6px}
  .tl-bar{flex:1;background:var(--accent-strong);border-radius:3px 3px 0 0;min-height:2px;opacity:.85}
  .tl-bar:hover{opacity:1}
  .tl-labels{display:flex;justify-content:space-between;font-size:10px;color:var(--dim);margin-top:5px}

  /* ---------- Drawer & modal ---------- */
  .scrim{position:fixed;inset:0;background:rgba(0,0,0,.5);opacity:0;pointer-events:none;transition:opacity .15s;z-index:40}
  .scrim.open{opacity:1;pointer-events:auto}
  .drawer{position:fixed;top:0;right:0;height:100%;width:min(560px,92vw);background:var(--surface);
    border-left:1px solid var(--border);box-shadow:var(--shadow);transform:translateX(100%);
    transition:transform .2s;z-index:50;display:flex;flex-direction:column}
  .drawer.open{transform:none}
  .drawer .dh{display:flex;align-items:center;gap:10px;padding:14px 16px;border-bottom:1px solid var(--border)}
  .drawer .dh h3{margin:0;font-size:14px;flex:1}
  .drawer .db{flex:1;overflow:auto;padding:14px 16px}
  .drawer pre{white-space:pre-wrap;word-break:break-word;font-size:12px;margin:0}
  .modal{position:fixed;top:50%;left:50%;transform:translate(-50%,-46%) scale(.98);opacity:0;pointer-events:none;
    width:min(560px,94vw);background:var(--surface);border:1px solid var(--border);border-radius:14px;
    box-shadow:var(--shadow);z-index:60;transition:opacity .15s,transform .15s}
  .modal.open{opacity:1;pointer-events:auto;transform:translate(-50%,-50%) scale(1)}
  .modal .mh{padding:14px 16px;border-bottom:1px solid var(--border);font-weight:700}
  .modal .mb{padding:16px}
  .modal .mf{padding:12px 16px;border-top:1px solid var(--border);display:flex;justify-content:flex-end;gap:8px}
  .seg{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px}
  .seg button{padding:7px 11px;border-radius:8px;border:1px solid var(--border);background:var(--surface-2);color:var(--muted);font-size:12px}
  .seg button.sel{background:var(--accent-soft);border-color:var(--accent);color:var(--accent)}
  .lbl{font-size:11px;text-transform:uppercase;letter-spacing:.5px;color:var(--dim);margin-bottom:4px;display:block}

  /* ---------- Search results ---------- */
  .search-pop{position:absolute;top:44px;left:0;width:min(560px,90vw);max-height:60vh;overflow:auto;
    background:var(--surface);border:1px solid var(--border);border-radius:12px;box-shadow:var(--shadow);
    z-index:30;display:none}
  .search-pop.open{display:block}
  .sr{padding:9px 12px;border-bottom:1px solid var(--border-soft);cursor:pointer}
  .sr:hover{background:var(--surface-2)}
  .sr .t{font-size:11px;color:var(--dim);display:flex;gap:8px}
  .sr .s{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}

  .toast{position:fixed;bottom:20px;left:50%;transform:translateX(-50%) translateY(20px);opacity:0;
    background:var(--surface-3);color:var(--text);padding:9px 16px;border-radius:10px;border:1px solid var(--border);
    box-shadow:var(--shadow);z-index:80;transition:.2s;font-size:12.5px}
  .toast.show{opacity:1;transform:translateX(-50%) translateY(0)}
  .notes-area{width:100%;min-height:54px;resize:vertical;padding:8px;border-radius:8px;border:1px solid var(--border);
    background:var(--surface-2);color:var(--text);font-size:12px;outline:none;margin-top:8px}
  .notes-area:focus{border-color:var(--accent)}
  .hint{color:var(--dim);font-size:11px}
  .kbd{font-family:ui-monospace,monospace;font-size:10.5px;border:1px solid var(--border);border-bottom-width:2px;
    border-radius:5px;padding:0 5px;color:var(--muted);background:var(--surface-2)}
</style>
</head>
<body>
<div class="app">
  <!-- Top bar -->
  <div class="topbar">
    <div class="brand"><span class="logo">◆</span><span>Claude&nbsp;Trace</span></div>
    <button class="icon-btn" id="sidebarToggle" title="Toggle sidebar (Ctrl/⌘B)">☰</button>
    <div class="search">
      <span class="icon">⌕</span>
      <input id="globalSearch" type="search" placeholder="Search all traces…  ( / )" autocomplete="off">
      <div class="search-pop" id="searchPop"></div>
    </div>
    <div class="spacer"></div>
    <div class="status"><span class="dot" id="connDot"></span><span id="connText">connecting…</span></div>
    <button class="btn" id="exportBtn">⤓ Export</button>
    <button class="icon-btn" id="themeBtn" title="Toggle theme">◐</button>
    <button class="icon-btn" id="helpBtn" title="Keyboard shortcuts (?)">?</button>
  </div>

  <div class="body" id="body">
    <!-- Sidebar -->
    <aside class="sidebar">
      <div class="controls">
        <div class="field"><input id="sessSearch" type="search" placeholder="Filter sessions…"></div>
        <div class="row">
          <select id="sortSel" class="field" style="flex:1">
            <option value="last_seen">Recent activity</option>
            <option value="first_seen">Newest</option>
            <option value="events">Most events</option>
            <option value="cost">Highest cost</option>
          </select>
          <label class="toggle"><input type="checkbox" id="bmOnly">★ only</label>
        </div>
      </div>
      <div class="list" id="sessionList"><div class="empty">Loading sessions…</div></div>
    </aside>

    <!-- Main -->
    <main class="main">
      <div class="session-head" id="sessionHead" style="display:none"></div>
      <div class="tabs">
        <div class="tab active" data-tab="live">Live feed</div>
        <div class="tab" data-tab="conversation">Conversation</div>
        <div class="tab" data-tab="analytics">Analytics</div>
      </div>

      <!-- Live -->
      <section class="panel" id="panel-live">
        <div class="panel-toolbar">
          <select id="liveType">
            <option value="all">All types</option>
            <option value="user">User</option>
            <option value="assistant">Assistant</option>
            <option value="tool_use">Tool use</option>
            <option value="tool_result">Tool result</option>
            <option value="system">System</option>
          </select>
          <input id="liveSearch" class="grow" placeholder="Filter feed text…">
          <button class="btn" id="pauseBtn">⏸ Pause</button>
          <button class="btn" id="clearBtn">Clear</button>
          <span class="hint" id="feedCount"></span>
        </div>
        <div id="feed"></div>
      </section>

      <!-- Conversation -->
      <section class="panel" id="panel-conversation" style="display:none">
        <div class="panel-toolbar">
          <select id="convType">
            <option value="all">Whole transcript</option>
            <option value="user">User only</option>
            <option value="assistant">Assistant only</option>
          </select>
          <input id="convSearch" class="grow" placeholder="Search within session…">
          <span class="hint" id="convCount"></span>
        </div>
        <div id="conversation"><div class="empty">Select a session to view its conversation.</div></div>
      </section>

      <!-- Analytics -->
      <section class="panel" id="panel-analytics" style="display:none">
        <div id="analytics"><div class="empty">Loading analytics…</div></div>
      </section>
    </main>
  </div>
</div>

<!-- Detail drawer -->
<div class="scrim" id="scrim"></div>
<div class="drawer" id="drawer">
  <div class="dh"><h3 id="drawerTitle">Event</h3>
    <button class="btn" id="copyJson">Copy JSON</button>
    <button class="icon-btn" id="drawerClose">✕</button>
  </div>
  <div class="db"><pre id="drawerBody" class="mono"></pre></div>
</div>

<!-- Export modal -->
<div class="modal" id="exportModal">
  <div class="mh">Export traces</div>
  <div class="mb">
    <span class="lbl">Scope</span>
    <div class="seg" id="exportScope">
      <button data-scope="all" class="sel">All sessions</button>
      <button data-scope="session">Selected session</button>
    </div>
    <span class="lbl" style="margin-top:14px">Format</span>
    <div class="seg" id="exportFmt">
      <button data-fmt="messages" class="sel">Anthropic</button>
      <button data-fmt="openai">OpenAI</button>
      <button data-fmt="sharegpt">ShareGPT</button>
      <button data-fmt="jsonl">Raw JSONL</button>
      <button data-fmt="markdown">Markdown</button>
      <button data-fmt="huggingface">HuggingFace</button>
    </div>
    <p class="hint" id="exportHint" style="margin-top:14px"></p>
  </div>
  <div class="mf">
    <button class="btn" id="exportCancel">Cancel</button>
    <button class="btn primary" id="exportGo">⤓ Download</button>
  </div>
</div>

<!-- Help modal -->
<div class="modal" id="helpModal">
  <div class="mh">Keyboard shortcuts</div>
  <div class="mb" style="display:grid;grid-template-columns:auto 1fr;gap:8px 16px;font-size:12.5px">
    <span class="kbd">/</span><span>Focus global search</span>
    <span class="kbd">1</span><span class="kbd">2</span>
    <span></span><span></span>
  </div>
  <div class="mb" style="margin-top:-12px;display:grid;grid-template-columns:90px 1fr;gap:8px 12px;font-size:12.5px">
    <span class="kbd">1 / 2 / 3</span><span>Live / Conversation / Analytics</span>
    <span class="kbd">Ctrl/⌘ B</span><span>Toggle sidebar</span>
    <span class="kbd">Space</span><span>Pause / resume the live feed</span>
    <span class="kbd">e</span><span>Open export</span>
    <span class="kbd">t</span><span>Toggle theme</span>
    <span class="kbd">Esc</span><span>Close panels / clear search</span>
  </div>
  <div class="mf"><button class="btn primary" id="helpClose">Got it</button></div>
</div>

<div class="toast" id="toast"></div>

<script>
"use strict";
const WS_URL = (location.protocol === 'https:' ? 'wss' : 'ws') + '://' + location.host + '/ws';
const $ = (s, r=document) => r.querySelector(s);
const $$ = (s, r=document) => Array.from(r.querySelectorAll(s));

// ---------- State ----------
const state = {
  sessions: [],            // sidebar sessions (from DB)
  selected: null,          // selected session id
  selectedMeta: {bookmarked:false, tags:[], notes:''},
  tab: 'live',
  feed: [],                // live events (capped)
  feedSeen: new Set(),
  paused: false,
  connected: false,
  conv: {events:[], total:0, offset:0, loading:false},
};
const FEED_CAP = 600;

// ---------- Helpers ----------
const fmtNum = n => (n||0).toLocaleString();
const fmtCost = c => '$' + (c||0).toFixed(c>=1?2:4);
function fmtTokens(n){ n=n||0; if(n>=1e6) return (n/1e6).toFixed(2)+'M'; if(n>=1e3) return (n/1e3).toFixed(1)+'k'; return ''+n; }
function escHtml(s){ return String(s??'').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
function shortId(id){ return id ? id.slice(0,8) : '—'; }
function projectName(cwd){ if(!cwd) return 'No project'; const p=cwd.replace(/\/+$/,'').split('/'); return p[p.length-1]||cwd; }
function relTime(iso){
  if(!iso) return '';
  const d=Date.parse(iso); if(isNaN(d)) return '';
  const s=Math.floor((Date.now()-d)/1000);
  if(s<5) return 'now'; if(s<60) return s+'s'; if(s<3600) return Math.floor(s/60)+'m';
  if(s<86400) return Math.floor(s/3600)+'h'; return Math.floor(s/86400)+'d';
}
function isLive(iso){ if(!iso) return false; const d=Date.parse(iso); return !isNaN(d) && (Date.now()-d) < 60000; }
function timeOf(ev){ const t=ev.timestamp||ev.observed_at; if(!t) return ''; const d=new Date(t); return isNaN(d)?'':d.toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'}); }
function toast(msg){ const t=$('#toast'); t.textContent=msg; t.classList.add('show'); clearTimeout(t._t); t._t=setTimeout(()=>t.classList.remove('show'),2000); }
async function api(path, opts){ const r=await fetch(path, opts); if(!r.ok) throw new Error(r.status+' '+path); return r.json(); }

// ---------- WebSocket (live feed) ----------
let ws, reconnectTimer;
function connect(){
  ws = new WebSocket(WS_URL);
  ws.onopen = () => { state.connected=true; updateConn(); };
  ws.onclose = () => { state.connected=false; updateConn(); clearTimeout(reconnectTimer); reconnectTimer=setTimeout(connect,1500); };
  ws.onerror = () => { try{ws.close();}catch(e){} };
  ws.onmessage = (e) => {
    let msg; try{ msg=JSON.parse(e.data);}catch(_){return;}
    if(msg.type==='connected') return;
    if(msg.type==='snapshot'){ (msg.events||[]).forEach(ev=>ingest(ev,false)); renderFeed(); return; }
    ingest(msg, true); // live single event
  };
}
function updateConn(){
  const dot=$('#connDot'), txt=$('#connText');
  dot.className='dot '+(state.connected?'on':'off');
  txt.textContent = state.connected ? 'live' : 'reconnecting…';
}
let refreshDebounce;
function ingest(ev, live){
  const key = ev.session_id+':'+ev.line_index;
  if(state.feedSeen.has(key)) return;
  state.feedSeen.add(key);
  state.feed.push(ev);
  if(state.feed.length>FEED_CAP){ const drop=state.feed.shift(); state.feedSeen.delete(drop.session_id+':'+drop.line_index); }
  if(live && !state.paused){
    appendFeedRow(ev);
    // Refresh sidebar/analytics lazily as new data lands.
    clearTimeout(refreshDebounce);
    refreshDebounce=setTimeout(()=>{ loadSessions(); if(state.tab==='analytics') loadAnalytics(); }, 1200);
  }
}

// ---------- Sidebar ----------
async function loadSessions(){
  const params=new URLSearchParams();
  const q=$('#sessSearch').value.trim(); if(q) params.set('search', q);
  params.set('sort', $('#sortSel').value);
  if($('#bmOnly').checked) params.set('bookmarked','true');
  try{
    const d=await api('/api/db/sessions?'+params.toString());
    state.sessions=d.sessions||[];
    renderSessions();
  }catch(e){ /* keep prior */ }
}
function renderSessions(){
  const list=$('#sessionList');
  if(!state.sessions.length){ list.innerHTML='<div class="empty">No sessions yet.<br>Start a Claude Code session and it will appear here.</div>'; return; }
  // Group by project.
  const groups=new Map();
  for(const s of state.sessions){ const p=s.cwd||''; if(!groups.has(p)) groups.set(p,[]); groups.get(p).push(s); }
  let html='';
  for(const [cwd, arr] of groups){
    html+=`<div class="group-label">${escHtml(projectName(cwd))}<span class="count">${arr.length}</span></div>`;
    for(const s of arr){
      const live=isLive(s.last_seen);
      const name=s.title || shortId(s.id);
      html+=`<div class="session ${s.id===state.selected?'active':''}" data-id="${escHtml(s.id)}">
        <div class="line1">
          ${live?'<span class="dot on" style="width:7px;height:7px"></span>':''}
          <span class="title">${escHtml(name)}</span>
          <span class="star ${s.bookmarked?'on':''}" data-star="${escHtml(s.id)}">${s.bookmarked?'★':'☆'}</span>
        </div>
        <div class="line2">
          ${s.git_branch?`<span class="pill">⎇ ${escHtml(s.git_branch)}</span>`:''}
          <span>${fmtNum(s.event_count)} ev</span>
          <span class="meta-right">${fmtCost(s.cost_usd)} · ${relTime(s.last_seen)}</span>
        </div>
      </div>`;
    }
  }
  list.innerHTML=html;
  $$('.session', list).forEach(el=> el.addEventListener('click', ()=> selectSession(el.dataset.id)));
  $$('[data-star]', list).forEach(el=> el.addEventListener('click', (e)=>{ e.stopPropagation(); toggleBookmark(el.dataset.star); }));
}

async function toggleBookmark(id){
  const s=state.sessions.find(x=>x.id===id); if(!s) return;
  const meta = (id===state.selected) ? state.selectedMeta : await api('/api/db/sessions/'+id+'/meta');
  meta.bookmarked = !s.bookmarked;
  await api('/api/db/sessions/'+id+'/meta', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(meta)});
  if(id===state.selected){ state.selectedMeta=meta; renderHead(); }
  loadSessions();
}

// ---------- Selection & header ----------
async function selectSession(id){
  state.selected=id;
  renderSessions();
  try{ state.selectedMeta=await api('/api/db/sessions/'+id+'/meta'); }catch(e){ state.selectedMeta={bookmarked:false,tags:[],notes:''}; }
  renderHead();
  if(state.tab==='conversation') loadConversation(true);
  if(state.tab==='live') renderFeed();
}
function renderHead(){
  const head=$('#sessionHead');
  const s=state.sessions.find(x=>x.id===state.selected);
  if(!s){ head.style.display='none'; return; }
  head.style.display='block';
  const tokens=(s.input_tokens||0)+(s.output_tokens||0);
  const cacheHit = (s.cache_read_tokens||0)+(s.input_tokens||0) > 0
    ? Math.round(100*(s.cache_read_tokens||0)/((s.cache_read_tokens||0)+(s.input_tokens||0))) : 0;
  const tags=(state.selectedMeta.tags||[]).map(t=>`<span class="tag-chip">${escHtml(t)}<span class="x" data-rmtag="${escHtml(t)}">✕</span></span>`).join('');
  head.innerHTML=`
    <div class="h1">
      <span class="star ${state.selectedMeta.bookmarked?'on':''}" id="headStar" style="cursor:pointer;font-size:16px;color:${state.selectedMeta.bookmarked?'var(--amber)':'var(--dim)'}">${state.selectedMeta.bookmarked?'★':'☆'}</span>
      <h2>${escHtml(s.title||shortId(s.id))}</h2>
      <button class="btn" id="exportThis">⤓ Export</button>
    </div>
    <div class="sub">
      <span class="mono">${escHtml(s.id)}</span>
      ${s.cwd?`<span>📁 ${escHtml(s.cwd)}</span>`:''}
      ${s.git_branch?`<span>⎇ ${escHtml(s.git_branch)}</span>`:''}
      ${s.model?`<span>🤖 ${escHtml(s.model)}</span>`:''}
    </div>
    <div class="stat-strip">
      <div class="stat"><span class="v">${fmtNum(s.event_count)}</span><span class="k">Events</span></div>
      <div class="stat"><span class="v">${fmtNum(s.user_count)}/${fmtNum(s.assistant_count)}</span><span class="k">User / Asst</span></div>
      <div class="stat"><span class="v">${fmtNum(s.tool_use_count)}</span><span class="k">Tool calls</span></div>
      <div class="stat"><span class="v">${fmtTokens(tokens)}</span><span class="k">Tokens</span></div>
      <div class="stat"><span class="v">${cacheHit}%</span><span class="k">Cache hit</span></div>
      <div class="stat"><span class="v">${fmtCost(s.cost_usd)}</span><span class="k">Est. cost</span></div>
    </div>
    <div class="sub" style="margin-top:10px;align-items:center">
      ${tags}
      <input id="tagInput" placeholder="+ tag" style="width:80px;padding:2px 8px;border-radius:12px;border:1px solid var(--border);background:var(--surface-2);color:var(--text);font-size:11px">
    </div>
    <textarea class="notes-area" id="notesArea" placeholder="Notes for this session (saved automatically)…">${escHtml(state.selectedMeta.notes||'')}</textarea>
  `;
  $('#headStar').addEventListener('click', ()=> toggleBookmark(s.id));
  $('#exportThis').addEventListener('click', ()=> openExport('session'));
  $('#tagInput').addEventListener('keydown', (e)=>{ if(e.key==='Enter'&&e.target.value.trim()){ addTag(e.target.value.trim()); e.target.value=''; }});
  $$('[data-rmtag]').forEach(el=> el.addEventListener('click', ()=> removeTag(el.dataset.rmtag)));
  let nt; $('#notesArea').addEventListener('input', (e)=>{ clearTimeout(nt); nt=setTimeout(()=>saveNotes(e.target.value), 600); });
}
async function saveMeta(){ await api('/api/db/sessions/'+state.selected+'/meta', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(state.selectedMeta)}); }
async function addTag(t){ if(!state.selectedMeta.tags.includes(t)){ state.selectedMeta.tags.push(t); await saveMeta(); renderHead(); } }
async function removeTag(t){ state.selectedMeta.tags=state.selectedMeta.tags.filter(x=>x!==t); await saveMeta(); renderHead(); }
async function saveNotes(v){ state.selectedMeta.notes=v; await saveMeta(); toast('Notes saved'); }

// ---------- Live feed ----------
function feedFilter(ev){
  const t=$('#liveType').value, q=$('#liveSearch').value.trim().toLowerCase();
  if(t!=='all' && ev.event_type!==t) return false;
  if(state.selected && ev.session_id!==state.selected && $('#scopeFeed')?.checked) return false;
  if(q && !(ev.summary||'').toLowerCase().includes(q)) return false;
  return true;
}
function eventRow(ev){
  const u=ev.usage||{}; const tok=(u.input||0)+(u.output||0);
  const nums=[tok?fmtTokens(tok)+' tok':'', ev.cost_usd?fmtCost(ev.cost_usd):''].filter(Boolean).join(' · ');
  return `<div class="event" data-key="${escHtml(ev.session_id+':'+ev.line_index)}">
    <span class="time">${timeOf(ev)}</span>
    <span class="badge-type t-${escHtml(ev.event_type)}">${escHtml(ev.event_type)}</span>
    <span class="summary">${escHtml(ev.summary||'')}</span>
    <span class="nums">${nums}</span>
  </div>`;
}
function renderFeed(){
  const feed=$('#feed');
  const rows=state.feed.filter(feedFilter).slice(-FEED_CAP).reverse();
  feed.innerHTML = rows.length ? rows.map(eventRow).join('') : '<div class="empty">Waiting for live events…</div>';
  $('#feedCount').textContent = state.feed.length+' buffered';
  bindFeed(feed);
}
function appendFeedRow(ev){
  if(!feedFilter(ev)) return;
  const feed=$('#feed');
  if(feed.querySelector('.empty')) feed.innerHTML='';
  feed.insertAdjacentHTML('afterbegin', eventRow(ev));
  while(feed.children.length>FEED_CAP) feed.lastChild.remove();
  const first=feed.firstElementChild; if(first) bindRow(first);
  $('#feedCount').textContent = state.feed.length+' buffered';
}
function bindFeed(feed){ $$('.event',feed).forEach(bindRow); }
function bindRow(el){ el.addEventListener('click', ()=>{ const ev=state.feed.find(e=> (e.session_id+':'+e.line_index)===el.dataset.key); if(ev) openDrawer(ev); }); }

// ---------- Conversation (from DB) ----------
async function loadConversation(reset){
  if(!state.selected){ $('#conversation').innerHTML='<div class="empty">Select a session to view its conversation.</div>'; return; }
  if(reset){ state.conv={events:[],total:0,offset:0,loading:false}; $('#conversation').innerHTML='<div class="empty">Loading…</div>'; }
  if(state.conv.loading) return; state.conv.loading=true;
  const params=new URLSearchParams({limit:'150', offset:String(state.conv.offset)});
  const t=$('#convType').value; if(t!=='all') params.set('type', t);
  const q=$('#convSearch').value.trim(); if(q) params.set('search', q);
  try{
    const d=await api('/api/db/sessions/'+state.selected+'/events?'+params.toString());
    state.conv.total=d.total; state.conv.offset+=d.events.length;
    state.conv.events=state.conv.events.concat(d.events);
    renderConversation();
  }catch(e){ $('#conversation').innerHTML='<div class="empty">Could not load conversation.</div>'; }
  state.conv.loading=false;
}
function latencyMap(evs){
  const m={}; let lastUser=null;
  for(const e of evs){
    const t=Date.parse(e.timestamp||e.observed_at);
    if(e.event_type==='user') lastUser=t;
    else if(e.event_type==='assistant' && lastUser && !isNaN(t)) m[e.session_id+':'+e.line_index]=t-lastUser;
  }
  return m;
}
function renderContent(content){
  if(typeof content==='string') return `<pre>${escHtml(content)}</pre>`;
  if(!Array.isArray(content)) return '';
  let html='';
  for(const b of content){
    if(b.type==='text') html+=`<pre>${escHtml(b.text||'')}</pre>`;
    else if(b.type==='thinking') html+=`<div class="think">${escHtml(b.thinking||'')}</div>`;
    else if(b.type==='tool_use') html+=`<div class="toolcard"><div class="h">🔧 ${escHtml(b.name||'tool')}</div><pre>${escHtml(JSON.stringify(b.input||{},null,2))}</pre></div>`;
    else if(b.type==='tool_result'){ let c=b.content; if(Array.isArray(c)) c=c.map(x=>x.text||JSON.stringify(x)).join('\n'); else if(typeof c!=='string') c=JSON.stringify(c,null,2); html+=`<div class="toolcard"><div class="h">📦 Tool result</div><pre>${escHtml(c||'')}</pre></div>`; }
  }
  return html;
}
function convMessage(ev, lat){
  const e=ev.entry||{};
  const content = e.message?.content ?? e.content ?? '';
  const body=renderContent(content);
  if(!body) return '';
  const role=ev.event_type;
  const roleColor = role==='user'?'var(--accent)':role==='assistant'?'var(--green)':'var(--muted)';
  const latTxt = lat ? `<span class="lat">⚡ ${(lat/1000).toFixed(1)}s</span>` : '';
  return `<div class="msg ${role}">
    <div class="who"><span class="role" style="background:var(--surface-3);color:${roleColor}">${escHtml(role)}</span>
      <span style="color:var(--dim);font-weight:400">${timeOf(ev)}</span>${latTxt}</div>
    <div class="bubble">${body}</div>
  </div>`;
}
function renderConversation(){
  const wrap=$('#conversation');
  const lats=latencyMap(state.conv.events);
  let html=state.conv.events.map(ev=>convMessage(ev, lats[ev.session_id+':'+ev.line_index])).join('');
  if(!html) html='<div class="empty">No messages match.</div>';
  if(state.conv.offset < state.conv.total) html+=`<button class="btn load-more" id="loadMore">Load more (${state.conv.offset}/${state.conv.total})</button>`;
  wrap.innerHTML=html;
  $('#convCount').textContent = state.conv.total ? state.conv.total+' events' : '';
  const lm=$('#loadMore'); if(lm) lm.addEventListener('click', ()=> loadConversation(false));
}

// ---------- Analytics (from DB) ----------
async function loadAnalytics(){
  try{
    const d=await api('/api/db/stats');
    renderAnalytics(d);
  }catch(e){ $('#analytics').innerHTML='<div class="empty">Could not load analytics.</div>'; }
}
function barList(items, key, label){
  const max=Math.max(1, ...items.map(x=>x[key]));
  return items.map(x=>`<div class="bar-row">
    <span class="name">${escHtml(x.name||x.key||x.model||'—')}</span>
    <div class="bar" style="width:${Math.max(2,100*x[key]/max)}%"></div>
    <span class="val">${label?label(x):fmtNum(x[key])}</span>
  </div>`).join('') || '<span class="hint">No data yet.</span>';
}
function renderAnalytics(d){
  const tk=d.tokens||{};
  const tl=d.timeline||[];
  const maxTl=Math.max(1,...tl.map(x=>x.events));
  const tlBars=tl.map(x=>`<div class="tl-bar" title="${escHtml(x.day)}: ${fmtNum(x.events)} events, ${fmtCost(x.cost_usd)}" style="height:${Math.max(2,100*x.events/maxTl)}%"></div>`).join('');
  $('#analytics').innerHTML=`
    <div class="cards">
      <div class="card"><div class="k">Sessions</div><div class="v">${fmtNum(d.sessions)}</div></div>
      <div class="card"><div class="k">Events</div><div class="v">${fmtNum(d.events)}</div></div>
      <div class="card"><div class="k">Total tokens</div><div class="v">${fmtTokens((tk.input||0)+(tk.output||0))}</div><div class="sub">${fmtNum(tk.input)} in · ${fmtNum(tk.output)} out</div></div>
      <div class="card"><div class="k">Cache read</div><div class="v">${fmtTokens(tk.cache_read)}</div><div class="sub">${fmtTokens(tk.cache_creation)} created</div></div>
      <div class="card"><div class="k">Est. cost</div><div class="v">${fmtCost(d.cost_usd)}</div></div>
    </div>
    <div class="block" style="margin-bottom:14px">
      <h3>Activity — last ${tl.length} day(s)</h3>
      <div class="timeline">${tlBars||'<span class="hint">No data yet.</span>'}</div>
      <div class="tl-labels"><span>${tl.length?escHtml(tl[0].day):''}</span><span>${tl.length?escHtml(tl[tl.length-1].day):''}</span></div>
    </div>
    <div class="grid2">
      <div class="block"><h3>Top tools</h3>${barList(d.top_tools||[],'count')}</div>
      <div class="block"><h3>Cost by model</h3>${barList((d.cost_by_model||[]),'cost_usd', x=>fmtCost(x.cost_usd))}</div>
      <div class="block"><h3>Events by type</h3>${barList((d.by_type||[]).map(x=>({name:x.key,count:x.count})),'count')}</div>
      <div class="block"><h3>Events by model</h3>${barList((d.by_model||[]).map(x=>({name:x.key,count:x.count})),'count')}</div>
    </div>`;
}

// ---------- Tabs ----------
function setTab(tab){
  state.tab=tab;
  $$('.tab').forEach(t=> t.classList.toggle('active', t.dataset.tab===tab));
  $('#panel-live').style.display = tab==='live'?'block':'none';
  $('#panel-conversation').style.display = tab==='conversation'?'block':'none';
  $('#panel-analytics').style.display = tab==='analytics'?'block':'none';
  if(tab==='conversation') loadConversation(true);
  if(tab==='analytics') loadAnalytics();
  if(tab==='live') renderFeed();
}

// ---------- Drawer ----------
let drawerEvent=null;
function openDrawer(ev){
  drawerEvent=ev;
  $('#drawerTitle').textContent = (ev.event_type||'event')+' · line '+ev.line_index;
  $('#drawerBody').textContent = JSON.stringify(ev.entry??ev, null, 2);
  $('#scrim').classList.add('open'); $('#drawer').classList.add('open');
}
function closeDrawer(){ $('#scrim').classList.remove('open'); $('#drawer').classList.remove('open'); }

// ---------- Export ----------
let exportScope='all', exportFmt='messages';
function openExport(scope){
  if(scope){ exportScope=scope; }
  if(exportScope==='session' && !state.selected){ exportScope='all'; }
  $$('#exportScope button').forEach(b=> b.classList.toggle('sel', b.dataset.scope===exportScope));
  $$('#exportFmt button').forEach(b=> b.classList.toggle('sel', b.dataset.fmt===exportFmt));
  updateExportHint();
  $('#scrim').classList.add('open'); $('#exportModal').classList.add('open');
}
function updateExportHint(){
  const sess = exportScope==='session' ? (state.sessions.find(s=>s.id===state.selected)?.title||shortId(state.selected)) : null;
  $('#exportHint').textContent = exportScope==='session'
    ? `Exporting session “${sess}” as ${exportFmt}.`
    : `Exporting all sessions as ${exportFmt}.` + (exportFmt==='huggingface'?' (downloads a dataset card + JSONL)':'');
}
function doExport(){
  let url;
  if(exportScope==='session' && state.selected) url='/api/sessions/'+encodeURIComponent(state.selected)+'/export?format='+exportFmt;
  else url='/api/export?format='+exportFmt;
  window.location.href=url;
  closeExport(); toast('Export started');
}
function closeExport(){ $('#exportModal').classList.remove('open'); if(!$('#drawer').classList.contains('open')) $('#scrim').classList.remove('open'); }

// ---------- Global search ----------
let searchTimer;
async function runSearch(q){
  if(!q.trim()){ $('#searchPop').classList.remove('open'); return; }
  try{
    const d=await api('/api/db/search?limit=40&q='+encodeURIComponent(q));
    const pop=$('#searchPop');
    if(!d.events.length){ pop.innerHTML='<div class="sr"><span class="hint">No matches.</span></div>'; }
    else pop.innerHTML=d.events.map(ev=>`<div class="sr" data-sid="${escHtml(ev.session_id)}" data-key="${escHtml(ev.session_id+':'+ev.line_index)}">
      <div class="t"><span class="badge-type t-${escHtml(ev.event_type)}">${escHtml(ev.event_type)}</span><span class="mono">${escHtml(shortId(ev.session_id))}</span><span>${timeOf(ev)}</span></div>
      <div class="s">${escHtml(ev.summary||'')}</div></div>`).join('');
    pop.classList.add('open');
    $$('.sr', pop).forEach(el=> el.addEventListener('click', ()=>{ if(el.dataset.sid){ selectSession(el.dataset.sid).then(()=>setTab('conversation')); } pop.classList.remove('open'); }));
  }catch(e){}
}

// ---------- Wiring ----------
function init(){
  // theme
  const savedTheme=localStorage.getItem('ct_theme'); if(savedTheme) document.documentElement.dataset.theme=savedTheme;
  $('#themeBtn').addEventListener('click', toggleTheme);
  $('#helpBtn').addEventListener('click', ()=>{ $('#scrim').classList.add('open'); $('#helpModal').classList.add('open'); });
  $('#helpClose').addEventListener('click', ()=>{ $('#helpModal').classList.remove('open'); $('#scrim').classList.remove('open'); });

  // sidebar collapse
  $('#sidebarToggle').addEventListener('click', ()=> $('#body').classList.toggle('collapsed'));

  // sidebar controls
  let st; $('#sessSearch').addEventListener('input', ()=>{ clearTimeout(st); st=setTimeout(loadSessions,250); });
  $('#sortSel').addEventListener('change', loadSessions);
  $('#bmOnly').addEventListener('change', loadSessions);

  // tabs
  $$('.tab').forEach(t=> t.addEventListener('click', ()=> setTab(t.dataset.tab)));

  // live controls
  $('#liveType').addEventListener('change', renderFeed);
  let lt; $('#liveSearch').addEventListener('input', ()=>{ clearTimeout(lt); lt=setTimeout(renderFeed,200); });
  $('#pauseBtn').addEventListener('click', togglePause);
  $('#clearBtn').addEventListener('click', ()=>{ state.feed=[]; state.feedSeen.clear(); renderFeed(); });

  // conversation controls
  $('#convType').addEventListener('change', ()=> loadConversation(true));
  let ct; $('#convSearch').addEventListener('input', ()=>{ clearTimeout(ct); ct=setTimeout(()=>loadConversation(true),300); });

  // drawer
  $('#drawerClose').addEventListener('click', closeDrawer);
  $('#scrim').addEventListener('click', ()=>{ closeDrawer(); closeExport(); $('#helpModal').classList.remove('open'); });
  $('#copyJson').addEventListener('click', ()=>{ navigator.clipboard?.writeText($('#drawerBody').textContent); toast('Copied JSON'); });

  // export
  $('#exportBtn').addEventListener('click', ()=> openExport('all'));
  $('#exportCancel').addEventListener('click', closeExport);
  $('#exportGo').addEventListener('click', doExport);
  $$('#exportScope button').forEach(b=> b.addEventListener('click', ()=>{ exportScope=b.dataset.scope; openExport(); }));
  $$('#exportFmt button').forEach(b=> b.addEventListener('click', ()=>{ exportFmt=b.dataset.fmt; $$('#exportFmt button').forEach(x=>x.classList.toggle('sel',x===b)); updateExportHint(); }));

  // global search
  $('#globalSearch').addEventListener('input', (e)=>{ clearTimeout(searchTimer); searchTimer=setTimeout(()=>runSearch(e.target.value),250); });
  $('#globalSearch').addEventListener('blur', ()=> setTimeout(()=>$('#searchPop').classList.remove('open'),200));

  // shortcuts
  document.addEventListener('keydown', onKey);

  connect();
  loadSessions();
  setInterval(loadSessions, 5000);
}
function toggleTheme(){ const cur=document.documentElement.dataset.theme==='light'?'dark':'light'; document.documentElement.dataset.theme=cur; localStorage.setItem('ct_theme',cur); }
function togglePause(){ state.paused=!state.paused; $('#pauseBtn').textContent=state.paused?'▶ Resume':'⏸ Pause'; if(!state.paused) renderFeed(); }
function onKey(e){
  if(e.target.matches('input,textarea,select')){ if(e.key==='Escape') e.target.blur(); return; }
  if(e.key==='/'){ e.preventDefault(); $('#globalSearch').focus(); }
  else if(e.key==='1') setTab('live');
  else if(e.key==='2') setTab('conversation');
  else if(e.key==='3') setTab('analytics');
  else if(e.key==='e') openExport(state.selected?'session':'all');
  else if(e.key==='t') toggleTheme();
  else if(e.key===' '){ e.preventDefault(); togglePause(); }
  else if((e.key==='b')&&(e.ctrlKey||e.metaKey)){ e.preventDefault(); $('#body').classList.toggle('collapsed'); }
  else if(e.key==='Escape'){ closeDrawer(); closeExport(); $('#helpModal').classList.remove('open'); $('#searchPop').classList.remove('open'); }
}
init();
</script>
</body>
</html>
"##;
