# Super-Router Architecture Specification: Multi-Account, Combos & Native Providers

## 1. Executive Summary

This specification synthesizes the provider management, multi-account routing, and combo fallback mechanisms from 9Router (`https://github.com/decolua/9router`) into `opencode2claude` (`opencode2api`) as a native, zero-dependency, high-performance Rust subsystem.

By implementing this architecture directly in Rust:
1. **Single Binary & Ultra-low Overhead**: Replaces the bulky Node.js/Next.js stack with an ultra-fast, memory-safe daemon.
2. **Solves OpenCode `MissingSessionID`**: Directly integrates with local OpenCode servers (`http://127.0.0.1:4096`) using authenticated local sessions and tokens, preventing the cloud console anti-abuse block that breaks 9Router.
3. **Retains Anthropic/Claude Code Excellence**: Keeps `opencode2api`'s best-in-class Claude Code compatibility (`/v1/messages`, 1M context emulation `claude-sonnet-5[1m]`, thinking tag reassembly, DSML/XML tool use parsing).
4. **Leverages Built-in Proxy Pool**: Requests route through `opencode2api`'s active proxy pool to bypass Cloudflare and provider IP bans.

---

## 2. Core Architecture

```text
Claude Code CLI (claude)
   │ (Anthropic Messages API: /v1/messages)
   ▼
opencode2api Daemon (Port 4000)
   ├── 1. Anthropic Protocol & Parser Engine
   │     ├── Stream Chunking, Reasoning Delta Splitting
   │     └── Tool Calling & Parameter Accumulation
   │
   ├── 2. Combo & Fallback Engine (`src/router/combos/`)
   │     ├── Resolves `model` to single target or combo list
   │     ├── Strategies: `fallback` (sequential), `round-robin`
   │     └── Auto-switches to next model when upstream fails
   │
   ├── 3. Account Pool & Per-Model Cooldown (`src/router/accounts/`)
   │     ├── Multi-Account management per provider
   │     ├── Per-Model Lock (`model_lock_{model}`): rate limit on one model
   │     │   does NOT block other models on the same account
   │     └── Account selection: `FillFirst` (priority) or `StickyRoundRobin`
   │
   ├── 4. Rust Proxy Pool (`src/proxy_pool/`)
   │     └── Outbound requests route through verified healthy proxies
   │
   └── 5. Native Provider Connectors (`src/router/registry/`)
         ├── OpenCode: local daemon (port 4096) with session token
         ├── Cline: auto-loaded token from `~/.cline/`
         └── OpenAI-compatible: DeepSeek, Google Gemini, Groq, Ollama, etc.
```

---

## 3. Data Structures & Contract

### 3.1 Account & Model Lock
```rust
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub priority: u32,
    pub is_active: bool,
}

#[derive(Debug, Default)]
pub struct ModelLockTracker {
    // Key: (account_id, model_id) -> lock expiry Instant
    locks: Mutex<HashMap<(String, String), Instant>>,
}
```

### 3.2 Combo & Fallback Policy
```rust
#[derive(Debug, Clone)]
pub struct ModelCombo {
    pub name: String,
    pub models: Vec<String>,
    pub strategy: ComboStrategy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboStrategy {
    Fallback,
    RoundRobin,
}
```

### 3.3 Provider Registry
```rust
pub struct ProviderDefinition {
    pub id: &'static str,
    pub default_base_url: &'static str,
    pub auth_header: &'static str,
    pub auth_scheme: &'static str,
}
```

---

## 4. Error Handling & Cooldown Rules

Aligned with 9Router's proven cooldown classification:
- **HTTP 429 / Rate Limit**:
  - If `Retry-After` header or `resetsAtMs` is present in error body, lock for exact duration.
  - Otherwise, apply exponential backoff (1s, 2s, 4s ... max 4 minutes).
- **HTTP 401 / 403 (Invalid Key)**:
  - Mark account as permanently unavailable for that provider until updated.
- **HTTP 502 / 503 / 504 (Transient Server Overload)**:
  - Transient cooldown (1-2s) before falling back to the next account or model.
