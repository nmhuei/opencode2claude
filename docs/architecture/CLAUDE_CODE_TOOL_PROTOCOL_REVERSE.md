# Claude Code Tool Protocol Reverse Map

Date: 2026-07-25
Repository: `/home/light/GitHub/opencode2claude`
Claude Code inspected: `2.1.219`

## 1. Scope and evidence standard

This document maps the complete tool-call path observed between:

1. the upstream OpenAI-compatible model,
2. the OpenCode2API bridge,
3. the Anthropic Messages/SSE protocol exposed by the bridge,
4. Claude Code's tool registry and executor,
5. client-side/deferred tool execution,
6. the resulting `tool_result`, next model turn, and recap/summary paths.

The conclusions below are based on four kinds of evidence:

- Static inspection of the installed Claude Code executable.
- Full tool-schema capture from real Claude Code requests.
- Deterministic fake Anthropic and fake OpenAI servers.
- Real Claude Code `--output-format stream-json` runs through both direct and bridged paths.

A model statement such as “scheduled successfully” is not accepted as evidence. A tool is considered successful only when the stream contains a matching `tool_result` and, when applicable, runtime evidence such as a subsequent `CronList` or filesystem check.

## 2. Claude Code executable type

Resolved executable:

```text
/home/light/.local/bin/claude
  -> /home/light/.local/share/claude/versions/2.1.219
```

Observed file type:

```text
ELF 64-bit LSB executable, x86-64, dynamically linked, not stripped
size: approximately 263 MiB
```

Important sections:

```text
.text
.rodata
.data
.bun       file offset 0x05251000, size 0x0b3e6e16
.symtab
.strtab
```

The `.bun` section and readable minified JavaScript establish that Claude Code 2.1.219 is a Bun-compiled executable containing an embedded JavaScript bundle. The relevant tool-control logic can be recovered with ELF section inspection, targeted string windows, request capture, and dynamic tracing. IDA Pro was not required for this task because the relevant implementation was readable in the embedded bundle.

## 3. Protocol map

```text
Claude Code user turn
    |
    | Anthropic Messages request
    | - tools[] schemas
    | - prior assistant tool_use blocks
    | - user tool_result blocks
    v
OpenCode2API /v1/messages
    |
    | map Anthropic -> OpenAI-compatible request
    v
Upstream model
    |
    | one of:
    | A. native OpenAI tool_calls
    | B. DSML text tool calls
    | C. compatibility text marker
    | D. ordinary assistant text/reasoning
    v
OpenCode2API response parser
    |
    | - validate tool name against request tools[]
    | - normalize arguments against input_schema
    | - deduplicate semantic invocations
    | - intercept bridge-owned search tools only
    | - emit Anthropic tool_use for client-side tools
    v
Anthropic response / SSE
    |
    | content_block_start(type=tool_use, id, name)
    | input_json_delta(partial_json)
    | content_block_stop
    | message_delta(stop_reason=tool_use)
    | message_stop
    v
Claude Code structured tool executor
    |
    | lookup registered tool by name
    | validate input schema
    | check permission / approval policy
    | execute immediately or defer
    v
Tool result
    |
    | user content block:
    | {type: tool_result, tool_use_id, content, is_error?}
    v
Next model turn
    |
    | model may now truthfully report success/failure
    v
Optional tool-use summary / recap
```

## 4. What Claude Code actually executes

Claude Code executes structured `tool_use` blocks. It does not execute assistant text merely because the text resembles a tool request.

Dynamic comparison:

| Path | Tool uses | Tool results | Raw marker visible | Outcome |
|---|---:|---:|---:|---|
| Fake Anthropic native `tool_use` -> Claude Code | 4 | 4 | 0 | Create/list/delete/list succeeded |
| Fake Anthropic text marker -> Claude Code | 0 | 0 | 3 | Marker remained inert visible text |
| Fake OpenAI text marker -> bridge -> Claude Code | 4 | 4 | 0 | Bridge converted marker; lifecycle succeeded |

