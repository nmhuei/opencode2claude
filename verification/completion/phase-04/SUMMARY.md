# Phase 04 — Infrastructure Adapter Consolidation

Status: **PASS**

## Implemented

- Added bounded `CommandRunner` with timeout, non-zero status, stdout/stderr, and IO-error separation.
- Added one canonical `ProxySpec` defining container name, volume, image, ports, capabilities, sysctl, entrypoint, and startup command.
- Added injectable `ContainerRuntime` and concrete `DockerCliRuntime`.
- Removed duplicate Docker command construction from proxy maintenance.
- Added idempotent reconciliation for missing, running, stopped-volume-cached, and legacy managed containers.
- Protected standby behavior is fail-closed:
  - absent protected node may be created once during explicit bootstrap;
  - existing protected node is never migrated, resumed, restarted, stopped, removed, purged, or recreated;
  - failed verification never triggers protected lifecycle recovery.
- Fixed the prior bulk-purge defect: bulk stop/purge enumerates only ports 40001-40003, never 40004-40005.
- Added injectable `WarpController`; retry code no longer invokes `warp-cli` directly.
- Added cross-platform `ProcessManager`; supervisor no longer owns direct process commands or unsafe `pre_exec/setsid` code.
- Added `AtomicFileStore` with same-directory temp files, fsync, rename, cleanup, and Unix 0600 sensitive-file permissions.
- Dashboard config transport now uses the injected atomic file store rather than direct filesystem writes.
- Management proxy restart delegates to the injected container runtime and rejects protected or leased nodes before adapter invocation.
- Added `scripts/check_infrastructure_boundary.py`; CI rejects direct process execution outside `src/infrastructure/`.

## Adapter/failure evidence

- command timeout and non-zero status;
- malformed Docker inspect output;
- missing/stopped/legacy container reconciliation;
- exact canonical `docker run` argument contract;
- protected destructive calls produce zero runner invocations;
- protected bootstrap states produce zero mutations;
- management restart uses fake runtime;
- active lease returns HTTP 409 and no runtime call;
- atomic file replacement leaves no temporary file;
- sensitive atomic file uses mode 0600;
- process identity detects current PID and rejects impossible PID.

## Verification

```text
scripts/check_infrastructure_boundary.py             PASS
cargo fmt --all -- --check                         PASS
cargo clippy --all-targets -- -D warnings          PASS
cargo test --all-targets                           PASS
library/unit: 231 passed, 1 live-network ignored
fast HTTP/dashboard: 81 passed
real WARP identity: opt-in and previously verified
```

Direct process execution scan result:

```text
src/infrastructure/command.rs
src/infrastructure/process.rs
```

No domain, HTTP transport, egress, management, Docker wrapper, or retry module invokes `Command::new` directly.

## Deferred by dependency

- Worker cancellation/join/health registry is Phase 05.
- Strong PID ownership validation against persisted identity is Phase 11.
- Typed config preview/apply/rollback uses `FileStore` in Phase 09/13.
- Real Docker lifecycle tests run in protected Tier B CI in Phase 15/16.
