/* ============================================================
   OpenCode2API Console — Application Logic
   Inspired by ds2api layout & behavior
   ============================================================ */

/* ==========================================================
   1. State
   ========================================================== */
var state = {
  status: null,
  proxies: [],
  config: null,
  activeView: 'overview',
  sse: null,
  reconnectAttempts: 0,
  reconnectTimeout: null,
  authenticated: false,
  bridgeToken: null,
  proxyRestarting: new Set(),
  configEditMode: false, // false = view/form, true = raw TOML editor
  eventCount: 0,
  testStreaming: false,
  statusReceivedAtMs: null,
};

/* ==========================================================
   2. DOM Utilities
   ========================================================== */
function $(sel, parent) { return (parent || document).querySelector(sel); }
function $$(sel, parent) { return (parent || document).querySelectorAll(sel); }

function escapeHtml(str) {
  if (str == null) return '';
  var div = document.createElement('div');
  div.textContent = String(str);
  return div.innerHTML;
}

function escapeToml(str) {
  if (str == null) return '';
  return String(str).replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n');
}

function tomlStringLine(key, value) {
  return key + ' = "' + escapeToml(value) + '"';
}

function parsePositiveInt(value, fallback, allowZero) {
  var n = parseInt(String(value == null ? '' : value).trim(), 10);
  if (!Number.isFinite(n) || Number.isNaN(n)) return fallback;
  if (allowZero ? n < 0 : n <= 0) return fallback;
  return n;
}

function listFromTextarea(value) {
  return String(value || '')
    .split(/[\n,]+/)
    .map(function(x) { return x.trim(); })
    .filter(Boolean);
}

function formatUptime(seconds) {
  if (seconds == null || seconds < 0) return '--';
  var d = Math.floor(seconds / 86400);
  var h = Math.floor((seconds % 86400) / 3600);
  var m = Math.floor((seconds % 3600) / 60);
  var s = Math.floor(seconds % 60);
  var parts = [];
  if (d > 0) parts.push(d + 'd');
  if (h > 0) parts.push(h + 'h');
  if (m > 0) parts.push(m + 'm');
  parts.push(s + 's');
  return parts.join(' ');
}

function liveUptimeSeconds(data) {
  data = data || state.status;
  if (!data || data.uptime_secs == null) return null;
  var base = Number(data.uptime_secs);
  if (!Number.isFinite(base) || base < 0) return null;
  var receivedAt = state.statusReceivedAtMs || Date.now();
  var delta = Math.max(0, Math.floor((Date.now() - receivedAt) / 1000));
  return base + delta;
}

function updateLiveUptime() {
  var live = liveUptimeSeconds();
  if (live == null) return;
  var text = formatUptime(live);

  var metricUptime = document.getElementById('metricUptimeValue');
  if (metricUptime) metricUptime.textContent = text;

  var sidebarUptime = document.getElementById('sidebarUptime');
  if (sidebarUptime) sidebarUptime.textContent = text;
}


function formatModelLabel(model) {
  if (!model) return '--';
  var name = String(model).trim();
  if (!name) return '--';

  // Dashboard cards are narrow; keep the full model available via tooltip,
  // but display the useful model id without provider/noise suffixes.
  var parts = name.split('/');
  name = parts[parts.length - 1] || name;
  name = name.replace(/-free$/i, '');
  name = name.replace(/^(claude-|openai-|opencode-)/i, '');

  return name || model;
}

function getTimestamp() {
  return new Date().toLocaleTimeString('en-US', { hour12: false });
}

function capitalize(str) {
  return str ? str.charAt(0).toUpperCase() + str.slice(1) : '';
}

function dotClass(status) {
  if (!status) return 'dot-muted';
  var s = String(status).toLowerCase();
  if (s === 'healthy' || s === 'online' || s === 'ok' || s === 'active') return 'dot-ok';
  if (s === 'degraded' || s === 'cooldown' || s === 'warning') return 'dot-warn';
  if (s === 'dead' || s === 'offline' || s === 'error' || s === 'failed') return 'dot-error';
  return 'dot-muted';
}

/* ==========================================================
   3. Toast Notifications
   ========================================================== */
function showToast(message, type) {
  type = type || 'info';
  var container = document.getElementById('toastContainer');
  var toast = document.createElement('div');
  toast.className = 'toast toast-' + type;

  var icons = {
    error: '<circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/>',
    success: '<path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/>',
    warning: '<path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/>',
    info: '<circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/>'
  };

  toast.innerHTML =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' + (icons[type] || icons.info) + '</svg>' +
    '<span class="toast-message">' + escapeHtml(message) + '</span>' +
    '<button class="toast-close">&times;</button>';

  toast.querySelector('.toast-close').onclick = function() { dismissToast(toast); };
  container.appendChild(toast);
  setTimeout(function() { dismissToast(toast); }, 5000);
}

function dismissToast(toast) {
  if (toast.classList.contains('toast-exit')) return;
  toast.classList.add('toast-exit');
  setTimeout(function() { toast.remove(); }, 200);
}

/* ==========================================================
   4. API Client — uses HttpOnly session cookie (no JS token)
   ========================================================== */
function readCookie(name) {
  var prefix = name + '=';
  var parts = document.cookie ? document.cookie.split(';') : [];
  for (var i = 0; i < parts.length; i++) {
    var item = parts[i].trim();
    if (item.indexOf(prefix) === 0) return decodeURIComponent(item.slice(prefix.length));
  }
  return '';
}

function addCsrfHeader(headers, method) {
  method = (method || 'GET').toUpperCase();
  if (method === 'GET' || method === 'HEAD' || method === 'OPTIONS') return;
  var token = readCookie('bridge_csrf_token');
  if (token) headers['X-CSRF-Token'] = token;
}

async function apiFetch(url, options) {
  options = options || {};
  var maxRetries = options.retries != null ? options.retries : 3;
  var delay = options.delay != null ? options.delay : 2000;
  var headers = options.headers || {};
  headers['Accept'] = 'application/json';
  addCsrfHeader(headers, options.method || 'GET');

  if (options.body && typeof options.body === 'string') {
    headers['Content-Type'] = 'application/x-toml';
  }

  var lastErr;
  for (var attempt = 0; attempt < maxRetries; attempt++) {
    try {
      var resp = await fetch(url, {
        method: options.method || 'GET',
        headers: headers,
        credentials: 'same-origin',
        body: options.body || undefined,
      });
      if (!resp.ok) {
        var text = await resp.text().catch(function() { return ''; });
        var err = new Error('HTTP ' + resp.status + ': ' + (text || resp.statusText));
        err.status = resp.status;
        throw err;
      }
      return await resp.json();
    } catch (err) {
      lastErr = err;
      if (err.status === 401 || (err.message && err.message.indexOf('HTTP 401') !== -1)) {
        showLoginScreen();
        throw err; // Stop retrying immediately on auth failures
      }
      if (attempt < maxRetries - 1) {
        await new Promise(function(r) { setTimeout(r, delay); });
      }
    }
  }
  throw lastErr;
}

