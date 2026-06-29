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
  gate_runtime_dir_created
  gate_pid_file_valid
)

gate_cli_help() {
  info "Gate 2.5: CLI help"
  local bin="$ROOT_DIR/target/debug/opencode2claude"
  "$bin" --help >/dev/null 2>&1 || return 1
  "$bin" start --help  >/dev/null 2>&1 || return 1
  "$bin" status --help >/dev/null 2>&1 || return 1
  "$bin" stop --help   >/dev/null 2>&1 || return 1
  pass "CLI help lists all expected subcommands"
}

gate_runtime_dir_created() {
  require_profile local heavy || return 0
  info "Gate 2.6: .runtime directory created on start"

  local bin="$ROOT_DIR/target/debug/opencode2claude"
  local log_file="$VERIFY_LOG_DIR/phase-2-start.log"
  local port; port="$(pick_free_port)"

  # Clean any leftover state
  rm -rf "$ROOT_DIR/.runtime" 2>/dev/null || true
  mkdir -p "$VERIFY_LOG_DIR"

  # Start bridge on the free port — this spawns serve in background and exits
  "$bin" start -p "$port" >"$log_file" 2>&1 || {
    error "start command failed"
    cat "$log_file"
    return 1
  }

  register_cleanup "\"$bin\" stop -p \"$port\" 2>/dev/null || true"
  register_cleanup "rm -rf \"$ROOT_DIR/.runtime\" 2>/dev/null || true"

  # Wait for runtime directory and PID file
  sleep 1

  if [[ ! -d "$ROOT_DIR/.runtime" ]]; then
    error ".runtime/ directory was not created"
    tail -n 20 "$log_file" || true
    return 1
  fi

  pass ".runtime directory created on start"
}

gate_pid_file_valid() {
  require_profile local heavy || return 0
  info "Gate 2.7: PID file has correct JSON structure"

  local pid_file="$ROOT_DIR/.runtime/opencode2claude.pid.json"

  if [[ ! -f "$pid_file" ]]; then
    error "PID file not found: $pid_file"
    ls -la "$ROOT_DIR/.runtime/" 2>/dev/null || true
    return 1
  fi

  # Validate JSON structure using the binary's status command
  local bin="$ROOT_DIR/target/debug/opencode2claude"
  local status_output
  status_output="$("$bin" status 2>&1)" || {
    error "status command failed"
    return 1
  }

  if echo "$status_output" | grep -q "Running"; then
    pass "PID file valid — bridge is running"
  else
    error "status shows unexpected state: $status_output"
    cat "$pid_file" 2>/dev/null || true
    return 1
  fi
}

run_gates
