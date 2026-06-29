# Phase 3: Security Hardening

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-3-security` |
| **Status** | Implementation Complete |
| **Dependencies** | Phase 1 (CLI skeleton), Phase 2 (Runtime + PID) |
| **Scope** | Default shell policy `disabled`. Public bind guard (`0.0.0.0` + no auth → refuse). Strict-mode guard (`0.0.0.0` + unrestricted shell → hard fail). `/health` must not leak secrets. |
| **Files modified** | `src/config.rs` (default `ShellPolicy::Disabled`, `validate_security()`, `Default` impl, 5 new tests), `src/main.rs` (call `validate_security()` before bind), `scripts/phases/phase-3-security.sh` (rewritten with real gates) |
| **Expected behavior contract** | Default `--shell-policy` is `disabled`. Starting with `BRIDGE_HOST=0.0.0.0` and no `BRIDGE_AUTH_TOKEN` exits with error. Starting with `0.0.0.0` and `unrestricted` shell exits with error even if auth is configured. `127.0.0.1` + no auth is allowed. `/health` exposes `auth_enabled` (boolean) but never `auth_tokens` or secrets. |
| **Acceptance gates** | cargo gates pass, shell default `disabled`, public bind without auth rejected, public bind + unrestricted shell rejected, public bind with auth allowed, localhost without auth allowed, /health no secrets, no 40010 reference |
| **Verification command** | `./scripts/verify.sh phase-3 --profile ci` |
| **Review requirements** | code-reviewer (MEDIUM+), security-reviewer (HIGH+) |
| **Out of scope** | Rate limiting changes, proxy auth, TLS support, Docker container security, Phase 8 CI/Release |
| **Definition of Done** | 1. All gates pass 2. Shell default is `disabled` 3. `validate_security()` rejects unsafe configs 4. `/health` does not expose secrets 5. No CRITICAL/HIGH findings 6. All enabled phases still pass |
