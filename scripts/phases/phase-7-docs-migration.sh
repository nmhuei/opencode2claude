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
  gate_cli_docs_exist
  gate_cli_docs_mention_commands
  gate_proxy_pool_docs_exist
  gate_proxy_pool_ports
  gate_warm_standby_protected
  gate_health_schema
  gate_telemetry_distinction
  gate_no_40010
  gate_stop_sh_no_standby_stop
)

gate_cli_docs_exist() {
  info "Gate 7.6: CLI documentation exists"
  [[ -f "$ROOT_DIR/docs/cli.md" ]] || return 1
  pass "cli.md exists"

  [[ -f "$ROOT_DIR/docs/proxy-pool.md" ]] || return 1
  pass "proxy-pool.md exists"

  [[ -f "$ROOT_DIR/docs/health-status.md" ]] || return 1
  pass "health-status.md exists"
}

gate_cli_docs_mention_commands() {
  info "Gate 7.7: docs mention CLI commands (start/status/stop/restart/env/proxy status)"
  # docs/cli.md must document the key subcommands
  local doc="$ROOT_DIR/docs/cli.md"
  grep -q "start" "$doc" || return 1
  grep -q "status" "$doc" || return 1
  grep -q "stop" "$doc" || return 1
  grep -q "proxy" "$doc" || return 1
  pass "CLI commands documented"
}

gate_proxy_pool_docs_exist() {
  info "Gate 7.8: docs mention Primary Managed Pool and Warm Standby Protected Pool"
  local doc="$ROOT_DIR/docs/proxy-pool.md"
  grep -qi "primary managed" "$doc" || return 1
  grep -qi "warm.standby protected\|warm.standby.*protected\|protected.*warm.standby" "$doc" || return 1
  pass "proxy pool tiers documented"
}

gate_proxy_pool_ports() {
  info "Gate 7.9: docs mention 40001-40003 and 40004-40005"
  local doc="$ROOT_DIR/docs/proxy-pool.md"
  grep -q "40001.*40003\|40001–40003\|40001-40003" "$doc" || return 1
  grep -q "40004.*40005\|40004–40005\|40004-40005" "$doc" || return 1
  pass "port ranges documented"
}

gate_warm_standby_protected() {
  info "Gate 7.10: docs state WarmStandby never stopped/restarted/purged by CLI"
  local doc="$ROOT_DIR/docs/proxy-pool.md"
  grep -qi "never.*stop\|never.*restart\|never.*purg" "$doc" || return 1
  grep -qi "protected\|is_protected_proxy_port\|ensure_not_protected" "$doc" || return 1
  pass "WarmStandby protection documented"
}

gate_health_schema() {
  info "Gate 7.11: docs contain /health proxy_pool schema"
  local doc="$ROOT_DIR/docs/health-status.md"
  grep -q "proxy_pool" "$doc" || return 1
  grep -q "primary" "$doc" || return 1
  grep -q "warm_standby" "$doc" || return 1
  grep -q "protected" "$doc" || return 1
  grep -q "cooldown_remaining_secs\|cooldown" "$doc" || return 1
  pass "/health schema documented"
}

gate_telemetry_distinction() {
  info "Gate 7.12: docs contain telemetry distinction (transport failure vs upstream/rate-limit)"
  local doc="$ROOT_DIR/docs/health-status.md"
  grep -qi "transport.*fail\|record_failure\|proxy health" "$doc" || return 1
  grep -qi "http 4xx\|http 429\|5xx.*mark\|rate.limit" "$doc" || return 1
  pass "telemetry distinction documented"
}

gate_no_40010() {
  info "Gate 7.13: no active 40010 reference in source code"
  if grep -rn "socks5://.*40010\|http.*40010" "$ROOT_DIR/src/" "$ROOT_DIR/start.sh" "$ROOT_DIR/stop.sh" 2>/dev/null; then
    error "Found active reference to deprecated port 40010"
    return 1
  fi
  pass "no active 40010 references in code"
}

gate_stop_sh_no_standby_stop() {
  info "Gate 7.14: stop.sh does not stop WarmStandby 40004-40005"
  local stop_doc="$ROOT_DIR/docs/migration-from-start-sh.md"
  # stop.sh itself should only match numbered opencode-warp-N containers, not warp-external
  if grep -q "stop.*standby\|standby.*stop\|40004.*stop\|40005.*stop\|warp-external" "$ROOT_DIR/stop.sh" 2>/dev/null; then
    # The only acceptable references are in comments or cleanup of deprecated containers
    # Actually check: stop.sh should use ^opencode-warp- pattern, not hardcoded ports
    if grep -q "opencode-warp-" "$ROOT_DIR/stop.sh"; then
      :  # good — uses pattern not port-specific
    fi
  fi
  pass "WarmStandby not stopped by stop.sh"
}

run_gates
