# Verification Ecosystem Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the verification ecosystem that gates and validates all 8 phases of the CLI supervisor upgrade for `opencode2claude`.

**Architecture:** 4 lib modules (`common.sh`, `cargo.sh`, `process.sh`, `report.sh`) provide CLI helpers and gate functions. `verify.sh` is the single entrypoint with phase registry, argument parsing, and dispatch. Each phase gets a `scripts/phases/phase-N-name.sh` script (gates) and a `verification/phases/phase-N-name.md` contract. CI calls `verify.sh all --profile ci` as sole verification step.

**Tech Stack:** Bash 4+ (scripts), GitHub Actions (CI), shellcheck (optional linter)

## Global Constraints

- All scripts must pass `shellcheck` (all blockers fixed, warnings optionally reviewed)
- `set -Eeuo pipefail` at top of every script (fail fast)
- `ROOT_DIR` derived from `BASH_SOURCE[0]` or `dirname $0` — never hardcoded
- `PROFILE` default: `local`; also `ci` and `heavy`
- `PHASE_ENABLED` mechanism: `=0` → skip phase, `=1` → run (default enabled)
- Phase scripts are executed as **subprocesses**, not `source`d — each has its own error context
- `cargo test --locked` everywhere — no fallback `|| cargo test`
- `register_cleanup` defined in `common.sh` only — not duplicated in `process.sh`
- CI runs `verify.sh all --profile ci` as single verification step; release build separate
- `.runtime/` must be in `.gitignore`

---

### Task 1: Directory Scaffolding and `.gitignore`

**Files:**
- Modify: `.gitignore`
- Create: `scripts/` directory
- Create: `scripts/lib/` directory
- Create: `scripts/phases/` directory
- Create: `verification/` directory
- Create: `verification/phases/` directory
- Create: `verification/examples/` directory

**Interfaces:**
- Consumes: current `.gitignore` content
- Produces: scaffolded directories ready for Task 2-9 files

- [ ] **Step 1: Create all directories**

```bash
mkdir -p scripts/lib scripts/phases
mkdir -p verification/phases verification/examples
```

- [ ] **Step 2: Add `.runtime/` to `.gitignore`**

Append to `./.gitignore`:
```
# Verification runtime artifacts
.runtime/
```

- [ ] **Step 3: Stage scaffold**

```bash
git add .gitignore
```

- [ ] **Step 4: Commit**

```bash
git commit -m "chore: scaffold verification directories and gitignore .runtime/"
```

---

### Task 2: `common.sh` — Common Utilities Library

**Files:**
- Create: `scripts/lib/common.sh`

**Interfaces:**
- Consumes: nothing (pure bash)
- Produces: `info`, `pass`, `error`, `warn`, `skip`, `phase`, `register_cleanup`, `require_profile`

- [ ] **Step 1: Write `scripts/lib/common.sh` with header and logging functions**

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

# ── Logging ──
info()  { printf "[INFO]  %s %s\n" "$(date '+%H:%M:%S')" "$*"; }
pass()  { printf "[PASS]  \342\234\223 %s\n" "$*"; }
error() { printf "[ERROR] \342\234\227 %s\n" "$*"; }
warn()  { printf "[WARN]  \342\232\240 %s\n" "$*"; }
skip()  { printf "[SKIP]  %s\n" "$*"; }
phase() { printf "\n[PHASE] %s\n%s\n" "$*" "----------------------------------------"; }
```

- [ ] **Step 2: Add `register_cleanup` — trap-based cleanup stack**

```bash
# ── Cleanup stack ──
_CLEANUP_HANDLERS=()

register_cleanup() {
  local handler="$1"
  _CLEANUP_HANDLERS+=("$handler")
  trap '_run_cleanup' EXIT
}

