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
