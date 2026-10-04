# Claude Code rate-limit stability design

## Goal
Keep Claude Code usable through opencode2claude when the upstream legitimately rate-limits requests, without bypassing provider quota or amplifying retries.

## Design
1. Preserve the existing fail-closed proxy recovery gate for fresh requests. Do not route a rate-limited request to another proxy to evade a provider limit.
2. Eliminate Claude Code's narrowly identifiable session-title inference request from upstream traffic. Detect only the Claude Agent SDK title prompt plus the exact single-field `title` JSON schema, no tools, and disabled thinking. Synthesize a short title locally and emit a normal Anthropic Messages response/SSE lifecycle.
3. Make retry state explicit. A retry belonging to the same in-flight request must not be mistaken for a fresh request by the global recovery gate. It may wait only for a bounded retry delay and retry the same selected egress identity after it becomes eligible; it must not spill to another healthy proxy. Fresh concurrent requests remain fail-closed.
4. Verify with focused unit tests, full Rust tests, release build, one real Claude Code simple request, and one real tool-call turn. Confirm history has no title upstream attempt and no retry storm.

## Safety and correctness constraints
- Never mutate host WARP or rotate IPs as a mechanism to bypass quota.
- Respect provider Retry-After/cooldown semantics.
- Do not change the main task model silently.
- Preserve all pre-existing dirty changes in the repository.
