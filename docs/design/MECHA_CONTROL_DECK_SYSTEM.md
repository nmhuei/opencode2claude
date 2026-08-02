# OpenCode2API — Mecha Control Deck Design System

Version: 1.0
Date: 2026-07-22
Theme selector: `data-theme="mecha"`
Modern fallback: `data-theme="modern"`

## Visual premise

OpenCode2API is presented as an original local mecha command centre. The product language uses control-deck modules, AI cores, mission records and unit networks while preserving ordinary dashboard semantics and accessibility.

Original universe:

- **ARIA-02** — AI operator and mecha systems engineer.
- **Relay Drone** — small API-gateway companion.
- **OC2 Maintenance Unit** — original broad-shouldered service mecha silhouette.

No character, silhouette or insignia is based on a named anime or mecha franchise.

## Token contract

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
--primary-soft: rgba(139, 108, 255, .16);

--anime-pink: #FF8FCF;
--anime-pink-soft: rgba(255, 143, 207, .14);
--cyber-cyan: #5CCBFF;
--cyber-cyan-soft: rgba(92, 203, 255, .14);
--success: #55E6A5;
--success-soft: rgba(85, 230, 165, .14);
--warning: #FFBD68;
--warning-soft: rgba(255, 189, 104, .14);
--error: #FF6F91;
--error-soft: rgba(255, 111, 145, .14);

--text-primary: #F8F7FF;
--text-secondary: #B8B7D3;
--text-muted: #777A9C;
--text-disabled: #4C4F70;
--border-subtle: #24284B;
--border-default: #343963;
--border-active: #8B6CFF;
--divider: rgba(183, 184, 220, .12);
```

## Typography

The production theme intentionally uses local system stacks so it remains available offline.

```text
Technical headings: Oxanium-like local tech stack / system sans fallback
Pixel labels: system monospace pixel-display stack
Body: Inter-like system sans stack
Code and IDs: JetBrains Mono-like system mono stack
```

Pixel/technical display treatment is limited to:

- Page and section headings.
- Compact badges.
- Metric labels and values.
- Short commands.

Body descriptions, JSON, request IDs, tables and form content remain sans or monospace.

## Spacing and geometry

```text
Base spacing: 4px
Control height: 40px
Primary touch target: 44px on mobile
Panel notch: 10px desktop, 7px mobile
Panel radius fallback: 3px
Single-viewport desktop metric height: 84px
History metric height: 88px
Single-viewport Quick Actions height: 66px
```

The notched HUD shape is decorative. Content boxes keep standard DOM flow and do not rely on rasterised text.

## Component primitives

### PixelPanel

Applied to:

- `.section-card`
- `.modal-shell`
- `.drawer-shell`
- `.login-panel`

Properties:

- Dark translucent surface.
- Nine-slice-compatible frame overlay.
- Notched corners.
- Subtle cyan/purple edge light.
- No continuous animation.

### PixelCard

Applied to metric and resource cards. Cards use real HTML text and existing live data. Icons are separate PNG assets.

### PixelButton

Button labels remain HTML. Raster assets only decorate the background. States:

```text
default
hover
active/pressed
disabled
danger
```

### StatusBadge

Semantic colours remain stable:

```text
healthy/completed/online -> green
warning/degraded          -> amber
failed/error/offline      -> pink-red
neutral/connecting        -> cyan/purple
```

### DataTable and ResourceList

- Real HTML table/list content.
- Technical IDs use monospace.
- Hover and selected rows use restrained edge lighting.
- No image-rendered table labels or fake data.

### EmptyState

A text region remains accessible. A generated state asset or ARIA-02 illustration is attached by `mecha-components.js` and hidden in modern mode.

### Theme switch

The topbar/login theme control sets:

```text
data-theme="mecha"
data-theme="modern"
```

Preference is persisted in localStorage under:

```text
opencode2api-dashboard-theme
```

## Page-specific mapping

### Dashboard

```text
Metric cards        -> mecha subsystem modules
System status       -> unit health check
Recent activity     -> mission/event log
Quick Actions       -> command shortcuts
ARIA-02             -> small edge-only operator illustration
```

### API Keys

```text
Key rows            -> pilot access cards
Active              -> green HUD status
Production          -> pink/purple environment badge
Create              -> primary command
Check               -> secondary command
```

### Models

```text
Current model       -> active AI core
Available models    -> selectable AI units
Test Model          -> simulation chamber
Run Test            -> launch simulation command
```

### History

```text
Request list        -> mission records
Detail pane         -> selected mission telemetry
Reasoning/response  -> fixed-height internally scrolling technical panes
Protocol/status     -> compact HUD badges
```

### System

```text
Server              -> command core / main reactor
Security            -> defence system
Proxy pool          -> unit network
Restart             -> warning/maintenance action
Stop                -> explicit danger action
```

## Responsive rules

### Desktop

- Fixed sidebar.
- Existing single-viewport layout retained.
- Four metric cards in one row.
- Master-detail workspaces remain visible simultaneously.
- Decorative art is confined to panel edges.

### Tablet

- Existing compact navigation behaviour retained.
- Two-column and split layouts use repository breakpoints.
- Large silhouette is removed.

### Mobile

- Sidebar uses the existing drawer behaviour.
- Cards stack into one column.
- ARIA-02 is represented by a 34px topbar avatar.
- Large mascot and control-room illustrations are hidden.
- Pixel background density and scanlines are reduced.

## Motion

Allowed:

- 1–2px button press.
- Small stepped drone float.
- Connecting/status pulse.
- Short colour and border transitions.

Disabled under `prefers-reduced-motion`:

- All repeating animation.
- Smooth scrolling and long transitions.

## Asset production

Generator:

```text
scripts/generate_mecha_assets.py
```

Output:

```text
src/webui/assets/mecha/
```

Typed manifest:

```text
src/assets/mecha/manifest.ts
```

Nine-slice metadata:

```text
src/webui/assets/mecha/frames/NINE_SLICE.md
```

Generated files use transparent PNG for pixel assets and lossless WebP for large backgrounds/states. All runtime image assets are served from the RustEmbed directory under `/dashboard/assets/mecha/`.

## Accessibility contract

- Button labels are real text.
- Icons and mascot art are decorative unless explicitly labelled.
- Existing IDs, ARIA attributes, dialog semantics and keyboard handlers are preserved.
- Focus uses a 2px cyan outline with a visible outer halo.
- Body text and primary buttons are tested against WCAG contrast thresholds.
- Model-produced content is still rendered as text, not HTML.

## Validation matrix

```text
1920x1080
1440x900
1024x768
390x844
320x800
English
Vietnamese
Mecha theme
Modern fallback
Reduced motion
```