Therefore, raw `[Requesting ...]` leakage is a bridge parsing failure, not a hidden Claude Code parser feature.

## 5. Claude Code tool registry

A real Claude Code request was captured in multiple permission modes. The non-interactive registry contained 27 tools in every tested mode:

```text
Agent
Bash
CronCreate
CronDelete
CronList
DesignSync
Edit
EnterWorktree
ExitWorktree
Monitor
NotebookEdit
PushNotification
Read
ReportFindings
ScheduleWakeup
SendMessage
Skill
TaskCreate
TaskGet
TaskList
TaskOutput
TaskStop
TaskUpdate
WebFetch
WebSearch
Workflow
Write
```

The full descriptions and input schemas are preserved in:

```text
artifacts/claude-tool-protocol-reverse/registry-matrix-summary.json
artifacts/claude-tool-protocol-reverse/registry-matrix-requests.json
```

### 5.1 Interactive-only and control-plane surfaces

`AskUserQuestion` exists in the embedded bundle and is used for interactive decisions. It is not exposed in the captured `claude -p` non-interactive registry.

`approval` is not a normal model tool in the captured registry. Approval is a permission/control-plane flow reached through tool `checkPermissions`, classifiers, policy dialogs, MCP approval handling, and plan-approval messages. A text marker named `approval` must not be treated as successful merely because it parses syntactically; unless a request actually exposes a tool with that name, the bridge rejects it as unavailable and performs bounded correction/retry.

MCP tools are dynamic. Their names and schemas appear in `tools[]` when the corresponding MCP server is connected. The bridge parser is registry-driven and does not require hard-coded MCP names.

### 5.2 Client-side and deferred tools

Static bundle inspection directly confirmed `shouldDefer: true` for:

```text
CronCreate
CronDelete
CronList
TaskList
```

The same executor contains a generic deferred-tool resume path and a `tool_deferred` terminal transition. Other tools may be conditionally deferred based on runtime mode; this document does not label them deferred without direct evidence.

Cron implementation details confirmed in the bundle:

- `CronCreate` validates a standard five-field cron expression.
- It creates a session scheduler job and returns an ID.
- `CronList` reads the session job store.
- `CronDelete` validates ownership/existence and removes the job.
- Default jobs are session-only unless durable cron support is enabled.
- The result string is generated by the tool implementation, not by the model.

## 6. Upstream tool encodings

The bridge supports three executable encodings.

### 6.1 Native OpenAI tool calls

```json
{
  "tool_calls": [
    {
      "id": "call-1",
      "function": {
        "name": "Read",
        "arguments": "{\"file_path\":\"README.md\"}"
      }
    }
  ]
}
```

Native calls are accumulated by source index, validated only after complete JSON is available, matched case-insensitively to `tools[]`, normalized using the matching input schema, and emitted once.

### 6.2 DSML text calls

The bridge recognizes the model-specific DSML tool wrapper and converts valid complete invocations to structured tool uses. Malformed blocks fail closed and request a bounded retry.

### 6.3 Compatibility markers

Canonical form:

```text
[Requesting Tool execution: 'ToolName' with arguments: {complete JSON object}]
```

Previously supported shorthand:

```text
[Requesting ToolName with arguments: {complete JSON object}]
```

New generic direct form:

```text
[Requesting ToolName: {complete JSON object}]
[Creating ToolName: {complete JSON object}]
```

The direct grammar is generic and validates the name against the request's real tool registry. It is not hard-coded to `CronCreate`.

Non-JSON formatter text such as:

```text
[Creating cron: */30 * * * *, prompt: verify, recurring: true]
```

is treated as protocol intent but is not guessed into arguments. It is redacted from client output and triggers bounded correction/retry. This prevents both marker leakage and accidental execution based on lossy prose parsing.

## 7. Parser context rules

A marker is executable only in ordinary assistant prose. The following contexts are inert:

- fenced code blocks,
- inline code,
- Markdown block quotes,
- escaped markers,
- JSON string examples.

