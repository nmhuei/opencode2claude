# Phase 17 — Documentation evidence

Implementation commit: `b9e0ea1`
Branch: `completion/full-repository-20260711`
Date: 2026-07-11

## Required documentation set

The following public guides are present and indexed from README:

- architecture overview;
- configuration and precedence;
- CLI and exit codes;
- Anthropic compatibility;
- management API and OpenAPI;
- proxy/WARP pool behavior;
- security model;
- production deployment;
- liveness/readiness/metrics;
- troubleshooting and incident playbooks;
- upgrade and rollback;
- contributor testing tiers;
- release checklist.

README was rewritten to describe only executable and tested contracts. Removed unsupported claims included nonexistent CLI flags/commands, a public Prometheus route, and direct request-handler shell execution.

## Automated consistency check

Executed:

```bash
python3 scripts/check_docs.py
```

Result:

```text
docs-check: PASS required=14
```

The checker verifies:

- every mandatory guide exists and is non-empty;
- README links required guides;
- known unsupported claims do not reappear;
- every environment variable in `.env.example` is documented in `docs/configuration.md`.

Version claims are independently checked with:

```bash
python3 scripts/check_version_consistency.py --binary target/release/opencode2api
```

Result:

```text
version-consistency: PASS version=0.5.0
```
