# Contributor testing guide

The repository uses explicit test tiers. A passing compile alone is not release evidence.

## Tier A — deterministic per commit

Run:

```bash
scripts/tier-a.sh
```

Tier A requires no Docker, WARP, or public Internet. It validates:

- feature-matrix structure;
- version and documentation consistency;
- configuration and infrastructure boundaries;
- repository secret scan and scanner self-test;
- formatting, all-target compile, and Clippy with warnings denied;
- all unit and deterministic integration tests;
- fake upstream protocol conformance;
- deterministic parser fuzz-smoke corpus;
- binary build;
- hermetic CLI lifecycle E2E.

A test that needs public Internet does not belong in Tier A.

## Tier B — protected release/security CI

Run:

```bash
scripts/tier-b.sh
```

Tier B includes Tier A unless `SKIP_TIER_A=1` is set, then adds:

- ShellCheck for Bash and POSIX shell scripts;
- `cargo audit`;
- `cargo deny check`;
- locked release build;
- disposable install/checksum/uninstall E2E;
- release-mode protocol and parser tests.

Required host tools are `shellcheck`, `cargo-audit`, and `cargo-deny`. Missing tools are a gate failure, not a skipped pass.

## Tier C — real system and scheduled CI

Run on a dedicated Linux runner with Docker, Internet, and WARP SOCKS endpoints on ports `40001-40003`:

```bash
scripts/tier-c.sh
```

Tier C validates:

- real exit-identity consensus;
- duplicate public-exit suppression;
- release build;
- bounded lifecycle/RSS soak;
- graceful shutdown;
- optional external DuckDuckGo canary.

Configure duration:

```bash
SOAK_SECONDS=3600 scripts/tier-c.sh
```

The short default smoke is not equivalent to the 24-hour release-candidate soak. Long-soak evidence must be archived separately.

## Test ownership

- `tests/fast.rs` — production-router dashboard and HTTP contract tests.
- `tests/integration.rs` — black-box process and API behavior.
- `tests/protocol_conformance.rs` — controlled sync/SSE upstream fixtures.
- `tests/parser_fuzz_smoke.rs` — deterministic malformed parser corpus.
- `tests/egress_identity_system.rs` — explicit Tier C real-WARP test.
- `tests/cli_e2e.sh` — hermetic CLI output and lifecycle contract.
- `tests/install_e2e.sh` — checksum/install/uninstall transaction.
- `tests/soak_smoke.sh` — bounded release-binary health and RSS smoke.

The only ignored Rust test is the explicitly classified real-WARP system test. It is invoked by Tier C with `--ignored`; it is not silently counted as Tier A coverage.

## Adding tests

Every bug fix must add a regression test. Prefer these layers in order:

1. pure unit test for state or parsing behavior;
2. fake adapter or local HTTP/SOCKS fixture;
3. production router test;
4. black-box process test;
5. real external system test only when the dependency cannot be represented locally.

Tests must not use blind retries to hide flakiness. Timeouts should bound external waits and failure output should identify the unmet condition.

## Evidence

Store durable completion evidence under:

```text
verification/completion/phase-XX/
verification/completion/final/
```

Evidence should include the exact command, commit, pass/fail result, environment prerequisites, and unresolved limitations. CI logs supplement but do not replace checked-in contract evidence.

## Local command subset

For focused development:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test protocol_conformance
cargo test --test parser_fuzz_smoke
bash tests/cli_e2e.sh debug
bash tests/install_e2e.sh ./target/release/opencode2api
```

Before a commit, run Tier A. Before release promotion, require Tier B and the latest Tier C result for the same release candidate commit.
