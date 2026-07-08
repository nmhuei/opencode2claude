# CLI Redesign — Design Spec

## OpenCode2API Command-Line Interface v2

## 1. Executive Summary

This spec describes a full UX/UI redesign of the `opencode2api` CLI. The current CLI (v0.3.2) uses basic clap derive output with no colors, no tables, no progress indicators, and no consistency in output formatting.

The redesigned CLI shifts from a flat subcommand list to a hierarchical command tree with consistent output formatting, semantic colors, structured tables, progress spinners, and machine-readable output for automation.

## 2. Goals

- Consistent visual language across all subcommands
- Provide rich human-readable output by default (colors, tables, spinners)
- Provide machine-readable JSON output via `--json` flag
- Support quiet mode for scripting via `--quiet` flag
- Follow CLI UX conventions from battle-tested Rust tools (ripgrep, bat, cargo)
- Add new subcommands: `doctor`, `completion`, `server config`, `server logs`
- Backward compatible aliases for existing commands

## 3. Command Tree

```
opencode2api
├── server                     # Server management group
│   ├── start [-f]             # Start bridge (default: daemon, -f: foreground)
│   ├── stop                   # Stop the bridge
│   ├── status                 # Show bridge status
│   ├── restart                # Restart the bridge
│   ├── logs                   # View bridge daemon logs
│   └── config                 # Show current configuration
├── proxy                      # Proxy pool management group
│   ├── ps                     # List proxy pool with health/role details
│   ├── restart                # Recreate primary proxy containers
│   ├── purge                  # Remove + recreate primary proxies
│   └── logs                   # View proxy container logs
├── env                        # Display environment information
├── doctor                     # Diagnose common issues (NEW)
└── completion <shell>         # Generate shell completions (NEW)
```

### 3.1. Backward Compatibility

| Old Command | New Command | Alias Kept? |
|-------------|-------------|-------------|
| `serve` | `server start -f` | Yes — `serve` remains as hidden alias |
| `start` | `server start` | Yes — `start` remains as hidden alias |
| `stop` | `server stop` | Yes — `stop` remains as hidden alias |
| `status` | `server status` | Yes — `status` remains as hidden alias |
| `restart` | `server restart` | Yes — `restart` remains as hidden alias |
| `logs` | `server logs` | Yes — `logs` remains as hidden alias |
| `env` | `env` | (unchanged) |
| `proxy status` | `proxy ps` | Yes — `proxy status` remains as alias |
| `proxy restart` | `proxy restart` | (unchanged) |
| `proxy purge` | `proxy purge` | (unchanged) |
| `proxy logs` | `proxy logs` | (unchanged) |

## 4. Crate Selection

| Crate | Version | Purpose | Dependencies |
|-------|---------|---------|-------------|
| `clap` | 4.x (existing) | Argument parsing, subcommands | Already in tree |
| `clap_complete` | 4.5 | Shell completion generation | **runtime dep** (not dev) |
| `yansi` | 1.0.1 | Terminal coloring | zero-dep |
| `comfy_table` | 7.x | ASCII tables for structured output | lightweight (~10 transitive) |
| `indicatif` | 0.17.x | Progress bars & spinners | medium (~25 deps) |

### 4.1. Explicit Rejections

| Crate | Reason |
|-------|--------|
| `inquire` / `dialoguer` | Interactive prompts break automation. Use `-y` / `--yes` flags instead. |
| `termimad` | No markdown rendering needed in this CLI. |
| `colored` | Heavier than yansi; yansi's `const Style` builders and zero-dep are better. |
| `owo-colors` | More flexible (gradients, 24-bit) but overkill for status icons and labels. |
| `tabled` | API is less ergonomic than comfy_table for our use case. |

## 5. Key Design Decisions

### 5.1. Output Abstraction: `OutputFormat` Enum

Every subcommand renders output through a shared `OutputFormat` dispatcher:

```rust
pub enum OutputFormat {
    Human,  // Default: comfy_table + yansi colors + spinners
    Json,   // --json flag: serde_json serialize
    Quiet,  // --quiet flag: minimal output, only errors/success
}
```

**Data/Display separation pattern:**

```rust
// 1. Data struct chung
#[derive(Serialize)]
struct ProxyListData {
    nodes: Vec<ProxyNodeInfo>,
}

// 2. Display trait cho Human rendering
impl Display for ProxyListData {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        // dùng comfy_table + yansi
    }
}

// 3. Render dispatch
fn render<T: Serialize + Display>(data: T, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Human => Ok(data.to_string()),
        OutputFormat::Json => serde_json::to_string_pretty(&data)
            .map_err(|e| anyhow!("JSON serialization failed: {e}")),
        OutputFormat::Quiet => {
            // compact one-line summary
            Ok(format!("{}", data)) // custom Display for quiet mode
        }
    }
}
```

