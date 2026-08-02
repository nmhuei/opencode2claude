# Release checklist

A release tag is permitted only from a clean release-candidate commit with Tier A, Tier B, and Tier C evidence.

## Source and contracts

- [ ] Working tree is clean.
- [ ] `scripts/check_feature_matrix.py` passes.
- [ ] `REQUIRE_VERIFIED=1 scripts/check_feature_matrix.py` passes for the release candidate.
- [ ] `scripts/check_version_consistency.py --binary ./target/release/opencode2api --tag "$TAG"` passes.
- [ ] `scripts/check_docs.py` passes.
- [ ] No production `todo!()`, `unimplemented!()`, or unreviewed `unsafe` remains.

## Test tiers

- [ ] Tier A passes on Linux.
- [ ] Tier B passes on Linux.
- [ ] Tier C passes on the dedicated Linux WARP runner for the same commit.
- [ ] Required long soak completed and evidence archived.
- [ ] No blind flaky-test rerun was accepted as evidence.

## Security

- [ ] Secret scanner and self-test pass.
- [ ] ShellCheck passes.
- [ ] `cargo audit` has no unresolved vulnerability or unmaintained blocker.
- [ ] `cargo deny check` passes advisories, bans, licenses, and sources.
- [ ] Parser fuzz-smoke passes in release mode.
- [ ] Public-bind, CSRF, redaction, fail-closed egress, and updater regression tests pass.

## Versioning and changelog

- [ ] Cargo package and lockfile versions match.
- [ ] Latest changelog heading matches the package version.
- [ ] Tag is exactly `v<package-version>`.
- [ ] Changelog describes security, compatibility, config, and migration changes.
- [ ] Release notes do not claim unsupported platforms or features.

## Artifacts

- [ ] Linux x86_64 binary built with `--locked`.
- [ ] Linux ARM64 binary built with `--locked`.
- [ ] Every binary has a companion `.sha256` file.
- [ ] Aggregate `SHA256SUMS` exists.
- [ ] SPDX JSON SBOM exists for each binary.
- [ ] GitHub build provenance attestation covers release artifacts.
- [ ] Linux release binary passes disposable install and uninstall smoke.
- [ ] Published container has registry SBOM/provenance and a keyless signature.

## Runtime smoke

- [ ] Installed release reports the expected version.
- [ ] Direct-mode foreground start reaches liveness and readiness.
- [ ] Graceful `SIGTERM` exits within configured deadline.
- [ ] Proxy mode remains fail closed without an eligible route.
- [ ] Real WARP identity and duplicate suppression evidence is current.
- [ ] Update check works against the release metadata.
- [ ] Rollback procedure has a verified prior binary and config backup.

## Documentation

- [ ] README reflects actual CLI and routes.
- [ ] Configuration guide includes every variable from `.env.example`.
- [ ] CLI help and `docs/cli.md` agree.
- [ ] Compatibility, management API, security, deployment, observability, troubleshooting, upgrade, and testing guides are reviewed.
- [ ] OpenAPI runtime document validates in tests.

## Promotion

- [ ] Release workflow system gate passed.
- [ ] Build artifacts were uploaded only after all prerequisite jobs passed.
- [ ] GitHub Release checksums and attestations are visible.
- [ ] Container digest and signature are recorded in release notes.
- [ ] Final evidence bundle identifies the exact commit and remaining non-blocking risks.
