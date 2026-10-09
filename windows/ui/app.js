'use strict';
const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args);
const listen = (ev, cb) => T.event.listen(ev, e => cb(e.payload));

// ---------- helpers ----------
const $ = sel => document.querySelector(sel);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const pct = v => v == null || isNaN(v) ? '—' : `${Number(v).toFixed(v < 10 ? 1 : 0)}%`;
const bytes = b => { if (b == null) return '—'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0; b = Number(b); while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; } return `${b.toFixed(b < 10 && i ? 1 : 0)} ${u[i]}`; };
const dur = s => { s = Math.floor(s || 0); const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60); return d ? `${d} d ${h} h` : h ? `${h} h ${m} min` : `${m} min`; };
const ago = ms => { if (!ms) return '—'; const s = Math.max(0, (Date.now() - ms) / 1000); return s < 60 ? `${Math.floor(s)} s ago` : s < 3600 ? `${Math.floor(s / 60)} min ago` : s < 86400 ? `${Math.floor(s / 3600)} h ago` : new Date(ms).toLocaleString(); };
const time = ms => new Date(ms).toLocaleString();
const icon = d => `<svg class="i" viewBox="0 0 24 24">${d}</svg>`;
function flash(msg) { const el = document.createElement('div'); el.className = 'toastmsg'; el.textContent = msg; document.body.appendChild(el); setTimeout(() => el.remove(), 2200); }
async function copy(text) {
  try { await navigator.clipboard.writeText(text); }
  catch { const ta = document.createElement('textarea'); ta.value = text; document.body.appendChild(ta); ta.select(); document.execCommand('copy'); ta.remove(); }
  flash('Скопировано');
}
const tone = state => ({ Ready: 'ok', Connecting: 'info', Authenticating: 'info', Degraded: 'warn', Offline: 'crit', Disconnected: 'neutral' }[state] || 'neutral');
const pill = (t, label) => `<span class="pill ${t}"><span class="dot"></span>${esc(label)}</span>`;
function meter(v, warnAt = 75, critAt = 90) {
  if (v == null) return '<span class="muted">—</span>';
  const cls = v >= critAt ? 'crit' : v >= warnAt ? 'warn' : '';
  return `<div class="meter"><div class="track"><div class="fill ${cls}" style="width:${Math.min(100, Math.max(0, v))}%"></div></div><span>${pct(v)}</span></div>`;
}
function spark(values, max) {
  const W = 352, H = 72; const v = values.filter(x => x != null && !isNaN(x));
  if (v.length < 2) return `<svg class="spark" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none"><line x1="0" y1="71.5" x2="${W}" y2="71.5" stroke="#2a2b30"/></svg>`;
  const mx = max ?? (Math.max(...v) * 1.15 || 1); const n = values.length;
  const pts = values.map((y, i) => [i / (n - 1) * W, H - 2 - (Math.min(y ?? 0, mx) / mx) * (H - 6)]);
  const line = pts.map(p => p.map(c => c.toFixed(1)).join(',')).join(' ');
  return `<svg class="spark" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none"><line x1="0" y1="71.5" x2="${W}" y2="71.5" stroke="#2a2b30"/><path d="M0,${H} L${line.replace(/ /g, ' L')} L${W},${H} Z" fill="var(--accent)" fill-opacity="0.09"/><polyline points="${line}" fill="none" stroke="var(--accent)" stroke-width="1.6" stroke-linejoin="round" vector-effect="non-scaling-stroke"/></svg>`;
}

// ---------- state ----------
const S = { page: 'fleet', serverId: null, tab: 'overview', status: null, servers: [], alerts: [], doSnap: null, config: null,
  detail: { catalog: {}, values: {}, series: {}, status: '' }, logs: [], logStatus: '', follow: false };

async function refreshStatus() { try { S.status = await invoke('status'); } catch (e) { console.error(e); } renderSide(); }
async function refreshServers() { try { S.servers = await invoke('servers'); } catch (e) { S.servers = []; } renderSide(); }
async function refreshAlerts() { S.alerts = await invoke('alerts_list'); renderSide(); }

function renderSide() {
  document.querySelectorAll('.nav-item[data-page]').forEach(el => el.classList.toggle('active', el.dataset.page === S.page));
  $('#nav-count').textContent = S.servers.length || '';
  const act = S.alerts.filter(a => a.active); const crit = act.some(a => a.severity === 'critical');
  $('#nav-alerts').innerHTML = act.length ? `<span class="badge ${crit ? 'crit' : 'warn'}">${act.length}</span>` : '';
  const d = S.doSnap?.droplet; $('#nav-do').innerHTML = d ? `<span class="pill ${d.status === 'active' ? 'ok' : 'crit'}" style="height:18px;font-size:11px">${esc(d.status)}</span>` : '';
  $('#nav-servers').innerHTML = S.servers.map(s => `<div class="nav-item ${S.page === 'server' && S.serverId === s.id ? 'active' : ''}" data-server="${esc(s.id)}"><span class="pill ${tone(s.state)}" style="padding:0;background:none"><span class="dot"></span></span><span class="grow">${esc(s.name)}</span><span class="count">${s.cpu != null ? pct(s.cpu) : ''}</span></div>`).join('');
  document.querySelectorAll('[data-server]').forEach(el => el.onclick = () => openServer(el.dataset.server));
  const st = S.status;
  $('#foot-title').textContent = st?.fleet_name ? `${st.fleet_name} · Windows` : 'Fleet';
  $('#foot-sub').textContent = !st ? 'starting…' : st.phase === 'ready' ? 'Ready' : st.phase === 'enrolling' ? 'Создание fleet (первый запуск)…' : st.phase === 'error' ? `Ошибка: ${st.error || ''}` : st.phase;
}

function go(page) { if (S.page === 'server' && page !== 'server') stopStreams(); S.page = page; render(); }
function openServer(id) { if (S.serverId !== id) stopStreams(); S.page = 'server'; S.serverId = id; S.tab = 'overview'; render(); }
function stopStreams() { invoke('stop_metrics'); invoke('stop_journal'); S.follow = false; S.detail = { catalog: {}, values: {}, series: {}, status: '' }; }

function render() {
  renderSide();
  const m = $('#main');
  ({ fleet: renderFleet, server: renderServer, alerts: renderAlerts, do: renderDo, settings: renderSettings }[S.page] || renderFleet)(m);
}

