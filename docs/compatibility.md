# Anthropic compatibility

OpenCode2API implements the Anthropic Messages subset required by Claude Code-style clients and maps it to an OpenAI-compatible Chat Completions upstream.

## Requests

| Anthropic field | Support | Behavior |
|---|---|---|
| `model` | Supported | Mapped to the configured upstream model name. |
| `messages` | Supported | String and typed content arrays are mapped. |
| `system` | Supported | String and text-block arrays are flattened safely. |
| `stream` | Supported | Produces Anthropic-compatible server-sent events. |
| `temperature` | Supported | Forwarded when present. |
| `max_tokens` | Supported | Preserved; reasoning streams may enforce the configured minimum. |
| `tools` | Supported | Mapped to OpenAI function tools. |
| `tool_choice` | Supported | Forwarded as the corresponding upstream choice object. |
| text content | Supported | Preserved and system-leakage tags are stripped outside compaction requests. |
| `tool_use` content | Supported | Converted to assistant tool calls. |
| `tool_result` content | Supported | Converted to upstream tool-result messages while preserving ordering. |
| image/document blocks | Not advertised | Unsupported blocks are not part of the verified contract. |

Invalid JSON, missing messages, empty message arrays, oversized bodies, and unsupported routes produce bounded client errors without stack traces or upstream bodies.

## Responses

Synchronous responses contain:

- `type: message`;
- assistant role;
- thinking blocks before visible text when reasoning is present;
- text blocks;
- native or DSML-derived `tool_use` blocks;
- `end_turn`, `tool_use`, or `max_tokens` stop reasons;
- mapped input/output usage.

Streaming responses follow this sequence:

```text
message_start
content_block_start
content_block_delta ...
content_block_stop
message_delta
message_stop
```

Reasoning and text blocks do not overlap. Tool-use blocks close active thinking/text blocks before opening. Duplicate upstream `[DONE]` markers, malformed data lines, fragmented UTF-8, fragmented JSON arguments, and premature EOF are handled without panic. Upstream line and body sizes are bounded.

## Tool handling

### Native tools

Anthropic tools are sent upstream as OpenAI function definitions. Streaming argument fragments are emitted as Anthropic `input_json_delta` events. Tool name casing is resolved against the names supplied in the original Anthropic request.

### DSML tools

Some models emit tool calls as DSML text. The DSML parser extracts invocation names and parameters, converts them to `tool_use`, and removes the DSML envelope from visible text. The DSML streaming buffer is capped to prevent unbounded allocation.

### Shell delegation

A final user message beginning with `!` can be transformed into a client-side tool request according to the shell policy. `disabled` is the default. `allowlist` rejects unknown commands and shell metacharacters. The bridge itself does not execute arbitrary request-provided shell commands in HTTP handlers.

## Reasoning models

Reasoning aliases `reasoning_content`, `reasoning`, and `thinking` are accepted from upstream. For streaming reasoning requests, implicit fallback to non-reasoning defaults is disabled unless explicit compatible fallbacks are configured.

## Search interception

Native or DSML web-search calls can be intercepted. Results are fetched through the typed provider chain and injected as an assistant `tool_use` turn followed by a user `tool_result` turn. Search loops, result count, snippet length, provider response body, and provider duration are bounded.

## Token counting

`POST /v1/messages/count_tokens` returns an explicit estimate, not a provider-authoritative tokenizer count. Metrics and responses should therefore be interpreted as estimates where upstream usage is unavailable.

## Conformance evidence

`tests/protocol_conformance.rs` covers sync and SSE behavior through the production router and a controlled upstream server, including cancellation and overflow. `tests/parser_fuzz_smoke.rs` exercises malformed JSON, DSML, search, and config parser inputs.