- Data struct là chung
- Renderer dispatch theo format
- `OutputFormat::Json` trả lỗi qua `Result` thay vì `.unwrap()`
- Dễ test riêng từng renderer

### 5.2. Global Flags

```rust
#[arg(long, conflicts_with = "quiet")]
json: bool,

#[arg(long, conflicts_with = "json")]
quiet: bool,

#[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
color: ColorChoice,
```

- `--json` và `--quiet` declared conflicts_with — clap tự báo lỗi nếu dùng cả hai
- `--color auto/always/never` — control ANSI output

### 5.3. Color Protocol

**Setup in `main()`, before any handler runs:**

```rust
fn setup_color(choice: &ColorChoice) {
    match choice {
        ColorChoice::Never => yansi::disable(),
        ColorChoice::Always => yansi::enable(),
        ColorChoice::Auto => {
            if std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal() {
                yansi::disable();
            }
        }
    }
}
```

- `--color never` → `yansi::disable()` — pipe-to-file không dính ANSI rác
- `--color always` → force enable (kể cả pipe)
- Mặc định Auto — phát hiện NO_COLOR / is_terminal

### 5.4. `clap_complete` is a Runtime Dependency

`completion <shell>` subcommand calls `clap_complete::generate()` at runtime in `main.rs`, so `clap_complete` must be in `[dependencies]`, NOT `[dev-dependencies]`.

```toml
[dependencies]
clap_complete = "4.5"
```

### 5.5. Error Propagation

All JSON render paths return `Result<String, Error>` — no `.unwrap()` on serialization.

## 6. Output Templates

### 6.1. `server status`

```
 Bridge:  ● Online           Model: deepseek-v4-flash
  PID:   137693              Auth: enabled
  Port:  4000                Uptime: 4h 12m
 Host:   127.0.0.1          Version: 0.3.2
```

### 6.2. `proxy ps`

```
 Proxy Pool Status
┌────────┬──────────┬──────────┬─────────┬───────────┬──────────────┐
│ Node   │ Role     │ Status   │ Latency │ Fail Count │ Last Check   │
├────────┼──────────┼──────────┼─────────┼───────────┼──────────────┤
│ WARP 1 │ Primary  │ ● Healthy│   8 ms  │     0     │ 2s ago       │
│ WARP 3 │ Primary  │ ⚠ Warm   │  42 ms  │     2     │ 2s ago       │
│ WARP 4 │ Standby  │ 🔒 Prot. │  18 ms  │     0     │ 5s ago       │
└────────┴──────────┴──────────┴─────────┴───────────┴──────────────┘
```

### 6.3. `env`

```
 Environment Configuration

 BRIDGE_PORT           4000
 BRIDGE_HOST           127.0.0.1
 BRIDGE_AUTH_TOKEN     enabled (2 tokens)
 BRIDGE_SHELL_POLICY   disabled
 OPENCODE_MODEL        auto (deepseek-v4-flash)
 
 Proxy Pool
   Primary             socks5://127.0.0.1:40001, 40002, 40003
   Warm Standby        socks5://127.0.0.1:40004, 40005
```

### 6.4. `doctor`

```
 Running diagnostics...

 ✓ docker-daemon      Docker is running (v27.0)
 ✓ port-4000          Port 4000 is free
 ⚠ proxy-containers   2/3 primary containers running (warp-3 missing)
 ✓ config-file        opencode2api.toml parsed OK
 ✓ auth-status        Auth enabled (2 tokens configured)

 Result: 1 warning — bridge should still operate
```

`doctor --json` → `DoctorReport` struct serialize, exit code 1 if failures > 0.

### 6.5. `server logs`

```
 12:34:56  INFO  bridge.request.accepted          POST /v1/messages 200
 12:34:50  WARN  proxy.node.cooldown              WARP-3: 429 rate limit
 12:34:45  INFO  health.check.passed              All proxies healthy
```

## 7. Color Semantics

| Semantic | yansi Style | Usage |
|----------|-------------|-------|
| Success | `green().bold()` | Healthy status, completed operations |
| Warning | `yellow().bold()` | Degraded, cooldown, non-critical |
| Error | `red().bold()` | Failed, offline, unreachable |
| Info | `cyan().bold()` | Neutral info, config values |
| Muted | `white().dim()` | Timestamps, secondary text |
| Highlight | `blue().bold()` | IDs, names, actionable items |
| Critical | `on_red().white().bold()` | Fatal errors, bridge down |
| Help header | `clap::Styles::styled().header(Green)` | clap built-in help styling |

**Prefix convention:** `✓` for success, `✗` for failure, `⚠` for warning.

## 8. Doctor Module Checklist

