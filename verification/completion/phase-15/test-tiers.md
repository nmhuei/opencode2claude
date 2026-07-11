# Phase 15 — Test-tier evidence

Implementation commit: `b9e0ea1`
Branch: `completion/full-repository-20260711`
Date: 2026-07-11

## Tier A

Executed:

```bash
scripts/tier-a.sh
```

Result: PASS.

Verified in one deterministic run:

```text
281 library/unit tests passed
81 router/dashboard tests passed
18 black-box integration tests passed
2 parser fuzz-smoke tests passed
12 protocol conformance tests passed
41 CLI E2E assertions passed
0 failed
```

Tier A also passed feature-matrix structure, version consistency, documentation consistency, release-workflow invariants, config boundary, infrastructure command boundary, secret scanner self-test, full working-tree secret scan, rustfmt, all-target check, Clippy with warnings denied, and debug binary build.

## Tier B

Executed:

```bash
SKIP_TIER_A=1 scripts/tier-b.sh
```

Result: PASS.

Tier B used the official ShellCheck container because the host had no native `shellcheck` executable. It passed:

- Bash and POSIX shell lint;
- `cargo audit`;
- `cargo deny check`;
- locked release build;
- disposable checksum/install/uninstall E2E;
- release-mode parser fuzz-smoke;
- release-mode protocol conformance.

## Tier C

Executed:

```bash
SOAK_SECONDS=15 RUN_EXTERNAL_SEARCH_CANARY=1 scripts/tier-c.sh
```

Result: PASS.

Observed real WARP identities:

```text
opencode-warp-1  104.28.222.73  canonical
opencode-warp-2  104.28.222.73  duplicate_of opencode-warp-1
opencode-warp-3  104.28.222.73  duplicate_of opencode-warp-1
```

The system correctly treated the three reachable proxies as one independent public exit.

Soak result:

```text
requests=2283
rss_start_kib=9188
rss_end_kib=9212
RSS growth=24 KiB
graceful shutdown=PASS
```

External search canary:

```text
DuckDuckGo HTTP status=202
body bytes=14178
canary=PASS
```

A longer release-candidate soak remains an operational release-checklist item; the bounded Tier C gate itself passed.
