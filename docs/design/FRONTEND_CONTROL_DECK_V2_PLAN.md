# Frontend Control Deck V2 — Renovation plan

Date: 2026-07-23
Scope: `src/webui/` only. Existing backend routes, API contracts, IDs and JavaScript business logic remain unchanged.

## Visual target

Rebuild the dashboard around the approved dark sci-fi control-plane reference:

- Deep navy canvas with restrained cyan/violet illumination.
- Clear geometric panels instead of a heavy pixel-game skin.
- Strong information hierarchy and compact operational density.
- One coherent visual language for Dashboard, API Keys, Models, History, System, login, dialogs, drawers, menus, confirmation windows and toasts.
- Subtle motion that communicates state without distracting from data.

## Implementation strategy

1. Keep the current semantic DOM, element IDs and data attributes so API behavior and regression tests remain stable.
2. Add a final scoped visual layer: `control-deck-v2.css`, loaded after `mecha.css`.
3. Add a small motion/controller layer: `control-deck-v2.js`, loaded after existing scripts.
4. Improve only selected markup where the approved visual requires it, notably Quick Actions.
5. Scope all new design rules to `html[data-theme="mecha"]`; Modern remains available as a fallback theme.

## Design system

### Color

- Canvas: `#030816` / `#071024`
- Panel: translucent `#09142b`
- Border: blue `rgba(73, 132, 214, .35)`
- Primary: violet `#8d63ff`
- Secondary: cyan `#37c7ff`
- Healthy: green `#48efaa`
- Warning: amber `#ffc857`
- Error: coral `#ff6f91`

### Typography

- Local system fonts only.
- Headings: compact, high-contrast, 600–750 weight.
- Operational labels: uppercase, 10–11px, increased tracking.
- Body: 12–14px depending on viewport.
- Monospace reserved for IDs, logs, timings and payload content.

### Geometry

- Sidebar: 232–250px.
- Topbar: 76px desktop, 62px mobile.
- Panels use 10–14px cut-corner geometry and a thin inner highlight.
- Controls: 40px standard height.
- Desktop content prioritizes one viewport; long data uses deliberate internal panes.

## Motion system

- Page/view entry: short opacity + translate transition.
- Active navigation: moving light rail.
- Status dots: low-amplitude pulse.
- Panels: subtle edge sweep on hover.
- Metric values: brief reveal/settle when refreshed.
- Dialogs and drawers: scale/translate entry with animated border energy.
- Background: very slow grid drift and two low-opacity ambient light fields.
- All motion disabled under `prefers-reduced-motion: reduce`.

## Dialog/window rules

- Consistent backdrop, header, body and footer across every popup.
- Animated cyan/violet border ring on open.
- No nested outer scrollbars.
- Create API Key should fit a common desktop viewport without body scrolling.
- Logs/config/history content may scroll only in their content pane.
- Drawers slide from the right and preserve 40px controls.

## View-specific layout

### Dashboard

- Four compact metric cards.
- Balanced two-column System Status / Recent Activity workspace.
- Quick Actions becomes three full action cards rather than small scattered buttons.
- Mascot remains decorative and does not cover content.

### API Keys

- Compact action row.
- List/detail workspace with clear selected state, search/filter hierarchy and restrained badges.
- Creation/edit/secret/check windows use the same animated window shell.

### Models

- Current model summary remains compact.
- Catalog and tester share available height.
- Prompt, response and reasoning panes use bounded internal scrolling.

### History

- Compact metrics.
- Dense filters that wrap predictably.
- List/detail master-detail layout fills the desktop viewport.
- Reasoning and response never expand the full page.

### System

- Server/security/maintenance cards share consistent density.
- Proxy pool occupies the main detail pane.
- Operational dialogs use the new window shell.

## Verification workflow

1. Static checks: `git diff --check`, JavaScript syntax checks.
2. Release build and restart because web assets are embedded in the binary.
3. Fresh Playwright screenshots for login and all five views at desktop, tablet and mobile sizes.
4. Visual review of real screenshots using a local vision model and manual inspection notes.
5. Fix issues found in screenshots; repeat capture.
6. Run UI regression suite and Rust quality gates.
7. Append `REPO_WORKLOG.md` without secrets.
