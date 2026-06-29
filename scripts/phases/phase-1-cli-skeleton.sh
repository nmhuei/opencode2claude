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
  # Top-level help (entry-point showing all subcommands)
  "$bin" --help >/dev/null 2>&1 || return 1
  # Each subcommand should have valid --help (will fail until Phase 1 Rust code)
  "$bin" serve --help  >/dev/null 2>&1 || return 1
  "$bin" start --help  >/dev/null 2>&1 || return 1
  "$bin" status --help >/dev/null 2>&1 || return 1
  "$bin" stop --help   >/dev/null 2>&1 || return 1
  "$bin" restart --help >/dev/null 2>&1 || return 1
  "$bin" env --help    >/dev/null 2>&1 || return 1
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
  cargo test --locked --test integration -- --ignored || {
    error "Integration tests failed"
    return 1
  }
  pass "Bridge integration tests"
}

run_gates
