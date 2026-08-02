# OpenCode2API — Mecha Control Deck

Status: implementation audit and delivery plan
Date: 2026-07-22

## 1. Frontend audit

### Runtime and framework

- Frontend is a vanilla HTML/CSS/JavaScript SPA.
- Dashboard assets are embedded by `rust-embed` from `src/webui/`.
- Routing is view-state based through `data-view` and `data-view-panel`; there is no client router dependency.
- Backend APIs, authentication, CSRF, data models and request lifecycle are already implemented and must remain unchanged.
- The five production views already exist: Dashboard, API Keys, Models, History and System.

### Current source ownership

```text
src/webui/index.html       DOM for all five pages, dialogs and drawers
src/webui/style.css        current modern theme and responsive rules
src/webui/app.js           API calls, state, rendering and interaction logic
src/webui/landing.html     login portal
src/dashboard/assets.rs    RustEmbed asset serving and CSP
```

### Shared components already represented in DOM/CSS

The current markup already has reusable semantic primitives which will be preserved and themed:

```text
app-shell / sidebar / topbar
section-card / metric-card
button / icon-button / compact-button
status-tag / count-badge / notice
resource-list / data-table
modal / drawer / tooltip-like popovers
toast region
split-workspace / list pane / detail pane
```

### Main visual problems before redesign

1. Visual language is generic dark SaaS and does not express OpenCode2API identity.
2. Cards, tables and controls use the same rectangular treatment with limited hierarchy.
3. No theme architecture beyond global root tokens.
4. Decorative assets are limited to a logo; no original control-room, mecha, mascot or empty-state system.
5. Navigation icons are line icons but not part of a distinct product icon system.
6. Login and dashboard do not share a strong visual universe.
7. Empty states are text-only.
8. Status, action and danger hierarchy are functional but not visually memorable.
9. Existing logic is concentrated in `app.js`; a full framework rewrite would introduce unnecessary regression risk.

## 2. Implementation approach

### Preserve

- All endpoint paths.
- Authentication and CSRF behavior.
- Existing IDs and data attributes consumed by `app.js`.
- Existing data rendering and mutation functions.
- Existing modal and drawer behavior.
- Existing responsive logic where it supports accessibility.

### Add/refactor

```text
src/webui/mecha.css                   theme and shared component layer
src/webui/mecha-components.js         theme state, decorative composition and accessibility helpers
src/webui/assets/mecha/**             production image assets
src/assets/mecha/manifest.ts          requested typed asset manifest
scripts/generate_mecha_assets.py      reproducible pixel-asset generator
```

### Theme strategy

```html
<html data-theme="mecha">
```

- `style.css` remains the modern baseline.
- `mecha.css` overrides tokens and shared component primitives only inside `[data-theme="mecha"]`.
- A theme switch permits `modern` and `mecha` without breaking the existing UI.
- Theme preference is stored in `localStorage`.

## 3. Component mapping

The project is not React, so components are implemented as semantic DOM/CSS primitives plus small JavaScript composition helpers:

```text
AppShell           .app-shell
Sidebar            .sidebar
TopHeader          .topbar
PixelPanel         .section-card, .modal-shell, .drawer-panel
PixelCard          .metric-card, .resource-item
PixelButton        .button, .icon-button, .compact-button
StatusBadge        .status-tag
MetricCard         .metric-card
DataTable          .data-table
FilterBar          .table-toolbar, .history-toolbar
EmptyState         .empty-state + data-mecha-state
LoadingState       .empty-state[data-loading]
MechaMascot        injected decorative <img> elements
NotificationToast  existing toast container, themed
Modal              existing dialog shells, themed
Drawer             existing drawer shell, themed
Tooltip            native title/menu popovers, themed
```

## 4. Planned asset structure

```text
src/webui/assets/mecha/
  branding/
  backgrounds/
  frames/
  icons/
  status/
  metrics/
  mascot/
  buttons/
  states/
```

### Runtime production assets

Branding:

```text
logo-primary.png
logo-icon.png
logo-monochrome.svg
favicon-16.png
favicon-32.png
favicon-64.png
app-icon-512.png
```