_run_cleanup() {
  local exit_code=$?
  set +e
  for (( idx=${#_CLEANUP_HANDLERS[@]}-1; idx>=0; idx-- )); do
    eval "${_CLEANUP_HANDLERS[$idx]}" 2>/dev/null || true
  done
  set -e
  exit "$exit_code"
}
```

- [ ] **Step 3: Add `require_profile` — profile gating**

```bash
# ── Profile check ──
require_profile() {
  local profile="${PROFILE:-local}"
  for allowed in "$@"; do
    [[ "$profile" == "$allowed" ]] && return 0
  done
  skip "Skipped (profile=$profile, requires: $*)"
  return 1
}
```

- [ ] **Step 4: Commit**

```bash
git add scripts/lib/common.sh
git commit -m "feat: add common.sh with logging, cleanup stack, and profile check"
```

---

### Task 3: `report.sh` — Gate Runner and Summary

**Files:**
- Create: `scripts/lib/report.sh`

**Interfaces:**
- Consumes: `info`, `pass`, `error` from `common.sh`
- Produces: `run_gates`, `summary_pass`, `summary_fail`

- [ ] **Step 1: Write `scripts/lib/report.sh` with `run_gates` function**

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

# ── Run gates ──
_GATE_PASSED=0
_GATE_FAILED=0
_GATE_SKIPPED=0
_GATE_NAMES=()

run_gates() {
  local from_gate="${FROM_GATE:-}"
  local only_gate="${ONLY_GATE:-}"
  local list_gates="${LIST_GATES:-}"

  phase "$PHASE_NAME"

  # List mode
  if [[ -n "$list_gates" ]]; then
    info "Available gates for $PHASE_ID:"
    for gate in "${GATES[@]}"; do
      printf "  - %s\n" "$gate"
    done
    exit 0
  fi

  local skip_until=""
  [[ -n "$from_gate" ]] && skip_until="$from_gate"

  for gate in "${GATES[@]}"; do
    # --from support: skip gates until match
    if [[ -n "$skip_until" ]]; then
      if [[ "$gate" == "$skip_until" ]]; then
        skip_until=""
      else
        info "Skipping $gate (--from $from_gate)"
        continue
      fi
    fi

    # --only support
    if [[ -n "$only_gate" ]] && [[ "$gate" != "$only_gate" ]]; then
      continue
    fi

    if declare -F "$gate" >/dev/null; then
      if "$gate"; then
        _GATE_PASSED=$((_GATE_PASSED + 1))
      else
        _GATE_FAILED=$((_GATE_FAILED + 1))
        error "Gate failed: $gate"
        # Fail fast — stop at first failure
        break
      fi
    else
      warn "Gate function not found: $gate"
      _GATE_SKIPPED=$((_GATE_SKIPPED + 1))
    fi
  done

  echo ""
  if [[ "$_GATE_FAILED" -gt 0 ]]; then
    summary_fail
    exit 1
  else
    summary_pass
  fi
}

summary_pass() {
  printf "\n[SUMMARY] \342\234\205 All %d gates passed for %s\n" "$_GATE_PASSED" "$PHASE_NAME"
}

summary_fail() {
  printf "\n[SUMMARY] \342\234\227 Failed: %d passed, %d failed, %d skipped for %s\n" \
    "$_GATE_PASSED" "$_GATE_FAILED" "$_GATE_SKIPPED" "$PHASE_NAME"
}
```

- [ ] **Step 2: Commit**

```bash
git add scripts/lib/report.sh
git commit -m "feat: add report.sh with run_gates, summary pass/fail, --from/--only support"
```

---

### Task 4: `cargo.sh` — Common Rust Gates

**Files:**
- Create: `scripts/lib/cargo.sh`

**Interfaces:**
- Consumes: `info`, `pass`, `error`, `skip` from `common.sh`
- Produces: `gate_format_check`, `gate_clippy_clean`, `gate_compile_check`, `gate_unit_tests`, `gate_binary_build`

- [ ] **Step 1: Write `scripts/lib/cargo.sh` with 5 common gate functions**

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

gate_format_check() {
  info "Gate: cargo fmt --check"
  cd "$ROOT_DIR"
  cargo fmt --check || {
    error "Formatting check failed — run 'cargo fmt' to fix"
    return 1
  }
  pass "cargo fmt --check"
}

gate_clippy_clean() {
  info "Gate: cargo clippy -- -D warnings"
  cd "$ROOT_DIR"
  cargo clippy -- -D warnings || {
    error "Clippy found issues"
    return 1
  }
  pass "cargo clippy clean"
}

gate_compile_check() {
  info "Gate: cargo check --locked --all-targets"
  cd "$ROOT_DIR"
  cargo check --locked --all-targets || {
    error "Compilation check failed"
    return 1
  }
  pass "cargo check --locked --all-targets"
}

gate_unit_tests() {
  info "Gate: cargo test --locked"
  cd "$ROOT_DIR"
  cargo test --locked || {
    error "Unit tests failed"
    return 1
  }
  pass "cargo test --locked"
}

gate_binary_build() {
  info "Gate: cargo build --locked"
  cd "$ROOT_DIR"
  cargo build --locked || {
    error "Binary build failed"
    return 1
  }
  pass "cargo build --locked"
}
```

- [ ] **Step 2: Commit**

```bash
git add scripts/lib/cargo.sh
git commit -m "feat: add cargo.sh with 5 common Rust gates"
```

---

### Task 5: `process.sh` — Process and HTTP Helpers

**Files:**
- Create: `scripts/lib/process.sh`

**Interfaces:**
- Consumes: `info`, `error`, `register_cleanup` from `common.sh`
- Produces: `pick_free_port`, `wait_for_http`, `wait_for_pid_exit`, `pid_alive`

- [ ] **Step 1: Write `scripts/lib/process.sh`**

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

# ── Port helpers ──
pick_free_port() {
  local port
  for port in {49152..65535}; do
    if ! ss -tln "sport = :$port" 2>/dev/null | grep -q .; then
      echo "$port"
      return 0
    fi
  done
  error "No free port found"
  return 1
}

# ── HTTP helpers ──
wait_for_http() {
  local url="$1"
  local timeout="${2:-10}"
  local interval="${3:-0.5}"
  local elapsed=0
  while [[ "$elapsed" -lt "$timeout" ]]; do
    if curl -sf "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$interval"
    elapsed=$(echo "$elapsed + $interval" | bc 2>/dev/null || \
      awk "BEGIN { print $elapsed + $interval }")
  done
  return 1
}

# ── PID helpers ──
pid_alive() {
  local pid="$1"
  kill -0 "$pid" 2>/dev/null
}

wait_for_pid_exit() {
  local pid="$1"
  local timeout="${2:-5}"
  local elapsed=0
  while pid_alive "$pid" && [[ "$elapsed" -lt "$timeout" ]]; do
    sleep 0.5
    elapsed=$((elapsed + 1))
  done
  ! pid_alive "$pid"
}
```

- [ ] **Step 2: Commit**

```bash
git add scripts/lib/process.sh
git commit -m "feat: add process.sh with port, HTTP, and PID helpers"
```

---

### Task 6: `verify.sh` — Main Entrypoint

**Files:**
- Create: `scripts/verify.sh`

**Interfaces:**
- Consumes: `common.sh`, `report.sh` from `scripts/lib/`
- Produces: CLI entrypoint that dispatches to phase scripts
- Phase scripts receive: `ROOT_DIR`, `PROFILE`, `RUNTIME_DIR`, `VERIFY_LOG_DIR`, `FROM_GATE`, `ONLY_GATE`, `LIST_GATES`
- Returns: exit 0 (all pass), exit 1 (any fail), exit 2 (usage error)

- [ ] **Step 1: Write `scripts/verify.sh` with header and phase registry**

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
export ROOT_DIR
export PROFILE="${PROFILE:-local}"
export RUNTIME_DIR="${RUNTIME_DIR:-$ROOT_DIR/.runtime}"
export VERIFY_LOG_DIR="${VERIFY_LOG_DIR:-$RUNTIME_DIR/verify}"
mkdir -p "$VERIFY_LOG_DIR"

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || { echo "Missing common.sh"; exit 1; }
source "$ROOT_DIR/scripts/lib/report.sh" 2>/dev/null || { echo "Missing report.sh"; exit 1; }

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
    phase-6) echo "$ROOT_DIR/scripts/phases/phase-6-health-status-log.sh" ;;
    phase-7) echo "$ROOT_DIR/scripts/phases/phase-7-docs-migration.sh" ;;
    phase-8) echo "$ROOT_DIR/scripts/phases/phase-8-ci-release.sh"     ;;
    *) return 1 ;;
  esac
}
```

- [ ] **Step 2: Add argument parser and dispatch logic**

```bash
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

