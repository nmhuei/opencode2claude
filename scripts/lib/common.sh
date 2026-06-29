#!/usr/bin/env bash
set -Eeuo pipefail

# ── Logging ──
info()  { printf "[INFO]  %s %s\n" "$(date '+%H:%M:%S')" "$*"; }
pass()  { printf "[PASS]  \342\234\223 %s\n" "$*"; }
error() { printf "[ERROR] \342\234\227 %s\n" "$*"; }
warn()  { printf "[WARN]  \342\232\240 %s\n" "$*"; }
skip()  { printf "[SKIP]  %s\n" "$*"; }
phase() { printf "\n[PHASE] %s\n%s\n" "$*" "----------------------------------------"; }

# ── Cleanup stack ──
_CLEANUP_HANDLERS=()

register_cleanup() {
  local handler="$1"
  _CLEANUP_HANDLERS+=("$handler")
  trap '_run_cleanup' EXIT
}

_run_cleanup() {
  local exit_code=$?
  set +e
  for (( idx=${#_CLEANUP_HANDLERS[@]}-1; idx>=0; idx-- )); do
    eval "${_CLEANUP_HANDLERS[$idx]}" 2>/dev/null || true
  done
  set -e
  exit "$exit_code"
}

# ── Profile check ──
require_profile() {
  local profile="${PROFILE:-local}"
  for allowed in "$@"; do
    [[ "$profile" == "$allowed" ]] && return 0
  done
  skip "Skipped (profile=$profile, requires: $*)"
  return 1
}
