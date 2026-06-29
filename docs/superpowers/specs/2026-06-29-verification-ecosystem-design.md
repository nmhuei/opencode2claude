# Verification Ecosystem for opencode2claude CLI Supervisor

> Date: 2026-06-29
> Status: Approved Design
> Phase: Pre-implementation

## Overview

This document defines the verification ecosystem for implementing the `opencode2claude` CLI supervisor upgrade (8 phases). The ecosystem combines **deterministic verification scripts**, **agent-based review**, and **CI integration** to ensure each phase is implemented completely, correctly, and without scope bleeding.

## Architecture

```
┌──────────────────────────────────────────────────┐
│                  verify.sh                        │
│         Entrypoint: phase selector                │
├──────────────────────────────────────────────────┤
│                                                    │
│  scripts/lib/          scripts/phases/             │
│  ┌──────────┐         ┌──────────────────┐        │
│  │common.sh │         │phase-1-cli-      │        │
│  │cargo.sh  │         │skeleton.sh       │        │
│  │process.sh│         │phase-2-runtime-  │        │
│  │report.sh │         │pid.sh            │        │
│  └──────────┘         │...               │        │
│                       └──────────────────┘        │
│                                                    │
│  verification/phases/   .runtime/verify/           │
│  ┌──────────────────┐  ┌──────────────────┐       │
│  │phase-1-*.md      │  │phase-1-*.log     │       │
│  │phase-2-*.md      │  │phase-2-*.log     │       │
│  │... (committed)   │  │... (gitignored)  │       │
│  └──────────────────┘  └──────────────────┘       │
└──────────────────────────────────────────────────┘
```

### Key Principles

1. **Gate = deterministic assertion** — Each gate is a bash function returning 0/1. No false passes.
2. **Phase = acceptance contract** — Scope, files, acceptance, OOS defined in `verification/phases/*.md`.
3. **Script = source of truth** — Not agent review; not human memory.
4. **CI calls verify.sh** — No duplicate checks. Single entrypoint.
5. **Agent review = advisory** — CRITICAL/HIGH must be fixed before commit, but agents do not replace gates.
6. **Fail fast + full re-verify** — Debug from failed gate; commit only after full phase pass.

---

## Section 1: Verification Gates

### Gate Mechanism

```bash
# Template
gate_name() {
  info "Gate N.M: description"
  # assertion logic
  # return 0 on pass, 1 on fail
  pass "Gate description"
}
```

Each phase script declares `GATES=()` array and calls `run_gates` at the bottom.

### Profile System

| Profile | cargo fmt | clippy | cargo check | cargo test | cargo build | CLI --help | Bridge serve | Integration | Docker |
|---------|-----------|--------|-------------|------------|-------------|------------|--------------|-------------|--------|
| `ci`    | ✅        | ✅     | ✅          | ✅         | ✅          | ✅         | ❌           | ❌          | ❌     |
| `local` | ✅        | ✅     | ✅          | ✅         | ✅          | ✅         | ✅           | ✅          | ❌     |
| `heavy` | ✅        | ✅     | ✅          | ✅         | ✅          | ✅         | ✅           | ✅          | ✅     |

### Gate Order (all phases)

```bash
GATES=(
  gate_format_check              # cargo fmt --check
  gate_clippy_clean              # cargo clippy -- -D warnings
  gate_compile_check             # cargo check --locked --all-targets
  gate_unit_tests                # cargo test --locked
  gate_binary_build              # cargo build --locked
  gate_cli_help                  # opencode2claude --help output
  gate_cli_smoke                 # bridge serve + /health (local/heavy only)
  gate_bridge_integration        # cargo test --test integration -- --ignored (local/heavy only)
)
```

Each phase adds phase-specific gates after the common ones.

---

## Section 2: Agent Implementation Flow

### Phase Lifecycle

