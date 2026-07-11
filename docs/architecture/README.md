# Architecture Documentation

- [Source audit and overhaul evidence](SOURCE_AUDIT_20260711.md)
- [Target architecture and migration rules](TARGET_ARCHITECTURE.md)
- [Verification evidence](VERIFICATION_EVIDENCE.md)
- [Full repository completion plan](FULL_REPOSITORY_COMPLETION_PLAN.md)

## Overhaul branch

```text
refactor/architecture-overhaul-20260711
```

## Commit sequence

```text
f94c2b8  checkpoint pre-overhaul behavior
5e6b41e  split CLI application orchestration
061ab92  unify management auth and operations
16314c9  split configuration schema/loading/security
0f80f37  split synchronous/streaming forwarding
26a97b5  split mapper policy/helpers/conversion
3121db7  split handlers and shell delegation
2f3c910  split Docker lifecycle/health/bootstrap
8b8d9d4  isolate proxy-pool state and fix retry exclusion
2a2ac01  split search providers and fix UTF-8 handling
13c63ef  split server arguments/routes/runtime
2a2cc61  split stream transport/context/execution
d281bbd  fix cross-platform process probing
b2b3bf6  use production router in tests and async env lock
689f0a1  preserve proxy restart attempt state
a961064  classify retry failures without penalizing healthy egress
```

The commit sequence is intentionally incremental so reviewers can inspect structural moves separately from behavior changes.
