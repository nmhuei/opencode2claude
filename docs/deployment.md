# Production deployment

OpenCode2API is intended to run as a local or internal bridge. The safest default is loopback binding with a local client. Public or shared-network deployment requires explicit authentication and external TLS termination.

## Deployment modes

### Direct egress

Use direct mode when the host network is the intended upstream path and Docker/WARP isolation is not required:

```toml
schema_version = 1
host = "127.0.0.1"
port = 4000
egress_mode = "direct"
primary_proxies = []
warm_standby_proxies = []
shell_policy = "disabled"
```

Start:

```bash
opencode2api server start --config opencode2api.toml --no-proxy
```

### Managed WARP/SOCKS egress

The default topology uses managed primaries on `40001-40003` and protected warm standbys on `40004-40005`. Docker must be available to the bridge user if the application is expected to reconcile managed primaries.

Pin the WARP image by digest in controlled deployments:

```toml
egress_mode = "proxy"
warp_image = "ghcr.io/mon-ius/docker-warp-socks@sha256:<digest>"
primary_proxies = [
  "socks5h://127.0.0.1:40001",
  "socks5h://127.0.0.1:40002",
  "socks5h://127.0.0.1:40003",
]
warm_standby_proxies = [
  "socks5h://127.0.0.1:40004",
  "socks5h://127.0.0.1:40005",
]
allow_direct_fallback = false
```

Warm-standby containers must be provisioned independently. The application treats them as protected and will not create, restart, stop, purge, or migrate them.

## Public binding

A non-loopback bind requires all of the following:

```toml
host = "0.0.0.0"
auth_tokens = ["long-client-token"]
dashboard_admin_token = "long-dashboard-token"
rest_api_token = "different-long-rest-token"
shell_policy = "disabled"
```

Place the service behind a TLS reverse proxy. Preserve `Authorization`, `Content-Type`, and the configured request-ID header. Disable proxy buffering for `/v1/messages` streaming responses and dashboard SSE routes.

Example reverse-proxy requirements:

- upstream HTTP/1.1 or HTTP/2 support;
- streaming response buffering disabled;
- request body limit at least as strict as the bridge limit;
- idle timeout greater than expected model response duration;
- no authentication tokens in access-log query strings.

## Runtime directory

Set an explicit writable runtime path for service-manager deployments:

```toml
runtime_dir = "/var/lib/opencode2api"
```

The service user must own the directory. PID, log, and supervisor state should not be shared across independent instances.

## Service managers

The most predictable deployment is foreground mode under a process supervisor:

```bash
opencode2api server start --foreground --config /etc/opencode2api/config.toml
```

The process handles termination signals through bounded graceful shutdown. Service managers should send `SIGTERM`, wait longer than `server_shutdown_timeout_secs`, then escalate only if necessary.

The built-in background supervisor is appropriate for workstation use. It validates PID ownership before termination, but it is not a replacement for systemd or launchd in multi-user production environments.

## Readiness probes

Use separate probes:

```text
Liveness:  GET /health/live
Readiness: GET /health/ready
```

Do not use `/health` for traffic admission. It is a minimal compatibility endpoint and intentionally remains green during many dependency failures.

In strict proxy mode, readiness depends on worker health, eligible egress, identity freshness, and the configured unique-exit minimum.

## Logging and retention

The bridge emits structured tracing fields and request IDs. It does not implement a full production log-retention subsystem. Run foreground output under journald, launchd, a container log driver, or another external collector with rotation and retention policy.

Never enable request-body logging at the reverse proxy. Upstream prompts, tool arguments, and search results may contain sensitive data.

## Container deployment

The published application image is separate from the WARP proxy images. Granting an application container access to the host Docker socket is highly privileged. Prefer one of these models:

1. run the bridge on the host and manage Docker through the local CLI;
2. provision proxy containers externally and configure them as external/protected endpoints;
3. use a narrowly scoped container-runtime proxy rather than mounting the unrestricted Docker socket.

The release workflow publishes image provenance, SBOM, and a keyless signature. Verify the digest and signature before deployment.

## Backup and rollback

Back up:

- `opencode2api.toml`;
- runtime state needed for operator diagnostics;
- the currently installed binary;
- pinned image digests and release checksums.

Config apply and self-update have automatic rollback for immediate validation failures. Operational rollback procedures are documented in [upgrade-rollback.md](upgrade-rollback.md).