// ---------- Fleet ----------
function renderFleet(m) {
  const online = S.servers.filter(s => s.state === 'Ready').length;
  const act = S.alerts.filter(a => a.active);
  const d = S.doSnap?.droplet, db = S.doSnap?.db;
  const st = S.status;
  m.innerHTML = `
  <header class="bar"><div class="col" style="gap:2px"><h1>Fleet</h1><span class="sub">${S.servers.length} server(s) · ${st?.phase === 'ready' ? 'agent-only monitoring' : esc(st?.phase || '')}</span></div><div class="spacer"></div>
    <button class="btn primary" id="add">${icon('<path d="M12 5v14M5 12h14"/>')}Add server</button></header>
  <div class="content">
    ${st?.error ? `<div class="err">${esc(st.error)}</div>` : ''}
    ${st?.config_error ? `<div class="err">config: ${esc(st.config_error)}</div>` : ''}
    ${st?.phase === 'enrolling' ? `<div class="note">Первый запуск: создаю fleet и ключи (Argon2, ~10–30 c)…</div>` : ''}
    <div class="stats">
      <div class="card pad stat"><span class="label">Online</span><div class="value">${online}<small> / ${S.servers.length}</small></div><span class="hint">${esc(S.servers.filter(s => s.state !== 'Ready').map(s => `${s.name}: ${s.state}`).join(' · ') || 'all connected')}</span></div>
      <div class="card pad stat"><span class="label">Open alerts</span><div class="value" style="color:${act.length ? 'var(--warn)' : 'inherit'}">${act.length}</div><span class="hint">${act.filter(a => a.severity === 'critical').length} critical · ${act.filter(a => a.severity !== 'critical').length} warnings</span></div>
      <div class="card pad stat"><span class="label">Droplet</span><div class="value" style="font-size:20px;padding:5px 0">${d ? pill(d.status === 'active' ? 'ok' : 'crit', d.status) : '<span class="muted">—</span>'}</div><span class="hint">${d ? esc(`${d.name} · ${d.region}`) : 'DigitalOcean не настроен'}</span></div>
      <div class="card pad stat"><span class="label">Managed DB</span><div class="value" style="font-size:20px;padding:5px 0">${db ? pill(db.status === 'online' ? 'ok' : 'crit', db.status) : '<span class="muted">—</span>'}</div><span class="hint">${db ? esc(`${db.name} · ${db.engine} ${db.version}`) : '—'}</span></div>
    </div>
    ${S.servers.length ? `<section class="card cardtable"><table class="t">
      <thead><tr><th>Server</th><th>Status</th><th>CPU</th><th>Memory</th><th>Disk /</th><th>Last sample</th></tr></thead>
      <tbody>${S.servers.map(s => `<tr class="click" data-open="${esc(s.id)}">
        <td><div class="col" style="gap:1px"><span style="font-weight:500">${esc(s.name)}</span><span class="mono muted" style="font-size:11px">${esc(s.user)}@${esc(s.host)}:${s.port}</span></div></td>
        <td>${s.agent_pinned ? pill(tone(s.state), s.state) : pill('warn', s.host_key_pinned ? 'agent not installed' : 'not set up')}</td>
        <td>${meter(s.cpu)}</td><td>${meter(s.mem)}</td><td>${meter(s.disk, 75, 85)}</td>
        <td class="muted">${ago(s.last_metrics_ms)}</td></tr>`).join('')}</tbody></table></section>`
      : `<div class="card empty"><div style="font-size:15px;color:var(--text)">Серверов пока нет</div><div>Добавьте сервер: ключ приложения → authorized_keys → подтверждение host key → установка агента (Agent-only).</div><button class="btn primary" id="add2">Add server</button></div>`}
  </div>`;
  $('#add').onclick = () => openWizard();
  const a2 = $('#add2'); if (a2) a2.onclick = () => openWizard();
  m.querySelectorAll('[data-open]').forEach(el => el.onclick = () => openServer(el.dataset.open));
}

// ---------- Server detail ----------
async function renderServer(m) {
  const s = S.servers.find(x => x.id === S.serverId);
  if (!s) { S.page = 'fleet'; return render(); }
  const tabs = [['overview', 'Overview'], ['docker', 'Docker'], ['logs', 'Logs']];
  m.innerHTML = `
  <header class="detail">
    <div class="crumbs"><a id="crumb">Fleet</a><span>/</span><span style="color:var(--nav)">${esc(s.name)}</span></div>
    <div class="row" style="gap:16px">
      <div class="col" style="gap:4px;flex-grow:1">
        <div class="row"><h1>${esc(s.name)}</h1>${s.agent_pinned ? pill(tone(s.state), s.state) : pill('warn', 'agent not installed')}</div>
        <div class="mono secondary" id="subline">${esc(s.user)}@${esc(s.host)}:${s.port}</div>
      </div>
      ${!s.agent_pinned ? `<button class="btn primary" id="install">Install agent</button>` : ''}
      ${s.agent_pinned ? '<button class="btn" id="reconnect">Reconnect</button>' : ''}
      <button class="btn danger" id="remove">Remove</button>
    </div>
    <nav class="tabs">${tabs.map(([k, l]) => `<div class="tab ${S.tab === k ? 'active' : ''}" data-tab="${k}">${l}</div>`).join('')}</nav>
  </header>
  <div class="content" id="tab"></div>`;
  $('#crumb').onclick = () => go('fleet');
  if ($('#reconnect')) $('#reconnect').onclick = async () => { try { await invoke('reconnect', { id: s.id }); flash('Переподключение…'); } catch (e) { flash(e); } };
  $('#remove').onclick = async () => { if (!confirm(`Удалить ${s.name} из приложения? (на сервере ничего не меняется)`)) return; stopStreams(); await invoke('remove_server', { id: s.id }); await refreshServers(); go('fleet'); };
  const ib = $('#install'); if (ib) ib.onclick = () => openWizard(s);
  m.querySelectorAll('[data-tab]').forEach(el => el.onclick = () => { if (S.tab === 'logs') { invoke('stop_journal'); S.follow = false; } S.tab = el.dataset.tab; renderServer(m); });
  const t = $('#tab');
  if (!s.agent_pinned) { t.innerHTML = `<div class="card empty"><div>Агент не установлен — данные недоступны.</div><button class="btn primary" id="install2">Install agent</button></div>`; $('#install2').onclick = () => openWizard(s); return; }
  if (S.tab === 'overview') return renderOverview(t, s);
  if (S.tab === 'docker') return renderDocker(t, s);
  if (S.tab === 'logs') return renderLogs(t, s);
}

