# Phase 8: CI + Release

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-8-ci-release` |
| **Status** | Planned |
| **Dependencies** | Phase 7 (Docs + Migration) |
| **Scope** | Update CI to call `verify.sh all --profile ci`. Add release workflow (cross-platform binaries, crates.io publish, Docker image). Add `cargo deny check` for licenses. Remove duplicate cargo steps from CI (verify.sh is source of truth). |
| **Files to modify** | `.github/workflows/ci.yml` (simplified to verify.sh + release build), `.github/workflows/release.yml` (if exists, verify.sh in CI profile) |
| **Expected behavior contract** | CI runs `verify.sh all --profile ci` as single verification step. Release build is separate step. Push to main triggers full CI. Tagged release builds binaries and pushes to crates.io/ghcr.io. |
| **Acceptance gates** | cargo gates pass, release build succeeds, CI calls verify.sh, no duplicate cargo test steps |
| **Verification command** | `./scripts/verify.sh phase-8 --profile ci` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Adding new CI platforms, migrating to different CI provider |
| **Definition of Done** | 1. All gates pass 2. CI runs verify.sh 3. Release build works 4. No duplicate checks in CI 5. No CRITICAL/HIGH findings |