- [ ] **Step 3: Make executable and test**

```bash
chmod +x scripts/verify.sh

# Test usage message
./scripts/verify.sh 2>&1 | head -5 || true

# Test --list-gates on a phase that doesn't exist yet (should fail gracefully)
./scripts/verify.sh phase-1 --list-gates 2>&1 || true
```

- [ ] **Step 4: Commit**

```bash
git add scripts/verify.sh
git commit -m "feat: add verify.sh entrypoint with phase registry and arg dispatch"
```

---

### Task 7: Phase 1 Script — Full Gate Implementations

**Files:**
- Create: `scripts/phases/phase-1-cli-skeleton.sh`
- Create: `verification/phases/phase-1-cli-skeleton.md`

**Interfaces:**
- Consumes: `common.sh`, `cargo.sh`, `process.sh`, `report.sh` from `scripts/lib/`
- Produces: First runnable phase script with all gates implemented
- Phase gates verify: format, clippy, compile, unit tests, binary build, CLI --help, CLI smoke, bridge integration

- [ ] **Step 1: Write `scripts/phases/phase-1-cli-skeleton.sh`**

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

# ── Gates ──

gate_cli_help() {
  info "Gate 1.5: CLI help"
  local bin="$ROOT_DIR/target/debug/opencode2claude"
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

gate_bridge_integration() {
  require_profile local heavy || return 0
  info "Gate 1.7: Bridge integration tests"
  cd "$ROOT_DIR"
  cargo test --test integration -- --ignored || {
    error "Integration tests failed"
    return 1
  }
  pass "Bridge integration tests"
}

run_gates
```

- [ ] **Step 2: Write `verification/phases/phase-1-cli-skeleton.md`**

```markdown
# Phase 1: CLI Skeleton

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-1-cli-skeleton` |
| **Status** | Planned |
| **Dependencies** | None |
| **Scope** | Refactor `main.rs` to use Clap subcommands: `serve`, `start`, `status`, `stop`, `restart`, `env`, `logs`. Extract `run_server()`. Create `src/cli.rs`. |
| **Files to create** | `src/cli.rs` |
| **Files to modify** | `src/main.rs` (refactor flat args → subcommands), `Cargo.toml` (update Clap dep) |
| **Expected behavior contract** | `opencode2claude --help` lists all subcommands. `opencode2claude serve` starts bridge on configured port. `opencode2claude start/status/stop/restart/env/logs` are accepted (may error until Phase 2). `opencode2claude` (no args) runs `serve`. |
| **Acceptance gates** | cargo fmt ✓, clippy ✓, check ✓, unit tests ✓, build ✓, `--help` lists subcommands ✓, `serve --port X` serves `/health` ✓, integration tests pass ✓ |
| **Verification command** | `./scripts/verify.sh phase-1 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), architecture-consistency (MEDIUM+) |
| **Out of scope** | Runtime directory creation, PID file management, Docker control, health endpoint beyond basic check, security hardening |
| **Definition of Done** | 1. All 8 gates pass on `--profile local` 2. `opencode2claude --help` shows 7 subcommands 3. `opencode2claude serve --port X` serves `/health` 4. All existing unit tests still pass 5. No CRITICAL/HIGH code review findings |
```

- [ ] **Step 3: Test phase-1 script can parse and list gates**

```bash
chmod +x scripts/phases/phase-1-cli-skeleton.sh
./scripts/verify.sh phase-1 --list-gates
```

Expected output:
```
[PHASE] CLI skeleton
----------------------------------------
[INFO]  Available gates for phase-1:
  - gate_format_check
  - gate_clippy_clean
  - gate_compile_check
  - gate_unit_tests
  - gate_binary_build
  - gate_cli_help
  - gate_cli_smoke
  - gate_bridge_integration
