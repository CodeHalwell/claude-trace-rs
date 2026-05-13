/// Returns the built-in dashboard HTML page as a static string.
pub fn dashboard_html(port: u16) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Claude Trace RS</title>
<style>
  :root {{
    --bg: #0d1117;
    --bg2: #161b22;
    --bg3: #21262d;
    --border: #30363d;
    --text: #c9d1d9;
    --text-muted: #8b949e;
    --accent: #58a6ff;
    --green: #3fb950;
    --yellow: #d29922;
    --red: #f85149;
    --orange: #e3b341;
    --purple: #bc8cff;
  }}
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{ background: var(--bg); color: var(--text); font-family: 'Segoe UI', system-ui, sans-serif; font-size: 14px; height: 100vh; display: flex; flex-direction: column; }}
  header {{ background: var(--bg2); border-bottom: 1px solid var(--border); padding: 8px 16px; display: flex; align-items: center; gap: 16px; flex-shrink: 0; }}
  header h1 {{ font-size: 16px; font-weight: 600; color: var(--accent); }}
  #status {{ font-size: 12px; padding: 2px 8px; border-radius: 12px; background: var(--bg3); }}
  #status.connected {{ color: var(--green); }}
  #status.disconnected {{ color: var(--red); }}
  #event-count {{ font-size: 12px; color: var(--text-muted); }}
  .spacer {{ flex: 1; }}
  #scroll-lock-label {{ font-size: 12px; display: flex; align-items: center; gap: 4px; color: var(--text-muted); cursor: pointer; }}
  .main {{ display: flex; flex: 1; overflow: hidden; }}
  aside {{ width: 220px; flex-shrink: 0; background: var(--bg2); border-right: 1px solid var(--border); display: flex; flex-direction: column; overflow: hidden; }}
  aside h2 {{ font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--text-muted); padding: 10px 12px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; }}
  #session-list {{ overflow-y: auto; flex: 1; padding: 4px 0; }}
  .session-item {{ padding: 6px 12px; cursor: pointer; border-left: 3px solid transparent; font-size: 13px; }}
  .session-item:hover {{ background: var(--bg3); }}
  .session-item.active {{ border-left-color: var(--accent); background: var(--bg3); }}
  .session-item .sid {{ font-family: monospace; font-size: 11px; color: var(--accent); word-break: break-all; }}
  .session-item .meta {{ font-size: 11px; color: var(--text-muted); margin-top: 2px; }}
  .content {{ flex: 1; display: flex; flex-direction: column; overflow: hidden; }}
  .toolbar {{ display: flex; gap: 8px; padding: 8px 12px; background: var(--bg2); border-bottom: 1px solid var(--border); flex-shrink: 0; flex-wrap: wrap; align-items: center; }}
  .type-btn {{ font-size: 11px; padding: 2px 8px; border-radius: 12px; border: 1px solid var(--border); background: var(--bg3); color: var(--text-muted); cursor: pointer; }}
  .type-btn.active {{ border-color: var(--accent); color: var(--accent); }}
  #search {{ flex: 1; min-width: 150px; background: var(--bg3); border: 1px solid var(--border); border-radius: 6px; color: var(--text); padding: 4px 10px; font-size: 13px; outline: none; }}
  #search:focus {{ border-color: var(--accent); }}
  .feed-container {{ display: flex; flex: 1; overflow: hidden; }}
  #feed {{ flex: 1; overflow-y: auto; padding: 4px 0; }}
  .event-row {{ display: flex; align-items: baseline; gap: 10px; padding: 5px 12px; cursor: pointer; border-bottom: 1px solid transparent; font-size: 13px; }}
  .event-row:hover {{ background: var(--bg3); }}
  .event-row.selected {{ background: var(--bg3); border-left: 3px solid var(--accent); padding-left: 9px; }}
  .event-row .idx {{ font-family: monospace; font-size: 11px; color: var(--text-muted); min-width: 38px; text-align: right; flex-shrink: 0; }}
  .event-row .badge {{ font-size: 10px; padding: 1px 5px; border-radius: 10px; flex-shrink: 0; }}
  .badge-user {{ background: #1c3461; color: #79c0ff; }}
  .badge-assistant {{ background: #1a3a1a; color: var(--green); }}
  .badge-tool_use {{ background: #3b2700; color: var(--orange); }}
  .badge-tool_result {{ background: #2d1f00; color: var(--yellow); }}
  .badge-system {{ background: #2d1f3d; color: var(--purple); }}
  .badge-unknown {{ background: var(--bg3); color: var(--text-muted); }}
  .event-row .summary {{ color: var(--text); flex: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }}
  .event-row .ts {{ font-family: monospace; font-size: 10px; color: var(--text-muted); flex-shrink: 0; }}
  #detail {{ width: 380px; flex-shrink: 0; border-left: 1px solid var(--border); background: var(--bg2); display: flex; flex-direction: column; overflow: hidden; }}
  #detail h3 {{ font-size: 12px; padding: 8px 12px; border-bottom: 1px solid var(--border); color: var(--text-muted); flex-shrink: 0; }}
  #detail-meta {{ padding: 10px 12px; font-size: 12px; color: var(--text-muted); border-bottom: 1px solid var(--border); flex-shrink: 0; font-family: monospace; }}
  #detail-json {{ flex: 1; overflow: auto; padding: 12px; font-family: monospace; font-size: 12px; line-height: 1.6; white-space: pre; color: var(--text); }}
  .statsbar {{ background: var(--bg2); border-top: 1px solid var(--border); padding: 5px 16px; display: flex; gap: 20px; font-size: 11px; color: var(--text-muted); flex-shrink: 0; }}
  .statsbar span b {{ color: var(--text); }}
  #no-events {{ padding: 32px; text-align: center; color: var(--text-muted); font-size: 13px; }}
</style>
</head>
<body>
<header>
  <h1>🔍 Claude Trace RS</h1>
  <span id="status" class="disconnected">● Disconnected</span>
  <span id="event-count">0 events</span>
  <span class="spacer"></span>
  <label id="scroll-lock-label"><input type="checkbox" id="scroll-lock" checked> Auto-scroll</label>
</header>
<div class="main">
  <aside>
    <h2>Sessions</h2>
    <div id="session-list"><div style="padding:12px;font-size:12px;color:var(--text-muted)">No sessions yet</div></div>
  </aside>
  <div class="content">
    <div class="toolbar">
      <button class="type-btn active" data-type="all">All</button>
      <button class="type-btn" data-type="user">👤 User</button>
      <button class="type-btn" data-type="assistant">🤖 Assistant</button>
      <button class="type-btn" data-type="tool_use">🔧 Tool Use</button>
      <button class="type-btn" data-type="tool_result">📦 Tool Result</button>
      <button class="type-btn" data-type="system">⚙️ System</button>
      <input id="search" type="text" placeholder="Search events…">
    </div>
    <div class="feed-container">
      <div id="feed"><div id="no-events">Waiting for events…</div></div>
      <div id="detail">
        <h3>Event Detail</h3>
        <div id="detail-meta">Select an event to inspect</div>
        <pre id="detail-json"></pre>
      </div>
    </div>
  </div>
</div>
<div class="statsbar">
  <span>Events: <b id="stat-events">0</b></span>
  <span>User: <b id="stat-user">0</b></span>
  <span>Assistant: <b id="stat-asst">0</b></span>
  <span>Tool calls: <b id="stat-tools">0</b></span>
  <span>Cost: <b id="stat-cost">$0.0000</b></span>
  <span>Tokens in: <b id="stat-tok-in">0</b></span>
  <span>Tokens out: <b id="stat-tok-out">0</b></span>
</div>

<script>
(function() {{
  const WS_URL = `ws://${{location.hostname}}:{port}/ws`;
  let ws, reconnectTimer;
  let allEvents = [];
  let sessions = {{}};
  let activeSession = null;
  let activeType = 'all';
  let searchQuery = '';
  let selectedId = null;
  let stats = {{ total: 0, user: 0, asst: 0, tools: 0, cost: 0, tokIn: 0, tokOut: 0 }};

  function connect() {{
    ws = new WebSocket(WS_URL);
    ws.onopen = () => {{
      document.getElementById('status').textContent = '● Connected';
      document.getElementById('status').className = 'connected';
      clearTimeout(reconnectTimer);
    }};
    ws.onclose = () => {{
      document.getElementById('status').textContent = '● Disconnected';
      document.getElementById('status').className = 'disconnected';
      reconnectTimer = setTimeout(connect, 2000);
    }};
    ws.onmessage = (e) => {{
      const msg = JSON.parse(e.data);
      if (msg.type === 'connected') return;
      handleEvent(msg);
    }};
  }}

  function handleEvent(ev) {{
    ev._idx = allEvents.length;
    allEvents.push(ev);
    stats.total++;

    const etype = ev.entry && ev.entry.type ? ev.entry.type : 'unknown';
    if (etype === 'user') stats.user++;
    else if (etype === 'assistant') stats.asst++;
    else if (etype === 'tool_use' || etype === 'tool_result') stats.tools++;

    if (etype === 'assistant') {{
      const cost = ev.entry.costUSD || 0;
      stats.cost += cost;
      const tokIn = (ev.entry.message && ev.entry.message.usage && ev.entry.message.usage.input_tokens) || 0;
      const tokOut = (ev.entry.message && ev.entry.message.usage && ev.entry.message.usage.output_tokens) || 0;
      stats.tokIn += tokIn;
      stats.tokOut += tokOut;
    }}

    if (!sessions[ev.session_id]) sessions[ev.session_id] = {{ count: 0, cost: 0 }};
    sessions[ev.session_id].count++;
    if (etype === 'assistant' && ev.entry.costUSD) sessions[ev.session_id].cost += ev.entry.costUSD;

    updateStats();
    renderSessions();
    if (activeSession === null || activeSession === ev.session_id) renderFeed();
    document.getElementById('event-count').textContent = allEvents.length + ' events';
  }}

  function updateStats() {{
    document.getElementById('stat-events').textContent = stats.total;
    document.getElementById('stat-user').textContent = stats.user;
    document.getElementById('stat-asst').textContent = stats.asst;
    document.getElementById('stat-tools').textContent = stats.tools;
    document.getElementById('stat-cost').textContent = '$' + stats.cost.toFixed(4);
    document.getElementById('stat-tok-in').textContent = stats.tokIn.toLocaleString();
    document.getElementById('stat-tok-out').textContent = stats.tokOut.toLocaleString();
  }}

  function renderSessions() {{
    const list = document.getElementById('session-list');
    const ids = Object.keys(sessions);
    if (ids.length === 0) {{ list.innerHTML = '<div style="padding:12px;font-size:12px;color:var(--text-muted)">No sessions yet</div>'; return; }}
    list.innerHTML = ids.map(id => {{
      const s = sessions[id];
      const active = id === activeSession ? ' active' : '';
      const short = id.length > 20 ? id.slice(0, 8) + '…' + id.slice(-6) : id;
      return `<div class="session-item${{active}}" data-id="${{escHtml(id)}}">
        <div class="sid" title="${{escHtml(id)}}">${{escHtml(short)}}</div>
        <div class="meta">${{s.count}} events · $${{s.cost.toFixed(4)}}</div>
      </div>`;
    }}).join('');
    list.querySelectorAll('.session-item').forEach(el => {{
      el.addEventListener('click', () => {{
        activeSession = el.dataset.id === activeSession ? null : el.dataset.id;
        renderSessions();
        renderFeed();
      }});
    }});
  }}

  function matchesFilter(ev) {{
    const etype = ev.entry && ev.entry.type ? ev.entry.type : 'unknown';
    if (activeType !== 'all' && etype !== activeType) return false;
    if (activeSession && ev.session_id !== activeSession) return false;
    if (searchQuery) {{
      const hay = JSON.stringify(ev).toLowerCase();
      if (!hay.includes(searchQuery.toLowerCase())) return false;
    }}
    return true;
  }}

  function renderFeed() {{
    const feed = document.getElementById('feed');
    const visible = allEvents.filter(matchesFilter);
    if (visible.length === 0) {{
      feed.innerHTML = '<div id="no-events">No matching events</div>';
      return;
    }}
    const scrollLock = document.getElementById('scroll-lock').checked;
    const wasAtBottom = feed.scrollHeight - feed.scrollTop <= feed.clientHeight + 40;
    feed.innerHTML = visible.map((ev) => {{
      const etype = ev.entry && ev.entry.type ? ev.entry.type : 'unknown';
      const safeEtype = etype.replace(/[^a-z0-9_]/gi, '');
      const ts = ev.observed_at ? ev.observed_at.slice(11, 19) : '';
      const selected = ev._idx === selectedId ? ' selected' : '';
      return `<div class="event-row${{selected}}" data-idx="${{ev._idx}}">
        <span class="idx">${{ev.line_index}}</span>
        <span class="badge badge-${{safeEtype}}">${{escHtml(etype)}}</span>
        <span class="summary">${{escHtml(ev.summary)}}</span>
        <span class="ts">${{ts}}</span>
      </div>`;
    }}).join('');
    feed.querySelectorAll('.event-row').forEach(el => {{
      el.addEventListener('click', () => {{
        selectedId = parseInt(el.dataset.idx, 10);
        renderFeed();
        showDetail(allEvents[selectedId]);
      }});
    }});
    if (scrollLock && wasAtBottom) feed.scrollTop = feed.scrollHeight;
  }}

  function showDetail(ev) {{
    if (!ev) return;
    const meta = document.getElementById('detail-meta');
    const json = document.getElementById('detail-json');
    meta.innerHTML = `<div>Session: <b style="color:var(--accent)">${{escHtml(ev.session_id)}}</b></div>
<div>Line: <b>${{ev.line_index}}</b> · Time: <b>${{escHtml(ev.observed_at || '')}}</b></div>`;
    json.textContent = JSON.stringify(ev.entry, null, 2);
  }}

  function escHtml(s) {{
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }}

  // Type filter buttons
  document.querySelectorAll('.type-btn').forEach(btn => {{
    btn.addEventListener('click', () => {{
      activeType = btn.dataset.type;
      document.querySelectorAll('.type-btn').forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      renderFeed();
    }});
  }});

  // Search
  document.getElementById('search').addEventListener('input', e => {{
    searchQuery = e.target.value;
    renderFeed();
  }});

  connect();
}})();
</script>
</body>
</html>"#,
        port = port
    )
}