```
┌────────┐   ┌────────┐   ┌────────┐   ┌──────────┐   ┌──────────┐   ┌────────┐
│ DESIGN │ → │  CODE  │ → │ VERIFY │ → │  REVIEW  │ → │  FINAL   │ → │ COMMIT │
│        │   │        │   │ script │   │  agent   │   │ VERIFY   │   │        │
└────────┘   └────────┘   └────────┘   └──────────┘   └──────────┘   └────────┘
                  ↑              │                          │
                  │              │ (fail)                   │ (CRITICAL/HIGH)
                  └──────────────┘                          │
                                                             │
                                                             v
                                                       (fix → re-verify)
```

### Rules

1. **DESIGN** — Read phase metadata (`verification/phases/*.md`). Understand scope, files, gates.
2. **CODE** — Implement only phase scope. Do NOT bleed into later phases.
3. **VERIFY** — Run `./scripts/verify.sh phase-N --profile local`. Fail → fix → optionally re-run from failed gate. **Before commit, always run FULL phase.**
4. **REVIEW** — Run advisory agent review:
   - `code-reviewer` (all phases)
   - `security-reviewer` (Phase 3+)
   - `architecture-consistency-reviewer` (Phase 1, 7)
5. **FINAL VERIFY** — Full phase verification from gate 1.
6. **COMMIT** — Commit only if:
   - Full phase verification passes
   - No CRITICAL/HIGH agent findings remain
   - Git diff matches phase scope

---

## Section 3: Directory Structure

```
opencode2claude/
├── src/
│   ├── main.rs              # (modified) match subcommand
│   ├── cli.rs               # (new) clap subcommands
│   ├── supervisor.rs         # (new) start/status/stop/restart/env/logs
│   ├── pidfile.rs            # (new) .runtime pid json
│   ├── runtime.rs            # (new) .runtime paths
│   ├── health.rs             # (new) port check, /health, wait ready
│   ├── docker.rs             # (new) docker WARP lifecycle
│   ├── config.rs
│   ├── handlers.rs
│   ├── middleware.rs
│   ├── proxy_pool.rs
│   ├── shell.rs
│   ├── sse.rs
│   └── opencode/
│
├── scripts/
│   ├── verify.sh             # Entrypoint: verify.sh phase-1 --profile local
│   ├── lib/
│   │   ├── common.sh         # info/error/pass/fail, trap cleanup
│   │   ├── cargo.sh          # cargo check/build/test helpers
│   │   ├── process.sh        # pick_free_port, wait_for_http, register_cleanup
│   │   └── report.sh         # gate pass/fail summary
│   └── phases/
│       ├── phase-1-cli-skeleton.sh
│       ├── phase-2-runtime-pid.sh
│       ├── phase-3-security.sh
│       ├── phase-4-proxy-cli.sh
│       ├── phase-5-proxy-pool-fix.sh
│       ├── phase-6-health-status-log.sh
│       ├── phase-7-docs-migration.sh
│       └── phase-8-ci-release.sh
│
├── verification/
│   ├── README.md             # How to use verification system
│   ├── phases/
│   │   ├── phase-1-cli-skeleton.md
│   │   ├── phase-2-runtime-pid.md
│   │   └── ... (8 phase contracts)
│   └── examples/
│       └── phase-1-sample-report.md
│
├── .runtime/                 # (gitignored) generated runtime + verify logs
│   ├── verify/
│   └── opencode2claude.pid.json
│
├── .github/workflows/
│   └── ci.yml                # (modified) uses verify.sh
│
├── .gitignore                # add .runtime/
├── start.sh                  # wrapper: exec opencode2claude start
├── stop.sh                   # wrapper: exec opencode2claude stop
└── docs/
    ├── cli.md
    ├── proxy-pool.md
    └── migration-from-start-sh.md
```

---

## Section 4: verify.sh Entrypoint

### Command-Line Interface

```bash
./scripts/verify.sh [phase] [options]
```

**Arguments:**
| Position | Description | Default |
|----------|-------------|---------|
| `phase`  | `all`, `phase-1`...`phase-8` | `all` |

**Options:**
| Flag | Effect |
|------|--------|
| `--profile ci|local|heavy` | Override `$PROFILE` (default: `local`) |
| `--from GATE` | Start execution from named gate (debug) |
| `--only GATE` | Run only the named gate (debug) |
| `--list-gates` | List available gates for phase |