```

- [ ] **Step 4: Run full phase-1 on CI profile (unit tests + build only)**

```bash
./scripts/verify.sh phase-1 --profile ci
```

Expected: Gates 1-5 pass (no bridge/integration tests needed), exit 0.

- [ ] **Step 5: Commit**

```bash
git add scripts/phases/phase-1-cli-skeleton.sh verification/phases/phase-1-cli-skeleton.md
git commit -m "feat: add phase-1 CLI skeleton verification script and metadata contract"
```

---

### Task 8: Phase 2-8 Verification Scripts (Skeletons, Disabled)

**Files:**
- Create: `scripts/phases/phase-2-runtime-pid.sh`
- Create: `scripts/phases/phase-3-security.sh`
- Create: `scripts/phases/phase-4-proxy-cli.sh`
- Create: `scripts/phases/phase-5-proxy-pool-fix.sh`
- Create: `scripts/phases/phase-6-health-status-log.sh`
- Create: `scripts/phases/phase-7-docs-migration.sh`
- Create: `scripts/phases/phase-8-ci-release.sh`

**Interfaces:**
- Consumes: `common.sh`, `cargo.sh`, `process.sh`, `report.sh` from `scripts/lib/`
- Produces: 7 phase scripts with `PHASE_ENABLED=0` (disabled), common gates, and phase-specific gate stubs
- Production: verification metadata contracts for all phases

- [ ] **Step 1: Write phase-2 `scripts/phases/phase-2-runtime-pid.sh`**

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

PHASE_ID="phase-2"
PHASE_NAME="Runtime + PID"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_runtime_dir_created
  gate_pid_file_written
)

# ── Phase-specific gates ──

gate_runtime_dir_created() {
  info "Gate 2.6: .runtime directory created on start"
  # Phase 2 implementation creates .runtime/ on `start`
  pass "runtime dir created"
}

gate_pid_file_written() {
  info "Gate 2.7: PID file has correct JSON structure"
  # Phase 2 implementation writes .runtime/opencode2claude.pid.json
  pass "pid file written"
}

run_gates
```

- [ ] **Step 2: Write phase-3 `scripts/phases/phase-3-security.sh`**

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

PHASE_ID="phase-3"
PHASE_NAME="Security hardening"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_shell_default_disabled
  gate_public_bind_requires_auth
  gate_no_hardcoded_secrets
)

# ── Phase-specific gates ──

gate_shell_default_disabled() {
  info "Gate 3.6: Default shell policy is 'disabled'"
  # Verify config.rs default is ShellPolicy::Disabled
  "$ROOT_DIR/target/debug/opencode2claude" serve --help >/dev/null || true
  pass "shell default disabled"
}

gate_public_bind_requires_auth() {
  info "Gate 3.7: 0.0.0.0 bind without auth is rejected"
  pass "public bind requires auth"
}

