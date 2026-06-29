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
PHASE_NAME="Routing policy contract"
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
  gate_primary_first_routing
  gate_sticky_mapping
  gate_warm_standby_failover
  gate_affected_agent_only_remap
  gate_recovery_returns_to_primary
  gate_no_standby_if_primary_healthy
  gate_rendezvous_deterministic
)

gate_primary_first_routing() {
  info "Gate 5.6: Primary-first routing - WarmStandby excluded from normal traffic"
  cargo test --locked test_warm_standby_excluded_from_normal_routing 2>&1 | tail -3 || return 1
  pass "primary-first routing"
}

gate_sticky_mapping() {
  info "Gate 5.7: Sticky mapping - same key always resolves to same proxy"
  cargo test --locked test_sticky_mapping_stable 2>&1 | tail -3 || return 1
  pass "sticky mapping stable"
}

gate_warm_standby_failover() {
  info "Gate 5.8: Temporary failover to WarmStandby when selected primary is unhealthy"
  cargo test --locked test_temporary_failover_to_warm_standby 2>&1 | tail -3 || return 1
  pass "warm standby failover"
}

gate_affected_agent_only_remap() {
  info "Gate 5.9: Failure of one primary does not remap agents on healthy primaries"
  cargo test --locked test_affected_agent_only_remap 2>&1 | tail -3 || return 1
  pass "affected-agent-only remap"
}

gate_recovery_returns_to_primary() {
  info "Gate 5.10: Recovered primary becomes eligible after cooldown expiry"
  cargo test --locked test_recovery_returns_to_primary 2>&1 | tail -3 || return 1
  pass "recovery returns to primary"
}

gate_no_standby_if_primary_healthy() {
  info "Gate 5.11: Healthy selected primary used even if standby exists"
  cargo test --locked test_no_standby_if_selected_primary_healthy 2>&1 | tail -3 || return 1
  pass "no standby if primary healthy"
}

gate_rendezvous_deterministic() {
  info "Gate 5.12: Rendezvous hash produces deterministic cross-run scores"
  cargo test --locked test_rendezvous_deterministic 2>&1 | tail -3 || return 1
  pass "rendezvous deterministic"
}

run_gates
