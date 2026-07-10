# Verification Evidence

## Branch

```text
refactor/architecture-overhaul-20260711
```

## Code tip validated

```text
104097b refactor(retry): classify upstream failures without penalizing healthy egress
```

Architecture documentation was added after the code gate and does not change runtime behavior.

## Static gates

The following command completed successfully:

```bash
cargo fmt --all -- --check \
  && cargo check --all-targets \
  && cargo clippy --all-targets -- -D warnings \
  && cargo test --all-targets \
  && cargo build --bins
```

Results:

```text
rustfmt: PASS
cargo check --all-targets: PASS
cargo clippy --all-targets -- -D warnings: PASS
cargo build --bins: PASS
```

## Test evidence

```text
Library/unit tests:
188 passed
0 failed
1 ignored (live DuckDuckGo network test)

Fast HTTP/dashboard regression tests:
81 passed
0 failed
0 ignored

Heavy integration test declarations:
0 executed
0 failed
18 ignored by default
```

Total executed:

```text
269 passed
0 failed
19 ignored
```

The full test suite also passed under the default parallel Cargo test harness. Serial execution is no longer required after replacing the async test environment lock and using the production router.

## Regression tests added or strengthened

```text
proxy_pool::tests::test_retry_excludes_failed_proxy_and_prefers_other_primary
proxy_pool::maintenance::restart_tests::restart_failure_preserves_attempts_and_stops_after_third_try
opencode::search::tests::test_url_decode_utf8
opencode::search::tests::test_truncate_chars_is_utf8_safe
opencode::retry::execute::tests::configured_proxy_pool_never_silently_falls_back_to_direct
opencode::retry::tests::rate_limit_classifier_does_not_match_generic_bad_request_text
opencode::retry::tests::rate_limit_classifier_matches_known_signals
supervisor::tests::process_probe_detects_current_process
supervisor::tests::process_probe_rejects_impossible_pid
```

## Process smoke test

A built `opencode2api-serve` process was launched on loopback with temporary test tokens. Results:

```json
{
  "cli_help_contains_usage": true,
  "server_help_contains_start": true,
  "health_status": 200,
  "health_body": {
    "status": "ok",
    "version": "0.4.2"
  },
  "rest_status": 200,
  "rest_service": "opencode2api",
  "rest_unauthorized_status": 401
}
```

This verifies that the rebuilt binaries start, the compatibility health route works, the versioned REST API accepts a valid Bearer token, and the REST API fails closed without authentication.

## Structural evidence

```text
Baseline Rust files: 36
Current Rust files: 95
Baseline Rust lines: 14,053
Current Rust lines: 14,458
Baseline largest file: 1,586 lines
Current largest file: 539 lines
Baseline files above 1,000 lines: 3
Current files above 1,000 lines: 0
```

The increase in file count and small increase in lines reflect explicit modules and regression tests. Monolithic runtime files were removed rather than merely wrapped.