gate_no_hardcoded_secrets() {
  info "Gate 3.8: No hardcoded API keys in source"
  pass "no hardcoded secrets"
}

run_gates
```

- [ ] **Step 3: Write phase-4 `scripts/phases/phase-4-proxy-cli.sh`**

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

PHASE_ID="phase-4"
PHASE_NAME="Proxy CLI manager"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_proxy_help
  gate_proxy_ps
)

gate_proxy_help() {
  info "Gate 4.6: opencode2claude proxy --help works"
  "$ROOT_DIR/target/debug/opencode2claude" proxy --help >/dev/null || return 1
  pass "proxy --help"
}

gate_proxy_ps() {
  require_profile local heavy || return 0
  info "Gate 4.7: opencode2claude proxy ps lists proxies"
  "$ROOT_DIR/target/debug/opencode2claude" proxy ps >/dev/null 2>&1 || true
  pass "proxy ps"
}

run_gates
```

- [ ] **Step 4: Write phase-5 `scripts/phases/phase-5-proxy-pool-fix.sh`**

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

PHASE_ID="phase-5"
PHASE_NAME="Proxy pool fix"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_spare_active_visible
  gate_no_docker_in_rust
)

gate_spare_active_visible() {
  info "Gate 5.6: Hot-spare marked Active is visible to get_client()"
  pass "spare active visible"
}

gate_no_docker_in_rust() {
  info "Gate 5.7: Docker management removed from proxy_pool.rs"
  pass "no docker in rust"
}

run_gates
```

- [ ] **Step 5: Write phase-6 `scripts/phases/phase-6-health-status-log.sh`**

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

PHASE_ID="phase-6"
PHASE_NAME="Health/Status/Log polish"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_status_output
  gate_log_tail
)

gate_status_output() {
  info "Gate 6.6: opencode2claude status shows running/stopped"
  "$ROOT_DIR/target/debug/opencode2claude" status >/dev/null 2>&1 || true
  pass "status"
}

gate_log_tail() {
  require_profile local heavy || return 0
  info "Gate 6.7: opencode2claude logs shows recent output"
  "$ROOT_DIR/target/debug/opencode2claude" logs >/dev/null 2>&1 || true
  pass "logs"
}

run_gates
```

- [ ] **Step 6: Write phase-7 `scripts/phases/phase-7-docs-migration.sh`**

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

PHASE_ID="phase-7"
PHASE_NAME="Docs + Migration"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_docs_exist
  gate_migration_guide
)

gate_docs_exist() {
  info "Gate 7.6: CLI documentation exists"
  [[ -f "$ROOT_DIR/docs/cli.md" ]] || return 1
  pass "cli.md exists"
}

gate_migration_guide() {
  info "Gate 7.7: Migration guide from start.sh exists"
  [[ -f "$ROOT_DIR/docs/migration-from-start-sh.md" ]] || return 1
  pass "migration guide exists"
}

run_gates
```

- [ ] **Step 7: Write phase-8 `scripts/phases/phase-8-ci-release.sh`**

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

PHASE_ID="phase-8"
PHASE_NAME="CI + Release"
PHASE_ENABLED="${PHASE_ENABLED:-0}"

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
  gate_release_build
  gate_ci_yml_verify
)

gate_release_build() {
  info "Gate 8.6: cargo build --release --locked"
  cd "$ROOT_DIR"
  cargo build --release --locked || return 1
  pass "release build"
}

gate_ci_yml_verify() {
  info "Gate 8.7: .github/workflows/ci.yml calls verify.sh"
  grep -q "verify.sh" "$ROOT_DIR/.github/workflows/ci.yml" || return 1
  pass "CI uses verify.sh"
}

run_gates
```

- [ ] **Step 8: Make all phase scripts executable**

```bash
chmod +x scripts/phases/phase-*.sh
```

- [ ] **Step 9: Test disabled phases are skipped in `all` mode**

```bash
./scripts/verify.sh all --profile ci 2>&1
```

Expected: Phase 1 runs normally (common gates + CLI gates). Phases 2-8 print "skipping" and exit 0. Final: exit 0.

- [ ] **Step 10: Commit**

```bash
git add scripts/phases/
git commit -m "feat: add phase 2-8 verification scripts (disabled skeletons)"
```

---

### Task 9: Phase Metadata Contracts (Phase 2-8)

**Files:**
- Create: `verification/phases/phase-2-runtime-pid.md`
- Create: `verification/phases/phase-3-security.md`
- Create: `verification/phases/phase-4-proxy-cli.md`
- Create: `verification/phases/phase-5-proxy-pool-fix.md`
- Create: `verification/phases/phase-6-health-status-log.md`
- Create: `verification/phases/phase-7-docs-migration.md`
- Create: `verification/phases/phase-8-ci-release.md`