/* ==========================================================
   5. Authentication & Login (server-managed HttpOnly cookie)
   ========================================================== */
function checkAuthStatus(statusData) {
  if (!statusData) return;
  
  // If server does not require token — session cookie is still set,
  // but the server allows unauthenticated access
  if (!statusData.admin_token_configured) {
    hideLoginScreen();
    document.getElementById('logoutBtn').style.display = 'none';
    return;
  }
  
  // If we got here, the server returned 200 for /api/dashboard/status
  // which means the HttpOnly session cookie was accepted.
  state.authenticated = true;
  hideLoginScreen();
  document.getElementById('logoutBtn').style.display = 'flex';
  loadConfig();
  loadProxies();
  connectSSE();
}

async function checkDashboardSession() {
  try {
    var resp = await fetch('/api/dashboard/auth/status', {
      credentials: 'same-origin'
    });
    var data = await resp.json();
    return data;
  } catch(e) {
    return { admin_token_configured: false, authenticated: false };
  }
}

function showLoginScreen() {
  window.location.href = '/';
}

function hideLoginScreen() {
  var overlay = document.getElementById('loginOverlay');
  var layout = document.getElementById('appLayout');
  overlay.classList.add('hidden');
  layout.classList.remove('blur');
}

async function handleLoginSubmit(e) {
  e.preventDefault();
  var tokenInput = document.getElementById('loginToken');
  var token = tokenInput.value.trim();
  if (!token) return;
  
  var submitBtn = document.getElementById('loginSubmitBtn');
  submitBtn.disabled = true;
  submitBtn.querySelector('span').textContent = 'Verifying...';
  
  try {
    // Login via JSON body; server sets HttpOnly session cookie on success
    var resp = await fetch('/api/dashboard/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Accept': 'application/json' },
      body: JSON.stringify({ token: token }),
      credentials: 'same-origin',
    });
    if (resp.ok) {
      state.authenticated = true;
      showToast('Logged in successfully', 'success');
      hideLoginScreen();
      document.getElementById('logoutBtn').style.display = 'flex';
      loadStatus();
      loadConfig();
      loadProxies();
      connectSSE();
    } else {
      showToast('Invalid admin key', 'error');
    }
  } catch (err) {
    showToast('Login verification failed: ' + err.message, 'error');
  } finally {
    submitBtn.disabled = false;
    submitBtn.querySelector('span').textContent = 'Sign In';
  }
}

async function handleLogout() {
  try {
    var headers = {};
    addCsrfHeader(headers, 'POST');
    await fetch('/api/dashboard/logout', {
      method: 'POST',
      headers: headers,
      credentials: 'same-origin'
    });
  } catch (e) { /* ignore */ }
  state.authenticated = false;
  showToast('Logged out', 'info');
  window.location.href = '/';
}

/* ==========================================================
   6. Data Loaders
   ========================================================== */
async function loadStatus() {
  try {
    var data = await apiFetch('/api/dashboard/status');
    state.status = data;
    state.statusReceivedAtMs = Date.now();
    renderMetrics(data);
    updateLiveUptime();
    renderSidebarStatus(data);
    setConnection('connected');
    
    // Auto populate tester model default
    var testerModelInput = document.getElementById('testerModel');
    if (testerModelInput && data.model && !testerModelInput.value) {
      testerModelInput.value = data.model;
    }
    
    return data;
  } catch (err) {
    console.error('loadStatus failed:', err);
    // Don't show toast for simple missing token (prompting for login on first load)
    if (err.status !== 401 || state.authenticated) {
      showToast('Failed to load status: ' + err.message, 'error');
    }
    setConnection('error');
  }
}

async function loadProxies() {
  try {
    var data = await apiFetch('/api/dashboard/proxies');
    state.proxies = data;
    if (state.activeView === 'overview') renderOverviewProxyTable(data);
    if (state.activeView === 'proxies') renderProxyDetailTable(data);
    renderProxySummary(data);
    return data;
  } catch (err) {
    console.error('loadProxies failed:', err);
    showToast('Failed to load proxies: ' + err.message, 'error');
  }
}

async function loadConfig() {
  try {
    var data = await apiFetch('/api/dashboard/config');
    state.config = data;
    if (state.activeView === 'config') renderConfig(data);
    return data;
  } catch (err) {
    console.error('loadConfig failed:', err);
    showToast('Failed to load config: ' + err.message, 'error');
  }
}

async function saveConfig(content) {
  try {
    var headers = { 'Content-Type': 'application/json' };
    addCsrfHeader(headers, 'POST');
    var resp = await fetch('/api/dashboard/config/save', {
      method: 'POST',
      headers: headers,
      body: JSON.stringify({ content: content }),
      credentials: 'same-origin',
    });
    var data = await resp.json();
    if (!resp.ok) {
      showToast(data.message || 'Save failed (' + resp.status + ')', 'error');
    } else if (data.success) {
      showToast('Configuration saved and merged', 'success');
    } else {
      showToast(data.message || 'Save failed', 'error');
    }
    return data;
  } catch (err) {
    showToast('Failed to save: ' + err.message, 'error');
    throw err;
  }
}

async function restartProxy(port) {
  if (state.proxyRestarting.has(port)) return;
  state.proxyRestarting.add(port);
  updateRestartButtons();

  try {
    var data = await apiFetch('/api/dashboard/proxy/' + port + '/restart', {
      method: 'POST',
      retries: 1,
    });
    if (data.status === 'error') {
      showToast(data.message, 'error');
      appendEvent('error', 'Proxy ' + port + ': ' + data.message);
    } else {
      showToast('Proxy ' + port + ' restarted', 'success');
      appendEvent('success', 'Proxy ' + port + ' restart initiated');
    }
    await loadProxies();
  } catch (err) {
    showToast('Restart failed: ' + err.message, 'error');
    appendEvent('error', 'Proxy ' + port + ' restart failed');
  } finally {
    state.proxyRestarting.delete(port);
    updateRestartButtons();
  }
}

/* ==========================================================
   7. Connection Status Banner
   ========================================================== */
function setConnection(s) {
  var badge = document.getElementById('systemStatusBadge');
  if (!badge) return;
  badge.className = 'status-badge ' + s;
  var text = badge.querySelector('.status-badge-text');
  if (text) {
    if (s === 'connected') text.textContent = 'Online';
    else if (s === 'connecting') text.textContent = 'Connecting';
    else text.textContent = 'Offline';
  }
}

