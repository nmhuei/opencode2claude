# Dashboard and CLI redesign plan — 2026-07-21

## Objective

Rebuild the dashboard and CLI presentation around a restrained Cursor-like interface: neutral dark surfaces, compact spacing, clear hierarchy, low-saturation status colors, consistent typography, and no decorative gradients or visual noise.

The dashboard must expose the operational capability of the CLI through typed backend services rather than shelling out to the CLI. The CLI and dashboard must share the same domain/application operations so behavior cannot drift.

## Non-negotiable acceptance criteria

1. Existing Anthropic and OpenAI APIs remain backward-compatible.
2. Every CLI capability has a dashboard equivalent or an explicitly defined in-process equivalent:
   - server status, config, restart, stop and idempotent start-state reporting;
   - proxy status, restart, purge/dry-run, logs;
   - dashboard status;
   - environment/export values;
   - doctor diagnostics;
   - API-key generation and persistence;
   - config initialization, preview and apply;
   - update check/apply;
   - completion generation/download.
3. Free OpenCode model selection includes the current official free catalog and custom model input:
   - `opencode/big-pickle`;
   - `opencode/mimo-v2.5-free`;
   - `opencode/north-mini-code-free`;
   - `opencode/nemotron-3-ultra-free`;
   - `opencode/deepseek-v4-flash-free`.
4. Model selection persists atomically through configuration preview/apply and clearly reports restart requirements.
5. Dashboard actions are authenticated, CSRF-safe where browser cookies are involved, audited, bounded, and reject destructive actions while requests are leased.
6. CLI output aligns correctly at terminal widths 60, 80, 100, 120 and 160 columns, with and without ANSI color.
7. JSON output remains stable and contains no presentation text or ANSI escapes.
8. Every destructive dashboard/CLI action has preview/confirmation semantics.
9. Manual verification captures terminal stdout/stderr, exit codes, browser screenshots, network responses and action order.

## Architecture changes

### Shared application layer

Create typed application services used by both CLI and HTTP transports:

- `application::server_control`
- `application::proxy_control`
- `application::configuration`
- `application::diagnostics`
- `application::model_catalog`
- `application::api_keys`
- `application::updates`
- `application::completions`

No dashboard handler may execute an arbitrary shell command. Infrastructure adapters remain injectable.

### Management API v2 additions

Add typed endpoints for:

- `/api/v1/capabilities`
- `/api/v1/models/free`
- `/api/v1/models/select`
- `/api/v1/server/restart`
- `/api/v1/server/stop`
- `/api/v1/server/logs`
- `/api/v1/proxies/plan`
- `/api/v1/proxies/purge`
- `/api/v1/doctor`
- `/api/v1/env`
- `/api/v1/api-keys/generate`
- `/api/v1/config/init`
- `/api/v1/update/check`
- `/api/v1/update/apply`
- `/api/v1/completions/:shell`

Where self-stop makes the current HTTP request terminate the dashboard, return an accepted response first and schedule bounded shutdown after the response is flushed.

### Dashboard information architecture

- **Activity**: bridge state, current model, request metrics, recent events.
- **Models**: free-model selector, compatibility/retention warning, quick test.
- **Server**: status, restart/stop, logs, environment exports, update status.
- **Network**: proxy topology, health, leases, restart/purge planning.
- **Configuration**: structured editor plus raw TOML, preview diff, apply/rollback status.
- **Diagnostics**: doctor checks and downloadable evidence.
- **Access**: generate/revoke/replace client API keys.

### Visual system

- Neutral palette based on near-black, graphite and subtle borders.
- One accent color for focus/selection only.
- Status colors used only for status, never as decoration.
- 4/8px spacing scale; compact 32–36px controls.
- System sans-serif UI font and monospace for values/logs.
- No gradients, large glowing cards, oversized rounded corners or ornamental illustrations.
- Responsive sidebar that becomes a command palette/mobile drawer.
- Keyboard navigation and visible focus rings.

### CLI presentation system

Use existing Rust presentation libraries consistently:

