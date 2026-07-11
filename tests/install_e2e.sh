#!/usr/bin/env bash
# Disposable installer/checksum/uninstaller contract test.
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$ROOT_DIR/target/debug/opencode2api}"
TEST_ROOT="$(mktemp -d -t opencode2api-install-e2e.XXXXXX)"
FIXTURE_DIR="$TEST_ROOT/fixture"
INSTALL_DIR="$TEST_ROOT/bin"
ASSET="$FIXTURE_DIR/opencode2api-linux-amd64"
CHECKSUM="$ASSET.sha256"

cleanup() { rm -rf "$TEST_ROOT"; }
trap cleanup EXIT

[[ -x "$BIN" ]] || { echo "missing executable fixture: $BIN" >&2; exit 2; }
mkdir -p "$FIXTURE_DIR" "$INSTALL_DIR"
cp "$BIN" "$ASSET"
chmod +x "$ASSET"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$ASSET" > "$CHECKSUM"
else
  shasum -a 256 "$ASSET" > "$CHECKSUM"
fi

export OPENCODE2API_BINDIR="$INSTALL_DIR"
export OPENCODE2API_DOWNLOAD_URL="file://$ASSET"
export OPENCODE2API_CHECKSUM_URL="file://$CHECKSUM"
export PATH="$INSTALL_DIR:$PATH"

sh "$ROOT_DIR/install.sh" </dev/null >"$TEST_ROOT/install.log" 2>&1
[[ -x "$INSTALL_DIR/opencode2api" ]]
"$INSTALL_DIR/opencode2api" --version | grep -q 'opencode2api'

echo "0$(cut -c2- "$CHECKSUM")" > "$CHECKSUM"
rm -f "$INSTALL_DIR/opencode2api"
if sh "$ROOT_DIR/install.sh" </dev/null >"$TEST_ROOT/bad-checksum.log" 2>&1; then
  echo "installer accepted invalid checksum" >&2
  exit 1
fi
[[ ! -e "$INSTALL_DIR/opencode2api" ]]
grep -q 'SHA-256 verification failed' "$TEST_ROOT/bad-checksum.log"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$ASSET" > "$CHECKSUM"
else
  shasum -a 256 "$ASSET" > "$CHECKSUM"
fi
sh "$ROOT_DIR/install.sh" </dev/null >/dev/null 2>&1
[[ -x "$INSTALL_DIR/opencode2api" ]]

export OPENCODE2API_UNINSTALL_DIRS="$INSTALL_DIR"
export OPENCODE2API_SKIP_BUILD_ARTIFACTS=true
bash "$ROOT_DIR/uninstall-local.sh" --dry-run >"$TEST_ROOT/uninstall-dry.log"
grep -q 'would remove' "$TEST_ROOT/uninstall-dry.log"
[[ -x "$INSTALL_DIR/opencode2api" ]]
bash "$ROOT_DIR/uninstall-local.sh" >"$TEST_ROOT/uninstall.log"
[[ ! -e "$INSTALL_DIR/opencode2api" ]]

echo "install-e2e: PASS checksum, smoke, rejection, dry-run, uninstall"
