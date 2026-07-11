# Health, readiness, diagnostics, and metrics

OpenCode2API separates public health checks from authenticated operational detail.

## Public health

### `GET /health`

Compatibility endpoint. It returns a minimal status and deliberately omits proxy topology, tokens, upstream URLs, and worker failure detail.

### `GET /health/live`

Liveness indicates that the HTTP process and event loop are running. It should remain healthy during an upstream outage or temporary proxy failure.

### `GET /health/ready`

Readiness evaluates whether the instance should receive traffic. It considers:

- configuration validity;
- critical worker state;
- direct versus proxy egress policy;
- available eligible nodes;
- strict exit-identity freshness and unique-exit minimum;
- runtime dependency state required by the configured mode.

A failed critical worker or unavailable fail-closed proxy route makes readiness fail while liveness remains healthy.

## Authenticated diagnostics

Use the management API or dashboard diagnostics to inspect the reason for readiness failure. Diagnostic responses are redacted and can include:

- resolved mode and model;
- worker names, health, heartbeat, and last failure;
- node role, health, circuit, lifecycle, active leases, and restart count;
- exit-identity freshness and duplicate ownership;
- runtime dependency checks.

Public health does not expose this detail.

## Request correlation

The default correlation header is `x-request-id`, configurable through `BRIDGE_REQUEST_ID_HEADER`. Incoming IDs are accepted only when they are printable, non-empty, at most 128 bytes, and do not contain comma or semicolon separators. Invalid or absent IDs are replaced with a generated ID and echoed on the response.

## Metrics endpoint

Metrics are available at:

```text
GET /api/v1/metrics
```

Authentication is the same as the management API. Disable collection with `BRIDGE_METRICS_ENABLED=false` or `metrics_enabled = false`.

The typed snapshot includes:

- request totals by response class;
- active and peak request counts;
- total and maximum latency;
- generated request-ID count;
- stream started, completed, cancelled, and failed counts;
- active and peak stream counts;
- retries by transport, timeout, rate limit, provider client, provider server, and malformed response class;
- model-fallback count;
- proxy restart attempts, successes, and failures;
- attempt, success, failure, and no-result counts for Tavily, Exa, Serper, SearXNG, and DuckDuckGo.

Example:

```bash
curl -sS \
  -H "Authorization: Bearer $REST_API_TOKEN" \
  http://127.0.0.1:4000/api/v1/metrics | jq
```

The endpoint is a bounded in-process snapshot, not an unauthenticated Prometheus exposition endpoint.

## Counter semantics

- A stream receives one terminal outcome: completed, cancelled, or failed.
- A retry counter increments only when the policy actually schedules another attempt.
- A model fallback increments independently from retry counters.
- Search metrics are recorded for each provider attempted in the fallback chain.
- Proxy restart metrics count managed restart operations, not health probes.

## Logging

Structured tracing should include request IDs and typed state rather than credentials or full upstream error bodies. Client-facing errors contain bounded status information. Full provider response content should not be copied into public logs.

Log rotation and retention are delegated to the deployment environment. Recommended choices are journald, launchd logging, a container log driver, or a dedicated collector.

## Alerting guidance

Useful alerts include:

- readiness failing while liveness remains healthy;
- critical worker failure;
- sustained retry or rate-limit growth;
- proxy restart failures;
- unique verified exit count below policy;
- rising stream cancellation or failure rate;
- search provider failure rate with fallback exhaustion;
- latency maximum or active-request saturation above operational thresholds.
