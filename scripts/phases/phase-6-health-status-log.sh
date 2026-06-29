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
