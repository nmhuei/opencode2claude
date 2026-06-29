#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

# ── Run gates ──
_GATE_PASSED=0
_GATE_FAILED=0
_GATE_SKIPPED=0
_GATE_NAMES=()

run_gates() {
  local from_gate="${FROM_GATE:-}"
  local only_gate="${ONLY_GATE:-}"
  local list_gates="${LIST_GATES:-}"

  phase "$PHASE_NAME"

  # List mode
  if [[ -n "$list_gates" ]]; then
    info "Available gates for $PHASE_ID:"
    for gate in "${GATES[@]}"; do
      printf "  - %s\n" "$gate"
    done
    exit 0
  fi

  local skip_until=""
  [[ -n "$from_gate" ]] && skip_until="$from_gate"

  for gate in "${GATES[@]}"; do
    # --from support: skip gates until match
    if [[ -n "$skip_until" ]]; then
      if [[ "$gate" == "$skip_until" ]]; then
        skip_until=""
      else
        info "Skipping $gate (--from $from_gate)"
        continue
      fi
    fi

    # --only support
    if [[ -n "$only_gate" ]] && [[ "$gate" != "$only_gate" ]]; then
      continue
    fi

    if declare -F "$gate" >/dev/null; then
      if "$gate"; then
        _GATE_PASSED=$((_GATE_PASSED + 1))
      else
        _GATE_FAILED=$((_GATE_FAILED + 1))
        error "Gate failed: $gate"
        # Fail fast — stop at first failure
        break
      fi
    else
      warn "Gate function not found: $gate"
      _GATE_SKIPPED=$((_GATE_SKIPPED + 1))
    fi
  done

  echo ""
  if [[ "$_GATE_FAILED" -gt 0 ]]; then
    summary_fail
    exit 1
  else
    summary_pass
  fi
}

summary_pass() {
  printf "\n[SUMMARY] \342\234\205 All %d gates passed for %s\n" "$_GATE_PASSED" "$PHASE_NAME"
}

summary_fail() {
  printf "\n[SUMMARY] \342\234\227 Failed: %d passed, %d failed, %d skipped for %s\n" \
    "$_GATE_PASSED" "$_GATE_FAILED" "$_GATE_SKIPPED" "$PHASE_NAME"
}
