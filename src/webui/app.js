(function () {
  'use strict';

  var state = {
    view: 'dashboard',
    lang: localStorage.getItem('oc2api-dashboard-language') || 'en',
    status: null,
    metrics: {},
    workers: [],
    proxies: [],
    models: [],
    selectedModel: '',
    environment: null,
    apiKeys: [],
    apiKeySummary: {},
    apiKeyModels: [],
    selectedApiKey: null,
    selectedApiKeyOverviewId: null,
    sessionSecrets: {},
    lastGeneratedKey: null,
    clientConfig: null,
    eventItems: [],
    eventSource: null,
    testerAbort: null,
    uptimeBaseSeconds: 0,
    uptimeSyncedAt: 0,
    uptimePid: null,
    uptimeTimer: null,
    refreshTimer: null,
    lastUpdate: null,
    configRaw: '',
    historyItems: [],
    historyTotal: 0,
    historyLimit: 50,
    historyOffset: 0,
    historyStats: null,
    historyStorage: null,
    historySettings: null,
    selectedHistory: null,
    historyContent: {},
    historySearchTimer: null
  };

  var translations = {
    en: {
      dashboard: 'Dashboard', apiKeys: 'API Keys', models: 'Models', history: 'History', system: 'System',
      dashboardDescription: 'Observe bridge health, traffic and recent changes.',
      apiDescription: 'Manage client credentials and access policies.',
      modelsDescription: 'Select and test the current upstream model.',
      historyDescription: 'Inspect prompts, reasoning, responses and request execution details.',
      systemDescription: 'Inspect the server, proxies, security and maintenance tools.',
      drain: 'Drain', undrain: 'Undrain', draining: 'Draining', egressMode: 'Egress mode', activeRoute: 'Active route', proxySubsystem: 'Proxy subsystem'
    },
    vi: {
      localControlPlane: 'Bảng điều khiển cục bộ', dashboard: 'Tổng quan', apiKeys: 'API Key', models: 'Mô hình', history: 'Lịch sử', system: 'Hệ thống',
      dashboardDescription: 'Theo dõi trạng thái bridge, lưu lượng và các thay đổi gần đây.', apiDescription: 'Quản lý thông tin xác thực và chính sách truy cập của client.', modelsDescription: 'Chọn và kiểm tra mô hình upstream hiện tại.', historyDescription: 'Kiểm tra prompt, reasoning, response và chi tiết thực thi request.', systemDescription: 'Theo dõi server, proxy, bảo mật và công cụ bảo trì.',
      connecting: 'Đang kết nối', connected: 'Đã kết nối', reconnecting: 'Đang kết nối lại', unavailable: 'Không khả dụng', offline: 'Ngoại tuyến', online: 'Trực tuyến',
      signOut: 'Đăng xuất', refresh: 'Làm mới', restartBridge: 'Khởi động lại bridge', openLogs: 'Mở nhật ký', runDiagnostics: 'Chạy chẩn đoán',
      service: 'Dịch vụ', model: 'Mô hình', currentUpstream: 'Upstream hiện tại', requests: 'Yêu cầu', uptime: 'Thời gian chạy',
      systemStatus: 'Trạng thái hệ thống', systemStatusDescription: 'Các dịch vụ cốt lõi và thành phần runtime được bảo vệ.', viewSystem: 'Xem hệ thống', bridge: 'Bridge', apiAuthentication: 'Xác thực API', primaryProxies: 'Proxy chính', standbyProxies: 'Proxy dự phòng', circuitBreakers: 'Circuit breaker', backgroundWorkers: 'Worker nền',
      recentActivity: 'Hoạt động gần đây', recentActivityDescription: 'Các thay đổi có ý nghĩa từ control plane cục bộ.', clear: 'Xóa', waitingForEvents: 'Đang chờ sự kiện…', quickActions: 'Thao tác nhanh', quickActionsDescription: 'Các tác vụ thường dùng ngay trên trang tổng quan.', createApiKey: 'Tạo API key', testModel: 'Kiểm tra mô hình', viewLogs: 'Xem nhật ký',
      hotReloadNote: 'Thay đổi có hiệu lực ngay mà không cần khởi động lại bridge.', checkApiKey: 'Kiểm tra API key', clientApiKeys: 'API key của client', clientApiKeysDescription: 'Bấm vào một dòng để xem hoặc cập nhật chính sách truy cập.', search: 'Tìm kiếm', status: 'Trạng thái', allStatuses: 'Tất cả trạng thái', active: 'Hoạt động', disabled: 'Đã tắt', expired: 'Hết hạn', searchKeyPlaceholder: 'Tên hoặc fingerprint', loadingApiKeys: 'Đang tải API key…',
      currentModel: 'Mô hình hiện tại', modelRestartRequired: 'Mô hình cấu hình đã thay đổi. Khởi động lại bridge để áp dụng.', availableModels: 'Mô hình khả dụng', availableModelsDescription: 'Chọn một mô hình upstream miễn phí cho bridge.', reload: 'Tải lại', searchModelPlaceholder: 'Tìm mô hình', loadingModels: 'Đang tải mô hình…', testModelDescription: 'Chạy kiểm tra suy luận nhiều bước và streaming qua endpoint tương thích OpenAI.', idle: 'Chờ', prompt: 'Prompt', thinking: 'Suy luận', streamResponse: 'Trả lời dạng stream', runTest: 'Chạy kiểm tra', response: 'Phản hồi', reasoning: 'Suy luận', noResponseYet: 'Chưa có phản hồi.', noReasoningYet: 'Chưa có suy luận.',
      server: 'Server', serverDescription: 'Trạng thái process và listener hiện tại của bridge.', restart: 'Khởi động lại', stop: 'Dừng', drain: 'Ngừng nhận request mới', undrain: 'Nhận request mới', draining: 'Đang drain', security: 'Bảo mật', securityDescription: 'Trạng thái xác thực và các tính năng nguy hiểm.', dashboardAuthentication: 'Xác thực dashboard', shellCommands: 'Lệnh shell', managedApiKeys: 'API key được quản lý', proxyPool: 'Nhóm proxy', loadingProxyHealth: 'Đang tải trạng thái proxy…', refreshHealth: 'Làm mới trạng thái', loadingNodes: 'Đang tải node…', maintenance: 'Bảo trì', maintenanceDescription: 'Nhật ký, chẩn đoán và cấu hình nâng cao chỉ mở khi cần.', viewLogsDescription: 'Xem output của bridge hoặc proxy.', runDiagnosticsDescription: 'Kiểm tra runtime, cấu hình và kết nối.', advancedConfiguration: 'Cấu hình nâng cao', advancedConfigurationDescription: 'Kiểm tra và sửa file TOML đang hoạt động.',
      newCredential: 'Thông tin xác thực mới', createApiKeyDescription: 'Tạo credential có tên với chính sách mặc định an toàn.', name: 'Tên', preset: 'Mẫu cấu hình', environment: 'Môi trường', defaultModel: 'Mô hình mặc định', description: 'Mô tả', advancedSettings: 'Cài đặt nâng cao', expires: 'Hết hạn', maxOutputTokens: 'Token output tối đa', reasoningMode: 'Chế độ suy luận', reasoningEffort: 'Mức suy luận', maxReasoningTokens: 'Token suy luận tối đa', overLimitBehavior: 'Xử lý khi vượt giới hạn', requestsPerMinute: 'Yêu cầu / phút', concurrentRequests: 'Yêu cầu đồng thời', dailyQuota: 'Hạn mức yêu cầu ngày', permissions: 'Quyền', cancel: 'Hủy',
      apiKeySettings: 'Cài đặt API key', general: 'Chung', policy: 'Chính sách', usage: 'Sử dụng', expirationDate: 'Ngày hết hạn', clientConfiguration: 'Cấu hình client', format: 'Định dạng', apiKeySource: 'Nguồn API key', generate: 'Tạo', copy: 'Sao chép', download: 'Tải xuống', dangerZone: 'Vùng nguy hiểm', dangerZoneDescription: 'Rotate sẽ vô hiệu secret hiện tại ngay lập tức. Revoke là vĩnh viễn.', rotateSecret: 'Đổi secret', revokeKey: 'Thu hồi key', allowModelOverride: 'Cho phép client đổi mô hình', allowedModels: 'Mô hình được phép', protocols: 'Giao thức', features: 'Tính năng', close: 'Đóng', saveChanges: 'Lưu thay đổi',
      secretCreated: 'Đã tạo secret', secretShownOnce: 'Secret chỉ hiển thị một lần. Hãy lưu ở nơi an toàn.', secretWarning: 'Đóng cửa sổ này sẽ ẩn secret vĩnh viễn.', downloadConfig: 'Tải cấu hình', done: 'Hoàn tất', credentialHealth: 'Sức khỏe credential', checkApiKeyDescription: 'Kiểm tra toàn bộ key được quản lý để phát hiện key đã tắt, hết hạn hoặc không khả dụng.', checkingAllKeys: 'Đang kiểm tra toàn bộ API key…', apiKeyCheckNote: 'Raw secret không bao giờ được lưu. Kiểm tra này xác nhận trạng thái registry hiện tại và khả năng dùng key để xác thực.', checkAgain: 'Kiểm tra lại', dead: 'Không khả dụng', expiringSoon: 'Sắp hết hạn', checkedAt: 'Kiểm tra lúc', apiKey: 'API key', check: 'Kiểm tra',
      logs: 'Nhật ký', logsDescription: 'Xem output gần đây của bridge hoặc proxy.', source: 'Nguồn', lines: 'Số dòng', diagnostics: 'Chẩn đoán', diagnosticsDescription: 'Chạy các kiểm tra cốt lõi về runtime, cấu hình và mạng.', runAgain: 'Chạy lại', diagnosticsNotRun: 'Chưa chạy chẩn đoán.', advanced: 'Nâng cao', configurationDescription: 'Kiểm tra và áp dụng atomically tài liệu TOML đang hoạt động.', loadTemplate: 'Nạp mẫu', validate: 'Kiểm tra', save: 'Lưu', moreTools: 'Công cụ khác', resetTemplate: 'Đặt lại mẫu', shellCompletion: 'Shell completion', updateNotChecked: 'Chưa kiểm tra cập nhật.', checkUpdate: 'Kiểm tra cập nhật', applyUpdate: 'Áp dụng cập nhật', confirmAction: 'Xác nhận thao tác', confirm: 'Xác nhận',
      loading: 'Đang tải…', enabled: 'Đã bật', unlimited: 'Không giới hạn', never: 'Chưa bao giờ', noApiKeys: 'Chưa có API key.', noMatchingKeys: 'Không có API key phù hợp.', edit: 'Sửa', healthy: 'Bình thường', allClosed: 'Tất cả đã đóng', running: 'đang chạy', failed: 'lỗi', protected: 'Được bảo vệ', viewProxyLogs: 'Xem log', restartNode: 'Khởi động lại', configured: 'Đã cấu hình', notConfigured: 'Chưa cấu hình',
      state: 'Trạng thái', version: 'Phiên bản', listener: 'Listener', clientAuth: 'Xác thực client', shellPolicy: 'Chính sách shell', egressMode: 'Chế độ egress', activeRoute: 'Route ưu tiên', proxySubsystem: 'Subsystem proxy', role: 'Vai trò', health: 'Sức khỏe', circuit: 'Circuit', activeRequests: 'Đang xử lý', exitIp: 'IP đầu ra', action: 'Thao tác', configuration: 'Cấu hình', limits: 'Giới hạn', lastUsed: 'Dùng lần cuối', usageToday: 'Dùng hôm nay',
      requestsToday: 'Request hôm nay', successRate: 'Tỷ lệ thành công', averageLatency: 'Độ trễ trung bình', storedSize: 'Dung lượng đã lưu', completedRequests: 'Request đã hoàn tất', loadingHistoryStatus: 'Đang tải trạng thái lịch sử…', historySettings: 'Cài đặt lịch sử', exportFiltered: 'Xuất kết quả lọc', purgeHistory: 'Xóa lịch sử', requestHistory: 'Lịch sử request', requestHistoryDescription: 'Kiểm tra prompt đầu vào, payload upstream thực tế, reasoning và response.', historySearchPlaceholder: 'Request ID, prompt, client hoặc model', protocol: 'Giao thức', allProtocols: 'Tất cả giao thức', allModels: 'Tất cả mô hình', all: 'Tất cả', clearFilters: 'Xóa bộ lọc', loadingHistory: 'Đang tải lịch sử request…', previous: 'Trước', next: 'Sau', completed: 'Hoàn tất', cancelled: 'Đã hủy', interrupted: 'Bị gián đoạn', requestDetail: 'Chi tiết request', overview: 'Tổng quan', inboundRequest: 'Request đầu vào', effectivePrompt: 'Prompt thực tế', toolsAndSearch: 'Tool và tìm kiếm', attempts: 'Lần thử', rawJson: 'JSON gốc', noToolEvents: 'Không có sự kiện tool hoặc tìm kiếm.', noAttempts: 'Không có lần thử được ghi nhận.', deleteRequest: 'Xóa request', exportJson: 'Xuất JSON', privacyAndRetention: 'Quyền riêng tư và lưu trữ', historySettingsDescription: 'Kiểm soát việc lưu prompt cục bộ, thời gian giữ và giới hạn dung lượng.', enableHistory: 'Bật lưu lịch sử request', captureMode: 'Chế độ lưu', retentionDays: 'Số ngày lưu', maxRecords: 'Số record tối đa', maxStorage: 'Dung lượng tối đa (MiB)', historySensitiveWarning: 'Prompt, reasoning và response có thể chứa dữ liệu nhạy cảm. Nội dung được che trước khi ghi xuống ổ đĩa.'
    }
  };

  var viewMeta = {
    dashboard: ['dashboard', 'dashboardDescription'],
    api: ['apiKeys', 'apiDescription'],
    models: ['models', 'modelsDescription'],
    history: ['history', 'historyDescription'],
    system: ['system', 'systemDescription']
  };

  function $(selector, root) { return (root || document).querySelector(selector); }
  function $$(selector, root) { return Array.prototype.slice.call((root || document).querySelectorAll(selector)); }
  function escapeHtml(value) {
    return String(value == null ? '' : value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#039;');
  }
  function t(key, fallback) { return (translations[state.lang] && translations[state.lang][key]) || (translations.en[key]) || fallback || key; }

  function applyLanguage() {
    document.documentElement.lang = state.lang;
    $$('[data-i18n]').forEach(function (node) {
      if (!node.dataset.englishText) node.dataset.englishText = node.textContent;
      var key = node.dataset.i18n;
      var value = state.lang === 'en' ? node.dataset.englishText : t(key, node.dataset.englishText);
      if (value) node.textContent = value;
    });
    $$('[data-i18n-placeholder]').forEach(function (node) {
      if (!node.dataset.englishPlaceholder) node.dataset.englishPlaceholder = node.placeholder;
      node.placeholder = state.lang === 'en' ? node.dataset.englishPlaceholder : t(node.dataset.i18nPlaceholder, node.dataset.englishPlaceholder);
    });
    $('#languageToggle').textContent = state.lang === 'en' ? 'VI' : 'EN';
    var meta = viewMeta[state.view];
    if (meta) {
      $('#viewTitle').textContent = t(meta[0]);
      $('#viewDescription').textContent = t(meta[1]);
    }
    renderEvents();
    if (state.apiKeys.length || state.view === 'api') {
      renderApiKeyTable();
      renderApiKeyOverview(state.apiKeys.find(function (item) { return item.id === state.selectedApiKeyOverviewId; }) || null);
    }
    if (state.historyItems.length || state.view === 'history') {
      renderHistoryMetrics();
      renderHistoryTable();
      if (state.selectedHistory) renderHistoryDetail(state.selectedHistory);
    }
  }

  function toggleLanguage() {
    state.lang = state.lang === 'en' ? 'vi' : 'en';
    localStorage.setItem('oc2api-dashboard-language', state.lang);
    applyLanguage();
    refreshView(state.view).catch(function () {});
  }

  function readCookie(name) {
    var target = name + '=';
    var parts = document.cookie.split(';');
    for (var i = 0; i < parts.length; i += 1) {
      var item = parts[i].trim();
      if (item.indexOf(target) === 0) return decodeURIComponent(item.slice(target.length));
    }
    return '';
  }
  function mutationHeaders(headers) {
    var output = Object.assign({}, headers || {});
    var csrf = readCookie('bridge_csrf_token');
    if (csrf) output['X-CSRF-Token'] = csrf;
    return output;
  }
  async function api(url, options) {
    options = options || {};
    var controller = new AbortController();
    var timeout = window.setTimeout(function () { controller.abort(); }, options.timeout || 15000);
    var method = (options.method || 'GET').toUpperCase();
    var headers = Object.assign({}, options.headers || {});
    var body = options.body;
    if (body && typeof body !== 'string' && !(body instanceof FormData)) {
      headers['content-type'] = 'application/json';
      body = JSON.stringify(body);
    }
    if (!['GET', 'HEAD', 'OPTIONS'].includes(method)) headers = mutationHeaders(headers);
    try {
      var response = await fetch(url, { method: method, headers: headers, body: body, credentials: 'same-origin', signal: controller.signal, cache: 'no-store' });
      if (response.status === 401) {
        location.replace('/');
        throw new Error('Dashboard session expired');
      }
      var payload = options.expect === 'text' ? await response.text() : await response.json().catch(function () { return {}; });
      if (!response.ok) {
        var message = payload && (payload.message || payload.detail || (payload.error && payload.error.message));
        throw new Error(message || ('Request failed with HTTP ' + response.status));
      }
      return payload;
    } catch (error) {
      if (error.name === 'AbortError') throw new Error('Request timed out');
      throw error;
    } finally {
      window.clearTimeout(timeout);
    }
  }

  function toast(message, kind) {
    var item = document.createElement('div');
    item.className = 'toast ' + (kind || '');
    item.textContent = message;
    $('#toastRegion').appendChild(item);
    window.setTimeout(function () { item.remove(); }, 4200);
  }
  function setBusy(button, busy, label) {
    if (!button) return;
    if (busy) {
      button.dataset.originalText = button.textContent;
      button.disabled = true;
      if (label) button.textContent = label;
    } else {
      button.disabled = false;
      if (button.dataset.originalText) button.textContent = button.dataset.originalText;
      delete button.dataset.originalText;
    }
  }
  async function withBusy(button, label, operation) {
    setBusy(button, true, label);
    try { return await operation(); }
    catch (error) { toast(error.message || String(error), 'error'); throw error; }
    finally { setBusy(button, false); }
  }
  function confirmAction(title, message) {
    var dialog = $('#confirmDialog');
    $('#confirmTitle').textContent = title;
    $('#confirmMessage').textContent = message;
    dialog.returnValue = 'cancel';
    dialog.showModal();
    return new Promise(function (resolve) {
      dialog.addEventListener('close', function onClose() {
        dialog.removeEventListener('close', onClose);
        resolve(dialog.returnValue === 'confirm');
      });
    });
  }

  function statusClass(value) {
    var text = String(value || '').toLowerCase();
    if (['healthy', 'alive', 'running', 'closed', 'ok', 'ready', 'pass', 'active', 'enabled'].some(function (part) { return text.indexOf(part) >= 0; })) return 'ok';
    if (['dead', 'failed', 'unhealthy', 'open', 'error', 'expired'].some(function (part) { return text.indexOf(part) >= 0; })) return 'error';
    if (['degraded', 'cooldown', 'recovering', 'half', 'warn', 'disabled', 'unverified'].some(function (part) { return text.indexOf(part) >= 0; })) return 'warn';
    return 'neutral';
  }
  function statusHtml(label, kind) {
    var statusKind = kind || statusClass(label);
    return '<span class="status-tag ' + statusKind + '"><span class="status-dot ' + statusKind + '"></span>' + escapeHtml(label) + '</span>';
  }
  function setConnection(kind, label) {
    $('#connectionPill').className = 'connection-pill ' + kind;
    $('#connectionPill').innerHTML = '<span class="status-dot ' + kind + '"></span><span>' + escapeHtml(label) + '</span>';
    $('#sidebarStatusDot').className = 'status-dot ' + kind;
    $('#sidebarStatusText').textContent = label;
  }

  function formatDuration(seconds) {
    seconds = Math.max(0, Math.floor(Number(seconds || 0)));
    var days = Math.floor(seconds / 86400);
    var hours = Math.floor((seconds % 86400) / 3600);
    var minutes = Math.floor((seconds % 3600) / 60);
    var remainingSeconds = seconds % 60;
    if (days) return days + 'd ' + hours + 'h ' + String(minutes).padStart(2, '0') + 'm';
    if (hours) return hours + 'h ' + String(minutes).padStart(2, '0') + 'm ' + String(remainingSeconds).padStart(2, '0') + 's';
    return minutes + 'm ' + String(remainingSeconds).padStart(2, '0') + 's';
  }
  function syncUptime(seconds, pid) {
    state.uptimeBaseSeconds = Math.max(0, Number(seconds || 0));
    state.uptimeSyncedAt = Date.now();
    state.uptimePid = pid || null;
    renderLiveUptime();
  }
  function currentUptimeSeconds() { return state.uptimeBaseSeconds + (state.uptimeSyncedAt ? Math.max(0, Math.floor((Date.now() - state.uptimeSyncedAt) / 1000)) : 0); }
  function renderLiveUptime() {
    $('#metricUptime').textContent = formatDuration(currentUptimeSeconds());
    $('#metricUptimeDetail').textContent = 'PID ' + (state.uptimePid || '—');
    var fact = $('#serverFactUptime');
    if (fact) fact.textContent = formatDuration(currentUptimeSeconds());
  }
  function formatTimestamp(seconds) {
    if (!seconds) return t('never', 'Never');
    var date = new Date(Number(seconds) * 1000);
    return Number.isNaN(date.getTime()) ? '—' : date.toLocaleString(state.lang === 'vi' ? 'vi-VN' : undefined);
  }
  function shortModel(modelId) {
    var model = state.models.concat(state.apiKeyModels).find(function (item) { return item.id === modelId; });
    if (model) return model.label;
    return String(modelId || 'Bridge default').replace(/^opencode\//, '');
  }

  function switchView(view, updateHash) {
    if (!viewMeta[view]) return;
    state.view = view;
    $$('.nav-item[data-view]').forEach(function (button) { button.classList.toggle('active', button.dataset.view === view); });
    $$('[data-view-panel]').forEach(function (panel) { panel.classList.toggle('active', panel.dataset.viewPanel === view); });
    $('#viewTitle').textContent = t(viewMeta[view][0]);
    $('#viewDescription').textContent = t(viewMeta[view][1]);
    document.body.classList.remove('sidebar-open');
    var mainShell = $('.main-shell');
    if (mainShell) mainShell.scrollTop = 0;
    if (updateHash !== false) history.replaceState(null, '', '#' + view);
    refreshView(view).catch(function () {});
  }
  async function ensureSession() {
    var status = await api('/api/dashboard/auth/status');
    if (!status.admin_token_configured || !status.authenticated) {
      location.replace('/');
      return false;
    }
    return true;
  }

  function pushEvent(message, level, timestamp) {
    if (!message || String(message).toLowerCase() === 'heartbeat') return;
    state.eventItems.unshift({ message: message, level: level || 'neutral', timestamp: timestamp || new Date().toISOString() });
    state.eventItems = state.eventItems.slice(0, 40);
    renderEvents();
  }
  function renderEvents() {
    var root = $('#eventList');
    if (!root) return;
    if (!state.eventItems.length) {
      root.innerHTML = '<div class="empty-state">' + escapeHtml(t('waitingForEvents', 'Waiting for events…')) + '</div>';
      return;
    }
    root.innerHTML = state.eventItems.slice(0, 8).map(function (entry) {
      var date = new Date(entry.timestamp);
      var time = Number.isNaN(date.getTime()) ? String(entry.timestamp).slice(-8) : date.toLocaleTimeString(state.lang === 'vi' ? 'vi-VN' : undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' });
      return '<div class="event-item"><span class="event-time">' + escapeHtml(time) + '</span><span class="event-message"><span class="status-dot ' + statusClass(entry.level) + '"></span> ' + escapeHtml(entry.message) + '</span></div>';
    }).join('');
  }

  function renderServerFacts(data) {
    var egress = data.egress || {};
    var proxySubsystem = egress.proxy_subsystem || {};
    var rows = [
      [t('state', 'State'), data.status || 'unknown'],
      [t('version', 'Version'), data.version || '—'],
      ['PID', data.pid || '—'],
      [t('listener', 'Listener'), '127.0.0.1:' + (data.bridge_port || '—')],
      [t('uptime', 'Uptime'), formatDuration(currentUptimeSeconds())],
      [t('model', 'Model'), data.model || 'auto'],
      [t('clientAuth', 'Client auth'), data.auth_enabled ? t('enabled', 'Enabled') : t('disabled', 'Disabled')],
      [t('shellPolicy', 'Shell policy'), data.shell_policy || '—'],
      [t('egressMode', 'Egress mode'), egress.mode || '—'],
      [t('activeRoute', 'Active route'), egress.active_route || '—'],
      [t('proxySubsystem', 'Proxy subsystem'), proxySubsystem.phase || '—']
    ];
    $('#serverFacts').innerHTML = rows.map(function (row) {
      var id = row[0] === t('uptime', 'Uptime') ? ' id="serverFactUptime"' : '';
      return '<div><dt>' + escapeHtml(row[0]) + '</dt><dd' + id + '>' + escapeHtml(row[1]) + '</dd></div>';
    }).join('');
  }

  async function loadStatus() {
    var data = await api('/api/dashboard/status');
    state.status = data;
    setConnection('ok', t('connected', 'Connected'));
    $('#sidebarVersion').textContent = 'v' + (data.version || '—');
    $('#sidebarModel').textContent = shortModel(data.model);
    $('#sidebarModel').title = data.model || '';
    $('#metricService').textContent = data.status === 'ok' ? t('online', 'Online') : String(data.status || 'Unknown');
    $('#metricServiceDetail').textContent = 'Port ' + (data.bridge_port || '—') + ' · v' + (data.version || '—');
    $('#metricModel').textContent = shortModel(data.model);
    $('#metricModel').title = data.model || '';
    syncUptime(data.uptime_secs, data.pid);
    renderServerFacts(data);

    $('#statusBridge').innerHTML = statusHtml(data.status === 'ok' ? t('healthy', 'Healthy') : data.status, data.status === 'ok' ? 'ok' : statusClass(data.status));
    $('#statusApiAuth').innerHTML = statusHtml(data.auth_enabled ? t('enabled', 'Enabled') : t('disabled', 'Disabled'), data.auth_enabled ? 'ok' : 'warn');
    var primary = data.primary_proxies || {};
    var standby = data.warm_standby || {};
    $('#statusPrimary').innerHTML = statusHtml((primary.healthy || 0) + ' / ' + (primary.total || 0) + ' ' + t('healthy', 'healthy'), (primary.healthy || 0) === (primary.total || 0) ? 'ok' : 'warn');
    $('#statusStandby').innerHTML = statusHtml((standby.healthy || 0) + ' / ' + (standby.total || 0) + ' ' + t('healthy', 'healthy'), (standby.healthy || 0) === (standby.total || 0) ? 'ok' : 'warn');
    $('#securityDashboard').innerHTML = statusHtml(data.admin_token_configured ? t('enabled', 'Enabled') : t('notConfigured', 'Not configured'), data.admin_token_configured ? 'ok' : 'error');
    $('#securityApi').innerHTML = statusHtml(data.auth_enabled ? t('enabled', 'Enabled') : t('disabled', 'Disabled'), data.auth_enabled ? 'ok' : 'warn');
    $('#securityShell').innerHTML = statusHtml(data.shell_policy || 'disabled', data.shell_policy === 'disabled' ? 'ok' : 'warn');
    updateCurrentModel();
    return data;
  }

  async function loadMetrics() {
    var data = await api('/api/dashboard/control/metrics');
    state.metrics = data.metrics || {};
    state.workers = (data.workers && data.workers.workers) || [];
    var failures = Number(state.metrics.responses_4xx || 0) + Number(state.metrics.responses_5xx || 0);
    $('#metricRequests').textContent = Number(state.metrics.requests_total || 0).toLocaleString();
    $('#metricRequestDetail').textContent = (state.metrics.active_requests || 0) + ' active · ' + failures + ' failed';
    var running = state.workers.filter(function (worker) { return String(worker.state).toLowerCase() === 'running'; }).length;
    var failed = state.workers.length - running;
    $('#statusWorkers').innerHTML = statusHtml(running + ' ' + t('running', 'running') + (failed ? ' · ' + failed + ' ' + t('failed', 'failed') : ''), failed ? 'warn' : 'ok');
    return data;
  }

  function proxyRows(nodes) {
    if (!nodes.length) return '<div class="empty-state">No proxy nodes configured.</div>';
    return '<table class="data-table"><thead><tr><th>Node</th><th>' + escapeHtml(t('role', 'Role')) + '</th><th>' + escapeHtml(t('health', 'Health')) + '</th><th>' + escapeHtml(t('circuit', 'Circuit')) + '</th><th>' + escapeHtml(t('activeRequests', 'Active')) + '</th><th>' + escapeHtml(t('exitIp', 'Exit IP')) + '</th><th></th></tr></thead><tbody>' + nodes.map(function (node) {
      var identity = node.exit_identity && (node.exit_identity.public_ip || node.exit_identity.ip || node.exit_identity.address);
      var mutable = String(node.lifecycle || '').toLowerCase() !== 'protected';
      var drainAction = node.draining ? 'undrain' : 'drain';
      var drainLabel = node.draining ? t('undrain', 'Undrain') : t('drain', 'Drain');
      var recovery = node.recovery_cause ? ' · recovery ' + node.recovery_cause + ' #' + Number(node.restart_attempts || 0) : '';
      var deadlines = [];
      if (node.cooldown_remaining_secs != null) deadlines.push('cooldown ' + formatDuration(Number(node.cooldown_remaining_secs)));
      if (node.rate_limit_remaining_secs != null) deadlines.push('quota ' + formatDuration(Number(node.rate_limit_remaining_secs)));
      var identityMeta = [];
      if (node.exit_identity && node.exit_identity.verified_at_unix_secs) identityMeta.push('verified ' + formatTimestamp(node.exit_identity.verified_at_unix_secs));
      if (node.duplicate_of) identityMeta.push('duplicate of ' + node.duplicate_of);
      return '<tr><td><strong class="mono">' + escapeHtml(node.id || ('proxy-' + node.port)) + '</strong><small class="mono">:' + escapeHtml(node.port) + '</small>' + (node.draining ? '<span class="count-badge">' + escapeHtml(t('draining', 'Draining')) + '</span>' : '') + '</td>' +
        '<td>' + escapeHtml(node.role || '—') + '</td>' +
        '<td>' + statusHtml(node.health || node.status || 'unknown') + (recovery ? '<small>' + escapeHtml(recovery) + '</small>' : '') + '</td>' +
        '<td class="mono">' + escapeHtml(node.circuit || '—') + (deadlines.length ? '<small>' + escapeHtml(deadlines.join(' · ')) + '</small>' : '') + '</td>' +
        '<td class="mono">' + escapeHtml(node.active_requests || 0) + '</td>' +
        '<td class="mono">' + escapeHtml(identity || 'unverified') + (identityMeta.length ? '<small>' + escapeHtml(identityMeta.join(' · ')) + '</small>' : '') + '</td>' +
        '<td class="proxy-actions-cell"><div class="proxy-actions">' + (mutable ? '<button class="row-action" type="button" data-drain-action="' + drainAction + '" data-drain-port="' + escapeHtml(node.port) + '">' + escapeHtml(drainLabel) + '</button><button class="row-action proxy-primary-action" type="button" data-restart-port="' + escapeHtml(node.port) + '">' + escapeHtml(t('restart', 'Restart')) + '</button>' : '<span class="count-badge proxy-primary-action">' + escapeHtml(t('protected', 'Protected')) + '</span>') + '<button class="row-action proxy-logs-action" type="button" data-proxy-logs="' + escapeHtml(node.port) + '">' + escapeHtml(t('viewProxyLogs', 'Logs')) + '</button></div></td></tr>';
    }).join('') + '</tbody></table>';
  }

  async function loadProxies() {
    var data = await api('/api/dashboard/proxies');
    state.proxies = Array.isArray(data) ? data : (data.nodes || []);
    $('#networkProxyTable').innerHTML = proxyRows(state.proxies);
    var healthy = state.proxies.filter(function (node) { return String(node.health).toLowerCase() === 'healthy'; }).length;
    var egress = (state.status && state.status.egress) || {};
    var subsystem = egress.proxy_subsystem || {};
    var summary = state.proxies.length + ' nodes · ' + healthy + ' healthy';
    if (egress.mode) summary += ' · ' + egress.mode + ' → ' + (egress.active_route || '—');
    if (subsystem.phase) summary += ' · proxy ' + subsystem.phase;
    if (egress.unique_verified_exits != null) summary += ' · ' + egress.unique_verified_exits + '/' + (egress.minimum_unique_exit_ips || 0) + ' unique exits';
    if (subsystem.backoff_until_unix_secs) summary += ' · backoff until ' + formatTimestamp(subsystem.backoff_until_unix_secs);
    if (subsystem.last_error) summary += ' · ' + subsystem.last_error;
    $('#systemProxySummary').textContent = summary;
    var allClosed = state.proxies.every(function (node) { return String(node.circuit).toLowerCase() === 'closed'; });
    $('#statusCircuits').innerHTML = statusHtml(allClosed ? t('allClosed', 'All closed') : 'Attention required', allClosed ? 'ok' : 'warn');
    return state.proxies;
  }

  function updateCurrentModel() {
    var configured = state.selectedModel || (state.status && state.status.model) || '';
    var runtime = (state.status && state.status.model) || configured;
    $('#currentModelName').textContent = shortModel(configured);
    $('#currentModelId').textContent = configured || '—';
    var requiresRestart = Boolean(runtime && configured && runtime !== configured);
    $('#modelRestartNotice').hidden = !requiresRestart;
    $('#restartModelButton').hidden = !requiresRestart;
    $('#currentModelState').innerHTML = statusHtml(requiresRestart ? 'Restart required' : t('active', 'Active'), requiresRestart ? 'warn' : 'ok');
  }

  function renderModels() {
    var root = $('#modelGrid');
    var query = ($('#modelSearch').value || '').trim().toLowerCase();
    var list = state.models.filter(function (model) { return !query || [model.label, model.id, model.provider].join(' ').toLowerCase().indexOf(query) >= 0; });
    if (!list.length) {
      root.innerHTML = '<div class="empty-state">No matching models.</div>';
      return;
    }
    root.innerHTML = list.map(function (model) {
      var selected = model.id === state.selectedModel;
      return '<div class="model-row"><div class="model-row-main"><h3>' + escapeHtml(model.label) + '</h3><code>' + escapeHtml(model.id) + '</code><div class="model-row-meta">' + escapeHtml(model.provider || '') + ' · ' + escapeHtml(model.protocol || '') + '</div></div><button class="button ' + (selected ? '' : 'primary') + '" type="button" data-select-model="' + escapeHtml(model.id) + '" ' + (selected ? 'disabled' : '') + '>' + escapeHtml(selected ? t('active', 'Active') : 'Select') + '</button></div>';
    }).join('');
  }

  function populateTesterModelControl(preferredModel) {
    var select = $('#testerModel');
    if (!select) return;
    var previous = select.value;
    var models = state.models.slice();
    var fallbackModel = preferredModel || previous || state.selectedModel || (state.status && state.status.model) || '';
    if (fallbackModel && !models.some(function (model) { return model.id === fallbackModel; })) {
      models.unshift({ id: fallbackModel, label: shortModel(fallbackModel) });
    }
    select.innerHTML = models.map(function (model) {
      return '<option value="' + escapeHtml(model.id) + '">' + escapeHtml(model.label || shortModel(model.id)) + '</option>';
    }).join('');
    if (models.some(function (model) { return model.id === fallbackModel; })) select.value = fallbackModel;
  }

  async function loadModels() {
    var data = await api('/api/dashboard/control/models');
    state.models = data.models || [];
    state.selectedModel = data.selected || (state.status && state.status.model) || '';
    populateApiModelControls();
    populateTesterModelControl();
    updateCurrentModel();
    renderModels();
    return data;
  }

  async function loadEnvironment() {
    state.environment = await api('/api/dashboard/control/env');
    return state.environment;
  }

  function optionalPositiveNumber(selector) {
    var input = $(selector);
    if (!input || String(input.value).trim() === '') return null;
    var value = Number(input.value);
    return Number.isFinite(value) && value > 0 ? Math.floor(value) : null;
  }
  function apiKeyStatus(key) {
    if (key.expired) return { label: t('expired', 'Expired'), kind: 'error' };
    if (key.status === 'disabled') return { label: t('disabled', 'Disabled'), kind: 'warn' };
    if (key.status === 'active') return { label: t('active', 'Active'), kind: 'ok' };
    return { label: key.status || 'Unknown', kind: 'neutral' };
  }
  function populateApiModelControls() {
    var options = '<option value="">Use bridge default</option>' + state.models.concat(state.apiKeyModels).filter(function (model, index, list) {
      return list.findIndex(function (item) { return item.id === model.id; }) === index;
    }).map(function (model) { return '<option value="' + escapeHtml(model.id) + '">' + escapeHtml(model.label) + '</option>'; }).join('');
    ['#createKeyModel', '#editKeyModel'].forEach(function (selector) {
      var select = $(selector);
      if (!select) return;
      var previous = select.value;
      select.innerHTML = options;
      if ($$('option', select).some(function (option) { return option.value === previous; })) select.value = previous;
    });
    var allowed = $('#editAllowedModels');
    if (allowed) {
      allowed.innerHTML = state.models.concat(state.apiKeyModels).filter(function (model, index, list) {
        return list.findIndex(function (item) { return item.id === model.id; }) === index;
      }).map(function (model) {
        return '<label class="check"><input type="checkbox" data-allowed-model="' + escapeHtml(model.id) + '"><span>' + escapeHtml(model.label) + '</span></label>';
      }).join('') + '<small>Leave all unchecked to allow any configured model.</small>';
    }
  }
  function filteredApiKeys() {
    var query = ($('#apiKeySearch').value || '').trim().toLowerCase();
    var status = $('#apiKeyStatusFilter').value;
    return state.apiKeys.filter(function (key) {
      var searchable = [key.name, key.id, key.fingerprint, key.description].join(' ').toLowerCase();
      if (query && searchable.indexOf(query) < 0) return false;
      if (status === 'active' && !key.active) return false;
      if (status === 'disabled' && key.status !== 'disabled') return false;
      if (status === 'expired' && !key.expired) return false;
      return true;
    });
  }

  function renderApiKeyOverview(key) {
    var title = $('#apiOverviewTitle');
    var subtitle = $('#apiOverviewSubtitle');
    var statusRoot = $('#apiOverviewStatus');
    var content = $('#apiKeyOverviewContent');
    var editButton = $('#editSelectedApiKeyButton');
    if (!key) {
      title.textContent = t('apiKeys', 'API key overview');
      subtitle.textContent = t('noApiKeys', 'Select a credential from the list.');
      statusRoot.className = 'status-tag neutral';
      statusRoot.innerHTML = '<span class="status-dot neutral"></span>—';
      content.innerHTML = '<div class="empty-state">' + escapeHtml(t('noApiKeys', 'No API key selected.')) + '</div>';
      editButton.disabled = true;
      return;
    }
    var policy = key.policy || {};
    var usage = key.usage || {};
    var status = apiKeyStatus(key);
    var permissions = Object.keys(policy.permissions || {}).filter(function (name) { return policy.permissions[name]; });
    var expires = key.expires_at ? formatTimestamp(key.expires_at) : t('never', 'Never');
    var rows = [
      [t('environment', 'Environment'), key.environment || 'production'],
      [t('defaultModel', 'Default model'), shortModel(policy.default_model)],
      [t('reasoningMode', 'Reasoning'), (policy.reasoning_mode || 'inherit') + (policy.reasoning_effort ? ' · ' + policy.reasoning_effort : '')],
      [t('maxOutputTokens', 'Output limit'), policy.max_output_tokens ? Number(policy.max_output_tokens).toLocaleString() : t('unlimited', 'Unlimited')],
      [t('requestsPerMinute', 'Requests / minute'), policy.requests_per_minute || t('unlimited', 'Unlimited')],
      [t('concurrentRequests', 'Concurrent'), policy.max_concurrent_requests || t('unlimited', 'Unlimited')],
      [t('expirationDate', 'Expires'), expires],
      [t('lastUsed', 'Last used'), usage.last_used_at ? formatTimestamp(usage.last_used_at) : t('never', 'Never')]
    ];
    title.textContent = key.name || 'API key';
    subtitle.textContent = (key.fingerprint || key.id) + ' · ' + Number(usage.requests || 0).toLocaleString() + ' requests';
    statusRoot.className = 'status-tag ' + status.kind;
    statusRoot.innerHTML = '<span class="status-dot ' + status.kind + '"></span>' + escapeHtml(status.label);
    content.innerHTML = '<div class="api-overview-facts">' + rows.map(function (row) {
      return '<div><span>' + escapeHtml(row[0]) + '</span><strong>' + escapeHtml(row[1]) + '</strong></div>';
    }).join('') + '</div>' +
      '<div class="api-overview-section"><h3>' + escapeHtml(t('permissions', 'Permissions')) + '</h3><div class="permission-chip-list">' + (permissions.length ? permissions.map(function (permission) { return '<span>' + escapeHtml(permission.replace(/_/g, ' ')) + '</span>'; }).join('') : '<span>' + escapeHtml(t('notConfigured', 'None enabled')) + '</span>') + '</div></div>' +
      '<div class="api-overview-section"><h3>' + escapeHtml(t('usage', 'Usage')) + '</h3><div class="usage-grid compact-usage-grid">' + [
        ['Requests', Number(usage.requests || 0).toLocaleString()],
        ['Rejected', Number(usage.rejected || 0).toLocaleString()],
        ['In flight', String(usage.in_flight || 0)],
        ['Today', String(usage.daily_requests || 0)]
      ].map(function (item) { return '<div><span>' + escapeHtml(item[0]) + '</span><strong>' + escapeHtml(item[1]) + '</strong></div>'; }).join('') + '</div></div>';
    editButton.disabled = false;
  }

  function selectApiKeyOverview(id) {
    var key = state.apiKeys.find(function (item) { return item.id === id; }) || null;
    state.selectedApiKeyOverviewId = key ? key.id : null;
    $$('#apiKeyInventory [data-api-key-id]').forEach(function (row) {
      row.classList.toggle('active', row.dataset.apiKeyId === state.selectedApiKeyOverviewId);
    });
    renderApiKeyOverview(key);
  }

  async function loadApiKeys() {
    var data = await api('/api/dashboard/control/api-keys');
    state.apiKeys = data.keys || [];
    state.apiKeySummary = data.summary || {};
    state.apiKeyModels = data.models || [];
    populateApiModelControls();
    var selected = state.apiKeys.find(function (item) { return item.id === state.selectedApiKeyOverviewId; }) || state.apiKeys[0] || null;
    state.selectedApiKeyOverviewId = selected ? selected.id : null;
    renderApiKeyTable();
    renderApiKeyOverview(selected);
    $('#apiKeyCountBadge').textContent = state.apiKeys.length + (state.apiKeys.length === 1 ? ' key' : ' keys');
    $('#securityKeys').innerHTML = statusHtml((state.apiKeySummary.active || 0) + ' ' + t('active', 'active'), (state.apiKeySummary.active || 0) ? 'ok' : 'neutral');
    return data;
  }

  function renderApiKeyTable() {
    var root = $('#apiKeyInventory');
    if (!state.apiKeys.length) {
      root.innerHTML = '<div class="empty-state"><div><strong>' + escapeHtml(t('noApiKeys', 'No API keys yet.')) + '</strong><br><span>Create the first client credential.</span></div></div>';
      return;
    }
    var keys = filteredApiKeys();
    if (!keys.length) {
      root.innerHTML = '<div class="empty-state">' + escapeHtml(t('noMatchingKeys', 'No API keys match the filters.')) + '</div>';
      return;
    }
    root.innerHTML = keys.map(function (key) {
      var policy = key.policy || {};
      var usage = key.usage || {};
      var status = apiKeyStatus(key);
      var selected = key.id === state.selectedApiKeyOverviewId;
      var model = shortModel(policy.default_model);
      var details = [model, Number(usage.requests || 0).toLocaleString() + ' requests', key.environment || 'production'].join(' · ');
      return '<button class="resource-list-item api-key-resource-item ' + (selected ? 'active' : '') + '" type="button" data-api-key-id="' + escapeHtml(key.id) + '">' +
        '<span class="resource-item-main"><strong>' + escapeHtml(key.name) + '</strong><small class="mono">' + escapeHtml(key.fingerprint) + '</small><span class="resource-item-meta">' + escapeHtml(details) + '</span></span>' +
        '<span class="resource-item-side">' + statusHtml(status.label, status.kind) + '<span class="mono resource-item-time">' + escapeHtml(usage.last_used_at ? formatTimestamp(usage.last_used_at) : t('never', 'Never')) + '</span></span></button>';
    }).join('');
  }
  function permissionsFrom(rootSelector) {
    var result = {};
    $$('[data-permission]', $(rootSelector)).forEach(function (input) { result[input.dataset.permission] = input.checked; });
    return result;
  }
  function setPermissions(rootSelector, permissions) {
    permissions = permissions || {};
    $$('[data-permission]', $(rootSelector)).forEach(function (input) { input.checked = Boolean(permissions[input.dataset.permission]); });
  }
  function setCreatePreset(preset) {
    var permission = { anthropic_messages: true, openai_chat: true, list_models: true, count_tokens: true, streaming: true, tools: true, web_search: true, shell: false };
    $('#createKeyRpm').value = '60';
    $('#createKeyConcurrent').value = '4';
    $('#createKeyReasoning').value = 'inherit';
    if (preset === 'claude-code') permission.openai_chat = false;
    if (preset === 'backend') { $('#createKeyRpm').value = '120'; $('#createKeyConcurrent').value = '8'; }
    if (preset === 'mobile') { permission.anthropic_messages = false; permission.tools = false; permission.web_search = false; $('#createKeyRpm').value = '30'; $('#createKeyConcurrent').value = '2'; }
    if (preset === 'readonly') { permission.openai_chat = false; permission.tools = false; permission.web_search = false; $('#createKeyRpm').value = '10'; $('#createKeyConcurrent').value = '1'; }
    setPermissions('#createKeyPermissions', permission);
  }
  function openCreateApiKeyDialog() {
    $('#apiKeyCreateForm').reset();
    $('#createKeyEnvironment').value = 'production';
    $('#createKeyExpiration').value = '0';
    $('#createKeyPreset').value = 'claude-code';
    $('#createKeyMaxOutput').value = '4096';
    $('#createKeyDailyQuota').value = '0';
    $('#createKeyLimitAction').value = 'reject';
    if (state.status && state.status.model) $('#createKeyModel').value = state.status.model;
    setCreatePreset('claude-code');
    $('#apiKeyCreateDialog').showModal();
    $('#createKeyName').focus();
  }
  async function createApiKey(event) {
    event.preventDefault();
    var button = event.submitter || $('#apiKeyCreateForm button[type="submit"]');
    var preset = $('#createKeyPreset').value;
    var body = {
      name: $('#createKeyName').value.trim(), description: $('#createKeyDescription').value.trim() || null,
      environment: $('#createKeyEnvironment').value, expires_in_days: Number($('#createKeyExpiration').value || 0), secret_bytes: 32,
      policy: {
        default_model: $('#createKeyModel').value || null, allowed_models: [], allow_model_override: !['mobile', 'readonly'].includes(preset),
        max_output_tokens: optionalPositiveNumber('#createKeyMaxOutput'), max_reasoning_tokens: optionalPositiveNumber('#createKeyMaxReasoning'),
        reasoning_mode: $('#createKeyReasoning').value, reasoning_effort: $('#createKeyReasoningEffort').value || null,
        limit_action: $('#createKeyLimitAction').value, max_concurrent_requests: optionalPositiveNumber('#createKeyConcurrent'),
        requests_per_minute: optionalPositiveNumber('#createKeyRpm'), daily_request_quota: optionalPositiveNumber('#createKeyDailyQuota'),
        permissions: permissionsFrom('#createKeyPermissions')
      }
    };
    await withBusy(button, 'Creating…', async function () {
      var result = await api('/api/dashboard/control/api-keys', { method: 'POST', body: body });
      var key = result.key || (result.keys_metadata || [])[0];
      var secret = result.secret_once || (result.keys || [])[0];
      if (!key || !secret) throw new Error('The server did not return the new key secret');
      state.sessionSecrets[key.id] = secret;
      state.lastGeneratedKey = key;
      $('#apiKeyCreateDialog').close();
      $('#secretKeyName').textContent = key.name || 'API key ready';
      $('#keyOutput').textContent = secret;
      $('#apiKeySecretDialog').showModal();
      await loadApiKeys();
      toast('API key created and activated immediately.', 'success');
      pushEvent('API key created · ' + key.name, 'ok');
    });
  }
  async function copyGeneratedSecret() {
    var secret = $('#keyOutput').textContent.trim();
    if (!secret || secret === '—') return;
    await navigator.clipboard.writeText(secret);
    toast('API key copied.', 'success');
  }
  async function downloadGeneratedConfig() {
    var key = state.lastGeneratedKey;
    if (!key) return;
    var secret = state.sessionSecrets[key.id];
    if (!secret) throw new Error('The generated secret is no longer available');
    var result = await api('/api/dashboard/control/client-config', { method: 'POST', body: { format: 'claude-code', key_source: 'provided', api_key: secret, model: key.policy && key.policy.default_model } });
    downloadText(result.filename || 'settings.json', result.content || '', result.content_type || 'application/json');
  }
  function openDrawerTab(name) {
    $$('[data-drawer-tab]').forEach(function (button) { button.classList.toggle('active', button.dataset.drawerTab === name); });
    $$('[data-drawer-panel]').forEach(function (panel) { panel.classList.toggle('active', panel.dataset.drawerPanel === name); });
  }
  async function openApiKeyDrawer(id) {
    $('#apiKeyDrawer').showModal();
    $('#editKeyTitle').textContent = 'Loading…';
    openDrawerTab('general');
    try {
      var data = await api('/api/dashboard/control/api-keys/' + encodeURIComponent(id));
      state.selectedApiKey = data.key;
      populateApiKeyDrawer(data.key);
    } catch (error) {
      $('#apiKeyDrawer').close();
      throw error;
    }
  }
  function populateApiKeyDrawer(key) {
    var policy = key.policy || {};
    var usage = key.usage || {};
    var status = apiKeyStatus(key);
    $('#editKeyId').value = key.id;
    $('#editKeyTitle').textContent = key.name;
    $('#editKeyMeta').innerHTML = statusHtml(status.label, status.kind) + '<span class="mono">' + escapeHtml(key.fingerprint) + '</span><span>Created ' + escapeHtml(formatTimestamp(key.created_at)) + '</span>';
    $('#editKeyName').value = key.name || '';
    $('#editKeyDescription').value = key.description || '';
    $('#editKeyStatus').value = key.status === 'disabled' ? 'disabled' : 'active';
    $('#editKeyEnvironment').value = key.environment || 'production';
    $('#editKeyExpiresAt').value = key.expires_at ? new Date(Number(key.expires_at) * 1000).toISOString().slice(0, 10) : '';
    $('#editKeyModel').value = policy.default_model || '';
    $('#editKeyAllowModelOverride').checked = policy.allow_model_override !== false;
    $('#editKeyMaxOutput').value = policy.max_output_tokens || '';
    $('#editKeyReasoning').value = policy.reasoning_mode || 'inherit';
    $('#editKeyReasoningEffort').value = policy.reasoning_effort || '';
    $('#editKeyMaxReasoning').value = policy.max_reasoning_tokens || '';
    $('#editKeyLimitAction').value = policy.limit_action || 'reject';
    $('#editKeyRpm').value = policy.requests_per_minute || '0';
    $('#editKeyConcurrent').value = policy.max_concurrent_requests || '0';
    $('#editKeyDailyQuota').value = policy.daily_request_quota || '0';
    $$('[data-allowed-model]', $('#editAllowedModels')).forEach(function (input) { input.checked = (policy.allowed_models || []).indexOf(input.dataset.allowedModel) >= 0; });
    setPermissions('#editKeyPermissions', policy.permissions || {});
    $('#editKeyUsage').innerHTML = [
      ['Requests', Number(usage.requests || 0).toLocaleString()], ['Rejected', Number(usage.rejected || 0).toLocaleString()],
      ['In flight', String(usage.in_flight || 0)], ['This minute', String(usage.minute_requests || 0)],
      ['Today', String(usage.daily_requests || 0)], ['Last used', usage.last_used_at ? formatTimestamp(usage.last_used_at) : t('never', 'Never')]
    ].map(function (item) { return '<div><span>' + escapeHtml(item[0]) + '</span><strong>' + escapeHtml(item[1]) + '</strong></div>'; }).join('');
    var hasSecret = Boolean(state.sessionSecrets[key.id]);
    var provided = $('#clientConfigKeySource option[value="provided"]');
    provided.disabled = !hasSecret;
    provided.textContent = hasSecret ? 'Current session secret' : 'Current session secret unavailable';
    $('#clientConfigKeySource').value = 'placeholder';
    $('#clientConfigOutput').textContent = 'Generate a client configuration.';
    $('#copyClientConfigButton').disabled = true;
    $('#downloadClientConfigButton').disabled = true;
    state.clientConfig = null;
  }
  function selectedAllowedModels() {
    return $$('[data-allowed-model]', $('#editAllowedModels')).filter(function (input) { return input.checked; }).map(function (input) { return input.dataset.allowedModel; });
  }
  async function saveApiKey(event) {
    event.preventDefault();
    var button = event.submitter || $('#apiKeyEditForm button[type="submit"]');
    var id = $('#editKeyId').value;
    var expiration = $('#editKeyExpiresAt').value;
    var body = {
      name: $('#editKeyName').value.trim(), description: $('#editKeyDescription').value.trim(), environment: $('#editKeyEnvironment').value,
      status: $('#editKeyStatus').value, clear_expiration: !expiration, expires_at: expiration ? Math.floor(new Date(expiration + 'T23:59:59').getTime() / 1000) : null,
      policy: {
        default_model: $('#editKeyModel').value || null, allowed_models: selectedAllowedModels(), allow_model_override: $('#editKeyAllowModelOverride').checked,
        max_output_tokens: optionalPositiveNumber('#editKeyMaxOutput'), max_reasoning_tokens: optionalPositiveNumber('#editKeyMaxReasoning'),
        reasoning_mode: $('#editKeyReasoning').value, reasoning_effort: $('#editKeyReasoningEffort').value || null,
        limit_action: $('#editKeyLimitAction').value, max_concurrent_requests: optionalPositiveNumber('#editKeyConcurrent'),
        requests_per_minute: optionalPositiveNumber('#editKeyRpm'), daily_request_quota: optionalPositiveNumber('#editKeyDailyQuota'),
        permissions: permissionsFrom('#editKeyPermissions')
      }
    };
    await withBusy(button, 'Saving…', async function () {
      var result = await api('/api/dashboard/control/api-keys/' + encodeURIComponent(id), { method: 'PATCH', body: body });
      state.selectedApiKey = result.key;
      populateApiKeyDrawer(result.key);
      await loadApiKeys();
      toast('API key updated immediately.', 'success');
      pushEvent('API key updated · ' + result.key.name, 'ok');
    });
  }
  async function rotateSelectedApiKey(button) {
    var key = state.selectedApiKey;
    if (!key) return;
    if (!await confirmAction('Rotate API key?', 'The current secret will stop working immediately.')) return;
    await withBusy(button, 'Rotating…', async function () {
      var result = await api('/api/dashboard/control/api-keys/' + encodeURIComponent(key.id) + '/rotate', { method: 'POST', body: { secret_bytes: 32 } });
      state.sessionSecrets[key.id] = result.secret_once;
      state.lastGeneratedKey = result.key;
      state.selectedApiKey = result.key;
      $('#apiKeyDrawer').close();
      $('#secretKeyName').textContent = result.key.name;
      $('#keyOutput').textContent = result.secret_once;
      $('#apiKeySecretDialog').showModal();
      await loadApiKeys();
      toast('API key rotated. The old secret is invalid.', 'success');
    });
  }
  async function revokeSelectedApiKey(button) {
    var key = state.selectedApiKey;
    if (!key) return;
    if (!await confirmAction('Revoke API key?', key.name + ' will be permanently revoked.')) return;
    await withBusy(button, 'Revoking…', async function () {
      await api('/api/dashboard/control/api-keys/' + encodeURIComponent(key.id), { method: 'DELETE' });
      delete state.sessionSecrets[key.id];
      state.selectedApiKey = null;
      $('#apiKeyDrawer').close();
      await loadApiKeys();
      toast('API key revoked immediately.', 'success');
      pushEvent('API key revoked · ' + key.name, 'warn');
    });
  }
  function apiKeyCheckLabel(check) {
    if (check.health === 'healthy') return { label: t('healthy', 'Healthy'), kind: 'ok' };
    if (check.health === 'warning') return { label: t('expiringSoon', 'Expiring soon'), kind: 'warn' };
    if (check.reason === 'disabled') return { label: t('disabled', 'Disabled'), kind: 'error' };
    if (check.reason === 'expired') return { label: t('expired', 'Expired'), kind: 'error' };
    return { label: t('dead', 'Unavailable'), kind: 'error' };
  }

  async function verifyAllApiKeys(button) {
    var summaryRoot = $('#verifyApiKeySummary');
    var resultRoot = $('#verifyApiKeyResult');
    summaryRoot.innerHTML = '<div class="empty-state">' + escapeHtml(t('checkingAllKeys', 'Checking all API keys…')) + '</div>';
    resultRoot.innerHTML = '';
    await withBusy(button, 'Checking…', async function () {
      var result = await api('/api/dashboard/control/api-keys/verify', { method: 'POST', body: {} });
      var summary = result.summary || {};
      var checks = result.checks || [];
      summaryRoot.innerHTML = [
        ['Total', summary.total || 0, 'neutral'],
        [t('healthy', 'Healthy'), summary.healthy || 0, 'ok'],
        [t('expiringSoon', 'Expiring soon'), summary.warning || 0, 'warn'],
        [t('dead', 'Unavailable'), summary.dead || 0, summary.dead ? 'error' : 'neutral']
      ].map(function (item) {
        return '<div class="api-key-check-metric ' + item[2] + '"><span>' + escapeHtml(item[0]) + '</span><strong>' + escapeHtml(item[1]) + '</strong></div>';
      }).join('');
      if (!checks.length) {
        resultRoot.innerHTML = '<div class="empty-state">' + escapeHtml(t('noApiKeys', 'No API keys configured.')) + '</div>';
      } else {
        resultRoot.innerHTML = checks.map(function (check) {
          var health = apiKeyCheckLabel(check);
          var expiry = check.expires_at ? formatTimestamp(check.expires_at) : t('never', 'Never');
          var lastUsed = check.last_used_at ? formatTimestamp(check.last_used_at) : t('never', 'Never');
          return '<div class="api-key-check-row"><div class="api-key-check-main"><strong>' + escapeHtml(check.name) + '</strong><span class="mono">' + escapeHtml(check.fingerprint) + '</span></div><div class="api-key-check-status">' + statusHtml(health.label, health.kind) + '</div><div class="api-key-check-detail"><span>' + escapeHtml(t('expires', 'Expires')) + ': ' + escapeHtml(expiry) + '</span><span>' + escapeHtml(t('lastUsed', 'Last used')) + ': ' + escapeHtml(lastUsed) + '</span></div></div>';
        }).join('');
      }
      var checked = result.checked_at ? formatTimestamp(result.checked_at) : '—';
      $('#verifyApiKeySummary').setAttribute('data-checked-at', checked);
      toast((summary.dead || 0) ? (summary.dead + ' API key(s) need attention.') : 'All managed API keys are available.', (summary.dead || 0) ? 'error' : 'success');
    });
  }
  async function generateClientConfig() {
    var key = state.selectedApiKey;
    if (!key) return;
    var source = $('#clientConfigKeySource').value;
    var secret = source === 'provided' ? state.sessionSecrets[key.id] : null;
    if (source === 'provided' && !secret) throw new Error('This secret is no longer available. Rotate the key first.');
    var result = await withBusy($('#generateClientConfigButton'), 'Generating…', function () {
      return api('/api/dashboard/control/client-config', { method: 'POST', body: { format: $('#clientConfigFormat').value, key_source: source, api_key: secret, model: key.policy && key.policy.default_model } });
    });
    state.clientConfig = result;
    $('#clientConfigOutput').textContent = result.content || '';
    $('#clientConfigSecretBadge').textContent = result.contains_secret ? 'Contains secret' : 'Placeholder key';
    $('#copyClientConfigButton').disabled = false;
    $('#downloadClientConfigButton').disabled = false;
  }
  async function copyClientConfig() {
    if (!state.clientConfig) return;
    await navigator.clipboard.writeText(state.clientConfig.content || '');
    toast('Client configuration copied.', 'success');
  }
  function downloadText(filename, content, type) {
    var blob = new Blob([content], { type: type || 'text/plain;charset=utf-8' });
    var link = document.createElement('a');
    link.href = URL.createObjectURL(blob);
    link.download = filename;
    document.body.appendChild(link);
    link.click();
    var url = link.href;
    link.remove();
    window.setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
  }
  function downloadClientConfig() {
    if (!state.clientConfig) return;
    downloadText(state.clientConfig.filename || 'opencode2api-client.conf', state.clientConfig.content || '', state.clientConfig.content_type);
  }

  async function loadServerLogs() {
    var data = await api('/api/dashboard/control/server/logs?tail=' + encodeURIComponent(Number($('#serverLogTail').value || 200)));
    $('#serverLogOutput').textContent = data.content || 'No log output.';
    $('#serverLogOutput').scrollTop = $('#serverLogOutput').scrollHeight;
    return data;
  }
  async function loadProxyLogs() {
    var data = await api('/api/dashboard/control/proxies/logs?tail=' + encodeURIComponent(Number($('#serverLogTail').value || 200)), { timeout: 30000 });
    $('#proxyLogOutput').textContent = (data.logs || []).map(function (entry) { return '--- proxy :' + entry.port + ' ---\n' + (entry.content || entry.error || 'No output'); }).join('\n\n') || 'No proxy logs.';
    $('#proxyLogOutput').scrollTop = $('#proxyLogOutput').scrollHeight;
    return data;
  }
  async function loadSelectedLogs() {
    var proxyMode = $('#logSource').value === 'proxies';
    $('#serverLogOutput').hidden = proxyMode;
    $('#proxyLogOutput').hidden = !proxyMode;
    return proxyMode ? loadProxyLogs() : loadServerLogs();
  }
  function openLogs(source) {
    $('#logSource').value = source === 'proxies' ? 'proxies' : 'bridge';
    $('#logsDialog').showModal();
    loadSelectedLogs().catch(function (error) { toast(error.message, 'error'); });
  }
  async function loadConfig() {
    var data = await api('/api/dashboard/config/raw');
    state.configRaw = data.raw || '';
    $('#configEditor').value = state.configRaw;
    $('#configPreviewOutput').textContent = 'Loaded active configuration. Validate before saving.';
    return data;
  }
  async function previewConfig(button) {
    return withBusy(button, 'Checking…', async function () {
      var result = await api('/api/dashboard/config/preview', { method: 'POST', body: { content: $('#configEditor').value } });
      $('#configPreviewOutput').textContent = JSON.stringify(result, null, 2);
      toast('Configuration is valid.', 'success');
      return result;
    });
  }
  async function applyConfig(button) {
    var preview = await previewConfig($('#previewConfigButton'));
    if (!await confirmAction('Apply configuration?', (preview.changed_keys || []).length + ' keys will change. ' + (preview.restart_required ? 'A restart will be required.' : 'No restart is required.'))) return;
    await withBusy(button, 'Saving…', async function () {
      var result = await api('/api/dashboard/config/save', { method: 'POST', body: { content: $('#configEditor').value } });
      state.configRaw = $('#configEditor').value;
      $('#configPreviewOutput').textContent = JSON.stringify(result, null, 2);
      toast('Configuration applied atomically.', 'success');
      pushEvent('Configuration applied', 'ok');
    });
  }
  async function initConfig(button) {
    if (!await confirmAction('Reset active configuration?', 'This replaces the active file with the documented template.')) return;
    await withBusy(button, 'Resetting…', async function () {
      await api('/api/dashboard/control/config/init', { method: 'POST', body: { force: true } });
      await loadConfig();
      toast('Configuration template restored.', 'success');
    });
  }
  async function loadTemplate(button) {
    await withBusy(button, 'Loading…', async function () {
      $('#configEditor').value = await api('/api/dashboard/control/config/template', { expect: 'text' });
      $('#configPreviewOutput').textContent = 'Template loaded in the editor. No file has been changed.';
    });
  }
  async function loadDoctor() {
    var data = await api('/api/dashboard/control/doctor', { timeout: 45000 });
    var checks = data.report && data.report.checks ? data.report.checks : [];
    $('#doctorResults').innerHTML = checks.length ? checks.map(function (check) {
      return '<div class="diagnostic-item"><span class="status-dot ' + statusClass(check.status) + '"></span><strong>' + escapeHtml(check.label) + '</strong><span>' + escapeHtml(check.message) + '</span></div>';
    }).join('') : '<div class="empty-state">No diagnostic checks returned.</div>';
    return data;
  }
  async function loadAudit() {
    var data = await api('/api/dashboard/control/audit');
    (data.events || []).slice().reverse().forEach(function (event) {
      if (String(event.action || '').toLowerCase().indexOf('heartbeat') >= 0) return;
      pushEvent(event.action + ' · ' + event.target + ' · ' + event.outcome, event.outcome, new Date(Number(event.timestamp_secs || 0) * 1000).toISOString());
    });
    return data;
  }
  async function selectModel(model, button) {
    await withBusy(button, 'Saving…', async function () {
      await api('/api/dashboard/control/models/select', { method: 'POST', body: { model: model } });
      state.selectedModel = model;
      populateTesterModelControl(model);
      updateCurrentModel();
      renderModels();
      pushEvent('Model selected · ' + model, 'ok');
      toast('Model saved. Restart required.', 'success');
    });
  }
  function testerRequestBody(model, prompt, streaming, thinkingEnabled) {
    var body = {
      model: model,
      messages: [{ role: 'user', content: prompt }],
      max_tokens: thinkingEnabled ? 4096 : 1024,
      stream: streaming,
      thinking: { type: thinkingEnabled ? 'enabled' : 'disabled' },
      include_reasoning: thinkingEnabled
    };
    if (thinkingEnabled) body.reasoning_effort = 'max';
    return body;
  }

  function testerHistoryId() {
    var random = (window.crypto && window.crypto.getRandomValues)
      ? Array.from(window.crypto.getRandomValues(new Uint32Array(2))).map(function (value) { return value.toString(36); }).join('')
      : Math.random().toString(36).slice(2);
    return 'tester-' + Date.now().toString(36) + '-' + random.slice(0, 18);
  }

  async function testerFetch(body, signal, historyContext) {
    var headers = { 'content-type': 'application/json', 'authorization': 'Bearer ' + state.environment.api_key };
    if (historyContext && historyContext.conversationId) headers['x-opencode-history-conversation-id'] = historyContext.conversationId;
    if (historyContext && historyContext.parentRequestId) headers['x-opencode-history-parent-request-id'] = historyContext.parentRequestId;
    if (historyContext && historyContext.operation) headers['x-opencode-history-operation'] = historyContext.operation;
    var response = await fetch('/v1/chat/completions', {
      method: 'POST',
      headers: headers,
      body: JSON.stringify(body),
      signal: signal
    });
    if (!response.ok) {
      var payload = await response.json().catch(function () { return {}; });
      throw new Error((payload.error && payload.error.message) || ('HTTP ' + response.status));
    }
    return response;
  }

  function readTesterStreamChunk(reader, timeoutMs) {
    return new Promise(function (resolve, reject) {
      var timer = window.setTimeout(function () {
        reject(new Error('The model stream stopped sending data.'));
      }, timeoutMs);
      reader.read().then(function (result) {
        window.clearTimeout(timer);
        resolve(result);
      }, function (error) {
        window.clearTimeout(timer);
        reject(error);
      });
    });
  }

  async function recoverTesterResponse(model, prompt, signal, historyContext) {
    var body = testerRequestBody(model, prompt, false, false);
    body.messages = [
      { role: 'system', content: 'Return only a concise final answer. Do not include hidden reasoning or analysis.' },
      { role: 'user', content: prompt }
    ];
    var response = await testerFetch(body, signal, historyContext);
    var data = await response.json();
    var choice = data.choices && data.choices[0] || {};
    var content = choice.message && choice.message.content || '';
    if (!content.trim()) throw new Error('The fallback request returned no final response.');
    return content;
  }

  async function runTester(event) {
    event.preventDefault();
    var button = $('#testerSubmit');
    if (state.testerAbort) state.testerAbort.abort();
    state.testerAbort = new AbortController();
    setBusy(button, true, 'Running…');
    $('#testerOutput').textContent = '';
    $('#testerReasoning').textContent = '';
    $('#testerLatency').textContent = 'Running';
    $('#testerLatency').removeAttribute('title');
    var started = performance.now();
    var recoveryUsed = false;
    try {
      if (!state.environment) await loadEnvironment();
      if (!state.status) await loadStatus();
      var streaming = $('#testerStream').checked;
      var thinkingEnabled = $('#testerThinking').checked;
      var testModel = $('#testerModel').value || state.selectedModel || state.status.model;
      var prompt = $('#testerPrompt').value;
      var testerHistory = { conversationId: testerHistoryId(), operation: 'model_test' };
      var response = await testerFetch(
        testerRequestBody(testModel, prompt, streaming, thinkingEnabled),
        state.testerAbort.signal,
        testerHistory
      );
      var primaryRequestId = response.headers.get('x-request-id') || '';
      var visible = '';
      var reasoning = '';
      var finishReason = '';
      var stalled = false;

      if (streaming) {
        var streamResult = await consumeOpenAiStream(response, thinkingEnabled);
        visible = streamResult.visible;
        reasoning = streamResult.reasoning;
        finishReason = streamResult.finishReason;
        stalled = streamResult.stalled;
      } else {
        var data = await response.json();
        var choice = data.choices && data.choices[0] || {};
        var message = choice.message || {};
        visible = message.content || '';
        reasoning = message.reasoning_content || message.reasoning || message.thinking || '';
        finishReason = choice.finish_reason || '';
        $('#testerOutput').textContent = visible || 'No visible response returned.';
        $('#testerReasoning').textContent = reasoning || (thinkingEnabled ? 'No reasoning returned.' : 'Thinking disabled for this test.');
      }

      var shouldRecover = thinkingEnabled && Boolean(reasoning.trim()) && (!visible.trim() || stalled || finishReason === 'length');
      if (shouldRecover) {
        $('#testerLatency').textContent = 'Recovering';
        $('#testerOutput').textContent = 'Reasoning finished. Recovering final response…';
        try {
          visible = await recoverTesterResponse(testModel, prompt, state.testerAbort.signal, {
            conversationId: testerHistory.conversationId,
            parentRequestId: primaryRequestId,
            operation: 'response_recovery'
          });
          $('#testerOutput').textContent = visible;
          recoveryUsed = true;
        } catch (recoveryError) {
          var recoveryReason = finishReason === 'length'
            ? 'Reasoning used the available output limit.'
            : (stalled ? 'The reasoning stream stopped before a final response.' : 'The model returned reasoning without final content.');
          $('#testerOutput').textContent = recoveryReason + ' Automatic recovery failed: ' + recoveryError.message;
        }
      } else if (!visible.trim()) {
        $('#testerOutput').textContent = finishReason === 'length'
          ? 'No final response returned because the output limit was reached.'
          : 'No visible response returned.';
      }

      var elapsed = Math.round(performance.now() - started);
      $('#testerLatency').textContent = elapsed + ' ms';
      if (recoveryUsed) $('#testerLatency').title = 'Final response recovered automatically after the reasoning stream ended.';
      pushEvent('Model test passed · ' + shortModel(testModel) + (thinkingEnabled ? ' · thinking' : '') + (recoveryUsed ? ' · recovered' : ''), 'ok');
    } catch (error) {
      if (error.name !== 'AbortError') {
        $('#testerOutput').textContent = 'Error: ' + error.message;
        $('#testerLatency').textContent = 'Failed';
        toast(error.message, 'error');
      }
    } finally {
      setBusy(button, false);
      state.testerAbort = null;
    }
  }

  async function consumeOpenAiStream(response, thinkingEnabled) {
    if (!response.body || !response.body.getReader) throw new Error('Streaming is not supported by this browser');
    var reader = response.body.getReader();
    var decoder = new TextDecoder();
    var buffer = '';
    var visible = '';
    var reasoning = '';
    var finishReason = '';
    var doneReceived = false;
    var stalled = false;

    function render() {
      var output = $('#testerOutput');
      var reasoningOutput = $('#testerReasoning');
      output.textContent = visible || 'Waiting for response…';
      reasoningOutput.textContent = reasoning || (thinkingEnabled ? 'Waiting for reasoning…' : 'Thinking disabled for this test.');
      output.scrollTop = output.scrollHeight;
      reasoningOutput.scrollTop = reasoningOutput.scrollHeight;
    }

    function processLine(line) {
      line = line.trim();
      if (line.indexOf('data:') !== 0) return false;
      var raw = line.slice(5).trim();
      if (!raw) return false;
      if (raw === '[DONE]') return true;
      try {
        var event = JSON.parse(raw);
        (event.choices || []).forEach(function (choice) {
          var delta = choice.delta || {};
          if (choice.finish_reason) finishReason = choice.finish_reason;
          reasoning += delta.reasoning_content || delta.reasoning || delta.thinking || '';
          visible += delta.content || '';
        });
      } catch (_error) {}
      return false;
    }

    try {
      while (!doneReceived) {
        var part;
        try {
          part = await readTesterStreamChunk(reader, 45000);
        } catch (readError) {
          if (readError.message === 'The model stream stopped sending data.') {
            stalled = true;
            break;
          }
          throw readError;
        }
        if (part.done) {
          buffer += decoder.decode();
          break;
        }
        buffer += decoder.decode(part.value, { stream: true });
        var lines = buffer.split(/\r?\n/);
        buffer = lines.pop() || '';
        for (var index = 0; index < lines.length; index += 1) {
          if (processLine(lines[index])) {
            doneReceived = true;
            break;
          }
        }
        render();
      }

      if (!doneReceived && buffer.trim()) doneReceived = processLine(buffer);
    } finally {
      if (doneReceived || stalled) reader.cancel().catch(function () {});
    }

    render();
    if (!reasoning) $('#testerReasoning').textContent = thinkingEnabled ? 'No reasoning returned.' : 'Thinking disabled for this test.';
    return {
      visible: visible,
      reasoning: reasoning,
      finishReason: finishReason,
      doneReceived: doneReceived,
      stalled: stalled
    };
  }
  async function restartProxy(port, button) {
    if (!await confirmAction('Restart proxy :' + port + '?', 'Protected standby nodes are never restarted.')) return;
    await withBusy(button, 'Restarting…', async function () {
      var result = await api('/api/dashboard/proxy/' + encodeURIComponent(port) + '/restart', { method: 'POST', body: {} });
      if (result.status === 'error') throw new Error(result.message || 'Proxy restart failed');
      await loadProxies();
      toast('Proxy :' + port + ' restarted.', 'success');
      pushEvent('Proxy restarted · :' + port, 'ok');
    });
  }
  async function setProxyDrain(port, action, button) {
    var draining = action === 'drain';
    var title = (draining ? t('drain', 'Drain') : t('undrain', 'Undrain')) + ' proxy :' + port + '?';
    var note = draining ? 'Fresh requests will stop using this node; existing requests may finish.' : 'The node becomes eligible for fresh traffic again only if its health and circuit state allow it.';
    if (!await confirmAction(title, note)) return;
    await withBusy(button, draining ? 'Draining…' : 'Restoring…', async function () {
      var result = await api('/api/dashboard/proxy/' + encodeURIComponent(port) + '/' + action, { method: 'POST', body: {} });
      if (result.status === 'error') throw new Error(result.message || 'Proxy drain operation failed');
      await loadProxies();
      toast('Proxy :' + port + (draining ? ' is draining.' : ' restored to routing eligibility.'), 'success');
      pushEvent('Proxy ' + (draining ? 'draining' : 'undrained') + ' · :' + port, draining ? 'warn' : 'ok');
    });
  }
  async function restartServer(button) {
    if (!await confirmAction('Restart bridge?', 'The dashboard will disconnect briefly and reconnect when the new process is healthy.')) return;
    var previousPid = state.status && state.status.pid;
    await withBusy(button, 'Scheduling…', async function () {
      await api('/api/dashboard/control/server/restart', { method: 'POST', body: {} });
      setConnection('warn', 'Restarting');
      pushEvent('Server restart scheduled', 'warn');
      await waitForServerCycle(true, previousPid);
    });
  }
  async function stopServer(button) {
    if (!await confirmAction('Stop bridge?', 'The dashboard and API will become unavailable until the CLI starts the server again.')) return;
    await withBusy(button, 'Scheduling…', async function () {
      await api('/api/dashboard/control/server/stop', { method: 'POST', body: {} });
      setConnection('warn', 'Stopping');
      await waitForServerCycle(false, null);
    });
  }
  async function probeServerStatus() {
    var controller = new AbortController();
    var timeout = window.setTimeout(function () { controller.abort(); }, 1800);
    try {
      var response = await fetch('/api/dashboard/status', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
      if (!response.ok) return null;
      return await response.json();
    } catch (_error) { return null; }
    finally { window.clearTimeout(timeout); }
  }
  async function waitForServerCycle(expectReturn, previousPid) {
    var sawDown = false;
    for (var attempt = 0; attempt < 50; attempt += 1) {
      await new Promise(function (resolve) { window.setTimeout(resolve, 500); });
      var current = await probeServerStatus();
      if (!current) { sawDown = true; setConnection('error', t('offline', 'Offline')); }
      var pidChanged = Boolean(current && previousPid && current.pid && current.pid !== previousPid);
      if (expectReturn && current && (sawDown || pidChanged)) { setConnection('ok', t('connected', 'Connected')); location.reload(); return; }
      if (!expectReturn && !current) { toast('Bridge stopped. Start it again from the CLI.', 'success'); return; }
    }
    throw new Error(expectReturn ? 'Bridge did not return before timeout' : 'Bridge did not stop before timeout');
  }
  async function checkUpdate(button) {
    $('#updateStatus').textContent = 'Checking release source…';
    try {
      await withBusy(button, 'Checking…', async function () {
        var result = await api('/api/dashboard/control/update/check', { timeout: 30000 });
        state.lastUpdate = result;
        $('#updateStatus').textContent = result.available ? ('Version ' + result.latest + ' is available.') : ('Current version ' + result.current + ' is up to date.');
        $('#applyUpdateButton').disabled = !result.available || !result.asset_available;
      });
    } catch (error) {
      state.lastUpdate = null;
      $('#applyUpdateButton').disabled = true;
      $('#updateStatus').textContent = 'Check failed: ' + error.message;
    }
  }
  async function applyUpdate(button) {
    if (!await confirmAction('Apply update?', 'The release binary will be verified before installation.')) return;
    await withBusy(button, 'Scheduling…', async function () {
      await api('/api/dashboard/control/update/apply', { method: 'POST', body: { confirm: true, force: false } });
      toast('Updater scheduled. Review logs for completion.', 'success');
    });
  }
  async function generateCompletion(button) {
    var shell = $('#completionShell').value;
    await withBusy(button, 'Generating…', async function () {
      var text = await api('/api/dashboard/control/completions/' + encodeURIComponent(shell), { expect: 'text' });
      downloadText('opencode2api.' + shell, text, 'text/plain;charset=utf-8');
    });
  }
  function formatBytes(value) {
    var bytes = Math.max(0, Number(value || 0));
    if (bytes < 1024) return bytes + ' B';
    var units = ['KiB', 'MiB', 'GiB', 'TiB'];
    var index = -1;
    do { bytes /= 1024; index += 1; } while (bytes >= 1024 && index < units.length - 1);
    return bytes.toFixed(bytes >= 10 ? 1 : 2) + ' ' + units[index];
  }

  function formatMilliseconds(value) {
    var ms = Math.max(0, Number(value || 0));
    if (ms < 1000) return Math.round(ms) + ' ms';
    return (ms / 1000).toFixed(ms >= 10000 ? 1 : 2) + ' s';
  }

  function formatHistoryTime(value) {
    var date = new Date(Number(value || 0));
    return Number.isNaN(date.getTime()) ? '—' : date.toLocaleString(state.lang === 'vi' ? 'vi-VN' : undefined);
  }

  function historyStatusKind(status) {
    if (status === 'completed') return 'ok';
    if (status === 'failed') return 'error';
    if (status === 'cancelled' || status === 'interrupted') return 'warn';
    return 'neutral';
  }

  function historyStatusLabel(status) {
    var labels = {
      completed: t('completed', 'Completed'),
      failed: t('failed', 'Failed'),
      cancelled: t('cancelled', 'Cancelled'),
      interrupted: t('interrupted', 'Interrupted'),
      running: t('running', 'Running')
    };
    return labels[status] || status || 'unknown';
  }

  function historyFilterObject() {
    var output = {};
    var q = $('#historySearch').value.trim();
    if (q) output.q = q;
    if ($('#historyStatusFilter').value) output.status = $('#historyStatusFilter').value;
    if ($('#historyProtocolFilter').value) output.protocol = $('#historyProtocolFilter').value;
    if ($('#historyModelFilter').value) output.model = $('#historyModelFilter').value;
    if ($('#historyThinkingFilter').value) output.thinking = $('#historyThinkingFilter').value === 'true';
    if ($('#historyStreamFilter').value) output.stream = $('#historyStreamFilter').value === 'true';
    return output;
  }

  function historyQuery() {
    var params = new URLSearchParams();
    var values = historyFilterObject();
    Object.keys(values).forEach(function (key) { params.set(key, String(values[key])); });
    params.set('limit', String(state.historyLimit));
    params.set('offset', String(state.historyOffset));
    return params;
  }

  function renderHistoryMetrics() {
    var stats = state.historyStats || {};
    var storage = state.historyStorage || {};
    $('#historyMetricToday').textContent = Number(stats.today || 0).toLocaleString();
    $('#historyMetricTotal').textContent = Number(stats.total || 0).toLocaleString() + ' total';
    var terminal = Number(stats.success || 0) + Number(stats.failed || 0) + Number(stats.cancelled || 0);
    $('#historyMetricSuccess').textContent = terminal ? Math.round(Number(stats.success || 0) * 100 / terminal) + '%' : '—';
    $('#historyMetricFailed').textContent = Number(stats.failed || 0).toLocaleString() + ' ' + t('failed', 'failed').toLowerCase();
    $('#historyMetricLatency').textContent = stats.average_latency_ms == null ? '—' : formatMilliseconds(stats.average_latency_ms);
    $('#historyMetricSize').textContent = formatBytes(storage.logical_bytes || stats.stored_bytes || 0);
    $('#historyMetricStorage').textContent = formatBytes(storage.physical_bytes || 0) + ' file · ' + Number(storage.records || stats.total || 0).toLocaleString() + ' records';
    var kind = storage.available && storage.enabled ? 'ok' : (storage.available ? 'warn' : 'error');
    var label = !storage.available ? 'History unavailable' : (storage.enabled ? ('History enabled · ' + (storage.capture_mode || 'redacted')) : 'History disabled');
    $('#historyCaptureState').innerHTML = '<span class="status-dot ' + kind + '"></span><span>' + escapeHtml(label) + '</span>';
  }

  function populateHistoryModelFilter() {
    var select = $('#historyModelFilter');
    var current = select.value;
    var models = {};
    state.models.forEach(function (item) { models[item.id] = item.label || item.id; });
    state.historyItems.forEach(function (item) {
      [item.requested_model, item.effective_model, item.response_model].forEach(function (model) { if (model) models[model] = shortModel(model); });
    });
    select.innerHTML = '<option value="">' + escapeHtml(t('allModels', 'All models')) + '</option>' + Object.keys(models).sort().map(function (id) {
      return '<option value="' + escapeHtml(id) + '">' + escapeHtml(models[id]) + '</option>';
    }).join('');
    if (Object.prototype.hasOwnProperty.call(models, current)) select.value = current;
  }

  function renderHistoryTable() {
    var root = $('#historyTable');
    var selectedId = state.selectedHistory && state.selectedHistory.request && state.selectedHistory.request.id;
    $('#historyCountBadge').textContent = Number(state.historyTotal || 0).toLocaleString();
    if (!state.historyItems.length) {
      root.innerHTML = '<div class="empty-state">' + escapeHtml(t('noHistoryRecords', 'No request history matches the current filters.')) + '</div>';
    } else {
      root.innerHTML = state.historyItems.map(function (item) {
        var tokens = Number(item.input_tokens || 0) + Number(item.output_tokens || 0);
        var model = item.response_model || item.effective_model || item.requested_model || '—';
        var prompt = item.prompt_preview || t('contentNotCaptured', 'Content not captured');
        var flags = [item.protocol, item.stream ? 'stream' : 'sync', item.thinking_requested ? 'thinking' : 'no-thinking'].filter(Boolean).join(' · ');
        return '<button class="resource-list-item history-resource-item ' + (item.id === selectedId ? 'active' : '') + '" type="button" data-history-id="' + escapeHtml(item.id) + '">' +
          '<span class="resource-item-main"><span class="history-resource-heading"><strong>' + escapeHtml(item.client_name || item.client_key_id || 'Anonymous') + '</strong><span class="mono">' + escapeHtml(formatHistoryTime(item.started_at_ms)) + '</span></span><span class="history-resource-prompt">' + escapeHtml(prompt) + '</span><span class="resource-item-meta">' + escapeHtml(flags + ' · ' + shortModel(model)) + '</span></span>' +
          '<span class="resource-item-side">' + statusHtml(historyStatusLabel(item.status), historyStatusKind(item.status)) + '<span class="mono resource-item-time">' + escapeHtml((item.duration_ms == null ? '—' : formatMilliseconds(item.duration_ms)) + ' · ' + (tokens ? tokens.toLocaleString() + ' tok' : '—')) + '</span></span></button>';
      }).join('');
    }
    var start = state.historyTotal ? state.historyOffset + 1 : 0;
    var end = Math.min(state.historyOffset + state.historyItems.length, state.historyTotal);
    $('#historyPageInfo').textContent = start + '–' + end + ' / ' + Number(state.historyTotal || 0).toLocaleString();
    $('#historyPreviousButton').disabled = state.historyOffset <= 0;
    $('#historyNextButton').disabled = state.historyOffset + state.historyLimit >= state.historyTotal;
    populateHistoryModelFilter();
  }

  async function loadHistoryStats() {
    var data = await api('/api/dashboard/control/history/stats', { timeout: 30000 });
    state.historyStats = data.stats || {};
    state.historyStorage = data.storage || {};
    renderHistoryMetrics();
    return data;
  }

  async function loadHistory(resetOffset) {
    if (resetOffset) state.historyOffset = 0;
    var data = await api('/api/dashboard/control/history?' + historyQuery().toString(), { timeout: 30000 });
    state.historyItems = data.items || [];
    state.historyTotal = Number(data.total || 0);
    state.historyLimit = Number(data.limit || state.historyLimit);
    state.historyOffset = Number(data.offset || 0);
    renderHistoryTable();
    var selectedId = state.selectedHistory && state.selectedHistory.request && state.selectedHistory.request.id;
    var selectedVisible = selectedId && state.historyItems.some(function (item) { return item.id === selectedId; });
    if (state.historyItems.length && !selectedVisible) await openHistoryDetail(state.historyItems[0].id);
    if (!state.historyItems.length) clearHistoryDetail();
    return data;
  }

  async function refreshHistory(resetOffset) {
    return Promise.allSettled([loadHistory(resetOffset), loadHistoryStats()]);
  }

  function historyOverviewRows(item) {
    return [
      ['Request ID', item.id],
      ['Operation', item.operation_kind || '—'],
      ['Conversation', item.conversation_id || '—'],
      ['Parent request', item.parent_request_id || '—'],
      [t('time', 'Started'), formatHistoryTime(item.started_at_ms)],
      [t('status', 'Status'), historyStatusLabel(item.status)],
      [t('protocol', 'Protocol'), item.protocol + ' · ' + item.endpoint],
      [t('client', 'Client'), item.client_name || item.client_key_id || 'Anonymous'],
      [t('environment', 'Environment'), item.client_environment || '—'],
      ['Requested model', item.requested_model || '—'],
      ['Effective model', item.effective_model || '—'],
      ['Response model', item.response_model || '—'],
      [t('streamResponse', 'Streaming'), item.stream ? t('enabled', 'Enabled') : t('disabled', 'Disabled')],
      [t('thinking', 'Thinking'), item.thinking_requested ? (item.reasoning_effort || t('enabled', 'Enabled')) : t('disabled', 'Disabled')],
      [t('latency', 'Latency'), item.duration_ms == null ? '—' : formatMilliseconds(item.duration_ms)],
      ['First chunk', item.time_to_first_chunk_ms == null ? '—' : formatMilliseconds(item.time_to_first_chunk_ms)],
      ['Input tokens', item.input_tokens == null ? '—' : Number(item.input_tokens).toLocaleString()],
      ['Output tokens', item.output_tokens == null ? '—' : Number(item.output_tokens).toLocaleString()],
      ['Reasoning tokens', item.reasoning_tokens == null ? '—' : Number(item.reasoning_tokens).toLocaleString()],
      ['Finish reason', item.finish_reason || '—'],
      ['Retries / fallbacks', Number(item.retry_count || 0) + ' / ' + Number(item.fallback_count || 0)],
      ['Tools / searches', Number(item.tool_call_count || 0) + ' / ' + Number(item.search_count || 0)],
      ['Capture', item.capture_mode + (item.redacted ? ' · redacted' : '') + (item.truncated ? ' · truncated' : '') + (item.capture_incomplete ? ' · incomplete' : '')],
      ['Stored', formatBytes(item.stored_bytes || 0)],
      ['Error', item.error_message || item.error_type || '—']
    ];
  }

  function clearHistoryDetail() {
    state.selectedHistory = null;
    state.historyContent = {};
    $('#historyDetailTitle').textContent = t('requestDetail', 'Select a request');
    $('#historyDetailMeta').textContent = t('requestHistoryDescription', 'Prompt, reasoning and response will appear here.');
    $('#historyOverview').innerHTML = '<div class="empty-state">' + escapeHtml(t('noHistoryRecords', 'Select a request from the left.')) + '</div>';
    $('#historyDeleteButton').disabled = true;
    $('#historyExportOneButton').disabled = true;
    ['#historyContentInbound', '#historyContentEffective', '#historyContentReasoning', '#historyContentResponse', '#historyContentRaw'].forEach(function (selector) {
      $(selector).textContent = t('contentNotCaptured', 'Select a request first.');
    });
    renderHistoryTable();
  }

  function renderHistoryDetail(detail) {
    state.selectedHistory = detail;
    state.historyContent = {};
    $('#historyDeleteButton').disabled = false;
    $('#historyExportOneButton').disabled = false;
    var item = detail.request;
    $('#historyDetailTitle').textContent = shortModel(item.effective_model || item.requested_model) + ' request';
    $('#historyDetailMeta').textContent = item.id + ' · ' + formatHistoryTime(item.started_at_ms) + ' · ' + historyStatusLabel(item.status);
    $('#historyOverview').innerHTML = historyOverviewRows(item).map(function (row) {
      return '<div class="history-overview-item"><span>' + escapeHtml(row[0]) + '</span><strong>' + escapeHtml(row[1]) + '</strong></div>';
    }).join('');
    var toolEvents = (detail.events || []).filter(function (event) { return ['tool_call', 'search_completed', 'retry_scheduled', 'model_fallback', 'response_recovered'].includes(event.event_type); });
    $('#historyToolsTimeline').innerHTML = toolEvents.length ? toolEvents.map(function (event) {
      return '<div class="history-event"><span class="history-event-time">' + escapeHtml(formatHistoryTime(event.timestamp_ms)) + '</span><div class="history-event-main"><strong>' + escapeHtml(event.event_type.replace(/_/g, ' ')) + '</strong><pre>' + escapeHtml(JSON.stringify(event.metadata || {}, null, 2)) + '</pre></div>' + statusHtml(event.severity || 'info', event.severity === 'warn' ? 'warn' : 'neutral') + '</div>';
    }).join('') : '<div class="empty-state">' + escapeHtml(t('noToolEvents', 'No tool or search events.')) + '</div>';
    $('#historyAttemptList').innerHTML = (detail.attempts || []).length ? detail.attempts.map(function (attempt) {
      var summary = { model: attempt.model, loop: attempt.loop_number, http_status: attempt.http_status, finish_reason: attempt.finish_reason, error: attempt.error_message };
      return '<div class="history-attempt"><span class="history-event-time">#' + escapeHtml(attempt.attempt_number) + ' · ' + escapeHtml(attempt.attempt_kind) + '</span><div class="history-attempt-main"><strong>' + escapeHtml(attempt.model || 'Unknown model') + '</strong><pre>' + escapeHtml(JSON.stringify(summary, null, 2)) + '</pre></div>' + statusHtml(historyStatusLabel(attempt.status), historyStatusKind(attempt.status)) + '</div>';
    }).join('') : '<div class="empty-state">' + escapeHtml(t('noAttempts', 'No attempts recorded.')) + '</div>';
    ['inbound_request', 'effective_request', 'reasoning', 'response', 'provider_raw_response'].forEach(function (kind) {
      var descriptor = (detail.contents || []).find(function (content) { return content.kind === kind; });
      var badge = document.getElementById({ inbound_request: 'historyInboundBadge', effective_request: 'historyEffectiveBadge', reasoning: 'historyReasoningBadge', response: 'historyResponseBadge', provider_raw_response: 'historyRawBadge' }[kind]);
      if (badge) badge.textContent = descriptor ? (formatBytes(descriptor.stored_bytes) + (descriptor.redacted ? ' · redacted' : '') + (descriptor.truncated ? ' · truncated' : '')) : t('notCaptured', 'Not captured');
      var pane = historyContentPane(kind);
      if (pane) pane.textContent = descriptor ? t('selectTabToLoad', 'Select this tab to load content.') : t('contentNotCaptured', 'Content was not captured for this request.');
    });
    renderHistoryTable();
    openHistoryTab('overview');
  }

  function historyContentPane(kind) {
    var ids = { inbound_request: '#historyContentInbound', effective_request: '#historyContentEffective', reasoning: '#historyContentReasoning', response: '#historyContentResponse', provider_raw_response: '#historyContentRaw' };
    return ids[kind] ? $(ids[kind]) : null;
  }

  async function loadHistoryContent(kind) {
    if (!state.selectedHistory || ['overview', 'tools', 'attempts'].includes(kind)) return;
    var pane = historyContentPane(kind);
    if (!pane) return;
    var descriptor = (state.selectedHistory.contents || []).find(function (content) { return content.kind === kind; });
    if (!descriptor) {
      pane.textContent = t('contentNotCaptured', 'Content was not captured for this request.');
      return;
    }
    if (Object.prototype.hasOwnProperty.call(state.historyContent, kind)) {
      pane.textContent = state.historyContent[kind];
      return;
    }
    pane.textContent = t('loading', 'Loading…');
    try {
      var data = await api('/api/dashboard/control/history/' + encodeURIComponent(state.selectedHistory.request.id) + '/content/' + encodeURIComponent(kind), { timeout: 30000 });
      state.historyContent[kind] = data.content && data.content.body || '';
      pane.textContent = state.historyContent[kind] || t('emptyContent', 'Empty content.');
    } catch (error) {
      pane.textContent = 'Error: ' + error.message;
    }
  }

  function openHistoryTab(kind) {
    $$('[data-history-tab]').forEach(function (button) { button.classList.toggle('active', button.dataset.historyTab === kind); });
    $$('[data-history-panel]').forEach(function (panel) { panel.classList.toggle('active', panel.dataset.historyPanel === kind); });
    loadHistoryContent(kind).catch(function () {});
  }

  async function openHistoryDetail(id) {
    var data = await api('/api/dashboard/control/history/' + encodeURIComponent(id), { timeout: 30000 });
    renderHistoryDetail(data.detail);
  }

  async function exportHistory(request, filename) {
    var data = await api('/api/dashboard/control/history/export', { method: 'POST', body: request, timeout: 60000 });
    downloadText(filename, JSON.stringify(data, null, 2), 'application/json;charset=utf-8');
    toast('History export created.', 'success');
  }

  async function deleteSelectedHistory(button) {
    if (!state.selectedHistory) return;
    if (!await confirmAction(t('deleteRequest', 'Delete request?'), 'This permanently removes the selected request and all captured content.')) return;
    await withBusy(button, 'Deleting…', async function () {
      await api('/api/dashboard/control/history/' + encodeURIComponent(state.selectedHistory.request.id), { method: 'DELETE' });
      clearHistoryDetail();
      await refreshHistory(false);
      toast('History request deleted.', 'success');
    });
  }

  async function purgeHistory(button) {
    if (!await confirmAction(t('purgeHistory', 'Purge all history?'), 'This permanently deletes all request history. This action cannot be undone.')) return;
    await withBusy(button, 'Purging…', async function () {
      await api('/api/dashboard/control/history/purge', { method: 'POST', body: { confirm: true, all: true }, timeout: 60000 });
      state.historyOffset = 0;
      await refreshHistory(true);
      toast('Request history purged.', 'success');
    });
  }

  function renderHistoryStorageFacts(settingsData) {
    var storage = settingsData.storage || {};
    $('#historyStorageFacts').innerHTML = [
      [t('status', 'Status'), storage.available ? (storage.enabled ? t('enabled', 'Enabled') : t('disabled', 'Disabled')) : t('unavailable', 'Unavailable')],
      [t('storedSize', 'Logical size'), formatBytes(storage.logical_bytes || 0)],
      ['Database file', formatBytes(storage.physical_bytes || 0)],
      [t('requests', 'Records'), Number(storage.records || 0).toLocaleString()],
      ['Path', storage.path || '—'],
      ['Last error', storage.last_error || '—']
    ].map(function (item) { return '<div class="history-storage-card"><span>' + escapeHtml(item[0]) + '</span><strong>' + escapeHtml(item[1]) + '</strong></div>'; }).join('');
  }

  async function openHistorySettings() {
    var data = await api('/api/dashboard/control/history/settings', { timeout: 30000 });
    state.historySettings = data.settings || {};
    state.historyStorage = data.storage || state.historyStorage;
    $('#historyEnabledSetting').checked = Boolean(state.historySettings.enabled);
    $('#historyCaptureModeSetting').value = state.historySettings.capture_mode || 'redacted';
    $('#historyRetentionSetting').value = state.historySettings.retention_days == null ? 30 : state.historySettings.retention_days;
    $('#historyMaxRecordsSetting').value = state.historySettings.max_records || 10000;
    $('#historyMaxBytesSetting').value = Math.max(1, Math.round(Number(state.historySettings.max_database_bytes || 1073741824) / 1048576));
    renderHistoryStorageFacts(data);
    $('#historySettingsDialog').showModal();
  }

  async function saveHistorySettings(event) {
    event.preventDefault();
    var button = event.submitter || $('#historySettingsForm button[type="submit"]');
    await withBusy(button, 'Saving…', async function () {
      var body = {
        enabled: $('#historyEnabledSetting').checked,
        capture_mode: $('#historyCaptureModeSetting').value,
        retention_days: Number($('#historyRetentionSetting').value || 0),
        max_records: Number($('#historyMaxRecordsSetting').value || 1),
        max_database_bytes: Number($('#historyMaxBytesSetting').value || 1) * 1048576
      };
      var data = await api('/api/dashboard/control/history/settings', { method: 'PATCH', body: body });
      state.historySettings = data.settings || body;
      $('#historySettingsDialog').close();
      await refreshHistory(false);
      toast('History settings updated for new requests.', 'success');
    });
  }

  function clearHistoryFilters() {
    ['historySearch', 'historyStatusFilter', 'historyProtocolFilter', 'historyModelFilter', 'historyThinkingFilter', 'historyStreamFilter'].forEach(function (id) { document.getElementById(id).value = ''; });
    loadHistory(true).catch(function (error) { toast(error.message, 'error'); });
  }

  async function logout() {
    try { await fetch('/api/dashboard/logout', { method: 'POST', credentials: 'same-origin' }); }
    finally { location.replace('/'); }
  }

  async function refreshOverview() { return Promise.allSettled([loadStatus(), loadMetrics(), loadProxies()]); }
  async function refreshView(view) {
    if (view === 'dashboard') return refreshOverview();
    if (view === 'api') return Promise.allSettled([loadApiKeys(), loadModels()]);
    if (view === 'models') return Promise.allSettled([loadStatus(), loadModels(), loadEnvironment()]);
    if (view === 'history') return refreshHistory(false);
    if (view === 'system') return Promise.allSettled([loadStatus(), loadMetrics(), loadProxies(), loadApiKeys()]);
  }
  function connectEvents() {
    if (state.eventSource) state.eventSource.close();
    var source = new EventSource('/api/dashboard/events', { withCredentials: true });
    state.eventSource = source;
    source.onopen = function () { setConnection('ok', t('connected', 'Connected')); };
    source.onerror = function () { setConnection('warn', t('reconnecting', 'Reconnecting')); };
    ['proxy_status', 'proxy_log', 'config_saved'].forEach(function (name) {
      source.addEventListener(name, function (event) {
        var data;
        try { data = JSON.parse(event.data); } catch (_error) { data = { message: event.data }; }
        var message = data.message || data.status || name.replace(/_/g, ' ');
        if (data.port) message += ' · :' + data.port;
        pushEvent(message, data.status || name, data.timestamp);
        if (name === 'proxy_status') loadProxies().catch(function () {});
      });
    });
  }

  function bindEvents() {
    $('#navList').addEventListener('click', function (event) { var button = event.target.closest('[data-view]'); if (button) switchView(button.dataset.view); });
    $('#refreshButton').addEventListener('click', function () { withBusy(this, 'Refreshing…', function () { return refreshView(state.view); }).catch(function () {}); });
    $('#menuButton').addEventListener('click', function () { document.body.classList.toggle('sidebar-open'); });
    $('#sidebarBackdrop').addEventListener('click', function () { document.body.classList.remove('sidebar-open'); });
    $('#languageToggle').addEventListener('click', toggleLanguage);
    $('#logoutButton').addEventListener('click', logout);
    $('#clearEventsButton').addEventListener('click', function () { state.eventItems = []; renderEvents(); });
    $$('[data-jump-view]').forEach(function (button) { button.addEventListener('click', function () { switchView(button.dataset.jumpView); }); });
    $('#quickCreateKey').addEventListener('click', function () { switchView('api'); openCreateApiKeyDialog(); });
    $('#quickTestModel').addEventListener('click', function () { switchView('models'); window.setTimeout(function () { $('#testerPrompt').focus(); }, 50); });
    $('#quickViewLogs').addEventListener('click', function () { openLogs('bridge'); });
    $$('[data-global-action]').forEach(function (button) {
      button.addEventListener('click', function () {
        var action = button.dataset.globalAction;
        if (action === 'restart') restartServer(button).catch(function () {});
        if (action === 'logs') openLogs('bridge');
        if (action === 'doctor') { $('#diagnosticsDialog').showModal(); loadDoctor().catch(function () {}); }
        if (action === 'logout') logout();
        var details = button.closest('details'); if (details) details.removeAttribute('open');
      });
    });

    $('#createApiKeyButton').addEventListener('click', openCreateApiKeyDialog);
    $('#verifyApiKeyButton').addEventListener('click', function () {
      $('#apiKeyVerifyDialog').showModal();
      verifyAllApiKeys($('#verifyAllApiKeysButton')).catch(function (error) { toast(error.message, 'error'); });
    });
    $('#apiKeySearch').addEventListener('input', renderApiKeyTable);
    $('#apiKeyStatusFilter').addEventListener('change', renderApiKeyTable);
    $('#apiKeyInventory').addEventListener('click', function (event) {
      var row = event.target.closest('[data-api-key-id]');
      if (row) selectApiKeyOverview(row.dataset.apiKeyId);
    });
    $('#apiKeyInventory').addEventListener('keydown', function (event) {
      var row = event.target.closest('[data-api-key-id]');
      if (row && ['Enter', ' '].includes(event.key)) { event.preventDefault(); selectApiKeyOverview(row.dataset.apiKeyId); }
    });
    $('#apiKeyInventory').addEventListener('dblclick', function (event) {
      var row = event.target.closest('[data-api-key-id]');
      if (row) openApiKeyDrawer(row.dataset.apiKeyId).catch(function (error) { toast(error.message, 'error'); });
    });
    $('#editSelectedApiKeyButton').addEventListener('click', function () {
      if (state.selectedApiKeyOverviewId) openApiKeyDrawer(state.selectedApiKeyOverviewId).catch(function (error) { toast(error.message, 'error'); });
    });
    $('#apiKeyCreateForm').addEventListener('submit', createApiKey);
    $('#createKeyPreset').addEventListener('change', function () { setCreatePreset(this.value); });
    $('#apiKeyEditForm').addEventListener('submit', saveApiKey);
    $('#rotateApiKeyButton').addEventListener('click', function () { rotateSelectedApiKey(this).catch(function () {}); });
    $('#revokeApiKeyButton').addEventListener('click', function () { revokeSelectedApiKey(this).catch(function () {}); });
    $('#verifyAllApiKeysButton').addEventListener('click', function () { verifyAllApiKeys(this).catch(function (error) { toast(error.message, 'error'); }); });
    $('#copyGeneratedKeyButton').addEventListener('click', function () { copyGeneratedSecret().catch(function (error) { toast(error.message, 'error'); }); });
    $('#downloadGeneratedConfigButton').addEventListener('click', function () { downloadGeneratedConfig().catch(function (error) { toast(error.message, 'error'); }); });
    $('#generateClientConfigButton').addEventListener('click', function () { generateClientConfig().catch(function (error) { toast(error.message, 'error'); }); });
    $('#copyClientConfigButton').addEventListener('click', function () { copyClientConfig().catch(function (error) { toast(error.message, 'error'); }); });
    $('#downloadClientConfigButton').addEventListener('click', downloadClientConfig);
    $$('[data-drawer-tab]').forEach(function (button) { button.addEventListener('click', function () { openDrawerTab(button.dataset.drawerTab); }); });

    $('#modelSearch').addEventListener('input', renderModels);
    $('#reloadModelsButton').addEventListener('click', function () { withBusy(this, 'Loading…', loadModels).catch(function () {}); });
    $('#modelGrid').addEventListener('click', function (event) { var button = event.target.closest('[data-select-model]'); if (button) selectModel(button.dataset.selectModel, button).catch(function () {}); });
    $('#testerForm').addEventListener('submit', runTester);
    $('#restartModelButton').addEventListener('click', function () { restartServer(this).catch(function () {}); });

    $('#historySearch').addEventListener('input', function () {
      if (state.historySearchTimer) window.clearTimeout(state.historySearchTimer);
      state.historySearchTimer = window.setTimeout(function () { loadHistory(true).catch(function (error) { toast(error.message, 'error'); }); }, 280);
    });
    ['historyStatusFilter', 'historyProtocolFilter', 'historyModelFilter', 'historyThinkingFilter', 'historyStreamFilter'].forEach(function (id) {
      document.getElementById(id).addEventListener('change', function () { loadHistory(true).catch(function (error) { toast(error.message, 'error'); }); });
    });
    $('#historyClearFiltersButton').addEventListener('click', clearHistoryFilters);
    $('#historyPreviousButton').addEventListener('click', function () { state.historyOffset = Math.max(0, state.historyOffset - state.historyLimit); loadHistory(false).catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyNextButton').addEventListener('click', function () { state.historyOffset += state.historyLimit; loadHistory(false).catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyTable').addEventListener('click', function (event) { var row = event.target.closest('[data-history-id]'); if (row) openHistoryDetail(row.dataset.historyId).catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyTable').addEventListener('keydown', function (event) { var row = event.target.closest('[data-history-id]'); if (row && ['Enter', ' '].includes(event.key)) { event.preventDefault(); openHistoryDetail(row.dataset.historyId).catch(function (error) { toast(error.message, 'error'); }); } });
    $('#historyDetailTabs').addEventListener('click', function (event) { var button = event.target.closest('[data-history-tab]'); if (button) openHistoryTab(button.dataset.historyTab); });
    $$('[data-copy-history-content]').forEach(function (button) { button.addEventListener('click', function () { var value = state.historyContent[button.dataset.copyHistoryContent]; if (!value) { toast('Open the tab before copying its content.', 'error'); return; } navigator.clipboard.writeText(value).then(function () { toast('History content copied.', 'success'); }).catch(function (error) { toast(error.message, 'error'); }); }); });
    $('#historyDeleteButton').addEventListener('click', function () { deleteSelectedHistory(this).catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyExportOneButton').addEventListener('click', function () { if (!state.selectedHistory) return; exportHistory({ ids: [state.selectedHistory.request.id], format: 'json' }, 'opencode2api-history-' + state.selectedHistory.request.id + '.json').catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyExportFilteredButton').addEventListener('click', function () { exportHistory({ query: historyFilterObject(), format: 'json' }, 'opencode2api-history-filtered.json').catch(function (error) { toast(error.message, 'error'); }); });
    $('#historyPurgeButton').addEventListener('click', function () { purgeHistory(this).catch(function (error) { toast(error.message, 'error'); }); });
    $('#historySettingsButton').addEventListener('click', function () { openHistorySettings().catch(function (error) { toast(error.message, 'error'); }); });
    $('#historySettingsForm').addEventListener('submit', saveHistorySettings);

    $('#restartServerButton').addEventListener('click', function () { restartServer(this).catch(function () {}); });
    $('#stopServerButton').addEventListener('click', function () { stopServer(this).catch(function () {}); });
    $('#reloadProxiesButton').addEventListener('click', function () { withBusy(this, 'Loading…', loadProxies).catch(function () {}); });
    $('#networkProxyTable').addEventListener('click', function (event) {
      var drain = event.target.closest('[data-drain-port]');
      var restart = event.target.closest('[data-restart-port]');
      var logs = event.target.closest('[data-proxy-logs]');
      if (drain) setProxyDrain(drain.dataset.drainPort, drain.dataset.drainAction, drain).catch(function () {});
      if (restart) restartProxy(restart.dataset.restartPort, restart).catch(function () {});
      if (logs) openLogs('proxies');
    });
    $('#openLogsButton').addEventListener('click', function () { openLogs('bridge'); });
    $('#openDiagnosticsButton').addEventListener('click', function () { $('#diagnosticsDialog').showModal(); loadDoctor().catch(function (error) { toast(error.message, 'error'); }); });
    $('#openConfigButton').addEventListener('click', function () { $('#configDialog').showModal(); loadConfig().catch(function (error) { toast(error.message, 'error'); }); });
    $('#logSource').addEventListener('change', function () { loadSelectedLogs().catch(function (error) { toast(error.message, 'error'); }); });
    $('#reloadServerLogsButton').addEventListener('click', function () { withBusy(this, 'Loading…', loadSelectedLogs).catch(function () {}); });
    $('#runDoctorButton').addEventListener('click', function () { withBusy(this, 'Running…', loadDoctor).catch(function () {}); });
    $('#reloadConfigButton').addEventListener('click', function () { withBusy(this, 'Loading…', loadConfig).catch(function () {}); });
    $('#loadTemplateButton').addEventListener('click', function () { loadTemplate(this).catch(function () {}); });
    $('#previewConfigButton').addEventListener('click', function () { previewConfig(this).catch(function () {}); });
    $('#applyConfigButton').addEventListener('click', function () { applyConfig(this).catch(function () {}); });
    $('#initConfigButton').addEventListener('click', function () { initConfig(this).catch(function () {}); });
    $('#checkUpdateButton').addEventListener('click', function () { checkUpdate(this).catch(function () {}); });
    $('#applyUpdateButton').addEventListener('click', function () { applyUpdate(this).catch(function () {}); });
    $('#downloadCompletionButton').addEventListener('click', function () { generateCompletion(this).catch(function () {}); });

    $$('[data-close-dialog]').forEach(function (button) { button.addEventListener('click', function () { var dialog = document.getElementById(button.dataset.closeDialog); if (dialog && dialog.open) dialog.close(); }); });
    document.addEventListener('keydown', function (event) { var target = event.target; if (target && /INPUT|TEXTAREA|SELECT/.test(target.tagName)) return; if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r') { event.preventDefault(); refreshView(state.view).catch(function () {}); } });
  }

  async function init() {
    try {
      await ensureSession();
      applyLanguage();
      bindEvents();
      var initial = location.hash.slice(1);
      if (initial === 'activity') initial = 'dashboard';
      if (['server', 'network', 'configuration', 'diagnostics'].includes(initial)) initial = 'system';
      switchView(viewMeta[initial] ? initial : 'dashboard', false);
      connectEvents();
      await Promise.allSettled([refreshOverview(), loadEnvironment(), loadModels(), loadApiKeys(), loadAudit()]);
      state.uptimeTimer = window.setInterval(renderLiveUptime, 1000);
      state.refreshTimer = window.setInterval(function () { if (document.visibilityState === 'visible') refreshOverview().catch(function () {}); }, 15000);
    } catch (error) {
      setConnection('error', t('unavailable', 'Unavailable'));
      toast(error.message || String(error), 'error');
    }
  }

  window.addEventListener('beforeunload', function () {
    if (state.eventSource) state.eventSource.close();
    if (state.refreshTimer) window.clearInterval(state.refreshTimer);
    if (state.uptimeTimer) window.clearInterval(state.uptimeTimer);
    if (state.testerAbort) state.testerAbort.abort();
  });
  document.addEventListener('DOMContentLoaded', init);
}());