const DETAIL_POINTS = 120;
function pushSeries(key, v) { const a = S.detail.series[key] ||= []; a.push(v); if (a.length > DETAIL_POINTS) a.shift(); }
function detailFigures() {
  const v = S.detail.values; const g = k => v[k];
  const sum = p => { let t = null; for (const [k, x] of Object.entries(v)) if (k.startsWith(p)) t = (t || 0) + x; return t; };
  const mem = g('mem.used') != null && g('mem.total') ? g('mem.used') / g('mem.total') * 100 : null;
  // "/" can be missing (containers: overlay root); fall back to the fullest mount.
  let disk = g('disk.used:/'), diskFree = g('disk.free:/');
  if (disk == null) for (const [k, x] of Object.entries(v)) if (k.startsWith('disk.used:') && (disk == null || x > disk)) { disk = x; diskFree = v['disk.free:' + k.slice(10)]; }
  return { cpu: g('cpu.busy'), mem, memUsed: g('mem.used'), memTotal: g('mem.total'), disk, diskFree,
    load1: g('load.1'), load5: g('load.5'), load15: g('load.15'), rx: sum('net.rx:'), tx: sum('net.tx:'), iowait: g('cpu.iowait'), steal: g('cpu.steal'),
    swap: g('swap.used'), swapTotal: g('swap.total') };
}

async function renderOverview(t, s) {
  t.innerHTML = `<div class="detail-grid">
    <div class="col" style="gap:16px;min-width:0">
      <div class="row"><h2 class="sec" style="flex-grow:1">Resources · live</h2><span class="muted" id="dstatus"></span></div>
      <div class="grid2" id="cards"></div>
      <div class="row"><h2 class="sec" style="flex-grow:1">Last hour</h2><span class="muted">10 s samples since app start</span></div>
      <div class="grid3" id="hist"></div>
    </div>
    <div class="col" style="gap:16px">
      <section class="card sidepanel"><h2>System</h2><div class="kv" id="sys"><span class="k">…</span><span></span></div></section>
      <section class="card sidepanel"><h2>Agent</h2><div class="kv" id="agent"><span class="k">…</span><span></span></div></section>
      <section class="card sidepanel"><h2>Connection</h2><div class="kv">
        <span class="k">State</span><span class="v">${esc(s.state)} · since ${ago(s.state_since_ms)}</span>
        <span class="k">Last error</span><span class="v mono">${esc(s.failure || '—')}</span>
        <span class="k">Security mode</span><span class="v">Agent-only (read-only use)</span></div></section>
    </div></div>`;
  drawCards();
  drawHistory(s.id);
  if (!S.detail.started || S.detail.started !== s.id) {
    S.detail = { catalog: {}, values: {}, series: {}, status: '', started: s.id };
    invoke('follow_metrics', { id: s.id }).catch(e => { $('#dstatus').textContent = e; });
  }
  invoke('system_info', { id: s.id }).then(i => {
    $('#subline').textContent = `${s.host} · ${i.os_id} ${i.os_version} · ${i.cpu_count} vCPU · ${bytes(i.mem_total_bytes)} · up ${dur(i.uptime_s)}`;
    $('#sys').innerHTML = `<span class="k">Hostname</span><span class="v">${esc(i.hostname)}</span><span class="k">OS</span><span class="v">${esc(i.os_id)} ${esc(i.os_version)}</span><span class="k">Kernel</span><span class="v mono">${esc(i.kernel)}</span><span class="k">Arch</span><span class="v">${esc(i.arch)}</span><span class="k">CPU</span><span class="v">${i.cpu_count} vCPU</span><span class="k">Memory</span><span class="v">${bytes(i.mem_total_bytes)}</span><span class="k">Uptime</span><span class="v">${dur(i.uptime_s)}</span>`;
  }).catch(e => { const el = $('#sys'); if (el) el.innerHTML = `<span class="k">Error</span><span class="v">${esc(e)}</span>`; });
  invoke('agent_health', { id: s.id }).then(h => {
    $('#agent').innerHTML = `<span class="k">Version</span><span class="v">${esc(h.agent_version)} (proto ${h.proto_version})</span><span class="k">Uptime</span><span class="v">${dur(h.uptime_s)}</span><span class="k">Memory</span><span class="v">gate ${bytes(h.gate_rss_bytes)} · exec ${bytes(h.exec_rss_bytes)}</span><span class="k">Roster / policy</span><span class="v">v${h.roster_version} / v${h.policy_version}</span>`;
  }).catch(e => { const el = $('#agent'); if (el) el.innerHTML = `<span class="k">Error</span><span class="v">${esc(e)}</span>`; });
}

function drawCards() {
  const el = $('#cards'); if (!el) return;
  const f = detailFigures(); const se = S.detail.series;
  const card = (label, sub, value, key, max) => `<div class="card metric"><div class="top"><span class="label">${label}</span><span class="sub">${sub}</span></div><span class="v">${value}</span>${spark(se[key] || [], max)}</div>`;
  el.innerHTML = [
    card('CPU', f.iowait != null ? `iowait ${pct(f.iowait)} · steal ${pct(f.steal)}` : '', pct(f.cpu), 'cpu', 100),
    card('Memory', f.memTotal ? `${bytes(f.memUsed)} / ${bytes(f.memTotal)}` : '', pct(f.mem), 'mem', 100),
    card('Load', f.load5 != null ? `5 m ${f.load5.toFixed(2)} · 15 m ${f.load15.toFixed(2)}` : '', f.load1 != null ? f.load1.toFixed(2) : '—', 'load'),
    card('Network', f.tx != null ? `out ${bytes(f.tx)}/s` : '', f.rx != null ? `${bytes(f.rx)}/s in` : '—', 'rx'),
    card('Disk /', f.diskFree != null ? `${bytes(f.diskFree)} free` : '', pct(f.disk), 'disk', 100),
    card('Swap', '', f.swapTotal ? `${bytes(f.swap)} / ${bytes(f.swapTotal)}` : '—', 'swap'),
  ].join('');
  const ds = $('#dstatus'); if (ds) ds.textContent = S.detail.status;
}