**Examples:**
```bash
./scripts/verify.sh                     # all phases, local profile
./scripts/verify.sh all --profile ci    # all phases, CI-safe
./scripts/verify.sh phase-1             # single phase
./scripts/verify.sh phase-1 --from gate_cli_smoke
./scripts/verify.sh phase-1 --only gate_cli_smoke
```

### Phase Enable/Disable Mechanism

Each phase script declares `PHASE_ENABLED="${PHASE_ENABLED:-1}"`. During development, phases not yet implementing can be disabled:

```bash
PHASE_ENABLED=0     # At top of phase script when not yet ready
```

When `verify.sh all` runs, disabled phases exit 0 immediately (skip). This allows the skeleton for all 8 phases to exist in `scripts/phases/` from Day 1 without breaking CI. In practice, during active development only the current phase is enabled and CI targets individual phases:

```bash
# During Phase 1 development — CI runs just phase-1
./scripts/verify.sh phase-1 --profile ci

# After all 8 phases complete — switch to all
./scripts/verify.sh all --profile ci
```

### Phase Script Template

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export ROOT_DIR
export RUNTIME_DIR="${RUNTIME_DIR:-$ROOT_DIR/.runtime}"
export VERIFY_LOG_DIR="${VERIFY_LOG_DIR:-$RUNTIME_DIR/verify}"
export PROFILE="${PROFILE:-local}"

mkdir -p "$VERIFY_LOG_DIR"

source "$ROOT_DIR/scripts/lib/common.sh"
source "$ROOT_DIR/scripts/lib/cargo.sh"
source "$ROOT_DIR/scripts/lib/process.sh"
source "$ROOT_DIR/scripts/lib/report.sh"

PHASE_ID="phase-1"
PHASE_NAME="CLI skeleton"
PHASE_ENABLED="${PHASE_ENABLED:-1}"

[[ "$PHASE_ENABLED" == "0" ]] && {
  info "Phase $PHASE_ID ($PHASE_NAME) is disabled — skipping"
  exit 0
}

GATES=(
  gate_format_check
  gate_clippy_clean
  gate_compile_check
  gate_unit_tests
  gate_binary_build
  gate_cli_help
  gate_cli_smoke
  gate_bridge_integration
)

gate_cli_help() {
  info "Gate 1.5: CLI help"
  local bin="$ROOT_DIR/target/debug/opencode2claude"
  # Test each subcommand has valid --help (more precise than grep)
  "$bin" serve --help  >/dev/null || return 1
  "$bin" start --help  >/dev/null || return 1
  "$bin" status --help >/dev/null || return 1
  "$bin" stop --help   >/dev/null || return 1
  "$bin" restart --help >/dev/null || return 1
  "$bin" env --help    >/dev/null || return 1
  pass "CLI help lists all expected subcommands"
}

gate_cli_smoke() {
  require_profile local heavy || return 0
  info "Gate 1.6: CLI smoke"
  local bin="$ROOT_DIR/target/debug/opencode2claude"
  local port; port="$(pick_free_port)"
  local pid; local log_file="$VERIFY_LOG_DIR/phase-1-cli-smoke.log"
  "$bin" serve --port "$port" >"$log_file" 2>&1 & pid=$!
  register_cleanup "kill $pid 2>/dev/null || true"

  # Wait for the process to be alive before health polling
  sleep 0.5
  if ! kill -0 "$pid" 2>/dev/null; then
    error "Bridge process exited early before health check"
    tail -n 100 "$log_file" || true
    return 1
  fi

  wait_for_http "http://127.0.0.1:$port/health" 8 || {
    error "Bridge not healthy"
    kill "$pid" 2>/dev/null || true
    tail -n 100 "$log_file" || true
    return 1
  }
  kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true
  pass "CLI smoke"
}

