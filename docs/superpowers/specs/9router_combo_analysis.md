# 9router: Combos, Model Fallback Chains & Capacity Adapters

**Files analyzed**

- `open-sse/services/combo.js` — shared combo handling (fallback, round-robin, fusion)
- `open-sse/services/capacityAdapter.js` — global capability fallback pools + context stripping
- `src/sse/services/model.js` — model-string parsing / combo detection (localDb-integrated re-export)
- Supporting context: `open-sse/services/accountFallback.js` (error classification), `src/sse/handlers/chat.js` (wiring)

---

## 1. Big Picture

9router exposes a single OpenAI-style model parameter that can name three different things:

1. **A provider model** — `provider/model` (e.g. `openai/gpt-4o`).
2. **An alias** — resolved through user alias maps (`getModelAliases` / `localDb`).
3. **A combo** — a user-defined *named list of models* that is executed as a fallback chain, a round-robin rotation, or a "fusion" panel+judge ensemble.

Resolution order in `src/sse/services/model.js::getModelInfo()`:

```
parseModel(modelStr)
  ├─ not an alias → try user-defined provider nodes (openai-compatible /
  │                  anthropic-compatible / custom-embedding), where the prefix
  │                  must NOT shadow a reserved built-in provider id/alias
  │                  (RESERVED_PROVIDER_PREFIXES) → { provider, model }
  ├─ is alias     → check getComboByName() FIRST → if it matches a combo,
  │                  return { provider: null, model } (a sentinel meaning
  │                  "handle as combo", detected by handleChat)
  └─ otherwise    → core alias resolution (getModelInfoCore + alias map)
```

`getComboModels(modelStr)` only treats a name as a combo when it contains **no `/`** — a slash always means provider/model and is never a combo name. This localDb layer adds provider-node and combo resolution on top of the shared `open-sse` core, plus one local provider-alias override (`xmtp → xiaomi-tokenplan`).

Once a request is identified as a combo (or as a single model that a **Capacity Adapter** augments), `open-sse/services/combo.js` takes over. The overall flow in `src/sse/handlers/chat.js`:

```
handleChat(body, modelStr)
  1. requiredCapabilities = detectRequiredCapabilities(body)      (combo.js)
  2. comboModels = getComboModels(modelStr)                        (model.js)
     ├─ combo found:
     │    strategy = comboStrategies[name].fallbackStrategy
     │              || settings.comboStrategy || "fallback"
     │    augmented = augmentModelsWithCapacityAdapter(...)         (capacityAdapter.js)
     │    ├─ strategy == "fusion" → handleFusionChat(...)           (combo.js)
     │    └─ else              → handleComboChat(...)  wrapped with
     │                           withCapacityAdapterStripping(...)   (capacityAdapter.js)
     └─ no combo (single model):
          soloAugmented = augmentModelsWithCapacityAdapter([modelStr], ...)
          ├─ len > 1 → handleComboChat over [adapter models..., original],
          │             strategy = getActiveAdapterStrategy(...)
          └─ len == 1 → plain handleSingleModelChat
```

---

## 2. Combos & the Fallback Chain (`handleComboChat`)

A combo is `{ name, models: string[] }` stored in the settings DB (resolved via `getComboModelsFromData`, which accepts both an array of combos and `{ combos: [...] }`).

### 2.1 Execution pipeline

`handleComboChat({ body, models, handleSingleModel, log, comboName, comboStrategy, comboStickyLimit = 1, autoSwitch = true })` runs three ordered transformations on the model list, then walks it:

1. **Rotation** (`getRotatedModels`) — applies round-robin if `comboStrategy === "round-robin"` (§3).
2. **Auto-switch** (`reorderByCapabilities`) — if `autoSwitch` (default true) and the request needs capabilities, floats capable models to the front (§4.2). Logs `auto-switch for [caps] → <model>` when the head changes.
3. **Sequential fallback loop** — for each model in order:

```
result = await handleSingleModel(body, modelStr)
  ├─ result.ok (2xx)  → return result immediately (first success wins)
  ├─ non-2xx:
  │    parse error body (clone().json()) → errorText, retryAfter
  │    track earliest retryAfter across all models
  │    { shouldFallback, cooldownMs } = checkFallbackError(status, errorText)
  │    ├─ !shouldFallback → return the failure response as-is (terminal error)
  │    └─ shouldFallback && cooldownMs ≤ 5000 && status ∈ {502,503,504}
  │          → sleep(cooldownMs) before trying the next model
  │            (gives a briefly-overloaded provider a chance to recover
  │             instead of being skipped — "transient 503" fix)
  │    → record lastError / lastStatus, continue to next model
  └─ exception thrown → record it, status treated as 500, continue
```

### 2.2 Error classification (`accountFallback.checkFallbackError`)

Config-driven via `ERROR_RULES` in `config/errorConfig.js`, matched top-to-bottom: **text rules first** (substring match, case-insensitive), then **status rules**. Rules may carry:

- `backoff: true` → exponential backoff cooldown: `base * 2^(level-1)`, capped at `BACKOFF_CONFIG.max` (1s, 2s, 4s … max 4 min).
- fixed `cooldownMs` otherwise.
- The **default for any unmatched error** is `{ shouldFallback: true, cooldownMs: TRANSIENT_COOLDOWN_MS }` — i.e. the chain is optimistic: almost everything falls through.

### 2.3 All-models-failed response

When the chain is exhausted:

- `status = 503` if the last error mentioned "no credentials" (all accounts disabled), else the **first** failure status recorded, else 503. The comment explains the choice: 503 (Service Unavailable) is retryable and accurate, whereas 406 would imply the request itself is invalid.
- If any model returned a `retryAfter`, the **earliest** one is surfaced via `unavailableResponse(status, msg, retryAfter, formatRetryAfter(...))` so clients know the soonest the combo could work ("reset after 2m 30s").
- Error text is normalized to a string in a Worker-safe way (`JSON.stringify` fallback → `String`).

### 2.4 Properties of the chain

- **Order matters, first success wins**; a model that returns a terminal (non-fallback) error short-circuits the chain and its response is passed through unchanged.
- The chain is **per-request** (no cross-request circuit breaker in combo.js itself; account-level cooldowns live in the account fallback layer).
- `handleSingleModel` is injected — the same engine also serves `search.js`, `fetch.js`, `imageGeneration.js`, `tts.js` handlers, so the fallback semantics are uniform across endpoints.

---

## 3. Combo Strategies

Per-combo strategy lives in `settings.comboStrategies[name].fallbackStrategy` with a global `settings.comboStrategy` default of `"fallback"`. Three values are meaningful: `fallback`, `round-robin`, `fusion`.

### 3.1 Round-robin with sticky limit

`getRotatedModels(models, comboName, strategy, stickyLimit)`:

- No-op unless the strategy is `round-robin` and there are ≥ 2 models.
- **Rotation state is a module-level in-memory `Map`** keyed by combo name (`"__default__"` when unnamed): `{ index, consecutiveUseCount }`. Legacy numeric entries are migrated on read.
- Each call rotates the array so `models[index]` is first, then increments `consecutiveUseCount`.
- **Sticky limit** (`settings.comboStickyRoundRobinLimit`, normalized via `normalizeStickyLimit`, default 1): the same head model is reused for `stickyLimit` consecutive requests before the index advances. With limit = 1 it is a pure round-robin (one request per model per cycle).
- `resetComboRotation(comboName?)` clears one entry or the whole map — invoked when combos/settings change so rotation state never goes stale.
- Note the state is **per-process memory**: in multi-instance deployments rotation is not globally coordinated.

### 3.2 Fusion (panel + judge) — `handleFusionChat`