async function drawHistory(id) {
  const el = $('#hist'); if (!el) return;
  const h = await invoke('history', { id });
  const col = i => h.map(r => r[i]);
  const last = i => { for (let k = h.length - 1; k >= 0; k--) if (h[k][i] != null) return h[k][i]; return null; };
  const card = (label, i) => `<div class="card metric"><div class="top"><span class="label">${label}</span><span class="sub">${h.length} pts</span></div><span class="v">${pct(last(i))}</span>${spark(col(i), 100)}</div>`;
  el.innerHTML = card('CPU', 1) + card('Memory', 2) + card('Disk /', 3);
}

async function renderDocker(t, s) {
  t.innerHTML = `<div class="row"><h2 class="sec" style="flex-grow:1">Containers</h2><span class="muted">read-only</span><button class="btn small" id="drefresh">Refresh</button></div><div id="dlist" class="card empty">Loading…</div>`;
  $('#drefresh').onclick = () => renderDocker(t, s);
  try {
    const list = await invoke('docker_containers', { id: s.id });
    const el = $('#dlist'); if (!el) return;
    if (!list.length) { el.textContent = 'Нет контейнеров'; return; }
    el.className = 'card cardtable';
    const st = x => x === 'running' ? 'ok' : x === 'restarting' || x === 'paused' ? 'warn' : x === 'dead' ? 'crit' : 'neutral';
    list.sort((a, b) => (a.state === 'running' ? 0 : 1) - (b.state === 'running' ? 0 : 1) || a.name.localeCompare(b.name));
    el.innerHTML = `<table class="t"><thead><tr><th>Name</th><th>Image</th><th>State</th><th>Status</th><th>Ports</th><th>Project</th></tr></thead><tbody>${list.map(c => `<tr>
      <td style="font-weight:500">${esc(c.name)}</td><td class="mono secondary">${esc(c.image)}</td><td>${pill(st(c.state), c.state)}</td>
      <td class="secondary">${esc(c.status)}</td><td class="mono muted" style="font-size:11px">${c.ports.map(esc).join('<br>')}</td><td class="muted">${esc(c.compose_project || '')}</td></tr>`).join('')}</tbody></table>`;
  } catch (e) { const el = $('#dlist'); if (el) el.innerHTML = `<div class="err">${esc(e)}</div>`; }
}

function renderLogs(t, s) {
  t.innerHTML = `<div class="row" style="flex-wrap:wrap">
      <input id="lunits" placeholder="units: nginx.service, docker.service" style="width:280px" value="${esc(S.logUnits || '')}">
      <input id="lgrep" placeholder="grep" style="width:180px" value="${esc(S.logGrep || '')}">
      <select id="lprio"><option value="">all priorities</option><option value="3">err+</option><option value="4">warning+</option><option value="6">info+</option></select>
      <select id="llimit"><option>200</option><option selected>500</option><option>2000</option></select>
      <button class="btn" id="lload">Load</button>
      <button class="btn ${S.follow ? 'primary' : ''}" id="lfollow">${S.follow ? 'Following…' : 'Follow'}</button>
      <span class="muted" id="lstatus">${esc(S.logStatus)}</span></div>
    <div class="logs" id="lout"></div>`;
  if (S.logPrio) $('#lprio').value = S.logPrio;
  const args = () => {
    S.logUnits = $('#lunits').value; S.logGrep = $('#lgrep').value; S.logPrio = $('#lprio').value;
    return { id: s.id, units: S.logUnits.split(',').map(x => x.trim()).filter(Boolean), grep: S.logGrep || null, priority: S.logPrio === '' ? null : Number(S.logPrio) };
  };
  $('#lload').onclick = async () => {
    $('#lstatus').textContent = 'loading…';
    try { const r = await invoke('journal', { ...args(), limit: Number($('#llimit').value) }); S.logs = r.entries; drawLogs(true); $('#lstatus').textContent = `${r.entries.length} entries`; }
    catch (e) { $('#lstatus').textContent = e; }
  };
  $('#lfollow').onclick = async () => {
    if (S.follow) { await invoke('stop_journal'); S.follow = false; S.logStatus = 'stopped'; return renderLogs(t, s); }
    try { S.logs = []; await invoke('follow_journal', args()); S.follow = true; S.logStatus = 'following'; renderLogs(t, s); } catch (e) { $('#lstatus').textContent = e; }
  };
  drawLogs(true);
}
function drawLogs(scroll) {
  const el = $('#lout'); if (!el) return;
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  el.innerHTML = S.logs.map(e => `<div class="ln p${e.priority}"><span class="ts">${new Date(e.time_us / 1000).toLocaleString()}</span> <span class="u">${esc(e.unit || e.identifier || '')}</span>${e.pid ? `[${e.pid}]` : ''}: ${esc(e.message)}</div>`).join('') || '<span class="muted">Нажмите Load или Follow</span>';
  if (scroll || atBottom) el.scrollTop = el.scrollHeight;
}