function renderSidebarStatus(data) {
  if (!data) return;
  var primary = data.primary_proxies || {};
  var standby = data.warm_standby || {};

  var sidebarPrimary = document.getElementById('sidebarPrimaryCount');
  if (sidebarPrimary) {
    sidebarPrimary.textContent = (primary.healthy || 0) + '/' + (primary.total || 0);
  }

  var sidebarStandby = document.getElementById('sidebarStandbyCount');
  if (sidebarStandby) {
    sidebarStandby.textContent = (standby.healthy || 0) + '/' + (standby.total || 0);
  }

  var sidebarVersion = document.getElementById('sidebarVersion');
  if (sidebarVersion) {
    sidebarVersion.textContent = data.version || '---';
  }

  var sidebarUptime = document.getElementById('sidebarUptime');
  if (sidebarUptime) {
    sidebarUptime.textContent = formatUptime(liveUptimeSeconds(data));
  }
}

/* ==========================================================
   8. Render — Metrics Cards
   ========================================================== */
function renderMetrics(data) {
  data = data || state.status;
  if (!data) return;

  var grid = document.getElementById('metricsGrid');
  if (!grid) return;

  var primary = data.primary_proxies || {};
  var standby = data.warm_standby || {};

  var overallDot = 'dot-ok';
  if (primary.dead > 0) overallDot = 'dot-error';
  else if (primary.degraded > 0 || primary.cooldown > 0) overallDot = 'dot-warn';

  var svgStatus = '<svg class="metric-watermark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>';
  var svgModel = '<svg class="metric-watermark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/></svg>';
  var svgTime = '<svg class="metric-watermark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>';
  var svgPort = '<svg class="metric-watermark" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="2" width="20" height="8" rx="2"/><rect x="2" y="14" width="20" height="8" rx="2"/></svg>';

  var modelFullName = data.model || '--';
  var modelShortName = formatModelLabel(modelFullName);

  var cards = [
    {
      label: 'Bridge Status',
      value: '<span class="dot ' + overallDot + '"></span>' + escapeHtml(capitalize(data.status || 'unknown')),
      accent: overallDot === 'dot-ok' ? 'accent-green' : (overallDot === 'dot-error' ? 'accent-red' : 'accent-amber'),
      watermark: svgStatus
    },
    {
      label: 'Active Model',
      value: '<span class="model-short-name" title="' + escapeHtml(modelFullName) + '">' + escapeHtml(modelShortName) + '</span>',
      accent: 'accent-indigo model-card',
      watermark: svgModel
    },
    {
      label: 'Uptime',
      value: '<span id="metricUptimeValue">' + escapeHtml(formatUptime(liveUptimeSeconds(data))) + '</span>',
      accent: 'accent-plum',
      watermark: svgTime
    },
    {
      label: 'Bridge Port',
      value: escapeHtml(String(data.bridge_port || '--')),
      accent: '',
      watermark: svgPort
    },
    {
      label: 'Primary Proxies',
      value: '<span class="dot dot-ok"></span>' + (primary.healthy || 0) + ' <span class="sub">healthy</span>' +
             (primary.degraded > 0 ? '&nbsp; <span class="dot dot-warn"></span>' + primary.degraded + ' <span class="sub">degraded</span>' : '') +
             (primary.dead > 0 ? '&nbsp; <span class="dot dot-error"></span>' + primary.dead + ' <span class="sub">down</span>' : ''),
      accent: '',
      watermark: svgPort
    },
    {
      label: 'Warm Standby',
      value: '<span class="dot dot-ok"></span>' + (standby.healthy || 0) + ' <span class="sub">ready</span>' +
             (standby.degraded > 0 ? '&nbsp; <span class="dot dot-warn"></span>' + standby.degraded + ' <span class="sub">degraded</span>' : ''),
      accent: '',
      watermark: svgPort
    },
    {
      label: 'Authentication',
      value: data.auth_enabled ? 'Enabled' : 'Disabled',
      accent: '',
      watermark: svgStatus
    },
    {
      label: 'Shell Policy',
      value: escapeHtml(capitalize(data.shell_policy || '--')),
      accent: '',
      watermark: svgModel
    },
  ];

  var html = '';
  for (var i = 0; i < cards.length; i++) {
    var c = cards[i];
    html += '<div class="metric-card ' + (c.accent || '') + '">';
    html += c.watermark;
    html += '<div>';
    html += '<div class="metric-label">' + escapeHtml(c.label) + '</div>';
    html += '<div class="metric-value">' + c.value + '</div>';
    html += '</div>';
    html += '</div>';
  }
  grid.innerHTML = html;
}

/* ==========================================================
   9. Render — Proxy Tables
   ========================================================== */
function proxyTableHtml(proxies, showActions) {
  if (!proxies || proxies.length === 0) {
    return '<div class="card-empty">No proxy data available</div>';
  }

  var html = '<table class="data-table">';
  html += '<thead><tr><th>Port</th><th>Role</th><th>Lifecycle</th><th>Status</th><th>Failures</th><th>Successes</th><th>Cooldown</th>';
  if (showActions) html += '<th>Actions</th>';
  html += '</tr></thead><tbody>';

  for (var i = 0; i < proxies.length; i++) {
    var p = proxies[i];
    var dc = dotClass(p.status);
    var role = (p.role || '').toLowerCase();
    var roleClass = role === 'warmstandby' ? 'warmstandby' : 'primary';
    var cooldown = p.cooldown_remaining_secs || 0;
    var isRestarting = state.proxyRestarting.has(p.port);

    html += '<tr>';
    html += '<td><code>' + p.port + '</code></td>';
    html += '<td><span class="role-badge ' + roleClass + '">' + escapeHtml(p.role || 'unknown') + '</span></td>';
    html += '<td>' + escapeHtml(p.lifecycle || '--') + '</td>';
    html += '<td><div class="status-cell"><span class="dot ' + dc + '"></span>' + escapeHtml(p.status || 'unknown') + '</div></td>';
    html += '<td>' + (p.failure_count != null ? p.failure_count : '--') + '</td>';
    html += '<td>' + (p.success_count != null ? p.success_count : '--') + '</td>';
    html += '<td>' + (cooldown > 0 ? cooldown + 's' : '--') + '</td>';
    if (showActions) {
      html += '<td>';
      html += '<button class="btn btn-secondary btn-sm' + (isRestarting ? ' restarting' : '') + '" data-restart="' + p.port + '"' + (isRestarting ? ' disabled' : '') + '>';
      html += '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><polyline points="1 20 1 14 7 14"/><path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/></svg>';
      html += '<span>' + (isRestarting ? 'Restarting...' : 'Restart') + '</span>';
      html += '</button></td>';
    }
    html += '</tr>';
  }

  html += '</tbody></table>';
  return html;
}