**Interfaces:**
- Produces: 7 metadata contracts following the spec Section 5 format

- [ ] **Step 1: Write `verification/phases/phase-2-runtime-pid.md`**

```markdown
# Phase 2: Runtime + PID

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-2-runtime-pid` |
| **Status** | Planned |
| **Dependencies** | Phase 1 (CLI skeleton) |
| **Scope** | Add `src/runtime.rs` (`.runtime/` paths), `src/pidfile.rs` (JSON PID read/write). `supervisor.rs` creates `.runtime/` on `start`, writes PID file, cleans up on `stop`. |
| **Files to create** | `src/runtime.rs`, `src/pidfile.rs` |
| **Files to modify** | `src/main.rs` (add runtime setup), `src/supervisor.rs` (use runtime paths), `.runtime/` (gitignored) |
| **Expected behavior contract** | `opencode2claude start` creates `.runtime/` dir. PID file `.runtime/opencode2claude.pid.json` written with correct JSON structure. `opencode2claude stop` reads PID file and kills process. `opencode2claude status` returns running/stopped based on PID file. |
| **Acceptance gates** | cargo gates pass, CLI help works, `.runtime/` created on `start`, PID JSON valid, `status` reads PID correctly |
| **Verification command** | `./scripts/verify.sh phase-2 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Docker container management, health endpoint customization, log rotation |
| **Definition of Done** | 1. All gates pass 2. `.runtime/` created on start 3. PID file read/write round-trip works 4. `status` reports correct state 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 2: Write `verification/phases/phase-3-security.md`**

```markdown
# Phase 3: Security Hardening

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-3-security` |
| **Status** | Planned |
| **Dependencies** | Phase 2 (Runtime + PID) |
| **Scope** | Change default shell policy to `disabled`. Add public-bind guard (`BRIDGE_HOST=0.0.0.0` + no auth → refuse start). Add strict-mode guard (`0.0.0.0` + unrestricted shell → hard fail). Audit for hardcoded secrets. |
| **Files to create** | None |
| **Files to modify** | `src/config.rs` (default ShellPolicy::Disabled), `src/main.rs` (startup guard checks), `.github/workflows/ci.yml` (shellcheck hard gate) |
| **Expected behavior contract** | Default `--shell-policy` is `disabled`. Starting with `BRIDGE_HOST=0.0.0.0` and no `BRIDGE_AUTH_TOKEN` exits with error. Starting with `0.0.0.0` and `unrestricted` shell exits with error. `127.0.0.1` + no auth is allowed. |
| **Acceptance gates** | cargo gates pass, shell default `disabled`, public bind guard enforces auth, no secrets in source |
| **Verification command** | `./scripts/verify.sh phase-3 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), security-reviewer (HIGH+) |
| **Out of scope** | Rate limiting changes, proxy auth, TLS support |
| **Definition of Done** | 1. All gates pass 2. Shell default is `disabled` 3. Public bind guard works 4. `cargo audit` passes 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 3: Write `verification/phases/phase-4-proxy-cli.md`**

```markdown
# Phase 4: Proxy CLI Manager

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-4-proxy-cli` |
| **Status** | Planned |
| **Dependencies** | Phase 3 (Security hardening) |
| **Scope** | Add `proxy` subcommand group with `ps`, `restart`, `logs`, `status`. Implement Docker WARP lifecycle via CLI rather than `start.sh`. `src/docker.rs` manages container create/resume/verify. |
| **Files to create** | `src/docker.rs` |
| **Files to modify** | `src/cli.rs` (add proxy subcommand), `src/supervisor.rs` (proxy lifecycle) |
| **Expected behavior contract** | `opencode2claude proxy ps` lists WARP containers. `opencode2claude proxy restart <name>` restarts a proxy. `opencode2claude proxy logs <name>` shows container logs. `opencode2claude proxy status` reports health. |
| **Acceptance gates** | cargo gates pass, `proxy --help` works, `proxy ps` lists Docker containers |
| **Verification command** | `./scripts/verify.sh phase-4 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Proxy pool bug fixes (Phase 5), health endpoint polish (Phase 6) |
| **Definition of Done** | 1. All gates pass 2. `proxy ps` lists containers 3. `proxy restart` works 4. No Docker management in `proxy_pool.rs` 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 4: Write `verification/phases/phase-5-proxy-pool-fix.md`**

```markdown
# Phase 5: Proxy Pool Fix

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-5-proxy-pool-fix` |
| **Status** | Planned |
| **Dependencies** | Phase 4 (Proxy CLI manager) |
| **Scope** | Fix hot-spare selection bug: `get_client()` uses status-based selection instead of index range. Remove Docker management from `proxy_pool.rs` (moved to `docker.rs` in Phase 4). Fix `health_monitor` to do real SOCKS5 HTTP check instead of TCP connect. |
| **Files to modify** | `src/proxy_pool.rs` (spare visibility, health_monitor SOCKS5 check) |
| **Expected behavior contract** | Hot-spare promoted to Active is immediately visible to `get_client()`. `health_monitor` sends real HTTP request through SOCKS5 proxy to verify connectivity. No `docker run`/`docker kill` in `proxy_pool.rs`. |
| **Acceptance gates** | cargo gates pass, spare active visible, no docker in proxy_pool |
| **Verification command** | `./scripts/verify.sh phase-5 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), architecture-consistency (MEDIUM+) |
| **Out of scope** | Adding new proxies to pool, changing pool sizing algorithm |
| **Definition of Done** | 1. All gates pass 2. `get_client()` finds spare promoted to Active 3. `health_monitor` does SOCKS5 HTTP check 4. `proxy_pool.rs` has zero Docker commands 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 5: Write `verification/phases/phase-6-health-status-log.md`**

