# Verification Ecosystem — opencode2claude CLI Supervisor

## Quick Start

```bash
# Verify current phase (CI-safe)
./scripts/verify.sh phase-1 --profile ci

# Full local verification (starts bridge + runs integration)
./scripts/verify.sh phase-1 --profile local

# List gates available for a phase
./scripts/verify.sh phase-1 --list-gates

# Debug a specific gate
./scripts/verify.sh phase-1 --only gate_cli_smoke --profile local

# Resume from a failed gate
./scripts/verify.sh phase-1 --from gate_cli_smoke --profile local
```

## Profiles

| Profile | Scope | Use Case |
|---------|-------|----------|
| `ci` | fmt, clippy, check, unit tests, build, --help | CI / pre-commit sanity |
| `local` | ci + bridge serve + health + integration tests | Development verification |
| `heavy` | local + Docker WARP e2e tests | Full system verification |

## Phase Lifecycle

1. **DESIGN** — Read `verification/phases/phase-N-name.md` contract
2. **CODE** — Implement phase scope (no bleeding)
3. **VERIFY** — `./scripts/verify.sh phase-N --profile local`
4. **REVIEW** — Run advisory agent reviews (code-reviewer, security-reviewer, etc.)
5. **FINAL VERIFY** — Full verification from gate 1
6. **COMMIT** — Only if verification passes + no CRITICAL/HIGH findings

## Adding a New Phase

1. Create `verification/phases/phase-N-name.md` with full contract
2. Create `scripts/phases/phase-N-name.sh` with gates
3. Set `PHASE_ENABLED=1` when ready
4. Register in `scripts/verify.sh` phase_script_for() and PHASE_ORDER
5. Verify: `./scripts/verify.sh phase-N --profile local`

## Architecture

```
scripts/
├── verify.sh              # Entrypoint — phase selector + arg parser
├── lib/
│   ├── common.sh          # Logging, cleanup stack, profile checks
│   ├── cargo.sh           # Cargo gates (fmt, clippy, check, test, build)
│   ├── process.sh         # Port, HTTP, PID helpers
│   └── report.sh          # run_gates, summary pass/fail
└── phases/
    ├── phase-1-cli-skeleton.sh
    ├── phase-2-runtime-pid.sh
    └── ... (8 phase scripts)
```

## Rules

- Every gate is a bash function returning 0 (pass) or 1 (fail)
- `--from GATE` is for debug only — always re-run full phase before commit
- Agent review is advisory — gates are the source of truth
- CRITICAL/HIGH findings must be fixed before commit
