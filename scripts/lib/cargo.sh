#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

gate_format_check() {
  info "Gate: cargo fmt --check"
  cd "$ROOT_DIR"
  cargo fmt --check || {
    error "Formatting check failed — run 'cargo fmt' to fix"
    return 1
  }
  pass "cargo fmt --check"
}

gate_clippy_clean() {
  info "Gate: cargo clippy -- -D warnings"
  cd "$ROOT_DIR"
  cargo clippy -- -D warnings || {
    error "Clippy found issues"
    return 1
  }
  pass "cargo clippy clean"
}

gate_compile_check() {
  info "Gate: cargo check --locked --all-targets"
  cd "$ROOT_DIR"
  cargo check --locked --all-targets || {
    error "Compilation check failed"
    return 1
  }
  pass "cargo check --locked --all-targets"
}

gate_unit_tests() {
  info "Gate: cargo test --locked"
  cd "$ROOT_DIR"
  if [[ "${PROFILE:-local}" == "heavy" ]]; then
    # Heavy profile: include ignored tests (live network, slow integration)
    cargo test --locked -- --include-ignored || {
      error "Unit tests (all, including ignored) failed"
      return 1
    }
  else
    # CI/local profile: skip #[ignore] tests (fast, no network)
    cargo test --locked || {
      error "Unit tests failed"
      return 1
    }
  fi
  pass "cargo test --locked"
}

gate_binary_build() {
  info "Gate: cargo build --locked"
  cd "$ROOT_DIR"
  cargo build --locked || {
    error "Binary build failed"
    return 1
  }
  pass "cargo build --locked"
}
