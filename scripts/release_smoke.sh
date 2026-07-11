#!/usr/bin/env bash
# Build and validate a local Linux release artifact bundle without publishing it.
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

BIN="${1:-./target/release/opencode2api}"
OUT_DIR="$ROOT_DIR/target/release-smoke"
ARTIFACT_NAME="opencode2api-linux-amd64"
ARTIFACT="$OUT_DIR/$ARTIFACT_NAME"
SBOM="$ARTIFACT.spdx.json"

[[ -x "$BIN" ]] || { echo "release-smoke: executable binary required: $BIN" >&2; exit 2; }
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
cp "$BIN" "$ARTIFACT"
chmod +x "$ARTIFACT"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$OUT_DIR" && sha256sum "$ARTIFACT_NAME" > "$ARTIFACT_NAME.sha256")
else
  (cd "$OUT_DIR" && shasum -a 256 "$ARTIFACT_NAME" > "$ARTIFACT_NAME.sha256")
fi
cp "$ARTIFACT.sha256" "$OUT_DIR/SHA256SUMS"
(
  cd "$OUT_DIR"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum --check SHA256SUMS
  else
    shasum -a 256 --check SHA256SUMS
  fi
)

if command -v syft >/dev/null 2>&1; then
  syft scan "file:$ARTIFACT" -q -o "spdx-json=$SBOM"
elif command -v docker >/dev/null 2>&1; then
  docker run --rm \
    -v "$ROOT_DIR:/work" -w /work \
    anchore/syft:latest scan "file:/work/target/release-smoke/$ARTIFACT_NAME" \
    -q -o "spdx-json=/work/target/release-smoke/$ARTIFACT_NAME.spdx.json"
else
  echo "release-smoke: syft or Docker is required for SBOM generation" >&2
  exit 2
fi

python3 - "$SBOM" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
data = json.loads(path.read_text(encoding="utf-8"))
assert data.get("spdxVersion") == "SPDX-2.3", data.get("spdxVersion")
assert data.get("name")
assert isinstance(data.get("packages", []), list)
print(f"release-smoke: SPDX PASS packages={len(data.get('packages', []))}")
PY

python3 scripts/check_version_consistency.py --binary "$ARTIFACT"
bash tests/install_e2e.sh "$ARTIFACT"
python3 scripts/check_release_workflow.py

echo "release-smoke: PASS artifact=$ARTIFACT_NAME"
