# Phase 01 — Typed Configuration Consolidation

Status: **PASS**

## Implemented

- Expanded the resolved `BridgeConfig` into typed management, retry, egress, runtime, observability, and protocol policy sections.
- Added `SecretString`; `Debug` and `Display` never expose secret bytes.
- Moved operational environment resolution into `src/config/loader.rs`:
  - upstream URL and model fallback policy;
  - retry attempts and bounded backoff;
  - active proxy count and egress mode;
  - exit-identity and worker policy;
  - management tokens and config path;
  - rate limiting, metrics, and request-ID settings;
  - Docker/WARP binary and image settings.
- Runtime modules now consume resolved configuration rather than reading process environment variables.
- Added semantic validation for inconsistent proxy, retry, stream, authentication, and public-binding configurations.
- Added TOML compatibility for both comma-separated and array-form auth tokens.
- Updated `.env.example` and generated TOML template with component-specific comments.
- Removed management-token values from dashboard JSON output.
- Added `scripts/check_config_boundary.py` and made it a CI gate.

## Boundary evidence

```text
./scripts/check_config_boundary.py
config-boundary: PASS
```

The only allowed environment reads outside `src/config/loader.rs` are:

- `src/runtime.rs`: HOME/RUNTIME_DIR bootstrap fallback;
- `src/output.rs`: standard NO_COLOR presentation convention.

## Verification

```text
cargo fmt --all -- --check                         PASS
cargo clippy --all-targets -- -D warnings          PASS
cargo test --all-targets                           PASS
library/unit: 195 passed, 1 live-network ignored
fast HTTP/dashboard: 81 passed
heavy/system: 18 classified for Phase 15
scripts/check_feature_matrix.py                     PASS
scripts/check_config_boundary.py                    PASS
git diff --check                                    PASS
```

## Regression coverage added

- operational policy precedence: environment over TOML;
- TOML auth token array compatibility;
- secret formatting redaction;
- numeric and HTTP-date Retry-After parsing;
- deterministic bounded retry jitter;
- provider/rate-limit status classification;
- management auth resolved without environment access.

## Deferred by dependency

- Schema-version migrations belong to Phase 13.
- Full log-capture proof of secret redaction belongs to Phase 14.
- Dashboard typed config apply/rollback belongs to Phase 09.
