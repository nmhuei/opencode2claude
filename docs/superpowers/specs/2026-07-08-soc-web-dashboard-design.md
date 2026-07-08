# SOC Web Dashboard — Design Specification

> IDE-Style Operations Console for OpenCode2API
> Date: 2026-07-08
> Status: Approved

## 1. Architecture Overview

A single-page application (SPA) embedded directly into the `opencode2api-serve` binary via `rust-embed`. The dashboard provides a VS Code/Cursor-inspired operations console for monitoring and managing the bridge server, proxy pool, and configuration.

```
Browser (SPA)                    Rust Axum Server
    │                                │
    ├── /dashboard/* ───────────►   rust-embed static assets (index.html, style.css, app.js)
    │                                │  SPA fallback: unknown routes → index.html
    ├── /api/dashboard/status ──►   AppState → BridgeConfig + ProxyPoolStats → JSON
    ├── /api/dashboard/proxies ─►   ProxyPool node snapshots → JSON
    ├── /api/dashboard/config ──►   BridgeConfig (sensitive values masked) → JSON
    ├── /api/dashboard/config/save ► Validate → atomic write TOML → JSON
    ├── /api/dashboard/events ──►   SSE stream (typed events via broadcast::Sender)
    ├── /api/dashboard/proxy/:port/restart → POST → docker::create_container → JSON
    │                                │
    └── ◄── SSE event stream ────   proxy_status, proxy_log, config_saved, heartbeat, error
```

### Module Structure

```
src/
├── server.rs              # Add dashboard route mounting /dashboard/* + /api/dashboard/*
├── dashboard.rs           # NEW: glue module — router builder, all handlers
└── webui/                 # NEW: Frontend static assets (embedded via RustEmbed)
    ├── index.html         # SPA shell — IDE layout structure
    ├── style.css          # VS Code/Cursor aesthetic tokens
    └── app.js             # SPA logic: fetch API, SSE client, tab manager, actions
```

### Binary

- Dashboard backend code lives in `opencode2api` library crate (`src/lib.rs` → `src/dashboard.rs`)
- Frontend assets embed in `opencode2api-serve` binary
- Reuses `AppState` for shared state access

## 2. Backend API Specification

All endpoints are prefix-routed under `/api/dashboard`.

### 2.1 Endpoints

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/api/dashboard/status` | GET | No | Bridge status, model, uptime, proxy tier summaries |
| `/api/dashboard/proxies` | GET | No | Detailed proxy node list with status/role/lifecycle |
| `/api/dashboard/config` | GET | No | Current TOML config (secrets masked) |
| `/api/dashboard/config/save` | POST | Yes* | Validate and atomic-write new config |
| `/api/dashboard/events` | GET | No | SSE stream of typed runtime events |
| `/api/dashboard/proxy/:port/restart` | POST | Yes* | Restart a single proxy container |

*POST endpoints require `X-Dashboard-Token` header if `DASHBOARD_ADMIN_TOKEN` is set.

### 2.2 Data Types

```rust
#[derive(Serialize, Clone)]
pub struct DashboardStatus {
    pub status: String,              // "running" | "stopped"
    pub pid: Option<u32>,
    pub version: String,
    pub uptime: Option<String>,
    pub model: Option<String>,
    pub bridge_port: u16,
    pub auth_enabled: bool,
    pub shell_policy: String,
    pub primary_proxies: ProxyTierStats,
    pub warm_standby: ProxyTierStats,
}

#[derive(Serialize, Clone)]
pub struct ProxyNodeView {
    pub port: u16,
    pub role: String,                // "primary" | "warm_standby"
    pub lifecycle: String,           // "managed" | "protected"
    pub status: String,              // "active" | "spare" | "cooldown" | "dead" | "starting"
    pub failure_count: u32,
    pub success_count: u32,
    pub cooldown_remaining_secs: Option<u64>,
}
```

### 2.3 SSE Event Types

```rust
#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    proxy_status {
        port: u16,
        status: String,
        timestamp: String,
    },
    proxy_log {
        port: u16,
        message: String,
        level: String,           // "info" | "warn" | "error"
        timestamp: String,
    },
    config_saved {
        timestamp: String,
    },
    proxy_restarted {
        port: u16,
        status: String,          // "ok" | "error"
        message: String,
        timestamp: String,
    },
    error {
        message: String,
        timestamp: String,
    },
    heartbeat {
        timestamp: String,
    },
}
```

### 2.4 State Model

```rust
#[derive(Clone)]
pub struct DashboardState {
    pub config: Arc<BridgeConfig>,
    pub proxy_pool: Arc<RwLock<ProxyPool>>,
    pub event_tx: broadcast::Sender<DashboardEvent>,
    pub supervisor_status: Arc<RwLock<SupervisorStatusCache>>,
    pub started_at: Arc<std::sync::atomic::AtomicU64>,
}

