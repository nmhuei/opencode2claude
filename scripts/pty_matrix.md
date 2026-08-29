# pty_matrix.py — Real-CLI PTY Verification Harness

Implements the **mandatory manual verification sequence** from the project
`CLAUDE.md` deploy gate: drive the REAL `claude` CLI in a pseudo-terminal
against a NON-PRODUCTION bridge instance fed by an OpenAI-compatible SSE
stub, with mechanical PASS/FAIL assertions.

The harness PREPARES the evidence; the coordinator runs the authoritative
full pass right before any deploy of parse/protocol/streaming/mapping
changes (unit tests alone are never sufficient for terminal behavior).

## Requirements

- Python 3.10+ (stdlib only — pexpect/pyte are NOT needed).
- Debug bridge binary: `cargo build` produces `target/debug/opencode2api-serve`.
- Real `claude` CLI on PATH (validated against v2.1.246).
- The OpenAI SSE stub, auto-discovered in this order:
  1. `tests/stub_openai.py`  *(policy path — present, byte-identical to the
     artifacts reference)*
  2. `artifacts/claude-upstream-reverse/tests/stub_openai.py`  *(fallback)*
  Override with `--stub PATH`. `tests/stub_upstream.py` (Anthropic-side
  baseline stub) is also checked in at the policy path per CLAUDE.md,
  though this harness only consumes `stub_openai.py`.

## Commands

```bash
# List scenarios and budgets
python3 scripts/pty_matrix.py list

# Cheap gate (scenarios 1 + 4) — ~30 s total
python3 scripts/pty_matrix.py smoke

# Single scenario / subset
python3 scripts/pty_matrix.py run --scenarios 5
python3 scripts/pty_matrix.py run --scenarios 1,4,6

# Full authoritative pass (all 8) — budget ~20–30 min
python3 scripts/pty_matrix.py run --scenarios all

# Useful flags
--out DIR            artifacts root (default artifacts/pty-matrix/, gitignored)
--bridge-bin PATH    default target/debug/opencode2api-serve
--shell-policy P     unrestricted (default) | allowlist | disabled
--model M            profile model string (default claude-sonnet-4-6)
```

Exit code is 0 iff every selected scenario passes. Each run writes
`<out>/<HHMMSS>-s<N>-<name>/` with `terminal.raw`, `events.jsonl`,
`stub.req.log`, `stub.done`, `egress.log`, `RESULT.json`, plus a top-level
`<timestamp>-summary.json`.

## Scenarios and mechanical assertions

| # | Name | Budget | Assertions |
|---|------|-------:|------------|
| 1 | two_consecutive      | 150 s | 2 turns complete upstream (done-marker); both `OK` replies render; >=2 stub requests |
| 2 | agent_tool_call      | 300 s | stub issues an Agent-tool call split across stream fragments; permission dialog auto-answered; tool-result continuation renders `TOOL_RESULT_ACCEPTED`; >=2 requests |
| 3 | streaming_tool_call  | 200 s | fragmented Bash tool call (`echo ok`) executes; continuation renders; echoed `ok` visible |
| 4 | shell_command        | 100 s | `!printf MARKER` executes locally in the TUI and renders; any model follow-up completes; PLUS direct HTTP POST proving BRIDGE-side `!` interception returns command output |
| 5 | ctrlc_midstream      | 220 s | Ctrl+C during the stub's mid-stream idle gap; post-interrupt window free of spinner glyphs/raw SSE; REPL survives (next turn round-trips and renders a standalone `OK` line) |
| 6 | upstream_error_retry | 220 s | stub 500s the FIRST attempt; >=2 upstream attempts prove the client re-sent the turn (bridge fails fast on ProviderServer and never retries internally); final reply renders a standalone `OK`; scenario window free of leaked error bodies/raw SSE/chunk JSON |
| 7 | ten_turns            | 420 s | 10 turns each complete and render; idle-window spinner scan; global raw-SSE/JSON-leak scan; no stuck-redraw line loops |
| 8 | midstream_error_terminates_cleanly | 220 s | stub streams `partial` then a raw in-band `data: {"error": …}` line (no `[DONE]`); EXACTLY ONE streaming upstream request delivers it (a re-driven stream = FINDING fail); bridge log carries exactly one "mid-stream upstream error event … no message_delta/message_stop" marker and zero wrong-path errors; scenario window free of raw SSE/chunk JSON; failed turn ENDS and the REPL completes a follow-up turn (`OK`). The CLI's silent `stream=False` recovery request is recorded, not gated (see limitations). |

## Isolation guarantees

- Bridge binds `127.0.0.1:<ephemeral>` (never :4000/:4096 — guarded), with a
  throwaway HOME/runtime dir and an allowlisted environment (no repo
  dotfiles, no ambient API keys, no proxy pool, auth disabled,
  `BRIDGE_EGRESS_MODE=direct`, `OPENCODE_UPSTREAM_BASE_URL` pinned to the
  local stub). The bridge runs with `RUST_LOG=info`: S8's bridge-side proof
  is the INFO-level "mid-stream upstream error event" marker from
  `forward/stream/execute.rs`; log volume stays negligible at scenario size.
- The CLI runs with `CLAUDE_CONFIG_DIR` pointed at a generated profile
  (pre-provisioned `.claude.json` skips theme/API-key onboarding;
  `ANTHROPIC_AUTH_TOKEN` avoids the logged-out "No" default),
  cwd OUTSIDE the repo (project `.claude/settings*.json` would otherwise be
  inherited via parent-directory walk), and `--setting-sources user`.
