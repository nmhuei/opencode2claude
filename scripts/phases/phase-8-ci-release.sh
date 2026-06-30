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

PHASE_ID="phase-8"
PHASE_NAME="CI + Release"
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
  gate_ci_workflow_exists
  gate_release_workflow_exists
  gate_ci_calls_verify
  gate_release_build_locked
  gate_dockerfile_locked
  gate_changelog_exists
  gate_version_consistent
  gate_no_active_40010
  gate_install_sh_present
)

# ── Phase-specific gates ──

gate_ci_workflow_exists() {
  info "Gate 8.6: CI workflow exists"
  [[ -f "$ROOT_DIR/.github/workflows/ci.yml" ]] || return 1
  pass "ci.yml exists"
}

gate_release_workflow_exists() {
  info "Gate 8.7: Release workflow exists"
  [[ -f "$ROOT_DIR/.github/workflows/release.yml" ]] || return 1
  pass "release.yml exists"
}

gate_ci_calls_verify() {
  info "Gate 8.8: CI workflow calls verify.sh"
  grep -qE '^\s+run:\s+\./scripts/verify\.sh' "$ROOT_DIR/.github/workflows/ci.yml" || return 1
  pass "CI uses verify.sh"
}

gate_release_build_locked() {
  info "Gate 8.9: Release workflow uses --locked builds"
  grep -q '\--locked' "$ROOT_DIR/.github/workflows/release.yml" || return 1
  pass "release.yml uses --locked"
}

gate_dockerfile_locked() {
  info "Gate 8.10: Dockerfile uses --locked build"
  grep -q '\--locked' "$ROOT_DIR/Dockerfile" || return 1
  pass "Dockerfile uses --locked"
}

gate_changelog_exists() {
  info "Gate 8.11: CHANGELOG.md exists"
  [[ -f "$ROOT_DIR/CHANGELOG.md" ]] || return 1
  grep -q "## \[" "$ROOT_DIR/CHANGELOG.md" || return 1
  pass "CHANGELOG.md with releases"
}

gate_version_consistent() {
  info "Gate 8.12: Version consistent across Cargo.toml and changelog"
  local cargo_version
  cargo_version=$(grep '^version =' "$ROOT_DIR/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
  grep -q "\[$cargo_version\]" "$ROOT_DIR/CHANGELOG.md" || return 1
  pass "version $cargo_version consistent"
}

gate_no_active_40010() {
  info "Gate 8.13: no active reference to deprecated port 40010 in source code"
  if grep -rn "socks5://.*40010\|http.*40010" "$ROOT_DIR/src/" "$ROOT_DIR/start.sh" "$ROOT_DIR/stop.sh" 2>/dev/null; then
    error "Found active reference to deprecated port 40010"
    return 1
  fi
  pass "no active 40010 references in code"
}

gate_install_sh_present() {
  info "Gate 8.14: install.sh exists"
  [[ -f "$ROOT_DIR/install.sh" ]] || return 1
  pass "install.sh exists"
}

run_gates
