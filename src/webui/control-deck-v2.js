(function () {
  'use strict';

  var REDUCED_MOTION = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var METRIC_SELECTOR = '.metric-card strong, .count-badge, .status-value';
  var PANEL_SELECTOR = '.metric-card, .section-card, .modal-shell, .drawer-shell, .menu-popover';

  function isMecha() {
    return document.documentElement.dataset.theme === 'mecha';
  }

  function installAmbientField() {
    if (document.querySelector('.v2-ambient-field')) return;
    var field = document.createElement('div');
    field.className = 'v2-ambient-field';
    field.setAttribute('aria-hidden', 'true');
    for (var i = 0; i < 12; i += 1) {
      var particle = document.createElement('span');
      particle.style.setProperty('--v2-particle-x', ((i * 37) % 97) + '%');
      particle.style.setProperty('--v2-particle-y', ((i * 53) % 91) + '%');
      particle.style.setProperty('--v2-particle-delay', (-i * .9) + 's');
      particle.style.setProperty('--v2-particle-duration', (12 + (i % 5) * 3) + 's');
      field.appendChild(particle);
    }
    document.body.prepend(field);
  }

  function revealPanels(root) {
    var scope = root && root.querySelectorAll ? root : document;
    scope.querySelectorAll(PANEL_SELECTOR).forEach(function (panel, index) {
      if (panel.dataset.v2Bound) return;
      panel.dataset.v2Bound = 'true';
      panel.style.setProperty('--v2-reveal-delay', Math.min(index % 8, 7) * 28 + 'ms');
      if (REDUCED_MOTION) panel.classList.add('v2-revealed');
    });
  }

  function installRevealObserver() {
    revealPanels(document);
    if (REDUCED_MOTION || !window.IntersectionObserver) {
      document.querySelectorAll(PANEL_SELECTOR).forEach(function (panel) { panel.classList.add('v2-revealed'); });
      return;
    }
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        entry.target.classList.add('v2-revealed');
        observer.unobserve(entry.target);
      });
    }, { threshold: .06 });
    document.querySelectorAll(PANEL_SELECTOR).forEach(function (panel) { observer.observe(panel); });

    var mutationObserver = new MutationObserver(function (mutations) {
      mutations.forEach(function (mutation) {
        mutation.addedNodes.forEach(function (node) {
          if (node.nodeType !== 1) return;
          revealPanels(node);
          if (node.matches && node.matches(PANEL_SELECTOR)) observer.observe(node);
          node.querySelectorAll && node.querySelectorAll(PANEL_SELECTOR).forEach(function (panel) { observer.observe(panel); });
        });
      });
    });
    mutationObserver.observe(document.body, { childList: true, subtree: true });
  }

  function pulseMetric(element) {
    if (REDUCED_MOTION || !isMecha()) return;
    element.classList.remove('v2-value-updated');
    void element.offsetWidth;
    element.classList.add('v2-value-updated');
  }

  function installMetricObserver() {
    var elements = Array.prototype.slice.call(document.querySelectorAll(METRIC_SELECTOR));
    var observer = new MutationObserver(function (mutations) {
      mutations.forEach(function (mutation) {
        pulseMetric(mutation.target.nodeType === 3 ? mutation.target.parentElement : mutation.target);
      });
    });
    elements.forEach(function (element) {
      observer.observe(element, { childList: true, characterData: true, subtree: true });
    });
  }

  function installPointerLighting() {
    if (REDUCED_MOTION || !window.matchMedia('(pointer:fine)').matches) return;
    document.addEventListener('pointermove', function (event) {
      var panel = event.target.closest && event.target.closest('.section-card, .metric-card, .quick-action-card');
      if (!panel || !isMecha()) return;
      var rect = panel.getBoundingClientRect();
      panel.style.setProperty('--v2-pointer-x', (event.clientX - rect.left) + 'px');
      panel.style.setProperty('--v2-pointer-y', (event.clientY - rect.top) + 'px');
      panel.classList.add('v2-pointer-active');
    }, { passive: true });
    document.addEventListener('pointerout', function (event) {
      var panel = event.target.closest && event.target.closest('.section-card, .metric-card, .quick-action-card');
      if (!panel || (event.relatedTarget && panel.contains(event.relatedTarget))) return;
      panel.classList.remove('v2-pointer-active');
    }, { passive: true });
  }

  function installDialogState() {
    document.querySelectorAll('dialog').forEach(function (dialog) {
      dialog.addEventListener('close', function () { dialog.classList.remove('v2-window-open'); });
      var observer = new MutationObserver(function () {
        if (dialog.open) dialog.classList.add('v2-window-open');
        else dialog.classList.remove('v2-window-open');
      });
      observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    });
  }

  function installViewState() {
    var main = document.getElementById('mainContent');
    if (!main) return;
    var observer = new MutationObserver(function (mutations) {
      mutations.forEach(function (mutation) {
        if (mutation.type !== 'attributes') return;
        var panel = mutation.target;
        var previousClasses = (mutation.oldValue || '').split(/\s+/);
        var wasActive = previousClasses.indexOf('active') !== -1;
        if (wasActive || !panel.classList.contains('active')) return;
        panel.classList.remove('v2-view-live');
        void panel.offsetWidth;
        panel.classList.add('v2-view-live');
      });
    });
    main.querySelectorAll('.view').forEach(function (view) {
      observer.observe(view, {
        attributes: true,
        attributeFilter: ['class'],
        attributeOldValue: true
      });
    });
  }

  function boot() {
    installAmbientField();
    installRevealObserver();
    installMetricObserver();
    installPointerLighting();
    installDialogState();
    installViewState();
    document.documentElement.classList.add('control-deck-v2-ready');
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot, { once: true });
  else boot();
}());