function renderOverviewProxyTable(proxies) {
  var el = document.getElementById('overviewProxyTable');
  if (el) el.innerHTML = proxyTableHtml(proxies || state.proxies, false);
}

function renderProxyDetailTable(proxies) {
  var el = document.getElementById('proxyDetailTable');
  if (el) el.innerHTML = proxyTableHtml(proxies || state.proxies, true);
}

function renderProxySummary(proxies) {
  var el = document.getElementById('overviewProxyBadge');
  if (!el) return;
  var healthy = 0, total = proxies ? proxies.length : 0;
  for (var i = 0; i < total; i++) {
    if (proxies[i].status === 'healthy') healthy++;
  }
  el.textContent = healthy + '/' + total + ' Healthy';
}

function updateRestartButtons() {
  var btns = document.querySelectorAll('[data-restart]');
  for (var i = 0; i < btns.length; i++) {
    var btn = btns[i];
    var port = parseInt(btn.getAttribute('data-restart'), 10);
    var isR = state.proxyRestarting.has(port);
    btn.classList.toggle('restarting', isR);
    btn.disabled = isR;
    var span = btn.querySelector('span');
    if (span) {
      span.textContent = isR ? 'Restarting...' : 'Restart';
    }
  }
}

/* ==========================================================
   10. Render — Configuration Form (ds2api style)
   ========================================================== */

// Normalize a proxy value to an array regardless of whether
// the backend sent it as an array or a joined string.
function toArray(v) {
  if (Array.isArray(v)) return v;
  if (typeof v === 'string') return v.split(',').map(function(x) { return x.trim(); }).filter(Boolean);
  return [];
}

function configToToml(config) {
  var lines = [
    tomlStringLine('host', config.host || '127.0.0.1'),
    'port = ' + Number(config.bridge_port || 4000),
    tomlStringLine('model', config.model || ''),
    tomlStringLine('shell_policy', config.shell_policy || 'disabled'),
    'max_body_size = ' + Number(config.max_body_size != null ? config.max_body_size : 10485760),
    'stream_buffer_size = ' + Number(config.stream_buffer_size || 4096),
    'channel_capacity = ' + Number(config.channel_capacity || 256),
    'max_search_loops = ' + Number(config.max_search_loops || 10),
  ];
  if (config.shell_allowlist) {
    lines.push(tomlStringLine('shell_allowlist', config.shell_allowlist));
  }
  if (config.searxng_url) {
    lines.push(tomlStringLine('searxng_url', config.searxng_url));
  }
  // Handle proxies as arrays (backend now returns them as arrays)
  var primaryProxies = toArray(config.primary_proxies);
  primaryProxies.forEach(function(p) {
    lines.push(tomlStringLine('primary_proxies', p));
  });
  var warmStandbyProxies = toArray(config.warm_standby_proxies);
  warmStandbyProxies.forEach(function(p) {
    lines.push(tomlStringLine('warm_standby_proxies', p));
  });
  lines.push('');
  lines.push('# Secrets (API keys, auth tokens) are omitted for security.');
  lines.push('# Re-enter them in the settings form to update.');
  return lines.join('\n');
}

async function loadRawConfig() {
  try {
    var resp = await fetch('/api/dashboard/config/raw', {
      credentials: 'same-origin'
    });
    var data = await resp.json();
    return data.raw || '';
  } catch(e) {
    console.error('loadRawConfig failed:', e);
    return '';
  }
}