pub struct SupervisorStatusCache {
    pub status: SupervisorStatus,
    pub last_checked: Instant,
}
```

The `event_tx` broadcast channel:
- Backend modules publish events via `event_tx.send(event)`
- SSE handler subscribes via `event_tx.subscribe()`
- Channel capacity: 256 (ring buffer — slow consumers drop events)
- Heartbeat: server sends `heartbeat` event every 30s to keep SSE alive

### 2.5 Config Save — Atomic Write Pattern

```
1. Validate incoming TOML (syntax-check, reject unknown keys)
2. Serialize to bytes
3. Write to <config_path>.tmp
4. fsync temp file (stdlib: File::sync_data)
5. Rename .tmp → config_path (atomic on POSIX)
6. Emit config_saved event via event_tx
7. On failure → emit error event (do not clobber original)
```

### 2.6 Security

- All POST endpoints check `X-Dashboard-Token` header against `DASHBOARD_ADMIN_TOKEN` env var
- If `DASHBOARD_ADMIN_TOKEN` is not set, POST endpoints are read-only (HTTP 403)
- Config GET endpoint masks secrets (`"sk-...a1b2"` format)
- Rate-limit POST endpoints to prevent abuse (5 req/min per IP via middleware)
- Config validation rejects: non-TCP ports, invalid host addresses, shell_policy values not in enum

### 2.7 Startup Banner

When `run_server` starts, the startup banner should include:

```
║  Dashboard: http://127.0.0.1:4000/dashboard     ║
```

## 3. Frontend Specification

### 3.1 Layout

```
┌──────┬──────────────────────┬──────────────────────────────────────┐
│ Act  │ Sidebar (260px)      │ Editor Area                          │
│ Bar  │                      │                                      │
│ 48px │ ▼ OPENCODE2CLAUDE    │ Tab: dashboard.json ✕               │
│      │   dashboard.json     │ ┌──────────────────────────────────┐ │
│ [≡]  │   proxy_pool.json    │ │ {                               │ │
│ DASH │   config.toml        │ │   "status": "running",          │ │
│      │                      │ │   "model": "deepseek-v4",       │ │
│ [◉]  │ ▼ PROXY POOL         │ │   "uptime": "2h 15m"           │ │
│ DOC  │   ● Node 1 (Alive)   │ │ }                               │ │
│      │   ● Node 2 (Cooldown)│ └──────────────────────────────────┘ │
│ [>_] │   ◉ Node 3 (Dead)    │                                      │
│ TERM │                      │                                      │
│      ├──────────────────────┴──────────────────────────────────────┤
│      │ Terminal Panel (Live Log — auto-scroll)                     │
│      │ [2026-07-08 12:31:47] ● Proxy 40001 → Alive                │
│      │ [2026-07-08 12:31:48] ◉ Proxy 40003 → Cooldown (45s)       │
├──────┴─────────────────────────────────────────────────────────────┤
│ StatusBar: ● Port: 4000  |  Model: deepseek-v4  |  Proxies: 4/5   │
└────────────────────────────────────────────────────────────────────┘
```

### 3.2 Components

#### Activity Bar (left, 48px)
- Fixed, dark background (`#000000`)
- Icons (SVG inline): Dashboard, Proxy, Config, Terminal, Settings
- Active state: left border highlight
- Click switches the sidebar panel

#### Sidebar (secondary, 260px)
- Tree view with collapsible sections
- Sections: OPENCODE2CLAUDE (virtual files), PROXY POOL (live nodes)
- Each proxy node gets a colored dot: green=Active, amber=Cooldown, red=Dead, gray=Spare
- Clicking a "file" opens it as an editor tab
- Proxy nodes open a detail view in editor area

#### Editor Area (main content)
- Tab bar at top with active file tabs (closeable with ✕)
- Content area renders based on tab type:
  - `dashboard.json` → Overview cards: status, model, port, auth, proxy summary
  - `proxy_pool.json` → Proxy table with per-node health, role, lifecycle
  - `config.toml` → Read-only display with syntax-highlighted TOML
  - `[proxy detail]` → Single proxy details with restart button

#### Terminal Panel (bottom, resizable)
- Real-time log output via SSE
- Auto-scroll to bottom (lock when user scrolls up, badge `⬇ N new`)
- Colorized: red=ERROR, yellow=WARN, cyan=INFO
- Clear button

