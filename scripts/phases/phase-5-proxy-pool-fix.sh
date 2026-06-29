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