function renderConfig(config) {
  config = config || state.config;
  if (!config) { loadConfig(); return; }

  var el = document.getElementById('configPanel');
  var actions = document.getElementById('configActions');
  if (!el) return;

  if (!state.configEditMode) {
    // Render Basic Configuration Form
    var html = '<form id="basicConfigForm" class="config-editor" style="display: flex; flex-direction: column; gap: 20px;">';
    
    // Server Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Server & Model Settings</div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgHost">Listen Host</label>';
    html += '      <input type="text" id="cfgHost" value="' + escapeHtml(config.host || '127.0.0.1') + '" required>';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgPort">Listen Port</label>';
    html += '      <input type="number" id="cfgPort" value="' + (config.bridge_port || 4000) + '" required>';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-group">';
    html += '    <label for="cfgModel">Default Model Identifier</label>';
    html += '    <input type="text" id="cfgModel" list="availableModels" value="' + escapeHtml(config.model || 'opencode/deepseek-v4-flash-free') + '" required>';
    html += '    <p class="form-help">Model forwarded upstream when none is requested.</p>';
    html += '  </div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgShellPolicy">Shell Interception Policy</label>';
    html += '      <select id="cfgShellPolicy">';
    html += '        <option value="disabled"' + (config.shell_policy === 'disabled' ? ' selected' : '') + '>Disabled</option>';
    html += '        <option value="allowlist"' + (config.shell_policy === 'allowlist' ? ' selected' : '') + '>Allow List Only</option>';
    html += '        <option value="unrestricted"' + (config.shell_policy === 'unrestricted' ? ' selected' : '') + '>Unrestricted</option>';
    html += '      </select>';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-group">';
    html += '    <label for="cfgShellAllowlist">Shell Allowlist</label>';
    html += '    <input type="text" id="cfgShellAllowlist" value="' + escapeHtml(config.shell_allowlist || '') + '" placeholder="git,ls,pwd,cat">';
    html += '    <p class="form-help">Used only when Shell Interception Policy is Allow List Only.</p>';
    html += '  </div>';
    html += '</div>';

    // Runtime tuning Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Runtime Limits</div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgMaxBodySize">Max Body Size Bytes</label>';
    html += '      <input type="number" id="cfgMaxBodySize" min="0" value="' + (config.max_body_size != null ? config.max_body_size : 10485760) + '">';
    html += '      <p class="form-help">0 disables request body limit.</p>';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgMaxSearchLoops">Max Search Loops</label>';
    html += '      <input type="number" id="cfgMaxSearchLoops" min="1" value="' + (config.max_search_loops || 10) + '">';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgStreamBufferSize">Stream Buffer Size</label>';
    html += '      <input type="number" id="cfgStreamBufferSize" min="1" value="' + (config.stream_buffer_size || 4096) + '">';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgChannelCapacity">SSE Channel Capacity</label>';
    html += '      <input type="number" id="cfgChannelCapacity" min="1" value="' + (config.channel_capacity || 256) + '">';
    html += '    </div>';
    html += '  </div>';
    html += '</div>';

    // Search integrations Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Web Search Integrations</div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgTavily">Tavily API Key</label>';
    html += '      <input type="password" id="cfgTavily" placeholder="' + (config.tavily_api_key_configured ? '•••••••• (Saved)' : 'Enter Tavily API key') + '">';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgExa">Exa API Key</label>';
    html += '      <input type="password" id="cfgExa" placeholder="' + (config.exa_api_key_configured ? '•••••••• (Saved)' : 'Enter Exa API key') + '">';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgSerper">Serper.dev API Key</label>';
    html += '      <input type="password" id="cfgSerper" placeholder="' + (config.serper_api_key_configured ? '•••••••• (Saved)' : 'Enter Serper API key') + '">';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgSearxng">SearXNG Instance URL</label>';
    html += '      <input type="text" id="cfgSearxng" value="' + escapeHtml(config.searxng_url || '') + '" placeholder="http://searxng.local">';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-group">';
    html += '    <label for="cfgSearxngApiKey">SearXNG API Key</label>';
    html += '    <input type="password" id="cfgSearxngApiKey" placeholder="' + (config.searxng_api_key_configured ? '•••••••• (Saved)' : 'Optional SearXNG API key') + '">';
    html += '  </div>';
    html += '</div>';

    // Proxy topology Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Proxy Topology</div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgPrimaryProxies">Primary Proxies</label>';
    html += '      <textarea id="cfgPrimaryProxies" rows="3" placeholder="socks5://127.0.0.1:40001">' + escapeHtml(toArray(config.primary_proxies).join('\n')) + '</textarea>';
    html += '      <p class="form-help">One proxy per line or comma-separated.</p>';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgWarmStandbyProxies">Warm-Standby Proxies</label>';
    html += '      <textarea id="cfgWarmStandbyProxies" rows="3" placeholder="socks5://127.0.0.1:40005">' + escapeHtml(toArray(config.warm_standby_proxies).join('\n')) + '</textarea>';
    html += '      <p class="form-help">Protected standby pool; dashboard restart avoids these.</p>';
    html += '    </div>';
    html += '  </div>';
    html += '</div>';

    // Security Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Security & Token Policies</div>';
    html += '  <div class="form-group">';
    html += '    <label for="cfgAuthTokens">Authorized Bearer Tokens</label>';
    html += '    <input type="password" id="cfgAuthTokens" placeholder="' + (config.auth_tokens_configured ? '•••••••• (Tokens configured)' : 'Comma-separated authorized keys (e.g. sk-1, sk-2)') + '">';
    html += '    <p class="form-help">Require clients to authenticate via Bearer token to use bridge. Blank preserves existing tokens; edit raw TOML to remove them.</p>';
    html += '  </div>';
    html += '</div>';

    // Form button footer
    html += '<div class="config-bar" style="border-top: 1px solid var(--border); margin-top: 10px; margin-left: -24px; margin-right: -24px; margin-bottom: -24px;">';
    html += '  <button type="submit" class="btn btn-primary">Save Settings</button>';
    html += '</div>';

    html += '</form>';
    
    el.innerHTML = html;

    // Hook basic form submission
    document.getElementById('basicConfigForm').onsubmit = handleBasicConfigSave;

    if (actions) {
      actions.innerHTML =
        '<button class="btn btn-secondary" id="editRawConfigBtn">' +
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>' +
        'Edit Raw TOML</button>';
    }

  } else {
    // Edit mode (Raw TOML editor)
    // Load raw config from backend to get full file content (including secrets)
    loadRawConfig().then(function(raw) {
      el.innerHTML =
        '<div class="config-editor"><textarea id="configTextarea">' + escapeHtml(raw || configToToml(config)) + '</textarea></div>' +
        '<div class="config-bar">' +
        '<button class="btn btn-primary" id="saveRawConfigBtn">Save Config</button>' +
        '<button class="btn btn-secondary" id="cancelEditBtn">Cancel</button>' +
        '</div>';
      if (actions) actions.innerHTML = '';
    });
    return;
  }
}

async function handleBasicConfigSave(e) {
  e.preventDefault();
  
  var host = document.getElementById('cfgHost').value.trim();
  var port = parsePositiveInt(document.getElementById('cfgPort').value, 4000, false);
  var model = document.getElementById('cfgModel').value.trim();
  var shellPolicy = document.getElementById('cfgShellPolicy').value;
  var shellAllowlist = document.getElementById('cfgShellAllowlist').value.trim();
  var searxng = document.getElementById('cfgSearxng').value.trim();
  var maxBodySize = parsePositiveInt(document.getElementById('cfgMaxBodySize').value, 10485760, true);
  var maxSearchLoops = parsePositiveInt(document.getElementById('cfgMaxSearchLoops').value, 10, false);
  var streamBufferSize = parsePositiveInt(document.getElementById('cfgStreamBufferSize').value, 4096, false);
  var channelCapacity = parsePositiveInt(document.getElementById('cfgChannelCapacity').value, 256, false);
  var primaryProxies = listFromTextarea(document.getElementById('cfgPrimaryProxies').value);
  var warmStandbyProxies = listFromTextarea(document.getElementById('cfgWarmStandbyProxies').value);
  
  var tavily = document.getElementById('cfgTavily').value;
  var exa = document.getElementById('cfgExa').value;
  var serper = document.getElementById('cfgSerper').value;
  var searxngApiKey = document.getElementById('cfgSearxngApiKey').value;
  var authTokens = document.getElementById('cfgAuthTokens').value.trim();

  // Re-generate TOML dynamically based on inputs. Omitted secrets are preserved by backend merge.
  if (!host || !model) {
    showToast('Host and model are required', 'error');
    return;
  }
  var tomlLines = [
    '# OpenCode2API Configuration Settings',
    tomlStringLine('host', host),
    'port = ' + port,
    tomlStringLine('model', model),
    tomlStringLine('shell_policy', shellPolicy),
    'max_body_size = ' + maxBodySize,
    'stream_buffer_size = ' + streamBufferSize,
    'channel_capacity = ' + channelCapacity,
    'max_search_loops = ' + maxSearchLoops,
  ];
  
  if (shellAllowlist) tomlLines.push(tomlStringLine('shell_allowlist', shellAllowlist));
  if (searxng) tomlLines.push(tomlStringLine('searxng_url', searxng));
  primaryProxies.forEach(function(p) { tomlLines.push(tomlStringLine('primary_proxies', p)); });
  warmStandbyProxies.forEach(function(p) { tomlLines.push(tomlStringLine('warm_standby_proxies', p)); });
  
  // Secrets: only include if user explicitly typed a new value
  if (tavily) tomlLines.push('tavily_api_key = "' + escapeToml(tavily) + '"');
  if (exa) tomlLines.push('exa_api_key = "' + escapeToml(exa) + '"');
  if (serper) tomlLines.push('serper_api_key = "' + escapeToml(serper) + '"');
  if (searxngApiKey) tomlLines.push(tomlStringLine('searxng_api_key', searxngApiKey));
  if (authTokens) tomlLines.push(tomlStringLine('auth_tokens', authTokens));

  var fullToml = tomlLines.join('\n');
  
  try {
    var saveBtn = e.target.querySelector('button[type="submit"]');
    saveBtn.disabled = true;
    saveBtn.textContent = 'Saving...';
    
    await saveConfig(fullToml);
    await loadConfig();
    await loadStatus();
  } catch (err) {
    // Already handled by toast in saveConfig
  } finally {
    var saveBtn = document.querySelector('#basicConfigForm button[type="submit"]');
    if (saveBtn) {
      saveBtn.disabled = false;
      saveBtn.textContent = 'Save Settings';
    }
  }
}

