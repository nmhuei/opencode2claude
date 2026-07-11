# Phase 16 — CI and release artifact evidence

Implementation commit: `b9e0ea1`
Branch: `completion/full-repository-20260711`
Date: 2026-07-11

## Workflow validation

Executed:

```bash
python3 scripts/check_release_workflow.py
docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest
```

Result:

```text
release-workflow: PASS invariants=18
actionlint: PASS
```

Validated release invariants include:

- exact tag/package version gate;
- mandatory Tier B and Tier C release gates;
- Linux x86_64 and ARM64 artifacts;
- macOS x86_64 and ARM64 artifacts;
- locked release builds;
- companion `.sha256` files;
- aggregate `SHA256SUMS` verification;
- SPDX JSON SBOM generation;
- GitHub build-provenance attestation;
- clean release-artifact install smoke;
- multi-architecture container SBOM and provenance;
- keyless cosign signature;
- use of `GITHUB_TOKEN` instead of a personal GHCR token.

## Local release bundle

Executed:

```bash
scripts/release_smoke.sh ./target/release/opencode2api
```

Result:

```text
opencode2api-linux-amd64: checksum OK
SPDX-2.3 SBOM: PASS
version-consistency: PASS version=0.5.0
install-e2e: PASS checksum, smoke, rejection, dry-run, uninstall
release-workflow: PASS invariants=18
release-smoke: PASS
```

The local bundle contains:

```text
opencode2api-linux-amd64
opencode2api-linux-amd64.sha256
opencode2api-linux-amd64.spdx.json
SHA256SUMS
```

GitHub publication, hosted attestation issuance, and registry signing occur only on a real `v0.5.0` tag after matrix closure. The workflow contract and local artifact transaction are verified; no release was published during this completion run.
