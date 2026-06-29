# Phase 3: Security Hardening

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-3-security` |
| **Status** | Planned |
| **Dependencies** | Phase 2 (Runtime + PID) |
| **Scope** | Change default shell policy to `disabled`. Add public-bind guard (`BRIDGE_HOST=0.0.0.0` + no auth → refuse start). Add strict-mode guard (`0.0.0.0` + unrestricted shell → hard fail). Audit for hardcoded secrets. |
| **Files to create** | None |
| **Files to modify** | `src/config.rs` (default ShellPolicy::Disabled), `src/main.rs` (startup guard checks), `.github/workflows/ci.yml` (shellcheck hard gate) |
| **Expected behavior contract** | Default `--shell-policy` is `disabled`. Starting with `BRIDGE_HOST=0.0.0.0` and no `BRIDGE_AUTH_TOKEN` exits with error. Starting with `0.0.0.0` and `unrestricted` shell exits with error. `127.0.0.1` + no auth is allowed. |
| **Acceptance gates** | cargo gates pass, shell default `disabled`, public bind guard enforces auth, no secrets in source |
| **Verification command** | `./scripts/verify.sh phase-3 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+), security-reviewer (HIGH+) |
| **Out of scope** | Rate limiting changes, proxy auth, TLS support |
| **Definition of Done** | 1. All gates pass 2. Shell default is `disabled` 3. Public bind guard works 4. `cargo audit` passes 5. No CRITICAL/HIGH findings |