/* ==========================================================
   11. API Tester (Model Testing & Chat streaming)
   ========================================================== */
async function handleTesterSubmit(e) {
  e.preventDefault();
  return runTesterStream(false);
}

async function handleSyntheticTesterClick(e) {
  e.preventDefault();
  return runTesterStream(true);
}

async function runTesterStream(useSynthetic) {
  if (state.testStreaming) return;
  
  var model = document.getElementById('testerModel').value.trim();
  var temp = parseFloat(document.getElementById('testerTemp').value || '0.7');
  var tokens = parseInt(document.getElementById('testerTokens').value || '2048', 10);
  var prompt = document.getElementById('testerPrompt').value.trim();
  
  if (!prompt) return;
  if (!useSynthetic && !model) return;

  state.testStreaming = true;
  var submitBtn = document.getElementById('testerSubmitBtn');
  var syntheticBtn = document.getElementById('testerSyntheticBtn');
  submitBtn.disabled = true;
  if (syntheticBtn) syntheticBtn.disabled = true;
  submitBtn.querySelector('span').textContent = useSynthetic ? 'Waiting...' : 'Generating...';
  if (syntheticBtn) syntheticBtn.querySelector('span').textContent = useSynthetic ? 'Streaming...' : 'Synthetic disabled...';

  var outputArea = document.getElementById('testResponseArea');
  outputArea.innerHTML = '';
  
  var latencyBadge = document.getElementById('testLatencyBadge');
  latencyBadge.textContent = 'STREAMING • connecting';
  latencyBadge.classList.remove('hidden');
  latencyBadge.classList.add('streaming');

  var startTime = Date.now();
  var firstTokenTime = null;
  var streamStats = {
    chunks: 0,
    events: 0,
    thinkingChars: 0,
    textChars: 0,
    phase: 'connecting'
  };

  function updateStreamingBadge(phase) {
    if (phase) streamStats.phase = phase;
    var elapsed = Date.now() - startTime;
    latencyBadge.textContent = 'STREAMING • ' + streamStats.phase
      + ' • think ' + streamStats.thinkingChars
      + ' • text ' + streamStats.textChars
      + ' • ' + elapsed + ' ms';
  }

  // Render collapsible thinking process box
  var thinkingBox = null;
  var thinkingContent = null;
  var renderQueue = [];
  var renderTimer = null;
  var renderDrainResolver = null;
  
  function getOrCreateThinkingBox() {
    if (thinkingBox) return thinkingContent;
    
    thinkingBox = document.createElement('div');
    thinkingBox.className = 'thinking-box';
    
    var header = document.createElement('div');
    header.className = 'thinking-header';
    header.textContent = 'Thinking Process';
    header.onclick = function() {
      thinkingBox.classList.toggle('collapsed');
    };
    
    thinkingContent = document.createElement('div');
    thinkingContent.className = 'thinking-content';
    
    thinkingBox.appendChild(header);
    thinkingBox.appendChild(thinkingContent);
    outputArea.appendChild(thinkingBox);
    
    return thinkingContent;
  }

  function scheduleRenderDrain() {
    if (renderTimer) return;
    renderTimer = setTimeout(drainRenderQueue, 16);
  }

  function appendRenderedText(kind, text) {
    if (!text) return;
    if (kind === 'thinking') {
      getOrCreateThinkingBox().appendChild(document.createTextNode(text));
    } else {
      outputArea.appendChild(document.createTextNode(text));
    }
  }

  function drainRenderQueue() {
    renderTimer = null;
    var started = performance.now();
    var charBudget = 120;

    while (renderQueue.length > 0 && charBudget > 0 && performance.now() - started < 12) {
      var item = renderQueue[0];
      if (!item.text) {
        renderQueue.shift();
        continue;
      }

      var takeLen = Math.min(item.text.length, charBudget);
      var piece = item.text.slice(0, takeLen);
      item.text = item.text.slice(takeLen);
      appendRenderedText(item.kind, piece);
      charBudget -= takeLen;

      if (!item.text) {
        renderQueue.shift();
      }
    }

    outputArea.scrollTop = outputArea.scrollHeight;

    if (renderQueue.length > 0) {
      scheduleRenderDrain();
    } else if (renderDrainResolver) {
      var resolve = renderDrainResolver;
      renderDrainResolver = null;
      resolve();
    }
  }

  function enqueueRenderedText(kind, text) {
    if (!text) return;
    renderQueue.push({ kind: kind, text: String(text) });
    scheduleRenderDrain();
  }

  function waitForRenderDrain() {
    if (renderQueue.length === 0 && !renderTimer) {
      return Promise.resolve();
    }
    return new Promise(function(resolve) {
      renderDrainResolver = resolve;
      scheduleRenderDrain();
    });
  }

  var url = useSynthetic ? '/api/dashboard/test/stream' : '/v1/messages';
  var headers = { 'Content-Type': 'application/json' };
  var body;

  if (useSynthetic) {
    body = {
      thinking: 'Synthetic thinking stream: I received the prompt, split the work into small steps, and will emit text after this thinking block.',
      text: 'Synthetic response stream is working. Prompt received: ' + prompt,
      delay_ms: 35
    };
  } else {
    if (state.bridgeToken) {
      headers['Authorization'] = 'Bearer ' + state.bridgeToken;
    }
    body = {
      model: model,
      messages: [
        { role: 'user', content: prompt }
      ],
      temperature: temp,
      max_tokens: tokens,
      stream: true
    };
  }

  var controller = new AbortController();
  var requestTimeout = null;

  try {
    updateStreamingBadge(useSynthetic ? 'synthetic requesting' : 'requesting');
    requestTimeout = setTimeout(function() {
      controller.abort();
    }, useSynthetic ? 15000 : 60000);
    var response = await fetch(url, {
      method: 'POST',
      headers: headers,
      credentials: 'same-origin',
      body: JSON.stringify(body),
      signal: controller.signal
    });
    clearTimeout(requestTimeout);
    requestTimeout = null;

    if (!response.ok) {
      var errText = await response.text();
      throw new Error('HTTP ' + response.status + ': ' + errText);
    }

    var reader = response.body.getReader();
    var decoder = new TextDecoder();
    var buffer = '';
    var currentEvent = null;
    updateStreamingBadge('connected');

    while (true) {
      var chunk = await reader.read();
      if (chunk.done) break;

      streamStats.chunks++;
      updateStreamingBadge('receiving');
      buffer += decoder.decode(chunk.value, { stream: true });
      var lines = buffer.split('\n');
      buffer = lines.pop(); // keep last partial line

      for (var i = 0; i < lines.length; i++) {
        var line = lines[i].trim();
        if (!line) continue;

        if (line.indexOf('event:') === 0) {
          currentEvent = line.substring(6).trim();
        } else if (line.indexOf('data:') === 0) {
          var dataStr = line.substring(5).trim();
          
          if (firstTokenTime === null) {
            firstTokenTime = Date.now();
            var ttft = firstTokenTime - startTime;
            latencyBadge.textContent = 'STREAMING • first event ' + ttft + ' ms';
          }

          try {
            var dataObj = JSON.parse(dataStr);
            streamStats.events++;
            handleSseEvent(currentEvent, dataObj);
          } catch (e) {
            console.error('Failed to parse SSE data:', dataStr, e);
          }
        }
      }
    }
    
    await waitForRenderDrain();
    var totalTime = Date.now() - startTime;
    latencyBadge.textContent = 'DONE • think ' + streamStats.thinkingChars
      + ' • text ' + streamStats.textChars
      + ' • total ' + totalTime + ' ms';
    showToast('Testing completed', 'success');
  } catch (err) {
    var message = err.name === 'AbortError'
      ? 'Request timed out before the stream started. Try Synthetic Stream, reduce Max Tokens, or check upstream/proxy logs.'
      : err.message;
    showToast('Test failed: ' + message, 'error');
    var errDiv = document.createElement('div');
    errDiv.style.color = 'var(--accent-red)';
    errDiv.style.marginTop = '10px';
    errDiv.textContent = 'Error: ' + message;
    outputArea.appendChild(errDiv);
  } finally {
    if (renderTimer) {
      clearTimeout(renderTimer);
      renderTimer = null;
    }
    renderQueue = [];
    if (requestTimeout) clearTimeout(requestTimeout);
    state.testStreaming = false;
    latencyBadge.classList.remove('streaming');
    submitBtn.disabled = false;
    if (syntheticBtn) syntheticBtn.disabled = false;
    submitBtn.querySelector('span').textContent = 'Run Prompt Test';
    if (syntheticBtn) syntheticBtn.querySelector('span').textContent = 'Run Synthetic Stream';
  }

  function handleSseEvent(event, data) {
    if (event === 'message_start') {
      updateStreamingBadge('message_start');
    } else if (event === 'content_block_start') {
      var blockType = data.content_block && data.content_block.type;
      updateStreamingBadge(blockType === 'thinking' ? 'streaming thinking' : 'streaming text');
    } else if (event === 'content_block_stop') {
      updateStreamingBadge('block closed');
    } else if (event === 'message_stop') {
      updateStreamingBadge('message_stop');
    }

    if (data.type === 'content_block_delta') {
      var delta = data.delta || {};
      
      // Support deepseek reasoning thinking tags
      if (delta.type === 'thinking_delta' || delta.thinking) {
        var thinkText = delta.thinking || '';
        streamStats.thinkingChars += thinkText.length;
        updateStreamingBadge('streaming thinking');
        enqueueRenderedText('thinking', thinkText);
      } else if (delta.type === 'text_delta' || delta.text) {
        var text = delta.text || '';
        streamStats.textChars += text.length;
        updateStreamingBadge('streaming text');
        enqueueRenderedText('text', text);
      }
    }
  }
}

