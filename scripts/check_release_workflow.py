#!/usr/bin/env python3
"""Validate release workflow invariants that must not drift silently."""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github/workflows/release.yml"

REQUIRED_SNIPPETS = {
    "release tag version gate": "scripts/check_version_consistency.py --tag",
    "matrix closure gate": "REQUIRE_VERIFIED",
    "Tier B gate": "scripts/tier-b.sh",
    "Tier C gate": "scripts/tier-c.sh",
    "Linux AMD64 artifact": "opencode2api-linux-amd64",
    "Linux ARM64 artifact": "opencode2api-linux-arm64",
    "native Linux version smoke": "if: matrix.target == 'x86_64-unknown-linux-gnu'",
    "per-file checksum": ".sha256",
    "aggregate checksums": "SHA256SUMS",
    "SPDX SBOM": "anchore/sbom-action@v0",
    "build provenance": "actions/attest-build-provenance@v2",
    "local release bundle gate": "scripts/release_smoke.sh",
    "clean install smoke": "tests/install_e2e.sh",
    "container SBOM": "sbom: true",
    "container provenance": "provenance: mode=max",
    "keyless signature": "cosign sign --yes",
    "GitHub token login": "secrets.GITHUB_TOKEN",
}
FORBIDDEN = {
    "personal access token for GHCR": "secrets.CR_PAT",
    "unlocked release build": "cargo build --release --target",
    "lockfile regeneration before publish": "cargo generate-lockfile",
    "macOS runner": "macos-latest",
    "macOS release target": "apple-darwin",
    "macOS release artifact": "opencode2api-macos-",
}


def main() -> int:
    try:
        text = WORKFLOW.read_text(encoding="utf-8")
    except OSError as exc:
        print(f"release-workflow: ERROR: {exc}", file=sys.stderr)
        return 1

    errors = [f"missing {name}: {needle}" for name, needle in REQUIRED_SNIPPETS.items() if needle not in text]
    errors += [f"forbidden {name}: {needle}" for name, needle in FORBIDDEN.items() if needle in text]
    if "needs: [release-gates, system-gate]" not in text:
        errors.append("release build must depend on both release-gates and system-gate")
    if errors:
        print("release-workflow: FAILED", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"release-workflow: PASS invariants={len(REQUIRED_SNIPPETS)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
