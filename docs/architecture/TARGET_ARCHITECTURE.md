# Target Architecture

## Design objectives

The bridge should remain a single deployable Rust binary while enforcing clear dependency direction:

```text
entry points
    -> transport
        -> application services
            -> domain policy
                -> infrastructure adapters
```

A higher layer may depend on a lower layer. Infrastructure implementations must not decide HTTP response shape, CLI presentation, or protocol policy.

## Target layers

### 1. Entry points

```text
src/bin/opencode2api.rs
src/serve_main.rs
```

Responsibilities:

- parse process arguments;
- initialize top-level dependencies;
- choose exit code;
- call application/runtime functions.

They should contain no routing, proxy, conversion, or Docker logic.

### 2. Application interfaces

```text
src/app/
src/server/
```

Responsibilities:

- CLI command orchestration;
- HTTP router composition;
- foreground/background lifecycle;
- presentation and output formatting.

Rules:

- application modules may call management, protocol, egress, and infrastructure services;
- presentation code must not mutate proxy state directly;
- server lifecycle should eventually return typed errors instead of terminating the process.

### 3. HTTP transports

```text
src/handlers/
src/dashboard/
src/rest_api.rs
```

Responsibilities:

- parse headers, paths, queries, and JSON;
- call application/domain services;
- map typed results to HTTP JSON or SSE;
- preserve compatibility contracts.

Rules:

- transports do not execute Docker commands;
- transports do not construct proxy snapshots independently;
- transports do not duplicate authentication policy;
- browser-specific cookie behavior stays in dashboard transport.

### 4. Application services

```text
src/management/
future: src/application/messages.rs
future: src/application/egress.rs
```

Responsibilities:

- coordinate operations spanning multiple domain components;
- expose transport-neutral request/response structures;
- enforce use-case rules such as managed-proxy restart validation.

The management service introduced in this overhaul is the first implementation of this layer.

### 5. Protocol domain

```text
src/opencode/mapper/
src/opencode/forward/
src/opencode/retry/
src/opencode/search/
src/opencode/sanitize.rs
src/opencode/types.rs
src/sse.rs
src/stream_tracker.rs
```

Responsibilities:

- Anthropic/OpenAI-compatible conversion;
- SSE state and content-block ordering;
- upstream retry classification;
- search fallback policy;
- DSML sanitation and parsing.

Rules:

- protocol mapping must be deterministic and unit tested;
- streaming code must process bounded incremental buffers;
- retry policy may classify egress failures but must not invoke Docker lifecycle directly;
- provider adapters should return typed provider errors rather than preformatted UI text in a future phase.

### 6. Egress domain

```text
src/proxy_pool/
```

Current responsibilities:

- sticky routing;
- primary/standby selection;
- failure/cooldown state;
- restart queue state;
- health snapshots.

Target data model:

```rust
struct EgressNode {
    id: NodeId,
    endpoint: ProxyEndpoint,
    role: EgressRole,
    health: HealthState,
    circuit: CircuitState,
    lifecycle: LifecyclePolicy,
    exit_identity: Option<ExitIdentity>,
    active_requests: usize,
    consecutive_failures: u32,
    cooldown_until: Option<Instant>,
}
```

These dimensions must be independent:

```text
role       = primary | standby
health     = unknown | healthy | degraded | unhealthy | recovering
circuit    = closed | open | half-open
lifecycle  = managed | protected
```

The current `ProxyStatus` enum is an intermediate compatibility model and should be replaced only with a migration backed by routing and state-machine tests.

### 7. Infrastructure adapters

```text
src/docker/
src/supervisor.rs
src/runtime.rs
src/pidfile.rs
src/update.rs
```

Responsibilities:

- execute OS/Docker commands;
- inspect process/container state;
- manage runtime files;
- download and replace binaries.

Target interfaces:

```rust
trait ContainerRuntime {
    async fn create_proxy(&self, spec: &ProxySpec) -> Result<(), ContainerError>;
    async fn remove_proxy(&self, id: &NodeId) -> Result<(), ContainerError>;
    async fn inspect_proxy(&self, id: &NodeId) -> Result<ContainerState, ContainerError>;
}

trait ProcessProbe {
    fn exists(&self, pid: u32) -> bool;
    fn terminate(&self, pid: u32) -> Result<(), ProcessError>;
}

trait ExitIdentityProbe {
    async fn probe(&self, endpoint: &ProxyEndpoint) -> Result<ExitIdentity, ProbeError>;
}
```

