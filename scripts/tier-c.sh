#!/usr/bin/env bash
# Scheduled/system gate. Requires Docker, Internet, and WARP SOCKS proxies.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

command -v docker >/dev/null 2>&1 || { echo "tier-c: docker required" >&2; exit 2; }
for port in 40001 40002 40003; do
  if ! (echo >/dev/tcp/127.0.0.1/$port) >/dev/null 2>&1; then
    echo "tier-c: WARP SOCKS proxy missing on 127.0.0.1:$port" >&2
    exit 2
  fi
done

cargo test --test egress_identity_system -- --ignored --nocapture
cargo build --release --locked --bins
SOAK_SECONDS="${SOAK_SECONDS:-30}" bash tests/soak_smoke.sh ./target/release/opencode2api

if [[ "${RUN_EXTERNAL_SEARCH_CANARY:-0}" == "1" ]]; then
  canary_body="$(mktemp "${TMPDIR:-/tmp}/opencode2api-search-canary.XXXXXX")"
  canary_code="$(curl -sS -L --max-time 20 \
    -A 'Mozilla/5.0 opencode2api-system-canary' \
    -o "$canary_body" -w '%{http_code}' \
    'https://html.duckduckgo.com/html/?q=rust+programming')"
  canary_bytes="$(wc -c < "$canary_body")"
  if [[ "$canary_code" != 2* ]] || (( canary_bytes < 1000 )) || ! grep -qi 'DuckDuckGo' "$canary_body"; then
    rm -f "$canary_body"
    echo "tier-c: external DuckDuckGo canary failed code=$canary_code bytes=$canary_bytes" >&2
    exit 1
  fi
  rm -f "$canary_body"
  echo "tier-c: external DuckDuckGo canary PASS code=$canary_code bytes=$canary_bytes"
else
  echo "tier-c: external canary skipped (set RUN_EXTERNAL_SEARCH_CANARY=1)"
fi

echo "tier-c: PASS"
