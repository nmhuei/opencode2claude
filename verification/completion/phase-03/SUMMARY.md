# Phase 03 — Exit Identity and Duplicate Suppression

Status: **PASS**

## Implemented

- Added bounded exit-identity probes executed through each node's own proxy client.
- Supports and normalizes:
  - Cloudflare trace (`ip`, `colo`, `warp`);
  - JSON (`ip`, `query`, `origin`);
  - plain public-IP responses.
- Rejects loopback, private, link-local, CGNAT, benchmark, documentation, multicast, unspecified, and reserved addresses.
- Requires endpoint consensus; default two-endpoint policy needs both probes to agree.
- Requires a positive Cloudflare WARP signal when a trace response supplies a WARP field.
- Caps response bodies at 64 KiB.
- Stores normalized provider, colo, IP, and verification timestamp.
- Deterministically selects one duplicate owner, preferring enabled primary nodes, then stable node ID.
- Excludes duplicates from routing and independent-capacity counts.
- Adds freshness TTL; stale identities are neither routable nor ready in strict mode.
- Clears old identity evidence when a managed container is restarted.
- Starts periodic identity refresh using the configured proxy-health interval.
- Default proxy URLs now use `socks5h://` so destination DNS is resolved by the proxy.

## Unit and fixture evidence

```text
cargo test proxy_pool::identity --lib
6 passed, 0 failed
```

Coverage includes response formats, public-IP validation, consensus, WARP signal, deterministic duplicate ownership, unique-exit readiness, and stale identity rejection.

## Real WARP system evidence

Read-only verification on the local Docker/WARP pool:

```text
opencode-warp-1 port=40001 ip=104.28.222.74 duplicate_of=None
opencode-warp-2 port=40002 ip=104.28.222.73 duplicate_of=None
opencode-warp-3 port=40003 ip=104.28.222.73 duplicate_of=opencode-warp-2
```

Command:

```bash
cargo test --test egress_identity_system -- --ignored --nocapture
```

Result:

```text
1 passed, 0 failed
```

This proves that three running WARP containers currently provide only two independent public exits and that the application suppresses the duplicate.

No lifecycle action was performed on ports 40004-40005.

## Full verification

```text
cargo clippy --all-targets -- -D warnings          PASS
cargo test --all-targets                           PASS
library/unit: 205 passed, 1 live-network ignored
real WARP identity system: 1 passed when invoked
fast HTTP/dashboard: 81 passed
```

## Deferred by dependency

- Readiness HTTP mapping is completed in Phase 10.
- Identity worker cancellation and health reporting are completed in Phase 05.
- Scheduled Tier C execution is completed in Phase 15/16.