| # | Check | Pass | Warning | Fail |
|---|-------|------|---------|------|
| 1 | Docker daemon reachable | `docker info` succeeds | — | daemon not running |
| 2 | Port bridge available | port not in use | — | port occupied |
| 3 | Proxy containers | all 3 primary running | 1+ missing | 0 running |
| 4 | Config file | file exists + parses | file missing → defaults used | parse error |
| 5 | Auth status | tokens configured | — | no tokens (warning only) |
| 6 | DNS/egress (optional) | opencode.ai reachable | slow response | unreachable |

`DoctorReport` struct implements `Serialize` + `Display` — consistent with `OutputFormat` dispatch.

## 9. Progress & Spinner Patterns

| Subcommand | Pattern | indicatif API |
|-----------|---------|---------------|
| `server start` | Spinner → `✓ Bridge started (PID: N)` | `ProgressBar::new_spinner()` |
| `proxy restart` | MultiProgress: 1 bar per WARP node | `MultiProgress::new()` |
| `proxy purge` | Progress bar + `-y` skip confirm | `ProgressBar::new(N)` + `inc()` |

**Style defaults:**
- Spinner: `{spinner} {wide_msg}` — template for spinner
- Progress: `[{bar:30.cyan/blue}] {pos}/{len} {msg}` — template for multi-node ops

**Rule:** Only show spinner/progress when expected duration > 500ms. Fast ops → sync output.

## 10. Error Display Patterns

| Error Type | Template | Example |
|-----------|----------|---------|
| Operation failed | `✗ {component}: {reason}` | `✗ bridge: port 4000 already in use` |
| Config error | `✗ config: {field} — {detail}` | `✗ config: auth_tokens — invalid format` |
| Docker error | `✗ docker: {context}: {err}` | `✗ docker: proxy restart — daemon not running` |
| Network error | `✗ network: {upstream} unreachable ({timeout}s)` | `✗ network: opencode.ai unreachable (30s)` |

**Suggestion rule:** Every error message includes a recovery hint when detectable.

## 11. Quiet Output Convention

| Scenario | Output |
|----------|--------|
| Success | (no output) — exit 0 |
| Warning | `<message>` to stderr — exit 0 |
| Error | `<error>` to stderr — exit 1 |
| Data needed | `<value>` to stdout — exit 0 |

## 12. Implementation Plan

### Phase 1: Core Infrastructure

Files: `src/output.rs` (new), `src/cli.rs` (modify)

- Define `OutputFormat` enum
- Define `ColorChoice` enum + `setup_color()` in main.rs
- Define `render()` dispatcher
- Add clap `--json`, `--quiet`, `--color` global flags
- Update `Cargo.toml` with new deps

### Phase 2: Command Tree Restructure

Files: `src/cli.rs` (modify), `src/main.rs` (modify)

- Add `server` subcommand group
- Add `completion` subcommand
- Add `doctor` subcommand
- Keep backward-compatible aliases
- Add `clap_complete` generation logic

### Phase 3: Output Templates

Files: `src/output.rs` (extend), `src/doctor.rs` (new)

- Implement `Display` + `Serialize` for each subcommand output struct
- `proxy ps` → comfy_table
- `env` → key-value formatting
- `server status` → key-value formatting
- `server logs` → colored line-by-line

### Phase 4: Doctor Module

Files: `src/doctor.rs`

- Implement all 6 diagnostic checks
- Render `DoctorReport` in all 3 output formats
- Wire into clap dispatch

### Phase 5: Polish

- `indicatif` spinner for server start / proxy restart
- `indicatif` MultiProgress for proxy purge
- Error message enrichment with suggestions
- Full test coverage for output format dispatch
- Verify `doctor --json` exit codes

## 13. File Changes Summary

```
src/
├── cli.rs            # Modified: new command tree, global flags
├── main.rs           # Modified: setup_color(), clap_complete generation
├── output.rs         # NEW: OutputFormat, render(), color setup
├── doctor.rs         # NEW: DoctorReport, 6 diagnostic checks
└── ... (rest unchanged)
```

## 14. Acceptance Criteria

1. `opencode2api server status` shows colored output with bridge state
2. `opencode2api server status --json` returns valid JSON
3. `opencode2api proxy ps` shows a formatted table
4. `opencode2api doctor` runs 6 checks and shows pass/warn/fail
5. `opencode2api doctor --json` returns structured JSON with exit code
6. `opencode2api completion bash` generates valid bash completions
7. `opencode2api server logs` shows colored log lines
8. `--color never` disables ANSI (pipe-safe)
9. `--json` and `--quiet` conflict detected by clap (not runtime)
10. Existing `serve`, `start`, `stop`, `status`, `restart`, `logs` commands still work
11. `cargo test` passes with new modules
12. No `.unwrap()` in output render path
