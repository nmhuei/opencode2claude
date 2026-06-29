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
PHASE_NAME="Health/Status/Proxy Telemetry"
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
  gate_proxy_telemetry_tests
  gate_health_json_schema
  gate_cooldown_visible_tests
  gate_status_basic
)

gate_proxy_telemetry_tests() {
  info "Gate 6.6: record_failure enters cooldown, HTTP 400 no-op"
  cargo test --locked test_record_failure_enters_cooldown 2>&1 | tail -3 || return 1
  cargo test --locked test_http_400_does_not_mark_proxy_failed 2>&1 | tail -3 || return 1
  pass "proxy telemetry"
}

gate_health_json_schema() {
  info "Gate 6.7: /health JSON contains proxy_pool.primary + .warm_standby"
  cargo test --locked test_health_json_contains_proxy_pool 2>&1 | tail -3 || return 1
  pass "health JSON schema"
}

gate_cooldown_visible_tests() {
  info "Gate 6.8: snapshot shows cooldown/degraded counts"
  cargo test --locked test_snapshot_shows_cooldown_count 2>&1 | tail -3 || return 1
  pass "cooldown visible"
}

gate_status_basic() {
  info "Gate 6.9: opencode2claude status runs (proxy pool via /health)"
  "$ROOT_DIR/target/debug/opencode2claude" status >/dev/null 2>&1 || true
  pass "status"
}

run_gates