run_gates
```

### verify.sh Implementation

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
export ROOT_DIR
export PROFILE="${PROFILE:-local}"
export RUNTIME_DIR="${RUNTIME_DIR:-$ROOT_DIR/.runtime}"
export VERIFY_LOG_DIR="${VERIFY_LOG_DIR:-$RUNTIME_DIR/verify}"
mkdir -p "$VERIFY_LOG_DIR"

source "$ROOT_DIR/scripts/lib/common.sh"
source "$ROOT_DIR/scripts/lib/report.sh"

# ── Phase registry ──
PHASE_ORDER=(
  phase-1 phase-2 phase-3 phase-4
  phase-5 phase-6 phase-7 phase-8
)

phase_script_for() {
  case "$1" in
    phase-1) echo "$ROOT_DIR/scripts/phases/phase-1-cli-skeleton.sh"   ;;
    phase-2) echo "$ROOT_DIR/scripts/phases/phase-2-runtime-pid.sh"    ;;
    phase-3) echo "$ROOT_DIR/scripts/phases/phase-3-security.sh"       ;;
    phase-4) echo "$ROOT_DIR/scripts/phases/phase-4-proxy-cli.sh"      ;;
    phase-5) echo "$ROOT_DIR/scripts/phases/phase-5-proxy-pool-fix.sh" ;;
    phase-6) echo "$ROOT_DIR/scripts/phases/phase-6-health-status-log.sh";;
    phase-7) echo "$ROOT_DIR/scripts/phases/phase-7-docs-migration.sh" ;;
    phase-8) echo "$ROOT_DIR/scripts/phases/phase-8-ci-release.sh"     ;;
    *) return 1 ;;
  esac
}

# ── Arg parser ──
PHASE="${1:-all}"
shift || true
FROM_GATE=""; ONLY_GATE=""; LIST_GATES=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --from) FROM_GATE="$2"; shift 2 ;;
    --only) ONLY_GATE="$2"; shift 2 ;;
    --list-gates) LIST_GATES=1; shift ;;
    *) echo "Unknown: $1"; exit 2 ;;
  esac
done
export PROFILE FROM_GATE ONLY_GATE LIST_GATES PHASE_ORDER

# ── Phase dispatch ──
if [[ "$PHASE" == "all" ]]; then
  for p in "${PHASE_ORDER[@]}"; do
    script="$(phase_script_for "$p")" || {
      error "Unknown phase: $p"; exit 1
    }
    info "--- Running $p: $(basename "$script") ---"
    PROFILE="$PROFILE" FROM_GATE="$FROM_GATE" ONLY_GATE="$ONLY_GATE" LIST_GATES="$LIST_GATES" \
      "$script" || { error "Phase failed: $p"; exit 1; }
  done
  summary_pass "All phases passed"
elif script=$(phase_script_for "$PHASE"); then
  exec "$script"
else
  echo "Usage: $0 {all|phase-N} [--profile ci|local|heavy] [--from GATE] [--only GATE] [--list-gates]"
  exit 2
fi
```

---

## Section 5: Phase Metadata Contract

Each phase has a metadata file at `verification/phases/phase-N-name.md` with these mandatory sections:

| Field | Purpose |
|-------|---------|
| `Phase ID` | Unique identifier (e.g., `phase-1-cli-skeleton`) |
| `Status` | Planned / In Progress / Implemented / Verified / Committed |
| `Dependencies` | Phases that must be complete before this one |
| `Scope` | What this phase achieves |
| `Files to create` | New source files |
| `Files to modify` | Existing files changed |
| `Expected behavior contract` | Precise CLI/API behavior after implementation |
| `Acceptance gates` | List of gates with expected outcomes |
| `Verification command` | Exact shell command to run |
| `Review requirements` | Required agent reviewers and blocking severity levels |
| `Out of scope` | Explicitly excluded work (prevents phase bleeding) |
| `Definition of Done` | 4-5 conditions that all must be met |

---

## Section 6: CI Integration

### GitHub Actions (`.github/workflows/ci.yml`)