- `clap` for command structure/help;
- `comfy-table` for width-aware tables;
- `unicode-width` for exact visible-width calculation;
- `yansi` for centralized semantic color tokens;
- `indicatif` only for interactive progress, never JSON/quiet modes.

Introduce reusable presentation components:

- semantic status line;
- section header;
- key/value list;
- width-aware table;
- warning/error panel;
- confirmation/plan renderer;
- terminal capability resolver.

## Implementation phases

### Phase 0 — Baseline capture

Capture current CLI output at multiple widths and current dashboard desktop/mobile screenshots. Inventory every route, action and DTO. Record current defects before replacement.

### Phase 1 — Backend parity foundation

Implement capability registry, free-model catalog, model selection, diagnostics, env, API-key, completion and lifecycle endpoints using shared services. Add tests before changing the frontend.

### Phase 2 — Dashboard rebuild

Replace `index.html`, `style.css` and `app.js` from scratch. Preserve only protocol contracts and authentication behavior. Implement all views and actions against typed endpoints.

### Phase 3 — CLI redesign

Refactor human output through shared components while keeping JSON schemas and exit codes unchanged. Capture and correct alignment at all target widths.

### Phase 4 — Automated verification

Run fmt, clippy, full tests, protocol conformance, parser fuzz, dashboard route tests, frontend static checks, accessibility checks and release build.

### Phase 5 — Manual verification and redesign loop

Run the scenario matrix below. Any failure produces:

1. defect evidence;
2. revised mini-plan;
3. implementation;
4. regression test;
5. rerun of the failed sequence and affected full suite.

The task is complete only when the entire matrix passes on the release binaries.

## State/order scenario matrix

### Server lifecycle

- status → start → status → start again → restart → status → stop → stop again.
- unmanaged process on target port → status → start refusal → safe recovery.
- stale PID file → status cleanup → start.
- restart after config model change.
- stop while an active streaming request exists.

### Configuration/model

- preview valid config → apply → restart → verify selected model.
- invalid config → preview rejection → confirm original file unchanged.
- model A → model B → restart → model A; verify no stale cache.
- apply while another config write occurs; verify atomicity/rollback.
- select each official free model and run a minimal completion probe.

### API keys/auth

- generate without save → confirm config unchanged.
- append key → restart → old and new keys both work.
- replace key → restart → old key rejected, new key accepted.
- dashboard cookie login → API-key replace → current browser session behavior documented.
- invalid/expired CSRF token after login refresh.

### Proxy lifecycle

- status → restart primary → status.
- restart same node twice.
- dry-run purge → real purge → status.
- restart protected standby rejection.
- restart node with active lease rejection → retry after lease release.
- proxy failure before/after model change.

### Dashboard/browser

- unauthenticated load, failed login, successful login, logout, session refresh.
- desktop 1440×900, laptop 1280×720, tablet 768×1024, mobile 390×844.
- keyboard-only navigation and focus order.
- network disconnect/reconnect and SSE reconnection.
- slow API response, duplicate click prevention, action timeout and safe retry.
- restart from dashboard followed by automatic reconnect.
- stop from dashboard followed by clear offline state.
- raw config edit followed by structured editor edit and vice versa.

### CLI output

For every command, capture stdout, stderr, exit code and ANSI-stripped output at widths 60/80/100/120/160:

- root help and every subcommand help;
- server start/stop/status/restart/logs/config;
- proxy ps/restart/purge/logs, including dry-run and rejection paths;
- dashboard start/status;
- env, doctor, api-key generate/save/replace;
- init success/existing-file failure/force;
- update check failure/success fixtures;
- completion for all supported shells;
- legacy aliases and deprecation messages;
- `--json`, `--quiet`, `--color always`, `--color never`, piped/non-TTY output.

## Evidence outputs

- `artifacts/redesign/baseline/`
- `artifacts/redesign/dashboard/`
- `artifacts/redesign/cli-captures/`
- `artifacts/redesign/scenario-matrix.json`
- `verification/DASHBOARD_CLI_REDESIGN_MANUAL_VERIFY_20260721.md`

## Completion gate

