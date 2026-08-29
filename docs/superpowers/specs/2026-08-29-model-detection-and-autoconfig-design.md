# OpenCode Free Model Detection, Dynamic Context Tuning & Model Management Design

## 1. Overview
This specification defines the architecture and behavior for:
1. **Dynamic Model Profiles & Context Tuning**: Calculating exact context windows and setting `CLAUDE_CODE_AUTO_COMPACT_WINDOW` to 80% of each model's maximum context length.
2. **Upstream Free Model Probing & Auto-Detection**: Filtering specifically for FREE models from OpenCode Zen (`*-free`, `big-pickle`, etc.), testing live availability, and auto-fallback to the best working free model when models expire or become unavailable.
3. **CLI Management**:
   - `opencode2api list` / `opencode2api models`: Displays free model catalog, context window, 80% autocompact window, thinking support, and live availability status.
   - `opencode2api model set <model>` / `opencode2api model use <model>`: Saves selected model to config.
   - `opencode2api model` / `opencode2api model status`: Displays active model profile.
4. **Out-of-the-Box Claude Code Launcher**: Bare `opencode2api` dynamically sets optimal environment variables for Claude Code based on the active model.

## 2. Free Model Profiles & Auto-Compact Formulation (80%)
Every free model supported by OpenCode has a `ModelProfile`:
- `id`: Canonical identifier (e.g. `mimo-v2.5-free`, `nemotron-3-ultra-free`, `big-pickle`).
- `context_window`: Total token context capacity.
- `max_output_tokens`: Maximum generation token limit.
- `auto_compact_window`: Evaluated strictly as `context_window * 80 / 100`.
- `supports_thinking`: Boolean indicating native reasoning capability.
- `disable_1m_context`: `0` if `context_window >= 1_000_000`, else `1`.
- `disable_thinking`: `0` if `supports_thinking` is true, else `1`.

### Free Model Matrix
- `opencode/mimo-v2.5-free` (or `mimo-v2.5-free`): Context 256,000 -> Auto-compact: 204,800. Max output: 64,000. Thinking: true.
- `opencode/nemotron-3-ultra-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: true.
- `opencode/nemotron-3.5-lightning-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: true.
- `opencode/deepseek-v4-flash-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: true.
- `opencode/big-pickle`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: true.
- `opencode/hy3-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: false.
- `opencode/ling-3.0-flash-fin-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: false.
- `opencode/laguna-s-2.1-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: false.
- `opencode/muse-spark-1.2-contributor-free`: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: false.
- Default fallback for other free models: Context 128,000 -> Auto-compact: 102,400. Max output: 16,384. Thinking: false.

## 3. Dynamic Environment Generation
When launching Claude Code via `opencode2api` or querying `opencode2api env`:
```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:<bridge_port>"
export ANTHROPIC_API_KEY="<api_key>"
export OPENAI_BASE_URL="http://127.0.0.1:<bridge_port>/v1"
export OPENAI_API_KEY="<api_key>"
export OPENCODE_MODEL="<active_model>"
export ANTHROPIC_MODEL="<anthropic_model_alias>"
export CLAUDE_CODE_AUTO_COMPACT_WINDOW="<auto_compact_window>"
export CLAUDE_CODE_MAX_OUTPUT_TOKENS="<max_output_tokens>"
export CLAUDE_CODE_DISABLE_1M_CONTEXT="<0 or 1>"
export CLAUDE_CODE_DISABLE_THINKING="<0 or 1>"
unset ANTHROPIC_AUTH_TOKEN
```

## 4. Free Model Probing & Auto-Detection
- Filter free models from upstream `/models` (matching `*-free` or recognized free catalog IDs like `big-pickle`).
- Probe queries `upstream_base_url/chat/completions` with a dry/minimal ping (1 token).
- Statuses:
  - `Online`: HTTP 200 OK.
  - `RateLimited`: HTTP 429.
  - `Unavailable`: HTTP 400 / 500 with "Model is unavailable" or "not supported".
- Auto-detect selects the first responsive free model if current configured model is offline.

## 5. CLI Interface
- `opencode2api list [--probe] [--json]`: Lists free models in a formatted table.
- `opencode2api model set <model>`: Writes the `model = "<model>"` entry to user config.
- `opencode2api model [get]`: Shows current active model and its calculated parameters.
- `opencode2api`: Launches Claude Code with calculated environment parameters.
