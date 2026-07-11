# Phase 02 — Egress Domain Redesign

Status: **PASS**

## Implemented

- Removed the overloaded `ProxyStatus` model.
- Added independent state dimensions:
  - `EgressRole`: primary or protected warm standby;
  - `HealthState`: unknown, healthy, degraded, unhealthy, recovering;
  - `CircuitState`: closed, open with deadline, half-open;
  - `LifecyclePolicy`: managed or protected;
  - `routing_enabled`, restart attempts, cooldown deadline, exit identity, duplicate owner, and active request count.
- Added a cross-build FNV-1a rendezvous hash with a golden-value regression test.
- Preserved affected-session stickiness while always trying another healthy primary before standby.
- Excluded duplicate exits, open circuits, disabled primaries, and already-leased half-open nodes.
- Added half-open probe semantics and bounded recovery transitions.
- Added atomic request leases.
- Wrapped upstream responses in `LeasedResponse` so leases remain active until JSON/text body completion or SSE stream drop.
- Prevented destructive lifecycle actions while a node has active request leases.
- Kept warm-standby nodes protected from restart/stop/purge/recreation.
- Expanded health snapshots with independent state, circuit, load, identity, and duplicate fields while retaining a compatibility `status` label.

## Verification

```text
cargo fmt --all -- --check                         PASS
cargo clippy --all-targets -- -D warnings          PASS
cargo test --all-targets                           PASS
library/unit: 199 passed, 1 live-network ignored
fast HTTP/dashboard: 81 passed
heavy/system: 18 classified for Phase 15
```

## Key regression tests

- `stable_hash_has_cross_build_golden_value`
- `retry_excludes_failed_proxy_and_prefers_other_primary`
- `standby_is_used_only_after_enabled_primaries_are_unavailable`
- `protected_node_cannot_be_modified_even_without_lease`
- `half_open_node_accepts_only_one_probe_lease`
- `leases_block_destructive_lifecycle_operations`
- `expired_circuit_becomes_half_open_and_successes_close_it`
- `text_body_holds_lease_until_consumed`
- `streaming_body_holds_lease_until_stream_drop`
- `restart_attempts_are_independent_from_health_state`

## Deferred by dependency

- Exit identity probing and duplicate ownership are implemented in Phase 03.
- Injectable Docker/WARP runtime replaces remaining direct commands in Phase 04.
- Worker cancellation/join health is completed in Phase 05.
- Fault-injected circuit integration is completed in Phase 07/15.
