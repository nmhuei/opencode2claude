# 9router Protocol Translation & Streaming Analysis

**Sources inspected:** `open-sse/translator/index.js`, `open-sse/translator/request/`, `open-sse/translator/response/`, `open-sse/translator/concerns/`, `open-sse/translator/schema/`.

---

## 1. Hub-and-Spoke Translation Architecture

9router pivots all format translation through **OpenAI Chat Completions** as the canonical intermediate representation:

```text
Source Client (Claude / Gemini / OpenAI)
    │
    ▼ (Request Translation)
Canonical OpenAI Body (`messages`, `tools`, `model`, `stream`, etc.)
    │
    ▼ (Provider Request Translation)
Upstream Provider Format (Claude Messages / Gemini Content / OpenAI)
    │
    ▼ [Upstream Response SSE Stream]
Upstream Provider Chunk
    │
    ▼ (Response Translation)
Canonical OpenAI Chunk (`choices[0].delta`, `reasoning_content`, `tool_calls`)
    │
    ▼ (Client Response Translation)
Client Chunk (Anthropic SSE: `content_block_delta`, `message_delta`, etc.)
```

### Direct Routes
When source format and upstream target format match (e.g. `claude:claude` or `openai:openai`), the translator detects a direct route and bypasses double translation to preserve high-fidelity fields (such as thinking signatures, tool call IDs, and raw cache control markers).

---

## 2. Key Translation Concerns

### 2.1 Thinking & Reasoning Content (`concerns/thinkingUnified.js`)
- Maps `reasoning_content` (OpenAI/DeepSeek), `thinking: { type: "enabled", budget_tokens }` (Claude), and `<thought>` XML blocks.
- In streaming:
  - When switching from reasoning to visible text, emits proper boundary transitions.
  - Generates synthetic `thinking` blocks for Claude clients when upstream returns `reasoning_content`.

### 2.2 Tool Calling & Function Calling (`concerns/toolCall.js`)
- Translates between:
  - Claude: `tool_use` content blocks with JSON inputs, and `tool_result` responses.
  - OpenAI: `tool_calls` array with `function: { name, arguments: "{...}" }`.
- Handles argument fragmentation: accumulates partial JSON fragments during SSE streaming until complete, ensuring valid JSON parsing.

### 2.3 Context Truncation & Cache Control
- Preserves Claude's `cache_control: { type: "ephemeral" }` metadata across message blocks.
- Strips unsupported parameters (e.g., `frequency_penalty`, `presence_penalty`) when converting from OpenAI to Claude.