Concrete command execution then becomes replaceable in tests without starting Docker or WARP.

## Request flow

### Anthropic messages request

```text
HTTP /v1/messages
    -> handlers::messages
        -> handlers::shell (optional local delegation)
        -> mapper::request
        -> retry::execute
            -> proxy_pool::routing
            -> upstream HTTP
        -> forward::sync or forward::stream
        -> Anthropic JSON/SSE response
```

### Management request

```text
HTTP /api/v1/* or /api/dashboard/*
    -> transport auth extraction
        -> management::auth
    -> management::service
        -> proxy_pool snapshot or Docker adapter
    -> transport-specific JSON
```

### Proxy maintenance

Current:

```text
proxy_pool maintenance worker
    -> direct Docker commands
```

Target:

```text
proxy_pool/control service
    -> ContainerRuntime adapter
    -> exit identity probe
    -> state transition
```

## Configuration architecture

All runtime policy should ultimately be represented by one resolved immutable configuration tree:

```rust
struct AppConfig {
    server: ServerConfig,
    upstream: UpstreamConfig,
    auth: AuthConfig,
    streaming: StreamingConfig,
    retry: RetryConfig,
    egress: EgressConfig,
    search: SearchConfig,
    runtime: RuntimeConfig,
}
```

Direct `std::env::var` calls should be confined to configuration loading. Tests should instantiate configuration values directly.

Important future configuration fields:

```text
upstream.base_url
egress.active_count
egress.max_restart_attempts
egress.health_interval
egress.require_verified_exit_ip
retry.max_network_attempts
retry.max_provider_attempts
retry.model_fallbacks
runtime.warp_image
runtime.docker_binary
```

## Health model

Preserve `/health` for compatibility, then add:

```text
GET /health/live
```

Returns success when the process and event loop are alive.

```text
GET /health/ready
```

Returns success only when:

- configuration is valid;
- at least one permitted egress route is available, or direct mode is intentionally configured;
- critical background workers are running;
- optional upstream probing has not declared a hard outage.

Management endpoints may expose detailed state, but public liveness should not reveal topology or secrets.

## Test strategy

### Unit tests

- mapping and model policy;
- stream content-block state machine;
- retry classification;
- routing and state transitions;
- configuration precedence;
- authentication and redaction.

### Integration tests

- production router, not a copied route tree;
- fake upstream HTTP/SSE server;
- controlled SOCKS fixture;
- fake container runtime;
- client disconnect and cancellation;
- graceful process lifecycle.

### Heavy/system tests

- real Docker/WARP lifecycle;
- exit-IP uniqueness;
- failover under container loss;
- Linux supervisor behavior;
- release binary smoke tests.

Heavy tests should be opt-in locally but mandatory in a scheduled or protected CI environment.

## Migration phases after this branch

### Phase 2 — Egress model redesign

- separate role/health/circuit/lifecycle;
- add exit identity;
- deduplicate public IPs;
- add lease and active-request accounting;
- introduce cancellation for workers.

### Phase 3 — Infrastructure interfaces

- one Docker/container adapter;
- remove duplicate Docker creation logic;
- inject process probes and command runners;
- make supervisor cross-platform end to end.

### Phase 4 — Configuration consolidation

- move remaining environment reads into typed config;
- make upstream URL and retry policy configurable;
- validate all settings once at startup.

### Phase 5 — Generated management contract

- typed REST response models;
- generated OpenAPI;
- readiness endpoint;
- dashboard consumes the versioned management API where practical.

## Architectural acceptance rules

A future change should not be merged when it introduces any of the following:

1. HTTP handlers invoking Docker or OS commands directly.
2. Dashboard and REST implementing the same policy independently.
3. Proxy retry silently falling back to direct egress.
4. Unbounded stream buffering.
5. New global environment reads outside configuration/runtime bootstrap.
6. A new state transition without a regression test.
7. A copied production route tree in integration tests.
8. A platform-specific process assumption without compile/runtime coverage.