The streaming parser maintains Markdown state across chunks. It retains only suffixes that can still become a marker, supports EOF finalization, bounds marker size and batch count, and resynchronizes after malformed markers without replaying an already emitted tool.

## 8. Reasoning versus visible text

Both streaming and synchronous paths now inspect:

```text
reasoning_content
content
```

A tool marker can appear in either channel. Marker wire text is removed before:

- emitting the client response,
- writing response history,
- writing reasoning history.

If the same semantic invocation appears in reasoning and visible text, or appears in native and text form, the bridge computes a fingerprint from case-normalized tool name plus normalized JSON arguments and emits it once.

## 9. Duplicate-execution rules

Within one assistant response turn, the following are duplicates when tool name and normalized arguments match:

- reasoning marker + visible marker,
- repeated visible marker,
- native call + marker,
- DSML call + marker,
- repeated native fragments that resolve to the same invocation.

Invariants:

```text
one semantic invocation -> one tool_use ID
one emitted tool block -> one content_block_start
one emitted tool block -> one content_block_stop
one client execution -> one matching tool_result
```

A malformed marker after an already emitted tool does not trigger a whole-turn retry, because replaying the turn could duplicate side effects.

## 10. False-success prevention

Before a `tool_result` exists, assistant text must not claim that the side effect succeeded.

Examples suppressed on a tool-use turn include:

```text
scheduled successfully
successfully created
has been created
completed successfully
đã tạo
đã chạy
đã hoàn thành
đã lên lịch
thành công
```

Streaming text with such a claim is retained while a potential marker is incomplete. If the response resolves to a tool-use turn, the unverified claim is omitted. The next model turn receives the real `tool_result` and can then report the result accurately.

This rule does not fabricate success or failure. It only prevents premature success narration.

## 11. Unknown and unavailable tools

A marker may be syntactically valid but request a tool absent from the current request's `tools[]`.

Policy:

1. Do not emit a `tool_use`.
2. Do not expose the raw marker.
3. Do not silently drop the intent.
4. Request a bounded correction using the exact available tool names.
5. If correction is exhausted, return an explicit upstream protocol error.

This applies equally to built-ins, interactive-only tools, disabled tools, and disconnected MCP tools.

## 12. Tool result and recap behavior

Claude Code checks tool-use/result consistency, including unique tool IDs and matching `tool_use_id` values. The executor builds `tool_result` content from the actual tool implementation.

Tool-use summaries are generated after results are collected. The embedded bundle maps each tool use to its corresponding result before summary generation.

The away-session recap is a separate model call with a fixed short-recap prompt and `canUseTool` forced to deny. A recap sentence is presentation metadata, not proof that a tool ran. The authoritative evidence remains the structured tool result and runtime artifact.

## 13. Root causes fixed

| Symptom | Root cause | Layer | Fix |
|---|---|---|---|
| Raw `[Requesting CronCreate: ...]` visible | Direct-colon grammar unsupported | Bridge parser | Generic direct marker grammar |
| Cron not created at first marker | Claude Code treats raw text as inert | Bridge/CLI boundary | Convert to structured `tool_use` |
| Marker repeated as prompt | Raw text entered conversation history | Bridge response sanitation | Remove marker from text/reasoning/history |
| Potential duplicate cron | Same intent repeated across channels | Bridge stream/sync | Semantic fingerprint dedupe |
| Marker in thinking ignored | Sync parsed visible content only | Bridge sync | Parse reasoning and visible channels |
| False “created” recap/text | Model narrated success before result | Bridge response policy | Suppress unverified success on tool-use turn |
| Unknown client-side tool leaked/dropped | Parser tied detection to known shorthand | Bridge parser | Generic intent detection + registry validation + bounded retry |
| Non-JSON `[Creating ...]` guessed or leaked | Formatter text not classified as protocol intent | Bridge parser | Fail closed; redact and retry |
| Native tool double stop regression risk | Multiple block finalizers | SSE tracker | Existing one-start/one-stop invariant retained and re-tested |

