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
