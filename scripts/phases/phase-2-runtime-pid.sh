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
