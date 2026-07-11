#!/usr/bin/env bash
# Per-commit deterministic gate. No Docker, WARP, or public network required.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

python3 scripts/check_feature_matrix.py
python3 scripts/check_version_consistency.py
python3 scripts/check_docs.py
python3 scripts/check_release_workflow.py
python3 scripts/check_config_boundary.py
python3 scripts/check_infrastructure_boundary.py
python3 scripts/check_secrets.py --self-test
python3 scripts/check_secrets.py
cargo fmt --all -- --check
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --bins --locked
timeout_cmd=()
if command -v timeout >/dev/null 2>&1; then
  timeout_cmd=(timeout 240)
elif command -v gtimeout >/dev/null 2>&1; then
  timeout_cmd=(gtimeout 240)
fi
"${timeout_cmd[@]}" bash tests/cli_e2e.sh debug

echo "tier-a: PASS"