#### Status Bar (bottom, 22px)
- Left: bridge status indicator (green dot = running, red = stopped)
- Center: active model name
- Right: proxy pool health "Active: 3/5"

### 3.3 Color System (from UI/UX Pro Max review)

```css
/* Theme tokens — dark-only, OLED-friendly */
--bg-activity:      #000000;
--bg-sidebar:       #111112;
--bg-editor:        #1a1a1e;
--bg-terminal:      #0d0d0f;
--bg-surface:       #1e1e24;

--text-primary:     #e4e4e7;
--text-muted:       #8a8f98;
--text-dim:         #52525b;
--text-link:        #5E6AD2;

--accent-indigo:    #5E6AD2;   /* Primary interactive */
--accent-green:     #22C55E;   /* Alive / success */
--accent-amber:     #F59E0B;   /* Cooldown / warning */
--accent-red:       #EF4444;   /* Error / offline / dead */

--border:           rgba(255, 255, 255, 0.08);
--border-active:    #5E6AD2;

--font-mono:        'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace;
--font-ui:          -apple-system, 'Inter', 'SF Pro', sans-serif;
```

### 3.4 SPA Lifecycle

```
Page Load
  │
  ├──→ Render IDE layout shell (sidebar, editor frame, terminal)
  │
  ├──→ fetchStatus()     → GET /api/dashboard/status   → render overview tab
  ├──→ fetchProxies()    → GET /api/dashboard/proxies  → render proxy sidebar + table
  ├──→ fetchConfig()     → GET /api/dashboard/config   → render config tab
  │
  └──→ connectEvents()   → EventSource(/api/dashboard/events)
        │
        ├── proxy_status  → Update proxy dot colors + editor table
        ├── proxy_log     → Append to terminal (auto-scroll)
        ├── config_saved  → Refresh config display
        ├── error         → Show toast notification
        └── heartbeat     → No-op (keeps connection alive)
```

### 3.5 Error & Edge Cases

| Scenario | Frontend Behavior | Backend Behavior |
|----------|------------------|-----------------|
| API unreachable (server down) | Skeleton loading → error banner "Server not reachable" after 5s retry (3 attempts) | — |
| SSE disconnected | Auto-reconnect with exponential backoff (1s, 2s, 4s, max 30s), show "Reconnecting..." badge in terminal | — |
| Config save fails (invalid TOML) | Show inline error in editor with line number | Validate and return 422 with details |
| Proxy restart fails (Docker error) | Show error notification + keep proxy card visible | Return JSON `{status: "error", message: "..."}` |
| No config file yet | Show empty state: "No config file. Run `opencode2api init`" | Return 200 with defaults |
| Empty proxy pool | Show empty state: "No proxies configured" | Return empty array |
| Dashboard admin token missing (POST) | Hide config save button, hide restart buttons | Return 403 "Dashboard admin token not configured" |

### 3.6 Accessibility

- Focus rings on all interactive elements (2px, --accent-indigo)
- aria-label on icon-only buttons (activity bar, tab close, proxy restart)
- prefers-reduced-motion: disable animations, keep transitions instant
- Tab order: Activity Bar → Sidebar Tree → Editor → Terminal
- Skip to content link for keyboard users

## 4. Implementation Plan

### Phase 1: Core Backend (dashboard.rs)

1. Add `rust-embed` dependency to Cargo.toml
2. Create `dashboard.rs` module with:
   - `DashboardEvent` enum and `DashboardState` struct
   - `/api/dashboard/*` REST handlers (status, proxies, config, config/save, proxy/restart)
   - `/api/dashboard/events` SSE handler with heartbeat
   - Config validation and atomic save
   - Admin token check middleware
3. Mount routes in `server.rs`
4. Wire `AppState` to provide shared state access

### Phase 2: Frontend MVP (webui/)

1. Create `src/webui/index.html` — full IDE layout shell
2. Create `src/webui/style.css` — VS Code/Cursor theme tokens and component styles
3. Create `src/webui/app.js` — SPA lifecycle, fetch API, SSE client, tab management

### Phase 3: Integration & Polish

1. Add startup banner with dashboard URL
2. Test SSE reconnect and heartbeat
3. Test config save atomicity
4. Verify embedded asset serving
5. Review focus/keyboard navigation

## 5. Open Questions / Future Work

- Should we support light theme? (Not in MVP — dashboard is exclusively dark-mode as recommended by UX skill)
- Should config editor be editable? (MVP: read-only. Phase 2: editable with save via POST)
- Should we add websocket instead of SSE? (SSE is simpler, sufficient for unidirectional event stream)
- Log retention for terminal panel: keep last 500 lines in memory buffer, drop oldest on overflow
