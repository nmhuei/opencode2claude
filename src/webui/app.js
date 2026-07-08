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
  token: null,
  proxyRestarting: new Set(),
  configEditMode: false, // false = view/form, true = raw TOML editor
  eventCount: 0,
  testStreaming: false,
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
   4. API Client
   ========================================================== */
async function apiFetch(url, options) {
  options = options || {};
  var maxRetries = options.retries != null ? options.retries : 3;
  var delay = options.delay != null ? options.delay : 2000;
  var headers = options.headers || {};
  headers['Accept'] = 'application/json';

  if (state.token) {
    headers['X-Dashboard-Token'] = state.token;
    headers['Authorization'] = 'Bearer ' + state.token;
  }
  if (options.body && typeof options.body === 'string') {
    headers['Content-Type'] = 'application/x-toml';
  }

  var lastErr;
  for (var attempt = 0; attempt < maxRetries; attempt++) {
    try {
      var resp = await fetch(url, {
        method: options.method || 'GET',
        headers: headers,
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

function getCookie(name) {
  var value = "; " + document.cookie;
  var parts = value.split("; " + name + "=");
  if (parts.length === 2) return parts.pop().split(";").shift();
}

function setCookie(name, value, days) {
  var expires = "";
  if (days) {
    var date = new Date();
    date.setTime(date.getTime() + (days * 24 * 60 * 60 * 1000));
    expires = "; expires=" + date.toUTCString();
  }
  document.cookie = name + "=" + (value || "")  + expires + "; path=/; SameSite=Strict";
}

function deleteCookie(name) {
  document.cookie = name + '=; Max-Age=-99999999; path=/; SameSite=Strict';
}

/* ==========================================================
   5. Authentication & Login
   ========================================================== */
function checkAuthStatus(statusData) {
  if (!statusData) return;
  
  // If server does not require token
  if (!statusData.admin_token_configured) {
    hideLoginScreen();
    document.getElementById('logoutBtn').style.display = 'none';
    return;
  }
  
  // Check if we have token
  var savedToken = getCookie('bridge_admin_token');
  if (savedToken) {
    state.token = savedToken;
    verifySavedToken();
  } else {
    showLoginScreen();
  }
}

async function verifySavedToken() {
  try {
    var data = await apiFetch('/api/dashboard/login', { method: 'POST', retries: 1 });
    if (data.success) {
      hideLoginScreen();
      document.getElementById('logoutBtn').style.display = 'flex';
      // Load other data since we are authenticated now
      loadConfig();
      loadProxies();
      connectSSE();
    } else {
      showLoginScreen();
    }
  } catch (e) {
    showLoginScreen();
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
    // Temporarily save to state to run query
    state.token = token;
    var data = await apiFetch('/api/dashboard/login', { method: 'POST', retries: 1 });
    if (data.success) {
      setCookie('bridge_admin_token', token, 365);
      showToast('Logged in successfully', 'success');
      hideLoginScreen();
      document.getElementById('logoutBtn').style.display = 'flex';
      loadStatus();
      loadConfig();
      loadProxies();
      connectSSE();
    } else {
      state.token = null;
      showToast('Invalid admin key', 'error');
    }
  } catch (err) {
    state.token = null;
    showToast('Login verification failed: ' + err.message, 'error');
  } finally {
    submitBtn.disabled = false;
    submitBtn.querySelector('span').textContent = 'Sign In';
  }
}

function handleLogout() {
  deleteCookie('bridge_admin_token');
  localStorage.removeItem('bridge_admin_token');
  sessionStorage.removeItem('bridge_admin_token');
  state.token = null;
  showToast('Logged out', 'info');
  showLoginScreen();
}

/* ==========================================================
   6. Data Loaders
   ========================================================== */
async function loadStatus() {
  try {
    var data = await apiFetch('/api/dashboard/status');
    state.status = data;
    renderMetrics(data);
    renderSidebarStatus(data);
    setConnection('connected');
    
    // Auto populate tester model default
    var testerModelInput = document.getElementById('testerModel');
    if (testerModelInput && data.model && !testerModelInput.value) {
      testerModelInput.value = data.model;
    }
    
    // Initial verification of login screen requirement
    checkAuthStatus(data);
    
    return data;
  } catch (err) {
    console.error('loadStatus failed:', err);
    // Don't show toast for simple missing token (prompting for login on first load)
    if (err.status !== 401 || state.token) {
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

async function saveConfig(tomlContent) {
  try {
    var data = await apiFetch('/api/dashboard/config/save', {
      method: 'POST',
      body: tomlContent,
      retries: 1,
    });
    if (data.status === 'error') {
      showToast(data.message || 'Save failed', 'error');
    } else {
      showToast('Configuration saved', 'success');
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
    sidebarUptime.textContent = formatUptime(data.uptime_secs);
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

  var cards = [
    {
      label: 'Bridge Status',
      value: '<span class="dot ' + overallDot + '"></span>' + escapeHtml(capitalize(data.status || 'unknown')),
      accent: overallDot === 'dot-ok' ? 'accent-green' : (overallDot === 'dot-error' ? 'accent-red' : 'accent-amber'),
      watermark: svgStatus
    },
    {
      label: 'Active Model',
      value: escapeHtml(data.model || '--'),
      accent: 'accent-indigo',
      watermark: svgModel
    },
    {
      label: 'Uptime',
      value: escapeHtml(formatUptime(data.uptime_secs)),
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
    html += '</div>';

    // Search integrations Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Web Search Integrations</div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgTavily">Tavily API Key</label>';
    html += '      <input type="password" id="cfgTavily" placeholder="' + (config.tavily_api_key ? '•••••••• (Saved)' : 'Enter Tavily API key') + '">';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgExa">Exa API Key</label>';
    html += '      <input type="password" id="cfgExa" placeholder="' + (config.exa_api_key ? '•••••••• (Saved)' : 'Enter Exa API key') + '">';
    html += '    </div>';
    html += '  </div>';
    html += '  <div class="form-row">';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgSerper">Serper.dev API Key</label>';
    html += '      <input type="password" id="cfgSerper" placeholder="' + (config.serper_api_key ? '•••••••• (Saved)' : 'Enter Serper API key') + '">';
    html += '    </div>';
    html += '    <div class="form-group flex-1">';
    html += '      <label for="cfgSearxng">SearXNG Instance URL</label>';
    html += '      <input type="text" id="cfgSearxng" value="' + escapeHtml(config.searxng_url || '') + '" placeholder="http://searxng.local">';
    html += '    </div>';
    html += '  </div>';
    html += '</div>';

    // Security Group
    html += '<div style="display: flex; flex-direction: column; gap: 14px; border-top: 1px solid var(--border); padding-top: 20px;">';
    html += '<div class="config-section-title" style="margin-bottom: 4px;">Security & Token Policies</div>';
    html += '  <div class="form-group">';
    html += '    <label for="cfgAuthTokens">Authorized Bearer Tokens</label>';
    html += '    <input type="password" id="cfgAuthTokens" placeholder="' + (config.auth_tokens ? '•••••••• (Tokens configured)' : 'Comma-separated authorized keys (e.g. sk-1, sk-2)') + '">';
    html += '    <p class="form-help">Require clients to authenticate via Bearer token to use bridge. Leave blank to disable auth.</p>';
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
    var toml = configToToml(config);
    el.innerHTML =
      '<div class="config-editor"><textarea id="configTextarea">' + escapeHtml(toml) + '</textarea></div>' +
      '<div class="config-bar">' +
      '<button class="btn btn-primary" id="saveRawConfigBtn">Save Config</button>' +
      '<button class="btn btn-secondary" id="cancelEditBtn">Cancel</button>' +
      '</div>';

    if (actions) actions.innerHTML = '';
  }
}

async function handleBasicConfigSave(e) {
  e.preventDefault();
  
  var host = document.getElementById('cfgHost').value.trim();
  var port = parseInt(document.getElementById('cfgPort').value, 10);
  var model = document.getElementById('cfgModel').value.trim();
  var shellPolicy = document.getElementById('cfgShellPolicy').value;
  var searxng = document.getElementById('cfgSearxng').value.trim();
  
  var tavily = document.getElementById('cfgTavily').value;
  var exa = document.getElementById('cfgExa').value;
  var serper = document.getElementById('cfgSerper').value;
  var authTokens = document.getElementById('cfgAuthTokens').value.trim();

  // Re-generate TOML dynamically based on inputs
  var tomlLines = [
    '# OpenCode2API Configuration Settings',
    'host = "' + host + '"',
    'port = ' + port,
    'model = "' + model + '"',
    'shell_policy = "' + shellPolicy + '"',
  ];
  
  if (searxng) tomlLines.push('searxng_url = "' + searxng + '"');
  
  // Handlers for keys: use existing if input is placeholder/empty
  var cfg = state.config || {};
  
  var activeTavily = tavily ? tavily : (cfg.tavily_api_key || '');
  if (activeTavily) tomlLines.push('tavily_api_key = "' + activeTavily + '"');

  var activeExa = exa ? exa : (cfg.exa_api_key || '');
  if (activeExa) tomlLines.push('exa_api_key = "' + activeExa + '"');

  var activeSerper = serper ? serper : (cfg.serper_api_key || '');
  if (activeSerper) tomlLines.push('serper_api_key = "' + activeSerper + '"');

  var activeAuth = authTokens ? authTokens : '';
  if (activeAuth) {
    tomlLines.push('auth_tokens = "' + activeAuth + '"');
  } else if (cfg.auth_tokens && !authTokens) {
    // If not modified, write existing masked/original
    // Note: configuration endpoints store it as list. So we join if list.
    if (Array.isArray(cfg.auth_tokens)) {
      tomlLines.push('auth_tokens = "' + cfg.auth_tokens.join(',') + '"');
    }
  }

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
  if (state.testStreaming) return;
  
  var model = document.getElementById('testerModel').value.trim();
  var temp = parseFloat(document.getElementById('testerTemp').value || '0.7');
  var tokens = parseInt(document.getElementById('testerTokens').value || '2048', 10);
  var prompt = document.getElementById('testerPrompt').value.trim();
  
  if (!model || !prompt) return;

  state.testStreaming = true;
  var submitBtn = document.getElementById('testerSubmitBtn');
  submitBtn.disabled = true;
  submitBtn.querySelector('span').textContent = 'Generating...';

  var outputArea = document.getElementById('testResponseArea');
  outputArea.innerHTML = '';
  
  var latencyBadge = document.getElementById('testLatencyBadge');
  latencyBadge.textContent = '--- ms';
  latencyBadge.classList.remove('hidden');

  var startTime = Date.now();
  var firstTokenTime = null;

  // Render collapsible thinking process box
  var thinkingBox = null;
  var thinkingContent = null;
  
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

  // Anthropic compatibility endpoint
  var url = '/v1/messages';
  var headers = {
    'Content-Type': 'application/json',
  };
  if (state.token) {
    headers['Authorization'] = 'Bearer ' + state.token;
  }

  var body = {
    model: model,
    messages: [
      { role: 'user', content: prompt }
    ],
    temperature: temp,
    max_tokens: tokens,
    stream: true
  };

  try {
    var response = await fetch(url, {
      method: 'POST',
      headers: headers,
      body: JSON.stringify(body)
    });

    if (!response.ok) {
      var errText = await response.text();
      throw new Error('HTTP ' + response.status + ': ' + errText);
    }

    var reader = response.body.getReader();
    var decoder = new TextDecoder();
    var buffer = '';

    while (true) {
      var chunk = await reader.read();
      if (chunk.done) break;

      buffer += decoder.decode(chunk.value, { stream: true });
      var lines = buffer.split('\n');
      buffer = lines.pop(); // keep last partial line

      var currentEvent = null;

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
            latencyBadge.textContent = 'TTFT: ' + ttft + ' ms';
          }

          try {
            var dataObj = JSON.parse(dataStr);
            handleSseEvent(currentEvent, dataObj);
          } catch (e) {
            console.error('Failed to parse SSE data:', dataStr, e);
          }
        }
      }
    }
    
    var totalTime = Date.now() - startTime;
    latencyBadge.textContent = 'Total: ' + totalTime + ' ms';
    showToast('Testing completed', 'success');
  } catch (err) {
    showToast('Test failed: ' + err.message, 'error');
    var errDiv = document.createElement('div');
    errDiv.style.color = 'var(--accent-red)';
    errDiv.style.marginTop = '10px';
    errDiv.textContent = 'Error: ' + err.message;
    outputArea.appendChild(errDiv);
  } finally {
    state.testStreaming = false;
    submitBtn.disabled = false;
    submitBtn.querySelector('span').textContent = 'Run Prompt Test';
  }

  function handleSseEvent(event, data) {
    if (data.type === 'content_block_delta') {
      var delta = data.delta || {};
      
      // Support deepseek reasoning thinking tags
      if (delta.type === 'thinking_delta' || delta.thinking) {
        var thinkText = delta.thinking || '';
        var block = getOrCreateThinkingBox();
        block.appendChild(document.createTextNode(thinkText));
        outputArea.scrollTop = outputArea.scrollHeight;
      } else if (delta.type === 'text_delta' || delta.text) {
        var text = delta.text || '';
        // If there's an active thinking box, collapse it when text starts to save space
        if (thinkingBox && !thinkingBox.classList.contains('collapsed')) {
          thinkingBox.classList.add('collapsed');
        }
        
        outputArea.appendChild(document.createTextNode(text));
        outputArea.scrollTop = outputArea.scrollHeight;
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
  if (state.token) url += '?token=' + encodeURIComponent(state.token);

  setConnection('connecting');
  var es = new EventSource(url);
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
    '<div class="modal-title">Authentication Authorization Token</div>' +
    '<div class="modal-desc">Configure your Bearer token to authorize server modifications and establish authenticated event lines.</div>' +
    '<input type="password" class="modal-input" id="tokenInput" placeholder="Bearer Token" value="' + escapeHtml(state.token || '') + '" />' +
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
    state.token = tk;
    container.innerHTML = '';
    if (tk) {
      localStorage.setItem('bridge_admin_token', tk);
      showToast('Token registered', 'success');
      if (state.sse) state.sse.close();
      if (state.reconnectTimeout) clearTimeout(state.reconnectTimeout);
      state.reconnectAttempts = 0;
      connectSSE();
      loadConfig();
    } else {
      localStorage.removeItem('bridge_admin_token');
      showToast('Token cleared', 'info');
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
      var textarea = document.getElementById('configTextarea');
      if (textarea) {
        state.configEditMode = false;
        saveConfig(textarea.value).then(function() {
          loadConfig();
          loadStatus();
        });
      }
      return;
    }
  });
}

/* ==========================================================
   17. Bootstrapper
   ========================================================== */
function init() {
  bindEvents();

  // Load status first to discover if login screen is required
  loadStatus().then(function() {
    // If not blocked by login overlay, download other views
    var overlay = document.getElementById('loginOverlay');
    if (overlay.classList.contains('hidden')) {
      loadProxies();
      loadConfig();
    }
  });

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
