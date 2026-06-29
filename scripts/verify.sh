#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
export ROOT_DIR
export PROFILE="${PROFILE:-local}"
export RUNTIME_DIR="${RUNTIME_DIR:-$ROOT_DIR/.runtime}"
export VERIFY_LOG_DIR="${VERIFY_LOG_DIR:-$RUNTIME_DIR/verify}"
mkdir -p "$VERIFY_LOG_DIR"

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || { echo "Missing common.sh"; exit 1; }
source "$ROOT_DIR/scripts/lib/report.sh" 2>/dev/null || { echo "Missing report.sh"; exit 1; }

# ── Phase registry ──
PHASE_ORDER=(
  phase-1 phase-2 phase-3 phase-4
  phase-5 phase-6 phase-7 phase-8
)

phase_script_for() {
  case "$1" in
    phase-1) echo "$ROOT_DIR/scripts/phases/phase-1-cli-skeleton.sh"   ;;
    phase-2) echo "$ROOT_DIR/scripts/phases/phase-2-runtime-pid.sh"    ;;
    phase-3) echo "$ROOT_DIR/scripts/phases/phase-3-security.sh"       ;;
    phase-4) echo "$ROOT_DIR/scripts/phases/phase-4-proxy-cli.sh"      ;;
    phase-5) echo "$ROOT_DIR/scripts/phases/phase-5-proxy-pool-fix.sh" ;;
    phase-6) echo "$ROOT_DIR/scripts/phases/phase-6-health-status-log.sh" ;;
    phase-7) echo "$ROOT_DIR/scripts/phases/phase-7-docs-migration.sh" ;;
    phase-8) echo "$ROOT_DIR/scripts/phases/phase-8-ci-release.sh"     ;;
    *) return 1 ;;
  esac
}

# ── Arg parser ──
PHASE="${1:-all}"
shift || true
FROM_GATE=""; ONLY_GATE=""; LIST_GATES=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --from) FROM_GATE="$2"; shift 2 ;;
    --only) ONLY_GATE="$2"; shift 2 ;;
    --list-gates) LIST_GATES=1; shift ;;
    *) echo "Unknown: $1"; exit 2 ;;
  esac
done
export PROFILE FROM_GATE ONLY_GATE LIST_GATES PHASE_ORDER

# ── Phase dispatch ──
if [[ "$PHASE" == "all" ]]; then
  for p in "${PHASE_ORDER[@]}"; do
    script="$(phase_script_for "$p")" || {
      error "Unknown phase: $p"; exit 1
    }
    info "--- Running $p: $(basename "$script") ---"
    PROFILE="$PROFILE" FROM_GATE="$FROM_GATE" ONLY_GATE="$ONLY_GATE" LIST_GATES="$LIST_GATES" \
      "$script" || { error "Phase failed: $p"; exit 1; }
  done
  summary_pass "All phases passed"
elif script=$(phase_script_for "$PHASE"); then
  exec "$script"
else
  echo "Usage: $0 {all|phase-N} [--profile ci|local|heavy] [--from GATE] [--only GATE] [--list-gates]"
  exit 2
fi