// ---------- Alerts ----------
function renderAlerts(m) {
  const act = S.alerts.filter(a => a.active);
  m.innerHTML = `<header class="bar"><div class="col" style="gap:2px"><h1>Alerts</h1><span class="sub">${act.length} active · ${S.alerts.length} total</span></div><div class="spacer"></div>
    <button class="btn" id="atest">Test toast</button><button class="btn" id="aack">Mark all read</button><button class="btn" id="aclear">Clear resolved</button></header>
    <div class="content">
      <div class="muted">Пороги: CPU > ${S.config?.alerts.cpu_pct}% дольше ${S.config?.alerts.cpu_for_minutes} мин · RAM > ${S.config?.alerts.mem_pct}% · диск > ${S.config?.alerts.disk_pct}% · DB CPU > ${S.config?.alerts.db_cpu_pct}% · DB диск > ${S.config?.alerts.db_disk_pct}% · агент недоступен > ${S.config?.alerts.unreachable_minutes} мин. Повтор уведомления не чаще раза в ${S.config?.alerts.cooldown_minutes} мин.</div>
      ${S.alerts.length ? `<section class="card cardtable">${S.alerts.map(a => {
        const t = a.severity === 'critical' ? 'crit' : a.severity === 'warning' ? 'warn' : 'info';
        return `<div class="alert-row ${a.active ? '' : 'resolved'}"><span>${a.active ? pill(t, a.severity) : pill('ok', 'resolved')}</span>
          <div class="col" style="gap:2px"><span style="font-weight:${a.acked ? 400 : 600}">${esc(a.title)}</span><span class="muted" style="font-size:12px">${esc(a.detail)}</span></div>
          <span class="muted" style="font-size:12px">${time(a.first_ms)}${a.resolved_ms ? `<br>→ ${time(a.resolved_ms)}` : ''}</span><span class="muted">${esc(a.source)}</span></div>`; }).join('')}</section>`
      : `<div class="card empty">Алертов нет</div>`}
    </div>`;
  $('#atest').onclick = () => invoke('alerts_test').then(() => flash('Toast отправлен'), e => flash('Toast error: ' + e));
  $('#aack').onclick = () => invoke('alerts_ack');
  $('#aclear').onclick = () => invoke('alerts_clear');
}

// ---------- DigitalOcean ----------
function renderDo(m) {
  const sn = S.doSnap; const d = sn?.droplet, dm = sn?.droplet_metrics, db = sn?.db, bm = sn?.db_metrics;
  const tokenSet = !!S.config?.digitalocean.token;
  const mcard = (label, v, sub = '') => `<div class="card metric"><div class="top"><span class="label">${label}</span><span class="sub">${sub}</span></div><span class="v">${v}</span></div>`;
  const num = v => v == null ? '—' : Number(v).toFixed(2);
  const mbps = v => v == null ? '—' : `${Number(v).toFixed(2)} Mbit/s`;
  m.innerHTML = `<header class="bar"><div class="col" style="gap:2px"><h1>DigitalOcean</h1><span class="sub">${sn?.fetched_at_unix ? `updated ${ago(sn.fetched_at_unix * 1000)} · every ${S.config?.digitalocean.poll_seconds}s` : 'no data yet'}</span></div><div class="spacer"></div>
    <button class="btn" id="dorefresh">Refresh</button></header>
    <div class="content">
      ${!tokenSet ? `<div class="note">Задайте API token, droplet id и database id в Settings (или в windows/config.local.toml).</div>` : ''}
      ${sn?.errors?.length ? `<div class="err">${sn.errors.map(esc).join('\n')}</div>` : ''}
      <div class="row"><h2 class="sec" style="flex-grow:1">Droplet</h2>${d ? pill(d.status === 'active' ? 'ok' : 'crit', d.status) : ''}</div>
      ${d ? `<div class="card sidepanel"><div class="kv"><span class="k">Name</span><span class="v">${esc(d.name)} <span class="muted">#${d.id}</span></span><span class="k">Region</span><span class="v">${esc(d.region)}</span><span class="k">Size</span><span class="v">${d.vcpus} vCPU · ${bytes(d.memory_mb * 1048576)} · ${d.disk_gb} GB disk</span><span class="k">Public IPv4</span><span class="v mono">${esc(d.public_ipv4 || '—')}</span></div></div>
        <div class="grid3">${mcard('CPU', pct(dm?.cpu_pct))}${mcard('Memory', pct(dm?.mem_pct))}${mcard('Disk', pct(dm?.disk_used_pct))}${mcard('Load 1 m', num(dm?.load1))}${mcard('Network in', mbps(dm?.net_in_mbps))}${mcard('Network out', mbps(dm?.net_out_mbps))}</div>`
        : `<div class="card empty">Нет данных по дроплету</div>`}
      <div class="row"><h2 class="sec" style="flex-grow:1">Managed database</h2>${db ? pill(db.status === 'online' ? 'ok' : 'crit', db.status) : ''}</div>
      ${db ? `<div class="card sidepanel"><div class="kv"><span class="k">Name</span><span class="v">${esc(db.name)}</span><span class="k">Engine</span><span class="v">${esc(db.engine)} ${esc(db.version)}</span><span class="k">Size</span><span class="v">${esc(db.size)} · ${db.num_nodes} node(s)</span><span class="k">Region</span><span class="v">${esc(db.region)}</span><span class="k">Host</span><span class="v mono">${esc(db.host || '—')}</span></div></div>
        <div class="grid3">${mcard('CPU', pct(bm?.cpu_pct))}${mcard('Memory', pct(bm?.mem_pct))}${mcard('Disk', pct(bm?.disk_pct))}${mcard('Connections', bm?.connections == null ? '—' : Math.round(bm.connections))}${mcard('Load 1 m', num(bm?.load1))}</div>`
        : `<div class="card empty">Нет данных по базе</div>`}
    </div>`;
  $('#dorefresh').onclick = async () => { $('#dorefresh').disabled = true; try { S.doSnap = await invoke('do_refresh'); } catch (e) { flash(e); } render(); };
}

