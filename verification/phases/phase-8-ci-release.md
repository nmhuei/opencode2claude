# Phase 8: CI + Release

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-8-ci-release` |
| **Status** | Implementation Complete |
| **Dependencies** | Phase 1–7 (all features complete), Phase 3 (security hardening) |
| **Scope** | CI workflow polish, release workflow with --locked builds, Dockerfile --locked, linux-arm64 target, CHANGELOG.md, version consistency, verification gates |
| **Files modified** | `.github/workflows/ci.yml` (removed stale comment), `.github/workflows/release.yml` (added `--locked`, added linux-arm64), `Dockerfile` (added `--locked`), `scripts/phases/phase-8-ci-release.sh` (real gates, enabled), `verification/phases/phase-8-ci-release.md` (this file) |
| **Files created** | `CHANGELOG.md` |
| **Expected behavior contract** | CI calls `verify.sh all --profile ci` as source of truth. Release workflow uses `--locked` for reproducible builds. Release produces binaries for linux-amd64, linux-arm64, macos-amd64, macos-arm64. Dockerfile uses `--locked`. Version consistent across Cargo.toml and CHANGELOG.md. install.sh present. No 40010 references. |
| **Acceptance gates** | cargo gates pass, CI workflow exists, release workflow exists, CI calls verify.sh, release build uses --locked, Dockerfile uses --locked, CHANGELOG.md exists and lists current version, version consistent, no 40010, install.sh present, all enabled phases pass |
| **Verification command** | `./scripts/verify.sh phase-8 --profile ci` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Adding new CI platforms, migrating CI provider, TLS certificates, code signing |
| **Definition of Done** | 1. All gates pass 2. CI calls verify.sh 3. Release build uses --locked 4. CHANGELOG.md lists current version 5. Version consistent 6. All enabled phases pass 7. No CRITICAL/HIGH findings |