- All automated tests pass.
- All manual scenario entries pass or are explicitly environment-skipped with reproducible reason.
- Release binaries rebuilt and stable daemon restarted.
- Final dashboard screenshots and CLI captures reviewed for alignment.
- No known functional or presentation defect remains in the verified matrix.


## Phase 8 — API workspace, client config generator, icons, and live uptime

### Goals

1. Rename the `Access` workspace to `API` and make it the single place for bridge API-key lifecycle and client setup.
2. Add a secret-safe API-key inventory with fingerprints, append/replace generation, and explicit revocation.
3. Add downloadable client configuration presets based on official SDK conventions:
   - `.env` using `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `ANTHROPIC_API_KEY`, and `ANTHROPIC_BASE_URL`.
   - Claude Code `settings.json` using the documented `env` object and official schema URL.
   - OpenAI Python and Anthropic Python starter files using custom `base_url`.
   - cURL shell starter.
4. Default generated files to a placeholder key; require an explicit choice to export the active or newly generated secret.
5. Increase dashboard typography and contrast.
6. Add a self-contained inline SVG icon system for navigation, activity metrics, and major page headings. No CDN/runtime dependency.
7. Make uptime update once per second using a backend-synchronized monotonic client clock and resynchronize on status refresh or PID change.

### Acceptance criteria

- `#api` replaces `#access`; the legacy `#access` hash redirects to `#api`.
- API-key list never exposes complete saved keys; it shows only index, prefix/suffix fingerprint, length, and active state.
- Revoking a saved key requires CSRF-protected confirmation, writes atomically, preserves unrelated TOML and comments, and reports restart required.
- Config presets have deterministic file names and valid syntax.
- Placeholder export contains no configured secret.
- Active/latest key export occurs only after the user explicitly selects that source.
- Uptime visibly advances at least twice during a 3-second browser probe and resets after a PID change.
- Navigation and metric icons render from embedded SVG symbols with no network requests.
- Desktop and mobile layouts have no horizontal overflow after typography changes.
- Existing CLI, OpenAI, Anthropic, dashboard auth, and config tests remain green.


## Phase 9 — Web-search loop recovery

### Evidence and root-cause hypotheses

- Claude Code 2.1.216 subagents terminate with `search_loop_protection` after repeated internal WebSearch turns.
- Runtime logs show every intercepted call resolving to an empty query and returning a very short error payload before the loop repeats.
- The current streaming parser keeps one global search argument buffer rather than per-tool-call state; argument fragments received before the function name or from parallel tool calls can be dropped or mixed.
- Query extraction only recognizes a shallow JSON object containing `query`, `q`, or the first string value.
- Reaching the configured loop limit currently finalizes the Anthropic stream with an API error and no content block, which causes the Agent task itself to terminate.

### Implementation plan

1. Introduce a shared `SearchLoopState` and normalized search-call representation used by sync and stream paths.
2. Accumulate streamed tool-call `id`, `name`, and `arguments` independently by OpenAI `tool_call.index`, including fragments that arrive before the tool name.
3. Expand query extraction to nested objects/arrays and common keys such as `query`, `q`, `search_query`, `text`, `prompt`, and `url`.
4. If arguments still contain no usable query, derive a bounded fallback from the most recent user text instead of calling the provider with an empty string.
5. Cache normalized queries and avoid running the same search repeatedly.
6. On duplicate search or loop budget exhaustion, remove intercepted WebSearch/WebFetch tools and perform one final synthesis request using accumulated tool results. Never terminate with an empty-content transport error.
7. Preserve normal non-search tool calls and Anthropic SSE block ordering.

### Acceptance criteria

- Empty/malformed arguments never reach `SearchClient::search` as an empty string.
- Fragmented arguments before/after tool name resolve to one correct query.
- Parallel tool-call fragments cannot be mixed.
- Duplicate query does not execute the network search twice.
- Loop exhaustion returns a valid Anthropic text/thinking response and `message_stop`, not an `error` event.
- Sync and streaming paths share the same loop policy.
- Exact Claude Code subagent scenarios complete without `search_loop_protection`.
- Parallel subagents can search concurrently and return usable results.