// ---------- Settings ----------
function renderSettings(m) {
  const c = S.config; const st = S.status || {};
  m.innerHTML = `<header class="bar"><h1>Settings</h1><div class="spacer"></div><button class="btn primary" id="save">Save</button></header>
  <div class="content">
    <section class="card settings-sec"><h2>DigitalOcean</h2><div class="form">
      <label>API token (read)</label><input id="do_token" type="password" value="${esc(c.digitalocean.token)}" autocomplete="off">
      <label>Droplet id</label><div class="row"><input id="do_droplet" type="number" value="${c.digitalocean.droplet_id}" style="width:200px"><select id="do_droplet_pick" style="display:none"></select></div>
      <label>Database id</label><div class="row"><input id="do_db" value="${esc(c.digitalocean.database_id)}" style="width:320px"><select id="do_db_pick" style="display:none"></select></div>
      <label>Poll every (s)</label><input id="do_poll" type="number" min="15" value="${c.digitalocean.poll_seconds}" style="width:120px">
      <span></span><div class="row"><button class="btn small" id="do_load">Загрузить список из аккаунта</button><span class="muted" id="do_msg"></span></div>
    </div></section>
    <section class="card settings-sec"><h2>Alert thresholds</h2><div class="form">
      <label>CPU %</label><div class="row"><input id="a_cpu" type="number" value="${c.alerts.cpu_pct}" style="width:100px"><span class="muted">дольше</span><input id="a_cpu_min" type="number" value="${c.alerts.cpu_for_minutes}" style="width:80px"><span class="muted">мин</span></div>
      <label>RAM %</label><input id="a_mem" type="number" value="${c.alerts.mem_pct}" style="width:100px">
      <label>Disk used %</label><input id="a_disk" type="number" value="${c.alerts.disk_pct}" style="width:100px">
      <label>DB CPU %</label><input id="a_dbcpu" type="number" value="${c.alerts.db_cpu_pct}" style="width:100px">
      <label>DB disk %</label><input id="a_dbdisk" type="number" value="${c.alerts.db_disk_pct}" style="width:100px">
      <label>Agent unreachable (min)</label><input id="a_unreach" type="number" value="${c.alerts.unreachable_minutes}" style="width:100px">
      <label>Toast cooldown (min)</label><input id="a_cd" type="number" value="${c.alerts.cooldown_minutes}" style="width:100px">
      <label>Windows notifications</label><label style="color:var(--text)"><input id="a_notif" type="checkbox" ${c.alerts.notifications ? 'checked' : ''} style="height:auto"> включены</label>
    </div></section>
    <section class="card settings-sec"><h2>This PC</h2>
      <div class="kv"><span class="k">Fleet</span><span class="v">${esc(st.fleet_name || '—')} · ${esc(st.phase || '')}</span>
      <span class="k">Data / keys</span><span class="v"><span class="mono">${esc(st.data_dir || "")}</span> <button class="btn small" id="opendata">Open folder</button></span>
      <span class="k">Config</span><span class="v mono">${esc(st.config_path || '')}</span>
      <span class="k">Recovery code</span><span class="v mono">${esc((st.data_dir || '') + '\\recovery.txt')}</span></div>
      <div class="err">Папка data = root-доступ к серверу (ключи). Передавайте её только по защищённому каналу.</div>
      <div class="col"><span class="secondary" style="font-size:12px">SSH public key этого приложения (добавьте в /root/.ssh/authorized_keys на сервере):</span>
      <div class="keybox"><code id="sshkey">${esc(st.ssh_public_key || '—')}</code><button class="btn" id="copykey">Copy</button></div></div>
    </section>
  </div>`;
  $('#copykey').onclick = () => copy(st.ssh_public_key || '');
  $('#opendata').onclick = () => invoke('open_data_dir').catch(e => flash(e));
  $('#do_load').onclick = async () => {
    $('#do_msg').textContent = 'loading…';
    try {
      const r = await invoke('do_list', { token: $('#do_token').value.trim() });
      const dp = $('#do_droplet_pick'), bp = $('#do_db_pick');
      dp.innerHTML = '<option value="">— droplet —</option>' + r.droplets.map(d => `<option value="${d.id}">${esc(d.name)} · ${esc(d.region)} · ${esc(d.status)}</option>`).join('');
      bp.innerHTML = '<option value="">— database —</option>' + r.databases.map(d => `<option value="${esc(d.id)}">${esc(d.name)} · ${esc(d.engine)} · ${esc(d.status)}</option>`).join('');
      dp.style.display = bp.style.display = '';
      dp.onchange = () => { if (dp.value) $('#do_droplet').value = dp.value; };
      bp.onchange = () => { if (bp.value) $('#do_db').value = bp.value; };
      $('#do_msg').textContent = `${r.droplets.length} droplets, ${r.databases.length} databases`;
    } catch (e) { $('#do_msg').textContent = e; }
  };
  $('#save').onclick = async () => {
    const n = (id, d) => { const v = Number($(id).value); return isNaN(v) ? d : v; };
    const cfg = { digitalocean: { token: $('#do_token').value.trim(), droplet_id: Math.max(0, Math.floor(n('#do_droplet', 0))), database_id: $('#do_db').value.trim(), poll_seconds: Math.max(15, Math.floor(n('#do_poll', 60))) },
      alerts: { cpu_pct: n('#a_cpu', 90), cpu_for_minutes: Math.max(0, Math.floor(n('#a_cpu_min', 5))), mem_pct: n('#a_mem', 90), disk_pct: n('#a_disk', 85), db_cpu_pct: n('#a_dbcpu', 90), db_disk_pct: n('#a_dbdisk', 85),
        unreachable_minutes: Math.max(1, Math.floor(n('#a_unreach', 2))), cooldown_minutes: Math.max(0, Math.floor(n('#a_cd', 30))), notifications: $('#a_notif').checked } };
    $('#save').disabled = true;
    try { await invoke('save_config', { config: cfg }); S.config = cfg; S.doSnap = await invoke('do_snapshot'); flash('Сохранено'); } catch (e) { flash(e); }
    $('#save').disabled = false;
  };
}

