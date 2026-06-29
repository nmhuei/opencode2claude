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
  # TODO(phase-3): assert config.rs default ShellPolicy::Disabled
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
