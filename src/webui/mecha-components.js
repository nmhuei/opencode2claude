(function () {
  'use strict';

  var STORAGE_KEY = 'opencode2api-dashboard-theme';
  var THEMES = ['mecha', 'modern'];
  var ASSET_BASE = '/dashboard/assets/mecha';

  function preferredTheme() {
    var saved = null;
    try { saved = localStorage.getItem(STORAGE_KEY); } catch (_) { /* storage may be blocked */ }
    return THEMES.indexOf(saved) >= 0 ? saved : 'mecha';
  }

  function themeLabel(theme) {
    return theme === 'mecha' ? 'Mecha deck active' : 'Modern deck active';
  }

  function updateThemeControls(theme) {
    document.querySelectorAll('[data-theme-toggle]').forEach(function (button) {
      var target = theme === 'mecha' ? 'modern' : 'mecha';
      button.dataset.targetTheme = target;
      button.setAttribute('aria-pressed', String(theme === 'mecha'));
      button.setAttribute('aria-label', 'Switch to ' + target + ' theme');
      button.title = 'Switch to ' + target + ' theme';
      var label = button.querySelector('[data-theme-label]');
      if (label) label.textContent = theme === 'mecha' ? 'MECHA' : 'MODERN';
    });
  }

  function applyTheme(theme, persist) {
    if (THEMES.indexOf(theme) < 0) theme = 'mecha';
    document.documentElement.dataset.theme = theme;
    if (document.body) document.body.dataset.theme = theme;
    if (persist) {
      try { localStorage.setItem(STORAGE_KEY, theme); } catch (_) { /* storage may be blocked */ }
    }
    updateThemeControls(theme);
    document.dispatchEvent(new CustomEvent('opencode-theme-change', { detail: { theme: theme } }));
  }

  applyTheme(preferredTheme(), false);

  function createDecorativeImage(className, src, alt) {
    var image = document.createElement('img');
    image.className = className + ' pixel-art';
    image.src = src;
    image.alt = alt || '';
    image.decoding = 'async';
    image.loading = 'eager';
    if (!alt) image.setAttribute('aria-hidden', 'true');
    return image;
  }

  function installDeckDecorations() {
    var menuButton = document.getElementById('menuButton');
    if (menuButton && !document.querySelector('.mecha-mobile-avatar')) {
      menuButton.insertAdjacentElement('afterend', createDecorativeImage(
        'mecha-mobile-avatar',
        ASSET_BASE + '/mascot/aria-avatar-64.png',
        ''
      ));
    }

    var sidebarFooter = document.querySelector('.sidebar-footer');
    if (sidebarFooter && !sidebarFooter.querySelector('.mecha-sidebar-drone')) {
      sidebarFooter.prepend(createDecorativeImage(
        'mecha-sidebar-drone',
        ASSET_BASE + '/mascot/drone-connected.png',
        ''
      ));
    }

    var quickActions = document.querySelector('.quick-actions');
    if (quickActions && !quickActions.querySelector('.mecha-operator-figure')) {
      quickActions.append(createDecorativeImage(
        'mecha-operator-figure',
        ASSET_BASE + '/mascot/aria-idle.png',
        ''
      ));
      quickActions.dataset.mechaModule = 'command-shortcuts';
    }

    var dashboard = document.querySelector('[data-view-panel="dashboard"]');
    if (dashboard && !dashboard.querySelector('.mecha-unit-silhouette')) {
      dashboard.append(createDecorativeImage(
        'mecha-unit-silhouette',
        ASSET_BASE + '/mascot/mecha-silhouette.webp',
        ''
      ));
    }

    var tester = document.getElementById('modelTesterPanel');
    if (tester && !tester.querySelector('.mecha-simulation-drone')) {
      tester.append(createDecorativeImage(
        'mecha-simulation-drone',
        ASSET_BASE + '/mascot/drone-mascot.png',
        ''
      ));
      tester.dataset.mechaModule = 'simulation-chamber';
    }

    document.querySelectorAll('.metric-card').forEach(function (card, index) {
      card.dataset.mechaMetric = String(index % 4);
    });
    document.querySelectorAll('.section-card').forEach(function (card) {
      if (!card.dataset.mechaModule) card.dataset.mechaModule = 'panel';
    });
  }

  function stateForEmptyElement(element) {
    var parent = element.parentElement;
    while (parent && parent !== document.body) {
      if (parent.id === 'apiKeyInventory') return 'empty-api-keys';
      if (parent.id === 'modelGrid') return 'empty-models';
      if (parent.id === 'historyTable') return 'empty-history';
      if (parent.id === 'proxyTableBody' || parent.id === 'proxyPoolTable') return 'empty-proxy';
      if (parent.id === 'testerOutput' || parent.id === 'testerReasoning') return 'loading-core';
      parent = parent.parentElement;
    }
    var text = (element.textContent || '').toLowerCase();
    if (text.indexOf('loading') >= 0 || text.indexOf('waiting') >= 0) return 'loading-core';
    if (text.indexOf('error') >= 0 || text.indexOf('failed') >= 0) return 'server-error';
    return 'empty-history';
  }

  function decorateEmptyStates(root) {
    var scope = root && root.querySelectorAll ? root : document;
    scope.querySelectorAll('.empty-state').forEach(function (element) {
      if (!element.dataset.mechaState) element.dataset.mechaState = stateForEmptyElement(element);
    });
  }

  function installEmptyStateObserver() {
    decorateEmptyStates(document);
    var target = document.getElementById('mainContent') || document.body;
    if (!target || !window.MutationObserver) return;
    var observer = new MutationObserver(function (mutations) {
      mutations.forEach(function (mutation) {
        mutation.addedNodes.forEach(function (node) {
          if (node.nodeType !== 1) return;
          if (node.matches && node.matches('.empty-state')) decorateEmptyStates(node.parentElement || node);
          else decorateEmptyStates(node);
        });
      });
    });
    observer.observe(target, { childList: true, subtree: true });
  }

  function installLoginDecorations() {
    var panel = document.querySelector('.login-panel');
    if (!panel) return;
    panel.dataset.mechaModule = 'access-gate';
    if (!panel.querySelector('.login-aria-operator')) {
      panel.append(createDecorativeImage(
        'login-aria-operator',
        ASSET_BASE + '/mascot/aria-avatar-128.png',
        ''
      ));
    }
    if (!panel.querySelector('.login-drone')) {
      panel.append(createDecorativeImage(
        'login-drone',
        ASSET_BASE + '/mascot/drone-connected.png',
        ''
      ));
    }
  }

  function bindThemeControls() {
    document.querySelectorAll('[data-theme-toggle]').forEach(function (button) {
      button.addEventListener('click', function () {
        var current = document.documentElement.dataset.theme || 'mecha';
        applyTheme(current === 'mecha' ? 'modern' : 'mecha', true);
      });
    });
    updateThemeControls(document.documentElement.dataset.theme || 'mecha');
  }

  function boot() {
    applyTheme(preferredTheme(), false);
    bindThemeControls();
    installDeckDecorations();
    installLoginDecorations();
    installEmptyStateObserver();
    document.documentElement.classList.add('mecha-components-ready');
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once: true });
  else boot();

  window.MechaUI = {
    applyTheme: applyTheme,
    currentTheme: function () { return document.documentElement.dataset.theme || 'mecha'; },
    assetBase: ASSET_BASE,
    themeLabel: themeLabel
  };
}());