// ---------- Add server wizard ----------
let W = null;
function openWizard(server) {
  W = { step: server ? (server.host_key_pinned ? 3 : 2) : 1, id: server?.id || null, name: server?.name || '', host: server?.host || '', port: server?.port || 22, user: server?.user || 'root',
    mode: 'mykey', keyPath: '', hostFps: null, auto: false, usePassword: false, password: '', prompt: null, artifact: S.status?.default_deb || '', progress: [], busy: false, error: '', existing: false };
  drawWizard();
}
function closeWizard() { if (W?.password) W.password = ''; W = null; $('#modal-root').innerHTML = ''; refreshServers().then(render); }
function drawWizard() {
  if (!W) return;
  const st = S.status || {};
  const steps = ['Сервер', 'Host key', 'Агент'].map((l, i) => `<span class="${W.step === i + 1 ? 'on' : ''}">${i + 1}. ${l}</span>`).join('<span>›</span>');
  let body = '';
  if (W.step === 1) body = `
    <div class="form">
      <label>Name</label><input id="w_name" value="${esc(W.name)}" placeholder="prod-1">
      <label>Host / IP</label><input id="w_host" value="${esc(W.host)}" placeholder="203.0.113.10">
      <label>Port</label><input id="w_port" type="number" value="${W.port}" style="width:100px">
      <label>User</label><input id="w_user" value="${esc(W.user)}">
    </div>
    <div class="col" style="gap:10px">
      <label class="row" style="font-size:12px"><input type="radio" name="w_mode" value="mykey" ${W.mode === 'mykey' ? 'checked' : ''} style="height:auto"> <b>Добавить ключ приложения через мой SSH-ключ</b> (рекомендуется)</label>
      ${W.mode === 'mykey' ? `<div class="row" style="padding-left:22px"><input id="w_keypath" value="${esc(W.keyPath)}" placeholder="пусто = ключи по умолчанию (~/.ssh/id_*) и ssh-agent" style="flex-grow:1"><button class="btn" id="w_pickkey">Browse…</button></div>
      <div class="muted" style="padding-left:22px;font-size:12px">Windows OpenSSH подключится вашим ключом и допишет ключ приложения в authorized_keys. Хост должен быть в вашем known_hosts.</div>` : ''}
      <label class="row" style="font-size:12px"><input type="radio" name="w_mode" value="manual" ${W.mode === 'manual' ? 'checked' : ''} style="height:auto"> Я добавлю ключ вручную</label>
      ${W.mode === 'manual' ? `<div class="col" style="padding-left:22px">
        <div class="keybox"><code>${esc(st.ssh_public_key || '—')}</code><button class="btn" id="w_copy">Copy</button></div>
        <div class="keybox"><code>echo '${esc(st.ssh_public_key || '')}' >> ~/.ssh/authorized_keys</code><button class="btn" id="w_copycmd">Copy cmd</button></div></div>` : ''}
      <label class="row" style="font-size:12px"><input type="radio" name="w_mode" value="password" ${W.mode === 'password' ? 'checked' : ''} style="height:auto"> Одноразовый пароль (не сохраняется)</label>
    </div>`;
  if (W.step === 2) body = W.prompt ? `
    <div>Сверьте отпечаток host key сервера (на сервере: <span class="mono">ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</span>):</div>
    <div class="fp">${esc(W.prompt.algorithm)}<br>${esc(W.prompt.fingerprint)}</div>
    ${W.prompt.target_unpinned ? '' : '<div class="note">Ключ уже закреплён.</div>'}`
    : `<div class="secondary">Подключение к ${esc(W.host || '')} ${W.usePassword ? '(только host key, без авторизации)' : '(с ключом приложения)'}…</div>`;
  if (W.step === 3) body = `
    <div class="note">Режим безопасности: <b>Agent-only</b> — Fleet не трогает firewall, баны и authorized_keys. Других изменений на сервере приложение не делает.</div>
    <div class="form"><label>Agent package (.deb)</label><div class="row"><input id="w_art" value="${esc(W.artifact)}" style="flex-grow:1"><button class="btn" id="w_pick">Browse…</button></div>
    ${W.usePassword ? `<label>Password (one-time)</label><input id="w_pw" type="password" autocomplete="off" placeholder="пароль ${esc(W.user)} — для добавления ключа">` : ''}</div>
    ${W.progress.length ? `<div class="progress">${W.progress.map(esc).join('<br>')}</div>` : ''}
    ${W.existing ? `<div class="row"><span class="secondary">Агент уже стоит:</span><button class="btn" id="w_adopt">Adopt (этот fleet)</button><button class="btn danger" id="w_replace">Replace</button></div>` : ''}`;
  if (W.step === 4) body = `<div class="note">Агент установлен и подключён.</div>${W.progress.length ? `<div class="progress">${W.progress.map(esc).join('<br>')}</div>` : ''}`;
  const actions = {
    1: `<button class="btn primary" id="w_next" ${W.busy ? 'disabled' : ''}>Далее</button>`,
    2: W.prompt ? `<button class="btn" id="w_reject">Отклонить</button><button class="btn primary" id="w_accept">Подтвердить отпечаток</button>` : `<button class="btn" id="w_back" ${W.busy ? 'disabled' : ''}>Назад</button><button class="btn" id="w_retry" ${W.busy ? 'disabled' : ''}>Повторить</button>`,
    3: `<button class="btn primary" id="w_install" ${W.busy ? 'disabled' : ''}>${W.busy ? 'Установка…' : 'Install agent (Agent-only)'}</button>`,
    4: `<button class="btn primary" id="w_done">Готово</button>`,
  }[W.step];
  $('#modal-root').innerHTML = `<div class="modal-bg"><div class="modal">
    <div class="row"><h2 style="flex-grow:1">Add server</h2><button class="btn small" id="w_close">✕</button></div>
    <div class="steps">${steps}</div>
    ${body}
    ${W.error ? `<div class="err">${esc(W.error)}</div>` : ''}
    <div class="row" style="justify-content:flex-end">${actions}</div></div></div>`;
  $('#w_close').onclick = closeWizard;
  const on = (id, f) => { const el = $(id); if (el) el.onclick = f; };
  on('#w_copy', () => copy(st.ssh_public_key || ''));
  on('#w_copycmd', () => copy(`echo '${st.ssh_public_key}' >> ~/.ssh/authorized_keys`));
  const readStep1 = () => {
    const v = id => { const el = $(id); return el ? el.value.trim() : null; };
    if (v('#w_name') != null) { W.name = v('#w_name'); W.host = v('#w_host'); W.port = Number(v('#w_port')) || 22; W.user = v('#w_user') || 'root'; }
    if (v('#w_keypath') != null) W.keyPath = v('#w_keypath');
  };
  document.querySelectorAll('input[name=w_mode]').forEach(r => r.onchange = () => { readStep1(); W.mode = r.value; W.usePassword = r.value === 'password'; drawWizard(); });
  on('#w_pickkey', async () => { const p = await invoke('pick_key'); if (p) { W.keyPath = p; $('#w_keypath').value = p; } });
  on('#w_next', async () => {
    readStep1();
    if (!W.name || !W.host) { W.error = 'Name и host обязательны'; return drawWizard(); }
    W.busy = true; W.error = ''; W.hostFps = null; W.auto = false; drawWizard();
    try {
      if (W.mode === 'mykey') {
        // Before the record exists: a failure leaves nothing behind.
        const r = await invoke('push_app_key', { host: W.host, port: W.port, user: W.user, keyPath: W.keyPath || null });
        W.hostFps = r.host_fingerprints; W.auto = true;
      }
      if (!W.id) W.id = await invoke('add_server', { name: W.name, host: W.host, port: W.port, user: W.user });
      W.step = 2; W.busy = false; probe();
    } catch (e) { W.busy = false; W.error = String(e); drawWizard(); }
  });
  on('#w_retry', probe);
  // Back to step 1: drop the half-added record so host/port can change.
  on('#w_back', async () => { if (W.id) { try { await invoke('remove_server', { id: W.id }); } catch { } W.id = null; } W.step = 1; W.error = ''; drawWizard(); });
  on('#w_reject', async () => { await invoke('reject_host_key', { id: W.id }); closeWizard(); });
  on('#w_accept', async () => {
    try { await invoke('accept_host_key', { id: W.id, fingerprint: W.prompt.fingerprint, jumps: W.prompt.jumps }); W.step = 3; W.error = ''; }
    catch (e) { W.error = String(e); W.auto = false; }
    drawWizard();
    // "My SSH key" path: go straight on to the install.
    if (W.auto && W.step === 3) { W.auto = false; $('#w_install')?.click(); }
  });
  on('#w_pick', async () => { const p = await invoke('pick_deb', { start: $('#w_art').value }); if (p) { W.artifact = p; $('#w_art').value = p; } });
  const install = async existing => {
    W.artifact = $('#w_art').value.trim(); const pwEl = $('#w_pw'); if (pwEl) W.password = pwEl.value;
    if (W.usePassword && !W.password) { W.error = 'Нужен пароль (или снимите галку и добавьте ключ вручную)'; return drawWizard(); }
    W.busy = true; W.error = ''; W.existing = false; W.progress = ['Starting…']; drawWizard();
    try {
      const h = await invoke('install_agent', { id: W.id, artifact: W.artifact, password: W.usePassword ? W.password : null, addKey: W.usePassword, existing });
      W.password = ''; W.progress.push(`Agent ${h.agent_version} healthy`); W.step = 4;
    } catch (e) {
      W.error = String(e); W.existing = String(e).startsWith('EXISTING_AGENT');
    }
    W.busy = false; drawWizard(); refreshServers();
  };
  on('#w_install', () => install('ask'));
  on('#w_adopt', () => install('adopt'));
  on('#w_replace', () => { if (confirm('Заменить существующий агент (его состояние будет архивировано)?')) install('replace'); });
  on('#w_done', () => { const id = W.id; closeWizard(); openServer(id); });
}
async function probe() {
  W.busy = true; W.error = ''; W.prompt = null; drawWizard();
  try { W.prompt = await invoke('probe_host_key', { id: W.id, withAuth: !W.usePassword }); }
  catch (e) { W.error = String(e); W.auto = false; }
  W.busy = false; drawWizard();
  // The fingerprints came over the user's known_hosts-verified ssh session:
  // a match pins without asking; anything else stays a manual comparison.
  const p = W.prompt;
  if (W.auto && p && p.target_unpinned && !p.jumps.length) {
    if (W.hostFps?.includes(p.fingerprint)) $('#w_accept')?.click();
    else { W.auto = false; W.error = 'Отпечаток не совпал с ключами, прочитанными через ваш SSH — сверьте вручную.'; drawWizard(); }
  }
}