/* ==========================================================
   12. Event Log Feed
   ========================================================== */
function appendEvent(level, message) {
  var log = document.getElementById('eventLog');
  if (!log) return;

  var item = document.createElement('div');
  item.className = 'event-item ' + (level || 'info');

  var ts = document.createElement('span');
  ts.className = 'ev-time';
  ts.textContent = getTimestamp();
  item.appendChild(ts);

  item.appendChild(document.createTextNode(message));
  log.appendChild(item);

  state.eventCount++;
  while (log.children.length > 100) {
    log.removeChild(log.firstChild);
  }

  log.scrollTop = log.scrollHeight;
}

function clearEvents() {
  var log = document.getElementById('eventLog');
  if (log) {
    log.innerHTML = '';
    state.eventCount = 0;
    appendEvent('system', 'Log buffer cleared');
  }
}

/* ==========================================================
   13. SSE Live Updates Connections
   ========================================================== */
function connectSSE() {
  if (state.sse) state.sse.close();

  var url = '/api/dashboard/events';

  setConnection('connecting');
  var es = new EventSource(url, { withCredentials: true });
  state.sse = es;

  es.onopen = function() {
    state.reconnectAttempts = 0;
    setConnection('connected');
    appendEvent('system', 'SSE pipe established');
  };

  es.addEventListener('proxy_status', function(e) {
    try {
      var data = JSON.parse(e.data);
      appendEvent('info', 'Proxy update: port ' + data.port + ' is ' + data.status);
      loadProxies();
      loadStatus();
    } catch (err) {
      console.error('SSE proxy_status parse error:', err);
    }
  });

  es.addEventListener('proxy_log', function(e) {
    try {
      var data = JSON.parse(e.data);
      appendEvent(data.level || 'info', '[Proxy] ' + (data.message || JSON.stringify(data)));
    } catch (err) {
      appendEvent('info', '[Proxy] ' + e.data);
    }
  });

  es.addEventListener('config_saved', function() {
    appendEvent('success', 'Configuration modified on disk');
    loadConfig();
    loadStatus();
  });

  es.onerror = function() {
    setConnection('error');
    appendEvent('error', 'SSE pipe disconnected');
    es.close();
    scheduleReconnect();
  };
}

function scheduleReconnect() {
  if (state.reconnectTimeout) clearTimeout(state.reconnectTimeout);
  state.reconnectAttempts++;
  var delay = Math.min(1000 * Math.pow(2, state.reconnectAttempts - 1), 30000);
  setConnection('connecting');
  appendEvent('system', 'Reconnecting in ' + (delay / 1000) + 's...');
  state.reconnectTimeout = setTimeout(connectSSE, delay);
}

/* ==========================================================
   14. View Router
   ========================================================== */