Fusion trades latency for quality: every panel model answers in parallel and a judge model synthesizes one final answer (modeled after OpenRouter's Fusion).

**Tuning** — `FUSION_DEFAULTS`, overridable per combo via `settings.comboStrategies[name].fusionTuning`:

| Key | Default | Meaning |
|---|---|---|
| `minPanel` | 2 | answers needed before stragglers get a grace window |
| `stragglerGraceMs` | 8000 | wait this long for laggards once quorum is reached |
| `panelHardTimeoutMs` | 90000 | absolute cap so one hung model can't stall forever |

**Pipeline:**

1. **Edge cases**: empty panel → 400; single model → answered directly (nothing to fuse). Judge = `judgeModel` param or `panel[0]`.
2. **Panel body rewrite**: tools (`tools`, `tool_choice`, `stream_options`) are stripped and `stream: false` forced — panel models must return complete prose. Two documented gotchas: DeepSeek rejects `stream_options` without `stream:true` (issue #3024), and the judge needs finished text. Additionally `flattenToolHistory()` rewrites prior tool turns into plain prose:
   - `tool`/`function` role messages → assistant text `[Tool result: …]`.
   - Assistant `tool_calls` → text with appended `[Called tools: name1, name2]`.
   - Claude-style content blocks containing `tool_use`/`tool_result` → same prose treatment, preserving text blocks.
   This keeps panel context intact while preventing panel models from looping on tools (they can't emit tool_calls).
   (The chat.js caller additionally strips tools from `clientRawRequest.body` for panel calls via the `isPanel` flag.)
3. **Fan-out**: `panel.map(m => withTimeout(handleSingleModel(panelBody, m, true), panelHardTimeoutMs))`. `withTimeout` resolves `{__timeout:true}` on expiry — the loser keeps running but its result is ignored (no cancellation).
4. **Quorum-grace collection** (`collectPanel`): resolves a sparse array aligned to the calls. Finishes when **all** calls settle, **or** when `ok >= minPanel` successes have arrived and `stragglerGraceMs` elapses, **or** at the hard timeout — whichever comes first. This caps the straggler penalty (the slowest model otherwise dominates wall time) while preferring a full panel.
5. **Answer extraction** (`extractPanelText`): parses the *client-format* JSON (panel responses have already been translated by chatCore) across OpenAI chat (`choices[0].message/delta.content`, `.text`), Claude messages (`content` text blocks), Gemini (`candidates[0].content.parts[].text`), and OpenAI Responses (`output[].content[].text`). Empty/unparseable/failed panels are logged and skipped.
6. **Graceful degradation**: 0 answers → 503 `"All fusion panel models failed"`; exactly 1 → re-run `handleSingleModel` with the **original** body on the surviving model (full tools/streaming restored, no fake synthesis).
7. **Judge call** (`buildJudgePrompt` + `appendUserTurn`): the judge receives the original conversation plus one appended user turn containing anonymized `[Source N]` texts. The directive: analyze consensus / contradictions / partial coverage / unique insights / blind spots, then write **one** authoritative answer without revealing that multiple models were used. Sources are anonymized so the judge weighs substance, not brand reputation. The judge call keeps the client's original `stream` flag and tools, so streaming and downstream tool use still work. (The comment notes ~3/4 of fusion's quality lift comes from this synthesis step.)

`appendUserTurn` handles all known request shapes (`messages`, `input`, `contents`) and fabricates a minimal `messages` array if none matched.

---

## 4. Capability Detection & Auto-Switch (combo.js)

### 4.1 Hard vs soft capabilities

```js
HARD_CAPS = { "vision", "pdf", "audioInput", "videoInput" }  // input modalities
```

Hard capabilities are about **request data**: if a model lacks one, the data must be dropped/stripped (e.g. image removed), which is destructive — so they must be prioritized. Soft capabilities (e.g. `search`) only degrade a feature. (`search` is currently disabled in auto-switch: "feature not wired yet".)

### 4.2 `detectRequiredCapabilities(body)`

Returns a `Set` of required capability strings. Design decisions:

- **Modalities are scanned only on the *current user turn*** — the trailing run of messages after the last assistant/model turn (`trailingUserItems`). History media in older turns must *not* pin the combo to a vision model, because history media gets stripped + placeholdered downstream. The trailing run may span several messages (text + image split across blocks).
- Scans every known request shape: `body.messages` (openai/claude/hermes/ollama), `body.input` (Responses), `body.contents` / `body.request.contents` (gemini/antigravity, via `parts`).
- Detection sources per message: content blocks (`image_url`/`image`/`input_image`, `input_audio`/…, `input_video`/…, `file`/`document`/`input_file` with MIME inference — generic files default to `pdf`), Ollama `images` arrays, Vercel AI SDK `experimental_attachments` / `attachments`, message-level `image_url`/`audio_url` properties, Gemini `inlineData.mimeType` / `fileData.mimeType`, and embedded `data:` URIs inside string content.
- `search` would come from `tools`, request-wide, but is disabled for now.

### 4.3 `reorderByCapabilities(models, required)`

A **stable, non-dropping** reorder used by the auto-switch step:

- **Tier 0**: model satisfies all hard + all soft requirements.
- **Tier 1**: satisfies all hard only.
- **Tier 2**: everything else (will fall back / have data stripped).

Stable sort (tier, then original index) so the user's configured order is preserved within a tier and fallback semantics remain intact — no model is ever removed. Capability lookup is `getCapabilitiesForModel(provider, model)` after splitting on the first `/`.

Interaction with capacity adapters: if the *original* combo already contains a model covering the requirement, reordering alone suffices and the adapter stays out of the way (§5.3).

---

## 5. Capacity Adapters (`capacityAdapter.js`)

Capacity adapters are **global fallback pools of models per hard capability** (`vision`, `pdf`, `audioInput`, `videoInput`), configured under `settings.capacityAdapter[cap]`. They rescue requests whose target/combo models can't handle the input modality at all.

### 5.1 Configuration model

- `normalizeCapEntry` accepts the current object form `{ enabled, roundRobin, models }` and the **legacy array form** `[{model, enabled}]` (treated as enabled, non-round-robin fallback). Anything else → disabled/empty.
- `getCapacityAdapterConfig(cap, settings)`: an **enabled pool with no models falls back to `DEFAULT_FALLBACK_MODEL` = `"oc/mimo-v2.5-free"`** — so the UI toggle is never a silent no-op.
- `getCapacityAdapterModels(settings)`: flattens enabled pools across all four capabilities **in fixed priority order** (vision → pdf → audioInput → videoInput), deduped.
- `getCapacityAdapterStrategy(cap, settings)`: `"round-robin"` if enabled+roundRobin, else `"fallback"`.
- `getActiveAdapterStrategy(requiredCapabilities, settings)`: picks the strategy from the **first hard capability** in the request that has an enabled, non-empty pool; defaults to `"fallback"`. Used when a single-model request is augmented into an ad-hoc combo (§5.3).

### 5.2 `augmentModelsWithCapacityAdapter(models, requiredCapabilities, settings)`

The core prepend rule:

- Only reacts to **hard** capabilities in the request.
- No-op if `models` is empty or if **any** original model already satisfies all hard requirements (`modelSatisfies` via `getCapabilitiesForModel`) — the combo's own auto-switch handles that case.
- Otherwise: filter the flattened adapter pool to models that (a) satisfy the requirements and (b) aren't already in the list, and **prepend them**: `[...pool, ...models]`. Adapter models go *first* (priority — otherwise the request would die on every incapable model first), original models follow as fallback.
- If no pool model qualifies, the list is untouched.

### 5.3 Context stripping — `stripHistoryForContext` + `withCapacityAdapterStripping`

Adapter rescue models are typically small/free, so the request that was meant for a big model may not fit. Two pieces handle this:

**`stripHistoryForContext(body, contextWindow)`** — trims history to fit by **dropping the middle**:

- Works on `messages` / `input` / `contents` (whichever array exists).
- Preserves: (1) all `system`/`developer` messages, (2) the first `HEAD_KEEP = 6` non-system messages as initial context, (3) the trailing user run (the current turn — the one carrying the media that triggered the adapter switch) — always kept.
- Budget: `contextWindow * 0.8 * CHARS_PER_TOKEN (4)` — 80% of the adapter model's window, leaving room for the response. Sizes are rough char counts (non-text blocks counted as 50 chars); this deliberately avoids a tokenizer dependency.
- If even head+tail overflows, head turns are dropped from the end (closest to the middle) first.
- No-op if nothing was dropped (returns the original body object).

**`withCapacityAdapterStripping(handleSingleModel, adapterModels)`** — a higher-order wrapper: for calls routed to an *adapter* model (matched against the set of models the augment step actually **added**, `adapterAdded`), it first computes the model's `contextWindow` from capabilities and strips history; all other calls pass through untouched. Returns the original callback when the adapter set is empty, so non-augmented paths pay zero cost. Note fusion does *not* use this wrapper — panel/judge calls go through a different lambda in chat.js.

---

## 6. How It All Fits Together (chat.js wiring)

For a combo request:

1. Strategy is resolved per-combo → global → `"fallback"`.
2. `augmentModelsWithCapacityAdapter(comboModels, required, settings)` may prepend rescue models; the added ones (`adapterAdded`) are remembered.
3. **Fusion**: `handleFusionChat` receives the *unaugmented* combo list; panel calls strip tools via the `isPanel` flag on `handleSingleModel`; `judgeModel` and `fusionTuning` come from `comboStrategies[name]`.
4. **Fallback / round-robin**: `handleComboChat` receives the augmented list, and `handleSingleModel` is wrapped with `withCapacityAdapterStripping(..., adapterAdded)` so only adapter-routed calls get history stripped. `comboStickyLimit` comes from `settings.comboStickyRoundRobinLimit`.

For a **single-model** request: `augmentModelsWithCapacityAdapter([modelStr], ...)`; if it grew, the same combo machinery runs over `[adapter..., original]` with `getActiveAdapterStrategy(...)` (so an adapter pool configured as round-robin actually rotates) — otherwise the request goes straight to `handleSingleModelChat`.

There's a second, mirrored code path inside `handleSingleModelChat` itself: when `getModelInfo` returns `{ provider: null }` (a combo name reached *through* alias resolution — e.g. a combo referenced by another combo or by an alias), the exact same strategy/augmentation/dispatch logic runs again. This nesting is what lets combos be referenced from aliases.

Other handlers (`search.js`, `fetch.js`, `imageGeneration.js`, `tts.js`) reuse `handleComboChat` directly for their combo targets.

---

## 7. Key Invariants & Design Notes

1. **Never drop a fallback candidate.** `reorderByCapabilities` only reorders (stable sort); `augmentModelsWithCapacityAdapter` only prepends; the combo loop only advances on fallback-eligible errors. Configured order is respected within capability tiers.
2. **Hard caps are about the payload; soft caps are about features.** Hard-cap satisfaction is checked structurally (capability tables), not by trying the request.
3. **Current-turn media decides routing; history media doesn't.** `detectRequiredCapabilities` scans only the trailing user run; older media is stripped/placeholdered downstream. This prevents stale images in history from pinning every request to a vision model.
4. **Adapter pools are a last-resort *priority* insert.** They only engage when *nothing* original can serve the request, and then they go *first* — but the originals remain behind them as fallback, and adapter calls alone pay the history-stripping tax.
5. **Fail-open error policy.** `checkFallbackError` defaults to `shouldFallback: true`; only explicit rules can make an error terminal. Transient 502/503/504 additionally get a short cooldown sleep (≤ 5s) so a blip doesn't cause a needless cascade.
6. **Graceful degradation everywhere.** Combo exhausted → 503 + earliest retryAfter. Fusion: 0 answers → 503, 1 answer → answered directly, ≥2 → judged. Adapter pool empty → toggle falls back to a default model rather than doing nothing.
7. **State is process-local.** Round-robin rotation (`comboRotationState`) and backoff levels live in memory; `resetComboRotation` exists because settings changes can invalidate them. Multi-worker deployments won't share rotation position.
8. **Format-agnostic plumbing.** Every step handles the four request shapes (`messages`, `input`, `contents`, plus Hermes/Ollama variants) — combo loops, tool-history flattening, judge-turn appending, and history stripping are all shape-polymorphic.

### Observations / potential caveats

- `withTimeout` doesn't cancel the underlying panel request; a hung model keeps consuming resources until its own request completes.
- `collectPanel` counts only `res.ok` toward quorum; a flood of fast *failures* can trigger the grace timer early, which is the desired behavior but worth knowing when tuning `minPanel`.
- Fusion runs panel calls **without** capacity augmentation or capability reordering (`models: comboModels` in chat.js) — a panel model lacking a hard capability would have its data stripped downstream rather than being swapped.
- The duplicated combo-dispatch block in `chat.js` (`handleChat` vs `handleSingleModelChat`) is intentional for alias→combo nesting but is a maintenance point to keep in sync.
- `augmentModelsWithCapacityAdapter` dedupes against the original list but not against *alias* duplicates (same underlying model under different names could appear twice).
- `stripHistoryForContext`'s char-based budgeting is approximate (50 chars per non-text block) — fine for gating, not for precise token accounting.