## 14. Regression assets

```text
tests/fixtures/claude_tool_markers.json
src/opencode/forward/stream/tests.rs
tests/protocol_conformance.rs
scripts/reverse_claude_tool_protocol.py
```

The fixture matrix covers:

- direct CronCreate after prose,
- marker in reasoning,
- EOF and fragmented SSE behavior,
- every valid UTF-8 split boundary,
- byte-fragmented HTTP/SSE transport,
- Unicode arguments,
- boolean `recurring=true`,
- consecutive and duplicate markers,
- valid then malformed,
- malformed then valid,
- fenced/inline/quote/escaped inert examples,
- unavailable tools,
- non-JSON Creating formatter text,
- false-success suppression.

## 15. Dynamic evidence

Harness:

```bash
python3 scripts/reverse_claude_tool_protocol.py
```

Artifacts:

```text
artifacts/claude-tool-protocol-reverse/dynamic/summary.json
artifacts/claude-tool-protocol-reverse/dynamic/direct-native.stdout.jsonl
artifacts/claude-tool-protocol-reverse/dynamic/direct-text-marker.stdout.jsonl
artifacts/claude-tool-protocol-reverse/dynamic/bridge-marker.stdout.jsonl
artifacts/claude-tool-protocol-reverse/dynamic/bridge-marker.bridge.log
artifacts/claude-tool-protocol-reverse/dynamic/*.requests.json
```

Verified lifecycle:

```text
CronCreate -> CronList -> CronDelete -> CronList
```

Both native and bridged paths ended with `No scheduled jobs.`

## 16. Security and correctness boundaries

- Tool names always come from the current request registry.
- Arguments must parse to JSON and are normalized against the real schema.
- Parser repair is bounded; prose-only formats are not guessed into side effects.
- Code examples remain inert.
- Search interception is limited to bridge-owned search tools.
- Client-side tools remain client-side; the bridge does not impersonate their runtime.
- Tool success requires a matching result and, where meaningful, runtime verification.
- No repository-wide reset, clean, checkout, or unrelated change was performed.

## 17. Model vs parser boundary audit (Claude Code 2.1.220)

The 2026-07-29 follow-up audit captured raw OpenAI SSE before parser conversion and classified failures by layer instead of treating every visible marker as a parser bug.

### Confirmed bridge defects

1. Reasoning containing a phrase such as “executed successfully” was held by the false-success buffer and flushed after final visible text. This reordered Anthropic blocks and could leave Claude Code's top-level `result` empty. Reasoning now buffers only possible protocol-marker suffixes; false-success buffering remains limited to visible content.
2. The free-model mapper converted prior `tool_use` and `tool_result` history into `[Requesting ...]` and `[Tool Result ...]` text. Direct OpenCode replay accepted native `assistant.tool_calls` plus `role=tool` in 5/5 attempts, so free-model history now remains native.
3. Inherited `socks5://` proxy URLs caused local DNS to select IPv6 for Cloudflare trace and IPv4 for ipify, producing false identity consensus failures. Proxy configuration and pool construction now normalize SOCKS5 to `socks5h://` remote DNS.

### Confirmed non-parser failures

- A Claude agent explicitly killed the temporary bridge process; the service did not crash.
- A cron verifier used separate sessions for create and list even though Claude cron jobs are session-only.
- An inline-code marker example was model formatting, not executable tool intent.
- Claude Code 2.1.220 removed `--max-turns`; the verification harness now detects option support.

### Production result

The final matrix on deployed port 4000 passed Cron, Task, Read/Edit/Bash, dynamic MCP, approval denial, and AskUserQuestion. Every tool use had one matching result; no duplicate IDs, orphan results, or executable-context marker leaks were observed.

Full evidence and classification table:

```text
artifacts/claude-tool-protocol-reverse/model-vs-parser/REPORT.md
artifacts/claude-tool-protocol-reverse/model-vs-parser/deployed-4000-final-matrix/summary.json
```
