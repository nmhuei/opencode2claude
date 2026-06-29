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
  gate_shell_default_disabled
  gate_public_bind_no_auth_rejected
  gate_public_bind_unrestricted_shell_rejected
  gate_public_bind_auth_allowed
  gate_localhost_no_auth_allowed
  gate_health_no_secrets
  gate_no_40010
)

# ── Phase-specific gates ──

gate_shell_default_disabled() {
  info "Gate 3.6: Default shell policy is 'disabled'"
  cargo test --locked test_security_default_shell_policy_is_disabled 2>&1 | tail -3 || return 1
  pass "default shell policy is disabled"
}

gate_public_bind_no_auth_rejected() {
  info "Gate 3.7: 0.0.0.0 bind without auth is rejected"
  cargo test --locked test_security_public_bind_without_auth_rejected 2>&1 | tail -3 || return 1
  pass "public bind without auth rejected"
}

gate_public_bind_unrestricted_shell_rejected() {
  info "Gate 3.8: 0.0.0.0 bind with unrestricted shell is rejected (even with auth)"
  cargo test --locked test_security_public_bind_with_unrestricted_shell_rejected 2>&1 | tail -3 || return 1
  pass "public bind + unrestricted shell rejected"
}

gate_public_bind_auth_allowed() {
  info "Gate 3.9: 0.0.0.0 bind with auth and disabled shell is allowed"
  cargo test --locked test_security_public_bind_with_auth_allowed 2>&1 | tail -3 || return 1
  pass "public bind with auth allowed"
}

gate_localhost_no_auth_allowed() {
  info "Gate 3.10: 127.0.0.1 without auth is allowed"
  cargo test --locked test_security_localhost_without_auth_allowed 2>&1 | tail -3 || return 1
  pass "localhost without auth allowed"
}

gate_health_no_secrets() {
  info "Gate 3.11: /health must not expose auth tokens or secrets"
  # The health handler uses state.config.auth_enabled() (boolean), not raw auth_tokens.
  # This gate verifies auth_tokens does not appear in the health handler's response JSON.
  local health_func
  health_func=$(sed -n '/pub async fn handle_health/,/^pub async/p' "$ROOT_DIR/src/handlers.rs" 2>/dev/null | head -n -1 || true)
  if echo "$health_func" | grep -q "auth_tokens" 2>/dev/null; then
    error "/health handler references auth_tokens directly"
    return 1
  fi
  pass "/health does not expose secrets"
}

gate_no_40010() {
  info "Gate 3.12: no active reference to deprecated port 40010 in source code"
  if grep -rn "socks5://.*40010\|http.*40010" "$ROOT_DIR/src/" "$ROOT_DIR/start.sh" "$ROOT_DIR/stop.sh" 2>/dev/null; then
    error "Found active reference to deprecated port 40010"
    return 1
  fi
  pass "no active 40010 references in code"
}

run_gates