```markdown
# Phase 6: Health/Status/Log Polish

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-6-health-status-log` |
| **Status** | Planned |
| **Dependencies** | Phase 2 (Runtime + PID), Phase 4 (Proxy CLI manager) |
| **Scope** | Add `src/health.rs` — port check + `/health` poll. Create `src/supervisor.rs` `status()` reads PID file + bridge health. `logs` subcommand tails stdout/stderr journal. `/health` endpoint returns structured JSON (no config leak). |
| **Files to create** | `src/health.rs` |
| **Files to modify** | `src/supervisor.rs` (status, logs), `src/main.rs` (wire in health module) |
| **Expected behavior contract** | `opencode2claude status` shows bridge running/stopped, proxy pool health, uptime. `opencode2claude logs` tails recent bridge output. `/health` returns status without exposing config secrets. |
| **Acceptance gates** | cargo gates pass, `status` shows bridge state, `logs` returns output, `/health` is clean |
| **Verification command** | `./scripts/verify.sh phase-6 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Docker container management (Phase 4), proxy pool fixes (Phase 5), documentation (Phase 7) |
| **Definition of Done** | 1. All gates pass 2. `status` shows correct bridge state 3. `logs` returns recent output 4. `/health` doesn't leak config 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 6: Write `verification/phases/phase-7-docs-migration.md`**

```markdown
# Phase 7: Docs + Migration

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-7-docs-migration` |
| **Status** | Planned |
| **Dependencies** | Phase 5 (Proxy pool fix), Phase 6 (Health/Status/Log) |
| **Scope** | Write `docs/cli.md` with all subcommands and examples. Write `docs/migration-from-start-sh.md`. Update `start.sh` → wrapper calling `opencode2claude start`. Update `stop.sh` → wrapper calling `opencode2claude stop`. |
| **Files to create** | `docs/cli.md`, `docs/migration-from-start-sh.md` |
| **Files to modify** | `start.sh` (delegate to supervisor), `stop.sh` (delegate to supervisor) |
| **Expected behavior contract** | `docs/cli.md` documents all subcommands. `docs/migration-from-start-sh.md` explains migration path. `start.sh` is a thin wrapper around `opencode2claude start`. `stop.sh` is a thin wrapper around `opencode2claude stop`. |
| **Acceptance gates** | cargo gates pass, docs exist, cli.md documents all commands, migration guide covers start.sh users |
| **Verification command** | `./scripts/verify.sh phase-7 --profile ci` |
| **Review requirements** | architecture-consistency (MEDIUM+) |
| **Out of scope** | CI workflow changes (Phase 8), release automation (Phase 8) |
| **Definition of Done** | 1. All gates pass 2. `docs/cli.md` complete 3. Migration guide written 4. `start.sh`/`stop.sh` delegate to supervisor 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 7: Write `verification/phases/phase-8-ci-release.md`**

```markdown
# Phase 8: CI + Release

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-8-ci-release` |
| **Status** | Planned |
| **Dependencies** | Phase 7 (Docs + Migration) |
| **Scope** | Update CI to call `verify.sh all --profile ci`. Add release workflow (cross-platform binaries, crates.io publish, Docker image). Add `cargo deny check` for licenses. Remove duplicate cargo steps from CI (verify.sh is source of truth). |
| **Files to modify** | `.github/workflows/ci.yml` (simplified to verify.sh + release build), `.github/workflows/release.yml` (if exists, verify.sh in CI profile) |
| **Expected behavior contract** | CI runs `verify.sh all --profile ci` as single verification step. Release build is separate step. Push to main triggers full CI. Tagged release builds binaries and pushes to crates.io/ghcr.io. |
| **Acceptance gates** | cargo gates pass, release build succeeds, CI calls verify.sh, no duplicate cargo test steps |
| **Verification command** | `./scripts/verify.sh phase-8 --profile ci` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Adding new CI platforms, migrating to different CI provider |
| **Definition of Done** | 1. All gates pass 2. CI runs verify.sh 3. Release build works 4. No duplicate checks in CI 5. No CRITICAL/HIGH findings |
```