Backgrounds:

```text
bg-app.webp
bg-app-mobile.webp
bg-sidebar.webp
bg-header.webp
bg-control-room.webp
bg-control-room-mobile.webp
bg-grid-texture.png
bg-stars-texture.png
bg-panel-noise.png
bg-empty-state.webp
```

Frames and ornaments:

```text
frame-card-default.9.png
frame-card-active.9.png
frame-card-danger.9.png
frame-modal.9.png
frame-tooltip.9.png
frame-sidebar-active.9.png
divider-horizontal.png
divider-vertical.png
corner-decoration-tl.png
corner-decoration-tr.png
corner-decoration-bl.png
corner-decoration-br.png
```

Icons and states are generated in 16, 20, 24 and 32 pixel variants. Navigation/action icons include default, hover, active and disabled variants.

Mascot and original universe:

```text
ARIA-02, an original AI operator and mecha systems engineer
a small original drone companion
an original non-franchise mecha silhouette
```

## 5. Design tokens

The required palette is implemented as canonical variables and mapped to existing component variables:

```css
--bg-canvas: #080B1A;
--bg-sidebar: #0D1025;
--bg-header: #10142B;
--bg-surface: #14182F;
--bg-surface-hover: #1B2040;
--bg-surface-active: #242957;
--primary: #8B6CFF;
--primary-hover: #A18BFF;
--primary-dark: #5B43C9;
--anime-pink: #FF8FCF;
--cyber-cyan: #5CCBFF;
--success: #55E6A5;
--warning: #FFBD68;
--error: #FF6F91;
```

Typography:

- Pixel-display stack for compact headings and labels, with a system monospace fallback so no font binary is shipped.
- Inter/system sans for body content.
- JetBrains Mono/system mono for technical data.

## 6. Responsive strategy

Desktop:

- Fixed sidebar.
- Existing single-viewport/master-detail layouts retained.
- Decorative control-room art is restricted to edges and empty space.

Tablet:

- Compact sidebar rail behavior.
- Two-column metrics.
- Smaller mascot art.

Mobile:

- Existing navigation drawer retained.
- One-column cards.
- Tables retain controlled horizontal scroll or resource-card representation.
- Decorative backgrounds reduced.
- Touch targets remain at least 44px where practical.

## 7. Risks and mitigation

| Risk | Mitigation |
|---|---|
| Existing IDs changed and JS breaks | Preserve every logic-bound ID/data attribute |
| Asset 404 because of embed path | Keep all runtime assets under `src/webui/` and test each URL |
| CSS theme causes overflow | Scope overrides to `[data-theme="mecha"]` and run viewport audits |
| Pixel aesthetic reduces readability | Use pixel font only for short labels/headings; body remains sans/mono |
| Heavy images affect load | Generate optimized PNG/WebP and use edge-only low-detail backgrounds |
| Animation accessibility | Add `prefers-reduced-motion` overrides |
| Backend regression | No endpoint/backend changes; run full Rust suite |

## 8. Files modified or created

Planned modifications:

```text
src/webui/index.html
src/webui/landing.html
src/webui/app.js
src/webui/style.css                  minimal compatibility additions only
src/dashboard/assets.rs             CSP/asset MIME behavior only if required
REPO_WORKLOG.md
```

Planned additions:

```text
src/webui/mecha.css
src/webui/mecha-components.js
src/webui/assets/mecha/**
src/assets/mecha/manifest.ts
scripts/generate_mecha_assets.py
docs/design/MECHA_CONTROL_DECK_AUDIT.md
artifacts/mecha-control-deck/**
```

## 9. Validation gates

- Build and Rust tests.
- JavaScript syntax checks.
- All five views in English and Vietnamese.
- Login portal.
- Buttons, modals, drawers and theme switch.
- 1920x1080, 1440x900, 1024x768, 390x844 and 320x800.
- Keyboard focus and reduced motion.
- Contrast sampling.
- Asset URL 404 scan.
- Pixel assets have `image-rendering: pixelated`.
- No console errors or page errors.
- Backend API behavior unchanged.