function switchView(viewId) {
  state.activeView = viewId;

  // Close mobile sidebar if open
  var sidebar = document.querySelector('.sidebar');
  var backdrop = document.getElementById('sidebarBackdrop');
  if (sidebar && backdrop) {
    sidebar.classList.remove('open');
    backdrop.classList.remove('active');
  }

  // Sidebar buttons active state
  var navBtns = $$('#sidebarNav .nav-btn');
  for (var i = 0; i < navBtns.length; i++) {
    navBtns[i].classList.toggle('active', navBtns[i].getAttribute('data-view') === viewId);
  }

  // Views display state
  var views = $$('.view');
  for (var j = 0; j < views.length; j++) {
    views[j].classList.toggle('active', views[j].id === 'view-' + viewId);
  }

  // Refresh target view's data
  if (viewId === 'overview') {
    renderMetrics();
    renderOverviewProxyTable();
  } else if (viewId === 'proxies') {
    renderProxyDetailTable();
  } else if (viewId === 'config') {
    state.configEditMode = false;
    renderConfig();
  } else if (viewId === 'tester') {
    // Populate test model
    var testerModelInput = document.getElementById('testerModel');
    if (testerModelInput && state.status && state.status.model) {
      testerModelInput.value = state.status.model;
    }
  }
}

/* ==========================================================
   15. Token Settings Modal (Sidebar backup config option)
   ========================================================== */
function showTokenModal() {
  var container = document.getElementById('modalContainer');
  container.innerHTML =
    '<div class="modal-overlay">' +
    '<div class="modal">' +
    '<div class="modal-title">Bridge API Bearer Token</div>' +
    '<div class="modal-desc">Configure your Bearer token to authorize Bridge API requests from the built-in tester.</div>' +
    '<input type="password" class="modal-input" id="tokenInput" placeholder="Bearer Token" value="' + escapeHtml(state.bridgeToken || '') + '" />' +
    '<div class="modal-actions">' +
    '<button class="btn btn-secondary" id="modalCancel">Cancel</button>' +
    '<button class="btn btn-primary" id="modalSave">Confirm</button>' +
    '</div>' +
    '</div>' +
    '</div>';

  var input = document.getElementById('tokenInput');

  document.getElementById('modalCancel').onclick = function() { container.innerHTML = ''; };

  document.getElementById('modalSave').onclick = function() {
    var tk = input.value.trim() || null;
    state.bridgeToken = tk;
    container.innerHTML = '';
    if (tk) {
      showToast('Bridge token registered', 'success');
    } else {
      showToast('Bridge token cleared', 'info');
    }
  };

  input.addEventListener('keydown', function(e) {
    if (e.key === 'Enter') document.getElementById('modalSave').click();
    if (e.key === 'Escape') document.getElementById('modalCancel').click();
  });

  setTimeout(function() { input.focus(); }, 80);
}

/* ==========================================================
   16. Events Binding
   ========================================================== */
function bindEvents() {
  // Login form submission
  document.getElementById('loginForm').addEventListener('submit', handleLoginSubmit);
  
  // Logout button
  document.getElementById('logoutBtn').addEventListener('click', handleLogout);

  // Sidebar navigation clicks
  document.getElementById('sidebarNav').addEventListener('click', function(e) {
    var btn = e.target.closest('.nav-btn');
    if (btn) switchView(btn.getAttribute('data-view'));
  });

  // API Tester Form submit
  document.getElementById('testerForm').addEventListener('submit', handleTesterSubmit);
  var syntheticBtn = document.getElementById('testerSyntheticBtn');
  if (syntheticBtn) syntheticBtn.addEventListener('click', handleSyntheticTesterClick);

  // Settings action
  document.getElementById('settingsBtn').addEventListener('click', showTokenModal);

  // Clear events logs
  document.getElementById('clearEventsBtn').addEventListener('click', clearEvents);

  // Refresh button handlers
  var refreshOverviewBtn = document.getElementById('refreshOverviewBtn');
  if (refreshOverviewBtn) {
    refreshOverviewBtn.onclick = function() {
      showToast('Refreshing overview...', 'info');
      loadStatus();
      loadProxies();
    };
  }

  var refreshProxiesBtn = document.getElementById('refreshProxiesBtn');
  if (refreshProxiesBtn) {
    refreshProxiesBtn.onclick = function() {
      showToast('Refreshing proxies...', 'info');
      loadProxies();
    };
  }

  var refreshConfigBtn = document.getElementById('refreshConfigBtn');
  if (refreshConfigBtn) {
    refreshConfigBtn.onclick = function() {
      showToast('Refreshing config...', 'info');
      loadConfig();
    };
  }

  // Mobile drawer trigger
  var mobileMenuBtn = document.getElementById('mobileMenuBtn');
  var sidebar = document.querySelector('.sidebar');
  var backdrop = document.getElementById('sidebarBackdrop');
  if (mobileMenuBtn && sidebar && backdrop) {
    mobileMenuBtn.onclick = function() {
      sidebar.classList.add('open');
      backdrop.classList.add('active');
    };
    backdrop.onclick = function() {
      sidebar.classList.remove('open');
      backdrop.classList.remove('active');
    };
  }

  // Main area delegated events
  document.getElementById('mainContent').addEventListener('click', function(e) {
    var restartBtn = e.target.closest('[data-restart]');
    if (restartBtn) {
      restartProxy(parseInt(restartBtn.getAttribute('data-restart'), 10));
      return;
    }

    if (e.target.id === 'editRawConfigBtn' || e.target.closest('#editRawConfigBtn')) {
      state.configEditMode = true;
      renderConfig();
      return;
    }

    if (e.target.id === 'cancelEditBtn') {
      state.configEditMode = false;
      renderConfig();
      return;
    }

    if (e.target.id === 'saveRawConfigBtn') {
      (async function() {
        var textarea = document.getElementById('configTextarea');
        if (textarea) {
          state.configEditMode = false;
          try {
            await saveConfig(textarea.value);
            await loadConfig();
            await loadStatus();
          } catch (err) {
            // Toast already shown by saveConfig
          }
        }
      })();
      return;
    }
  });
}

/* ==========================================================
   17. Bootstrapper
   ========================================================== */
function init() {
  bindEvents();

  // Verify session cookie first, then load data
  checkDashboardSession().then(function(auth) {
    if (!auth.authenticated) {
      window.location.href = '/';
      return;
    }
    hideLoginScreen();
    if (auth.admin_token_configured) {
      document.getElementById('logoutBtn').style.display = 'flex';
    }
    state.authenticated = auth.authenticated;
    loadStatus();
    loadProxies();
    loadConfig();
    connectSSE();
  });

  // Local live uptime ticker — no API refresh needed.
  setInterval(updateLiveUptime, 1000);

  // Auto-refresh stats/proxies every 30s
  setInterval(function() {
    var overlay = document.getElementById('loginOverlay');
    if (overlay.classList.contains('hidden')) {
      loadStatus();
      loadProxies();
    }
  }, 30000);

  appendEvent('system', 'Operations Console online');
  appendEvent('info', 'Heartbeat check every 30s');
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
