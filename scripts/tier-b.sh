#!/usr/bin/env bash
# Protected CI gate: supply-chain, shell, release build, disposable install.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${SKIP_TIER_A:-0}" != "1" ]]; then
  scripts/tier-a.sh
fi

run_shellcheck() {
  local shell="$1"
  shift
  if command -v shellcheck >/dev/null 2>&1; then
    shellcheck --shell="$shell" "$@"
  elif command -v docker >/dev/null 2>&1; then
    docker run --rm -v "$ROOT_DIR:/mnt" -w /mnt \
      koalaman/shellcheck:stable --shell="$shell" "$@"
  else
    echo "tier-b: shellcheck or Docker is required" >&2
    exit 2
  fi
}

command -v cargo-audit >/dev/null 2>&1 || {
  echo "tier-b: cargo-audit is required" >&2
  exit 2
}
command -v cargo-deny >/dev/null 2>&1 || {
  echo "tier-b: cargo-deny is required" >&2
  exit 2
}

run_shellcheck bash \
  scripts/*.sh scripts/lib/*.sh scripts/phases/*.sh \
  tests/*.sh install-local.sh uninstall-local.sh
run_shellcheck sh install.sh
cargo audit
cargo deny check
cargo build --release --locked --bins
bash tests/install_e2e.sh ./target/release/opencode2api
cargo test --release --test protocol_conformance --test parser_fuzz_smoke

echo "tier-b: PASS"