- An EgressWatcher samples `ss -tnp` every second; ANY non-loopback peer on
  flows touching our ports/pids fails the scenario (proves nothing leaves
  localhost even if the upstream pin ever breaks).
- Every child (stub, bridge, PTY claude) runs in its own process group and
  is SIGKILLed via atexit + SIGINT/SIGTERM/SIGHUP handlers. Temp dirs are
  removed. A Ctrl+C mid-run leaves zero processes behind.
- Fresh stub+bridge pair PER SCENARIO.

## What PASS looks like

```
[S1] two_consecutive        PASS  (10.4s)
[S4] shell_command          PASS  (10.0s)

Summary: 2/2 PASS -> artifacts/pty-matrix/20260826-173555-summary.json
```

Scenario failures print up to 8 bullet reasons; `RESULT.json` holds the
full list and artifact paths.

## Known limitations

- **Stub determinism**: default replies are exactly `OK`; tool continuations
  reply exactly `TOOL_RESULT_ACCEPTED`. Scenario 1/7 therefore cannot assert
  per-turn *textual* distinctness — the done-marker/request counts carry the
  proof; eyeball `terminal.raw` (or pipe through
  `artifacts/claude-upstream-reverse/tests/render_screen.py` if pyte is
  installed) for content quality.
- **No byte-idle gates**: v2.1.246 keeps re-rendering its splash banner when
  terminal capability queries go unanswered on a bare PTY, so readiness and
  cleanliness are judged on dialog-blocker absence (incremental text) and
  scoped windows, not on output silence.
- **Bash-mode `!` semantics**: interactive `!cmd` executes LOCALLY and then
  sends ONE model follow-up summarizing the result (matches production
  behavior); scenario 4 asserts that follow-up completes rather than
  requiring zero upstream traffic. The bridge-side `!` interception gate is
  covered by the direct HTTP sub-check.
- **Duplicate-line detection** is approximated (3+ consecutive identical
  lines = stuck redraw). Pixel-exact frame comparison would need `pyte`,
  which is not installed.
- Dialog auto-answers are whitespace-insensitive because the TUI styles
  words individually (ANSI stripping often eats inter-word spaces).
- **Tool-approval dialogs may never render on v2.1.246**: agent/Task spawns
  need no approval, and clearly-safe commands (`echo ok`) are auto-approved
  in default mode. Observed S2/S3 runs completed with zero `Bash`/`Agent`
  permission prompts. The `DIALOG_ANSWERS` heuristics stay armed for
  non-safe commands and other versions; their absence is NOT a failure.
- **v2.1.246 retries even WELL-FORMED terminal error events** (observed
  2026-08-26, S8, three stable passes): after the bridge correctly ends the
  stream with one Anthropic `error` event (bridge marker logged exactly
  once; no message_delta/message_stop — invariant #6 mid-stream half HOLDS),
  the CLI recovers through the same silent NON-streaming fallback as S6:
  one extra `stream=False` request the stub answers with plain JSON OK.
  Rendering is seamless (`Simmering… → OK`); no error banner, no leak. S8
  therefore gates on what invariant #6 actually claims — exactly ONE
  streaming delivery of the failed turn, exactly ONE bridge error marker,
  clean window, turn ENDS, REPL survives — while recording the fallback in
  `events.jsonl` (`client_nonstreaming_fallback`). A SECOND STREAMING
  attempt (client re-drive or bridge replay) gates FAIL as a contract
  FINDING. Distinguishing "CLI always falls back on retryable in-band
  errors" from "the bridge's error framing provokes the retry" would need an
  alternative-framing control script in the stub (out of scope: stub edits
  are restricted); treat any future S8 streaming re-drive failure as a
  deploy-gate blocker.

## Gaps to close before the authoritative run

1. ~~Policy references `tests/stub_openai.py` / `stub_upstream.py`, but
   they exist only under `artifacts/claude-upstream-reverse/tests/`.~~
   RESOLVED 2026-08-26: both copied byte-identically into `tests/`
   (verified with `diff`), harness discovery resolves the policy path
   first, and standalone parity checks confirmed identical scripted
   responses/nonces/mid-stream pacing. S2 + S3 smoke-ran PASS against the
   `tests/` stub through the real CLI v2.1.246.
2. Scenario budgets above are upper bounds; observed wall time will be much
   lower once warm (smoke/S2/S3: ~8–11 s/scenario). Reserve ~30 min wall
   clock for the full 8-scenario pass including retries.
3. Cosmetic: `stub.req.log` prints only the first 60 chars of the last user
   message — scenario markers merged after `<system-reminder>` prefixes are
   hidden there even when matching succeeded, which makes per-nonce req-log
   matching impossible BY CONSTRUCTION (S6 therefore proves retries via
   request-count deltas over its marked baseline, see below). Under
   concurrent requests the stub's `CTR` counter can also mislabel lines
   (read-modify-write race); request COUNTS stay correct, so assertions
   are unaffected.
4. **v2.1.246 error recovery is a silent non-streaming fallback** (observed
   2026-08-26, S6): after an immediate upstream 500 the CLI re-sends the
   turn with `stream=false` — seamless UI, no visible retry banner. The
   stub's non-streaming path returns before writing a done-marker, so S6
   treats EITHER signal as completion (done-marker delta OR >=2 requests)
   and never gates on done-markers alone. Bridge-side contract verified in
   the same run: exactly one fail-fast ProviderServer ERROR logged, zero
   internal bridge retries, zero protocol/error text leaked to the client.
4. The authoritative full-matrix run must rebuild from a quiescent tree:
   during this smoke campaign sibling agents were actively editing
   `src/**`, so any binary snapshot risks racing their WIP.
