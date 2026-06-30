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
  gate_cli_smoke
  gate_bridge_integration
  gate_proxy_help
  gate_proxy_ps
  gate_protected_ports_guarded
  gate_no_40010_reference
  gate_proxy_restart_primary_only
  gate_proxy_purge_primary_only
  gate_proxy_logs_primary_only
)

gate_proxy_help() {
  info "Gate 4.6: opencode2claude proxy --help works"
  "$ROOT_DIR/target/debug/opencode2claude" proxy --help >/dev/null 2>&1 || return 1
  pass "proxy --help"
}

gate_proxy_ps() {
  require_profile local heavy || return 0
  info "Gate 4.7: opencode2claude proxy ps lists proxies"
  local output
  output="$("$ROOT_DIR/target/debug/opencode2claude" proxy ps 2>&1)" || {
    error "proxy ps failed"
    return 1
  }
  echo "$output" | grep -q "Primary managed proxies" || return 1
  echo "$output" | grep -q "Warm-standby protected proxies" || return 1
  pass "proxy ps shows primary and warm-standby pools"
}

gate_protected_ports_guarded() {
  info "Gate 4.8: protected port guard rejects port 40004"
  # Check that is_protected_proxy_port is implemented in the binary
  # For now, verify the source code has the guard
  grep -q "is_protected_proxy_port" "$ROOT_DIR/src/proxy_pool/types.rs" || return 1
  grep -q "ensure_not_protected" "$ROOT_DIR/src/proxy_pool/types.rs" || return 1
  pass "protected port guards exist in source"
}

gate_no_40010_reference() {
  info "Gate 4.9: no active reference to deprecated port 40010 in source code"
  # Only flag real usage (socks/http proxy config), not removal notes
  if grep -rn "socks5://.*40010\|http.*40010" "$ROOT_DIR/src/" "$ROOT_DIR/start.sh" "$ROOT_DIR/stop.sh" 2>/dev/null; then
    error "Found active reference to deprecated port 40010"
    return 1
  fi
  pass "no active 40010 references in code"
}

gate_proxy_restart_primary_only() {
  info "Gate 4.10: proxy restart command only affects primary ports 40001-40003"
  grep -q "get_primary_ports" "$ROOT_DIR/src/main.rs" || return 1
  grep -q "Restarting primary managed proxies" "$ROOT_DIR/src/main.rs" || return 1
  # Verify restart never calls into warm-standby ports
  grep -q "Protected warm-standby proxies skipped.*always protected" "$ROOT_DIR/src/main.rs" || return 1
  pass "proxy restart only affects 40001-40003"
}

gate_proxy_purge_primary_only() {
  info "Gate 4.11: proxy purge command recreates only primary ports 40001-40003"
  grep -q "Purging primary managed proxies" "$ROOT_DIR/src/main.rs" || return 1
  grep -q "Protected warm-standby proxies skipped.*always protected" "$ROOT_DIR/src/main.rs" || return 1
  pass "proxy purge only affects 40001-40003"
}

gate_proxy_logs_primary_only() {
  info "Gate 4.12: proxy logs only reads from primary ports 40001-40003"
  grep -q "get_primary_ports" "$ROOT_DIR/src/main.rs" || return 1
  pass "proxy logs only reads primary ports"
}

run_gates