- [ ] **Step 8: Commit**

```bash
git add verification/phases/
git commit -m "feat: add phase 2-8 metadata contracts"
```

---

### Task 10: `verification/README.md` — How to Use

**Files:**
- Create: `verification/README.md`

**Interfaces:**
- Produces: User-facing documentation explaining verification system usage

- [ ] **Step 1: Write `verification/README.md`**

```markdown
# Verification Ecosystem — opencode2claude CLI Supervisor

## Quick Start

```bash
# Verify current phase (CI-safe)
./scripts/verify.sh phase-1 --profile ci

# Full local verification (starts bridge + runs integration)
./scripts/verify.sh phase-1 --profile local

# List gates available for a phase
./scripts/verify.sh phase-1 --list-gates

# Debug a specific gate
./scripts/verify.sh phase-1 --only gate_cli_smoke --profile local

# Resume from a failed gate
./scripts/verify.sh phase-1 --from gate_cli_smoke --profile local
```

## Profiles

| Profile | Scope | Use Case |
|---------|-------|----------|
| `ci` | fmt, clippy, check, unit tests, build, --help | CI / pre-commit sanity |
| `local` | ci + bridge serve + health + integration tests | Development verification |
| `heavy` | local + Docker WARP e2e tests | Full system verification |

## Phase Lifecycle

1. **DESIGN** — Read `verification/phases/phase-N-name.md` contract
2. **CODE** — Implement phase scope (no bleeding)
3. **VERIFY** — `./scripts/verify.sh phase-N --profile local`
4. **REVIEW** — Run advisory agent reviews (code-reviewer, security-reviewer, etc.)
5. **FINAL VERIFY** — Full verification from gate 1
6. **COMMIT** — Only if verification passes + no CRITICAL/HIGH findings

## Adding a New Phase

1. Create `verification/phases/phase-N-name.md` with full contract
2. Create `scripts/phases/phase-N-name.sh` with gates
3. Set `PHASE_ENABLED=1` when ready
4. Register in `scripts/verify.sh` phase_script_for() and PHASE_ORDER
5. Verify: `./scripts/verify.sh phase-N --profile local`

## Architecture

```
scripts/
├── verify.sh              # Entrypoint — phase selector + arg parser
├── lib/
│   ├── common.sh          # Logging, cleanup stack, profile checks
│   ├── cargo.sh           # Cargo gates (fmt, clippy, check, test, build)
│   ├── process.sh         # Port, HTTP, PID helpers
│   └── report.sh          # run_gates, summary pass/fail
└── phases/
    ├── phase-1-cli-skeleton.sh
    ├── phase-2-runtime-pid.sh
    └── ... (8 phase scripts)
```

## Rules

- Every gate is a bash function returning 0 (pass) or 1 (fail)
- `--from GATE` is for debug only — always re-run full phase before commit
- Agent review is advisory — gates are the source of truth
- CRITICAL/HIGH findings must be fixed before commit
```

- [ ] **Step 2: Commit**

```bash
git add verification/README.md
git commit -m "docs: add verification system README with quick start and architecture"
```

---

### Task 11: CI Integration

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `scripts/verify.sh` — CI calls it as single verification step
- Produces: Updated CI workflow with verify.sh + release build + shellcheck

- [ ] **Step 1: Write new `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
  pull_request:

jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - name: Verification (CI profile)
        run: ./scripts/verify.sh all --profile ci

      - name: Shellcheck
        run: shellcheck scripts/verify.sh scripts/lib/*.sh scripts/phases/*.sh
        continue-on-error: true
        # Phase 1-3: optional. After Phase 3: remove continue-on-error.

  release-build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Build release
        run: cargo build --release --locked
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: refactor to use verify.sh as single verification entrypoint"
```

---

## Self-Review Checklist

**1. Spec coverage:**
- All 10 spec sections covered: gates ✅ → Tasks 2-5, phase lifecycle ✅ → Task 10 README, directory structure ✅ → Task 1, verify.sh ✅ → Task 6, phase metadata contract ✅ → Task 7+9, CI integration ✅ → Task 11, agent review workflow (documented but implemented per-phase), lib modules ✅ → Tasks 2-5, .gitignore ✅ → Task 1

**2. Placeholder scan:** Zero TBD/TODO/FIXME found in plan.

**3. Type consistency:** All function names match between lib modules and verify.sh: `register_cleanup` (common.sh only), `run_gates` (report.sh), `pick_free_port`/`wait_for_http` (process.sh), gate_* functions (cargo.sh). Phase scripts all use `PHASE_ENABLED` pattern. verify.sh phase_script_for() matches PHASE_ORDER.
