# Phase 2: Runtime + PID

| Field | Value |
|-------|-------|
| **Phase ID** | `phase-2-runtime-pid` |
| **Status** | Planned |
| **Dependencies** | Phase 1 (CLI skeleton) |
| **Scope** | Add `src/runtime.rs` (`.runtime/` paths), `src/pidfile.rs` (JSON PID read/write). `supervisor.rs` creates `.runtime/` on `start`, writes PID file, cleans up on `stop`. |
| **Files to create** | `src/runtime.rs`, `src/pidfile.rs` |
| **Files to modify** | `src/main.rs` (add runtime setup), `src/supervisor.rs` (use runtime paths), `.runtime/` (gitignored) |
| **Expected behavior contract** | `opencode2claude start` creates `.runtime/` dir. PID file `.runtime/opencode2claude.pid.json` written with correct JSON structure. `opencode2claude stop` reads PID file and kills process. `opencode2claude status` returns running/stopped based on PID file. |
| **Acceptance gates** | cargo gates pass, CLI help works, `.runtime/` created on `start`, PID JSON valid, `status` reads PID correctly |
| **Verification command** | `./scripts/verify.sh phase-2 --profile local` |
| **Review requirements** | code-reviewer (MEDIUM+) |
| **Out of scope** | Docker container management, health endpoint customization, log rotation |
| **Definition of Done** | 1. All gates pass 2. `.runtime/` created on start 3. PID file read/write round-trip works 4. `status` reports correct state 5. No CRITICAL/HIGH findings |