// ---------- events ----------
let pendingServers = null;
function scheduleServers() { if (pendingServers) return; pendingServers = setTimeout(async () => { pendingServers = null; await refreshServers(); if (S.page === 'fleet') render(); }, 400); }

listen('status', async () => { await refreshStatus(); await refreshServers(); if (S.page !== 'server') render(); });
listen('servers', scheduleServers);
listen('metrics', p => {
  const s = S.servers.find(x => x.id === p.id); if (s) { s.cpu = p.cpu; s.mem = p.mem; s.disk = p.disk; s.last_metrics_ms = Date.now(); }
  if (S.page === 'fleet') render(); else renderSide();
  if (S.page === 'server' && S.tab === 'overview' && S.serverId === p.id) drawHistory(p.id);
});
listen('detail-catalog', c => { S.detail.catalog = Object.fromEntries(c.map(x => [x.id, x.name])); S.detail.values = {}; });
listen('detail-sample', smp => {
  for (const [id, v] of smp.values) { const n = S.detail.catalog[id]; if (n) S.detail.values[n] = v; }
  const f = detailFigures();
  pushSeries('cpu', f.cpu); pushSeries('mem', f.mem); pushSeries('load', f.load1); pushSeries('rx', f.rx); pushSeries('disk', f.disk); pushSeries('swap', f.swap);
  if (S.page === 'server' && S.tab === 'overview') drawCards();
});
listen('detail-status', st => { S.detail.status = st; const el = $('#dstatus'); if (el) el.textContent = st; });
listen('journal-entries', page => { S.logs.push(...page.entries); if (S.logs.length > 3000) S.logs.splice(0, S.logs.length - 3000); drawLogs(false); });
listen('journal-status', st => { S.logStatus = st; const el = $('#lstatus'); if (el) el.textContent = st; });
listen('install-progress', p => { if (!W) return; W.progress.push(p.total ? `${p.step} ${Math.round(p.done / p.total * 100)}%` : p.step); if (p.step === 'Uploading' && W.progress.length > 1 && W.progress[W.progress.length - 2].startsWith('Uploading')) W.progress.splice(W.progress.length - 2, 1); drawWizard(); });
listen('install-artifact', a => { if (!W) return; W.progress.push(`Artifact ${a.name} (${a.arch}) blake3 ${a.blake3.slice(0, 16)}…`); drawWizard(); });
listen('host-key', p => { if (W && W.id === p.server_id && W.step === 2) { W.prompt = p; drawWizard(); } });
listen('alerts', async () => { await refreshAlerts(); if (S.page === 'alerts' || S.page === 'fleet') render(); });
listen('do-snapshot', snap => { S.doSnap = snap; if (S.page === 'do' || S.page === 'fleet') render(); else renderSide(); });

document.querySelectorAll('.nav-item[data-page]').forEach(el => el.onclick = () => go(el.dataset.page));
$('#nav-add').onclick = () => openWizard();
setInterval(() => { if (S.page === 'fleet') render(); }, 30000);

(async () => {
  S.config = await invoke('get_config');
  await refreshStatus(); await refreshServers(); await refreshAlerts();
  S.doSnap = await invoke('do_snapshot');
  render();
})();