```yaml
name: CI
on: [push, pull_request]

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rust-lang/setup-rust-toolchain@v1

      - name: Verification (CI profile)
        run: ./scripts/verify.sh all --profile ci

      - name: Release build
        run: cargo build --release --locked
```

No duplicate checks. `verify.sh` is the single source of truth. Release build is separate for readability.

### CI Gate: shellcheck

```yaml
      - name: Shellcheck
        run: shellcheck scripts/verify.sh scripts/lib/*.sh
        continue-on-error: true   # Phase 1-3: optional. After Phase 3: remove this line to make it a hard gate.
```

**Migration:** During Phase 1–3, shellcheck runs with `continue-on-error: true` to avoid blocking early iteration. After Phase 3 (or once verification scripts stabilize), remove `continue-on-error` so shellcheck becomes a hard gate. Alternatively, add a `gate_shellcheck` to the common gate list that skips when `shellcheck` isn't installed and fails when it is — keeping the same behavior across CI and local.

---

## Section 7: Agent Review Workflow

After deterministic verification passes, advisory agent review runs:

```yaml
Required reviewers per phase:
  Phase 1 (CLI):            code-reviewer, architecture-consistency
  Phase 2 (Runtime/PID):    code-reviewer
  Phase 3 (Security):       code-reviewer, security-reviewer
  Phase 4 (Proxy CLI):      code-reviewer
  Phase 5 (Proxy fix):      code-reviewer, architecture-consistency
  Phase 6 (Health):         code-reviewer
  Phase 7 (Docs):           architecture-consistency
  Phase 8 (CI/Release):     code-reviewer
```

### Severity-Based Findings

| Severity | Action |
|----------|--------|
| CRITICAL | Must fix before commit |
| HIGH | Must fix before commit |
| MEDIUM | Should fix or create issue |
| LOW | Optional, can backlog |
| INFO | Note only |

### Blocking Rule

> **Commit blocked if:** Full verification fails OR any CRITICAL/HIGH agent finding remains.

---

## Section 8: Phase Implementation Order

```text
Phase 1: CLI Skeleton
  └── Phase 2: Runtime + PID
       └── Phase 3: Security Hardening
            ├── Phase 4: Proxy CLI Manager
            │    └── Phase 5: Proxy Pool Fix
            └── Phase 6: Health/Status/Log Polish
                 └── Phase 7: Docs + Migration
                      └── Phase 8: CI + Release
```

Phase 4 and Phase 6 can be parallelized if desired (no strict dependency between them).

---

## Section 9: lib.sh Module Specifications

### `common.sh`
```bash
info()    # [INFO]  timestamp message
pass()    # [PASS]  ✓ message
error()   # [ERROR] ✗ message
warn()    # [WARN]  ⚠ message
skip()    # [SKIP]  message (profile skip)
phase()   # [PHASE] phase name header
register_cleanup()  # trap-based cleanup stack
require_profile()   # gate only runs if profile matches
```

### `cargo.sh`
```bash
gate_format_check()        # cargo fmt --check
gate_clippy_clean()        # cargo clippy -- -D warnings
gate_compile_check()       # cargo check --locked --all-targets
gate_unit_tests()          # cargo test --locked
gate_binary_build()        # cargo build --locked
```

### `process.sh`
```bash
pick_free_port()           # find available TCP port
wait_for_http()            # poll URL until 200 or timeout
wait_for_pid_exit()        # wait until PID exits or timeout
pid_alive()                # check if PID is running (kill -0)
```
Note: `register_cleanup` is defined in `common.sh` — do not duplicate in `process.sh`.

### `report.sh`
```bash
summary_pass()             # print summary of all gates
summary_fail()             # print failure summary
```

---

## Section 10: .gitignore

```
.runtime/
```

---

## Future Considerations

1. **Benchmark gates** — Could add `--profile bench` with criterion benchmarks for hot paths
2. **JSON output** — `verify.sh --json` for machine-parseable results
3. **Phase rollback** — `./scripts/verify.sh phase-N --rollback` to revert phase files
4. **Docker-in-CI** — Future `--profile heavy` integration with Docker containers in GitHub Actions
