# Critical Fix Plan — CLI v2 & Streaming Refactor Recovery

Date: 2026-07-03  
Repo: `opencode2api`  
Scope: restore build correctness, fix broken CLI semantics, and add regression gates so the same class of bugs cannot return.

---

## 0. Executive Summary

The current working tree has two categories of critical issues:

1. **Build blocker**: the repo no longer compiles from a clean build because `src/opencode/forward.rs` still contains partially migrated streaming-state code after introducing `src/stream_tracker.rs`.
2. **CLI contract bugs**: the CLI v2 command tree exists, but several commands do not behave according to the spec:
   - daemon `server start` drops many CLI options;
   - `--json` does not reliably output JSON on error;
   - `--quiet` still prints human dashboards;
   - invalid `--shell-policy` silently downgrades to disabled instead of failing at parse time;
   - shell completions expose hidden legacy aliases;
   - docs still describe old flat commands as primary commands.

This plan fixes the repo in phases. Do not start CLI cosmetic work before the compile blocker is resolved.

---

## 1. Non-Negotiable Rules

```text
1. Do not disable tests, clippy, or verification gates to make the repo pass.
2. Do not use #[allow(dead_code)] / #[allow(unused)] as a shortcut for production code.
3. Do not change runtime security defaults to make demos easier.
4. Do not remove backward-compatible aliases unless explicitly marked in CHANGELOG.
5. Do not let JSON mode print human text, ANSI spinners, or hints outside JSON.
6. Do not let quiet mode print dashboards/tables/proxy lists.
7. Do not expose secrets from logs in docs, tests, examples, or generated output.
8. Do not touch warm-standby proxy ports 40004-40005 in restart/purge flows.
9. Each phase must end with a runnable validation command.
10. A phase is not done until its acceptance criteria pass from a fresh cargo invocation.
```

---

## 2. Current Known Failures

### 2.1 Build blocker

Command:

```bash
cargo check --locked
```

Observed failure class:

```text
error[E0425]: cannot find value `idx` in this scope
error[E0425]: cannot find value `thinking_block_index` in this scope
error[E0425]: cannot find value `text_block_index` in this scope
error[E0067]: invalid left-hand side of assignment: tracker.next_index() += 1
```

Affected file:

```text
src/opencode/forward.rs
```

Related new file:

```text
src/stream_tracker.rs
```

Root cause:

```text
The stream state refactor is incomplete. forward.rs now imports and partially uses SseBlockTracker, but old loose variables remain in several branches.
```

### 2.2 CLI option propagation bug

Command shape accepted by help:

```bash
opencode2api server start --model <MODEL> --config <PATH> --shell-policy <POLICY>
```

Actual behavior:

```text
Foreground mode passes full args.
Daemon mode only passes port/host/tls to the supervisor child.
```

Affected files:

```text
src/main.rs
src/supervisor.rs
src/cli.rs
```

### 2.3 JSON contract bug

Example:

```bash
opencode2api --json server start
```

When already running, output is human text on stderr instead of structured JSON.

### 2.4 Quiet contract bug

Example:

```bash
opencode2api --quiet server status
```

Current behavior still prints a human dashboard.

### 2.5 Invalid CLI enum bug

Example:

```bash
opencode2api server start --shell-policy typo -f --port 49999
```

Current behavior warns and runs with `disabled`. CLI should fail at parse time.

### 2.6 Completion hidden-alias bug

Generated completions currently include hidden legacy commands:

```text
serve start status stop restart logs
```

### 2.7 Documentation drift

`docs/cli.md` still presents flat commands as primary commands, while the CLI v2 spec says the primary command tree is:

```text
server start|stop|status|restart|logs|config
proxy ps|restart|purge|logs
env
doctor
completion
update
init
```

---

## 3. Branch Strategy

Use a dedicated branch.

```bash
git checkout main
git pull origin main
git checkout -b fix/cli-v2-critical-recovery
```

Before editing, record the current dirty state:

```bash
git status --short
git diff --stat
```

If there are uncommitted user changes, preserve them before large edits:

```bash
git diff > /tmp/opencode2api-pre-cli-fix.diff
```

---

## 4. File Ownership

### Phase 0 owner: streaming/runtime implementer

```text
src/opencode/forward.rs
src/stream_tracker.rs
src/lib.rs
```

### Phase 1-4 owner: CLI implementer

```text
src/cli.rs
src/main.rs
src/supervisor.rs
src/output.rs
src/config.rs
```

### Phase 5 owner: tests implementer

```text
tests/cli.rs
Cargo.toml
```

### Phase 6 owner: docs implementer

```text
docs/cli.md
README.md
CLAUDE.md
CHANGELOG.md
verification/phases/*.md
```

### Phase 7 owner: reviewer/security

```text
src/shell.rs
src/config.rs
src/middleware.rs
src/main.rs
src/supervisor.rs
src/opencode/forward.rs
```

Avoid simultaneous edits to `src/main.rs`, `src/supervisor.rs`, and `src/opencode/forward.rs` by different agents.

---

## 5. Phase 0 — Restore Compile Correctness

### 5.1 Goal

Make the repo compile again from a fresh cargo invocation.

Target commands:

```bash
cargo check --locked
cargo test --locked stream_tracker
```

### 5.2 Diagnosis

In `src/opencode/forward.rs`, old variables are still referenced:

```rust
thinking_block_index
text_block_index
idx
```

Invalid pattern:

```rust
tracker.next_index() += 1;
```

`SseBlockTracker::next_index()` already increments internally:

```rust
pub fn next_index(&mut self) -> usize {
    let i = self.next_idx;
    self.next_idx += 1;
    i
}
```

### 5.3 Patch Plan

#### Step 0.1 — Fix thinking delta branch

Find the block handling:

```rust
choice.delta.reasoning_content
```

Replace broken JSON index usage:

```rust
"index": idx
```

with:

```rust
"index": thinking_idx
```

Use the existing result of:

```rust
let (thinking_idx, thinking_is_new, closed_text) = tracker.ensure_thinking();
```

Expected pattern:

```rust
let (thinking_idx, thinking_is_new, closed_text) = tracker.ensure_thinking();
if let Some(closed) = closed_text {
    let _ = tx.send(crate::sse::emit_block_stop(closed)).await;
}
if thinking_is_new {
    let _ = tx
        .send(builder.content_block_start(thinking_idx, "thinking", None, None))
        .await;
}
let delta_ev = Event::default()
    .event("content_block_delta")
    .json_data(serde_json::json!({
        "type": "content_block_delta",
        "index": thinking_idx,
        "delta": {"type": "thinking_delta", "thinking": reasoning}
    }))
    .unwrap_or_else(|_| Event::default().data("{}"));
let _ = tx.send(delta_ev).await;
```

#### Step 0.2 — Replace old text-block creation in DSML preamble branch

Find old code pattern:

```rust
if let Some(idx) = thinking_block_index {
    emit_block_stop(idx)
    thinking_block_index = None;
}

let idx = match text_block_index {
    Some(i) => i,
    None => {
        let i = tracker.next_index();
        tracker.next_index() += 1;
        text_block_index = Some(i);
        content_block_start(i, "text", None, None)
        i
    }
};
```

Replace with tracker API:

```rust
let (idx, text_is_new, closed_thinking) = tracker.ensure_text();
if let Some(closed) = closed_thinking {
    let _ = tx.send(crate::sse::emit_block_stop(closed)).await;
}
if text_is_new {
    let _ = tx
        .send(builder.content_block_start(idx, "text", None, None))
        .await;
}
let _ = tx.send(builder.text_delta(idx, &cleaned)).await;
```

Apply this replacement to all repeated text-yielding branches.

Known locations from current failure:

```text
src/opencode/forward.rs around lines 633-647
src/opencode/forward.rs around lines 677-691
src/opencode/forward.rs around lines 831-841
```

#### Step 0.3 — Ensure DSML tool-use block allocation is correct

Current pattern uses:

```rust
let call_idx = tracker.next_index();
```

That is acceptable for content block index, but make sure it does not also need a separate call ordinal. If tool call map keys require stable call index, use a separate counter variable. Do not overload content block index as external tool-call key unless current code already depends on that behavior.

Preferred pattern for a standalone emitted DSML tool block:

```rust
if let Some(idx) = tracker.close_thinking() {
    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
}
if let Some(idx) = tracker.close_text() {
    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
}

let call_idx = tracker.next_index();
```

Do not call `next_index()` twice for the same block.

#### Step 0.4 — Remove all old variable references

Run:

```bash
rg -n "thinking_block_index|text_block_index|next_index\(\) \+=" src/opencode/forward.rs
```

Expected result:

```text
No matches.
```

Run:

```bash
rg -n '"index": idx' src/opencode/forward.rs
```

Expected result:

```text
No incorrect thinking/text delta index usage.
```

#### Step 0.5 — Add/adjust stream tracker tests if needed

If the text/thinking transition logic is not already covered, add tests in `src/stream_tracker.rs` for:

```text
1. ensure_thinking opens index 0.
2. ensure_text after thinking closes thinking and opens index 1.
3. ensure_thinking after text closes text and opens index 1.
4. close_all closes thinking/text/tool blocks exactly once.
```

### 5.4 Validation

```bash
cargo check --locked
cargo test --locked stream_tracker
```

### 5.5 Acceptance Criteria

```text
1. cargo check --locked passes.
2. No references to thinking_block_index/text_block_index remain in forward.rs.
3. No `tracker.next_index() += 1` remains.
4. No new #[allow(...)] was added to hide the issue.
```

---

## 6. Phase 1 — Fix Daemon CLI Option Propagation

### 6.1 Goal

Make these two commands equivalent in effective runtime config:

```bash
opencode2api server start -f --model test-model --shell-policy disabled --config ./x.toml
opencode2api server start    --model test-model --shell-policy disabled --config ./x.toml
```

Foreground and daemon modes must preserve all supported startup args.

### 6.2 Current Bug

In `src/main.rs`:

```rust
start_daemon(args.port, args.host, args.tls_cert, args.tls_key, fmt).await;
```

This drops:

```text
config
model
shell_policy
tavily_api_key
exa_api_key
serper_api_key
searxng_url
searxng_api_key
```

In `src/supervisor.rs`, child spawn only includes:

```rust
serve --port <port> --host <host> [tls]
```

### 6.3 Patch Plan

#### Step 1.1 — Introduce a single startup options struct

Use or extend `ServeArgsBridge` so it can be passed to both foreground and daemon startup.

Suggested structure:

```rust
#[derive(Debug, Clone, Default)]
struct ServeArgsBridge {
    pub port: Option<u16>,
    pub host: Option<String>,
    pub config: Option<String>,
    pub model: Option<String>,
    pub shell_policy: Option<String>,
    pub tavily_api_key: Option<String>,
    pub exa_api_key: Option<String>,
    pub serper_api_key: Option<String>,
    pub searxng_url: Option<String>,
    pub searxng_api_key: Option<String>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}
```

If this already exists, make it `Clone` and reuse it consistently.

#### Step 1.2 — Convert ServerStartArgs into ServeArgsBridge once

Add helper:

```rust
impl From<cli::ServerStartArgs> for ServeArgsBridge {
    fn from(args: cli::ServerStartArgs) -> Self { ... }
}
```

Or use a local helper function:

```rust
fn server_start_to_bridge_args(args: cli::ServerStartArgs) -> ServeArgsBridge { ... }
```

#### Step 1.3 — Change start_daemon signature

Current:

```rust
async fn start_daemon(
    port: Option<u16>,
    host: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    fmt: OutputFormat,
)
```

Target:

```rust
async fn start_daemon(args: ServeArgsBridge, fmt: OutputFormat)
```

#### Step 1.4 — Extend Supervisor config

Current supervisor stores:

```rust
port
host
tls_cert
tls_key
```

Target options:

```rust
pub struct Supervisor {
    paths: RuntimePaths,
    port: u16,
    host: String,
    config: Option<String>,
    model: Option<String>,
    shell_policy: Option<String>,
    tavily_api_key: Option<String>,
    exa_api_key: Option<String>,
    serper_api_key: Option<String>,
    searxng_url: Option<String>,
    searxng_api_key: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
}
```

Alternative: store a `DaemonSpawnOptions` struct inside Supervisor.

#### Step 1.5 — Add fluent builder for spawn args

Example:

```rust
impl Supervisor {
    pub fn with_spawn_options(mut self, options: DaemonSpawnOptions) -> Self {
        self.config = options.config;
        self.model = options.model;
        self.shell_policy = options.shell_policy;
        ...
        self
    }
}
```

#### Step 1.6 — Pass only present args to child

In `Supervisor::start()`:

```rust
cmd.arg("serve")
   .arg("--port").arg(self.port.to_string())
   .arg("--host").arg(&self.host);

push_optional_arg(&mut cmd, "--config", &self.config);
push_optional_arg(&mut cmd, "--model", &self.model);
push_optional_arg(&mut cmd, "--shell-policy", &self.shell_policy);
push_optional_arg(&mut cmd, "--tavily-api-key", &self.tavily_api_key);
push_optional_arg(&mut cmd, "--exa-api-key", &self.exa_api_key);
push_optional_arg(&mut cmd, "--serper-api-key", &self.serper_api_key);
push_optional_arg(&mut cmd, "--searxng-url", &self.searxng_url);
push_optional_arg(&mut cmd, "--searxng-api-key", &self.searxng_api_key);
push_optional_arg(&mut cmd, "--tls-cert", &self.tls_cert);
push_optional_arg(&mut cmd, "--tls-key", &self.tls_key);
```

Helper:

```rust
fn push_optional_arg(cmd: &mut Command, flag: &str, value: &Option<String>) {
    if let Some(v) = value {
        cmd.arg(flag).arg(v);
    }
}
```

#### Step 1.7 — Ensure legacy start remains backward compatible

`opencode2api start` only supports port/host today. Keep it working:

```rust
async fn cmd_start_legacy(args: cli::StartArgs, fmt: OutputFormat) {
    start_daemon(ServeArgsBridge {
        port: args.port,
        host: args.host,
        ..Default::default()
    }, fmt).await;
}
```

### 6.4 Validation

Manual validation:

```bash
cargo run -- server stop || true
cargo run -- server start --model cli-propagation-test --shell-policy disabled
sleep 1
cargo run -- server logs | tail -n 80 | grep 'Model:   cli-propagation-test'
cargo run -- server stop
```

### 6.5 Acceptance Criteria

```text
1. server start daemon passes model/config/shell-policy/search/tls args to child.
2. server start -f and server start have matching effective config for shared flags.
3. legacy start still works with port/host.
4. No CLI option shown in help is silently ignored in daemon mode.
```

---

## 7. Phase 2 — Standardize CLI Output Contracts

### 7.1 Goal

All commands must obey the selected output format:

```text
Human: readable, colored, tables/spinners allowed.
Json: valid JSON only, no ANSI, no spinner, no free-form hints.
Quiet: minimal one-line output or no success output; errors short and script-friendly.
```

### 7.2 Create shared result/error types

Add to `src/output.rs` or a new `src/cli_result.rs`:

```rust
#[derive(Debug, Serialize)]
pub struct CliResponse<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CliErrorInfo>,
}

#[derive(Debug, Serialize)]
pub struct CliErrorInfo {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
```

Add helpers:

```rust
pub fn print_json_success<T: Serialize>(data: T) { ... }
pub fn print_json_error(code: &str, message: &str, hint: Option<&str>) { ... }
pub fn exit_with_error(fmt: OutputFormat, code: &str, message: &str, hint: Option<&str>) -> ! { ... }
```

### 7.3 Fix `server start --json` error path

Current behavior prints human stderr.

Target JSON example:

```json
{
  "ok": false,
  "error": {
    "code": "already_running",
    "message": "Bridge is already running (PID: 30071)",
    "hint": "Run `opencode2api server stop` first."
  }
}
```

Exit code remains `1`.

### 7.4 Fix `server status --quiet`

Current behavior prints dashboard.

Target quiet examples:

Running:

```text
running pid=30071 port=4000
```

Stopped:

```text
stopped
```

Error:

```text
error: <message>
```

### 7.5 Fix `server stop --quiet`

Target:

```text
stopped
```

or no output on success. Choose one convention and apply consistently.

Recommended:

```text
stopped
```

because it is useful in scripts.

### 7.6 Fix `server restart --quiet`

Target:

```text
running pid=<pid> port=<port>
```

### 7.7 Fix `server logs --quiet`

Quiet should not colorize. It may print raw tail lines, but no decorative formatting.

Recommended:

```text
<last 20 raw lines>
```

Human can show 100 colored lines; JSON can return structured entries.

### 7.8 Fix `proxy ps --quiet`

Target one-line summary:

```text
primary=3/3 standby=2/2
```

### 7.9 Fix `doctor --quiet`

Target:

```text
warnings=2 failures=0
```

Exit code:

```text
0 if failures=0
1 if failures>0
```

### 7.10 Validation

```bash
cargo run -- --json server start >/tmp/out.json 2>/tmp/err.txt || true
python3 -m json.tool /tmp/out.json

a=$(cargo run --quiet -- --quiet server status)
echo "$a" | grep -E '^(running|stopped|error)'

cargo run --quiet -- --json doctor | python3 -m json.tool
cargo run --quiet -- --quiet doctor
```

### 7.11 Acceptance Criteria

```text
1. Every --json path emits parseable JSON only.
2. JSON errors are JSON, not colored stderr text.
3. Quiet mode for status does not print dashboard or proxy table.
4. Quiet mode for proxy ps does not print table.
5. Human output remains readable.
```

---

## 8. Phase 3 — Make CLI Args Type-Safe

### 8.1 Goal

Invalid user input should fail at CLI parsing, not silently downgrade behavior.

### 8.2 Shell policy enum

In `src/cli.rs`, add:

```rust
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum CliShellPolicy {
    Disabled,
    Allowlist,
    Unrestricted,
}

impl std::fmt::Display for CliShellPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            CliShellPolicy::Disabled => "disabled",
            CliShellPolicy::Allowlist => "allowlist",
            CliShellPolicy::Unrestricted => "unrestricted",
        };
        write!(f, "{value}")
    }
}
```

Update args:

```rust
#[arg(long = "shell-policy", value_enum)]
pub shell_policy: Option<CliShellPolicy>,
```

Do this for:

```text
ServerStartArgs
ServeArgs
```

Bridge conversion can use:

```rust
shell_policy: args.shell_policy.map(|p| p.to_string())
```

### 8.3 TLS arg validation

Add clap grouping or runtime validation:

```text
--tls-cert requires --tls-key
--tls-key requires --tls-cert
```

Preferred clap syntax:

```rust
#[arg(long, requires = "tls_key")]
pub tls_cert: Option<String>,

#[arg(long, requires = "tls_cert")]
pub tls_key: Option<String>,
```

Be careful with field names used by clap.

### 8.4 Host validation

Keep `host: Option<String>` if config layer already handles fallback, but add tests for invalid host fallback behavior.

Future stricter option:

```rust
pub host: Option<std::net::IpAddr>
```

Do not switch to `IpAddr` in this phase unless all config conversions remain simple.

### 8.5 Validation

```bash
cargo run -- server start --shell-policy typo
# expected: clap parse error, exit 2

echo $?
```

```bash
cargo run -- server start --tls-cert ./cert.pem
# expected: clap parse error because --tls-key missing
```

### 8.6 Acceptance Criteria

```text
1. Invalid --shell-policy exits before server startup.
2. Invalid --shell-policy exit code is clap parse failure code 2.
3. Missing TLS pair fails before runtime bind.
4. Help shows allowed shell policy values.
```

---

## 9. Phase 4 — Fix Completion Hidden Aliases

### 9.1 Goal

Generated completions should not promote hidden legacy top-level aliases.

### 9.2 Current issue

`opencode2api completion bash` includes:

```text
serve start status stop restart logs
```

Even though top-level help hides them.

### 9.3 Patch Options

#### Option A — Generate completion from public command tree

Create a second parser used only for completion generation, without legacy hidden aliases.

Pros:

```text
- Cleanest user experience.
- Matches v2 docs.
```

Cons:

```text
- Requires maintaining two command tree definitions or a builder transform.
```

#### Option B — Keep hidden aliases in completion and document it

Pros:

```text
- Less code.
- Backward compatibility friendly.
```

Cons:

```text
- Hidden aliases are not truly hidden.
- Contradicts current v2 spec intent.
```

Recommended: **Option A**.

### 9.4 Implementation Sketch

If using clap derive makes stripping hard, implement a small public command builder for completions:

```rust
pub fn public_command_for_completion() -> clap::Command {
    let mut cmd = Cli::command();
    // remove or hide legacy aliases from generated subcommands
    cmd
}
```

If clap does not expose convenient mutable subcommand removal, consider:

```text
1. Accept Option B temporarily.
2. Add explicit documentation that legacy aliases remain available in completion for compatibility.
3. Open follow-up issue for true removal.
```

### 9.5 Validation

```bash
cargo run -- completion bash > /tmp/opencode2api.bash
rg -n 'serve| start| status| stop| restart| logs' /tmp/opencode2api.bash
```

Expected under Option A:

```text
No top-level legacy alias suggestions.
```

Be careful: `server start/status/stop/restart/logs` should still appear as nested commands.

### 9.6 Acceptance Criteria

```text
1. Help and completion agree on primary command tree.
2. server nested commands still complete correctly.
3. proxy nested commands still complete correctly.
4. Backward-compatible aliases still parse manually unless intentionally removed.
```

---

## 10. Phase 5 — Add CLI Regression Tests

### 10.1 Goal

Prevent recurrence of the exact CLI bugs found.

### 10.2 Add dev dependencies if absent

In `Cargo.toml`:

```toml
[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = "3"
```

Only add what is not already present.

### 10.3 New test file

Create:

```text
tests/cli.rs
```

### 10.4 Test cases

#### Test 1 — JSON and quiet conflict

```rust
#[test]
fn json_and_quiet_conflict() {
    Command::cargo_bin("opencode2api")
        .unwrap()
        .args(["--json", "--quiet", "server", "status"])
        .assert()
        .failure();
}
```

#### Test 2 — invalid shell policy fails parse

```rust
#[test]
fn invalid_shell_policy_fails() {
    Command::cargo_bin("opencode2api")
        .unwrap()
        .args(["server", "start", "--shell-policy", "typo"])
        .assert()
        .code(2);
}
```

#### Test 3 — server start help includes v2 options

Check that help contains:

```text
--foreground
--model
--config
--shell-policy
```

#### Test 4 — status JSON is valid JSON

Run:

```text
opencode2api --json server status
```

Assert stdout parses as JSON and has:

```text
status
```

#### Test 5 — quiet status is compact

Assert it does not contain dashboard markers:

```text
Proxy Pool
Model:
Auth:
```

#### Test 6 — completion does not expose top-level legacy aliases

If Phase 4 Option A is implemented, assert generated bash completion does not suggest old top-level aliases.

If Option B is selected, test should instead assert documented behavior.

#### Test 7 — daemon propagation unit test

Prefer unit-testing command construction without actually spawning a daemon.

Refactor `Supervisor::start()` to use a helper:

```rust
fn build_serve_command_args(&self) -> Vec<String>
```

Then test:

```rust
assert!(args.contains(&"--model".to_string()));
assert!(args.contains(&"cli-propagation-test".to_string()));
```

This avoids flaky process spawning in CI.

### 10.5 Validation

```bash
cargo test --locked cli
cargo test --locked
```

### 10.6 Acceptance Criteria

```text
1. CLI regression tests cover all found bugs.
2. Tests do not require Docker unless explicitly ignored/gated.
3. Tests do not start long-running foreground server without timeout.
4. Tests pass in CI.
```

---

## 11. Phase 6 — Fix Docs Drift

### 11.1 Goal

Docs must describe actual CLI v2 behavior.

### 11.2 Files to update

```text
docs/cli.md
README.md
CLAUDE.md
CHANGELOG.md
verification/phases/phase-7-docs-migration.md
```

### 11.3 docs/cli.md rewrite structure

Recommended outline:

```markdown
# CLI Reference

## Command Tree

## Global Flags
- --json
- --quiet
- --color auto|always|never

## server
### server start
### server start -f
### server stop
### server status
### server restart
### server logs
### server config

## proxy
### proxy ps
### proxy restart
### proxy purge
### proxy logs

## env
## doctor
## completion
## update
## init

## Backward-Compatible Aliases
serve -> server start -f
start -> server start
status -> server status
stop -> server stop
restart -> server restart
logs -> server logs
proxy status -> proxy ps

## Output Modes
Human
JSON
Quiet

## Examples
```

### 11.4 Update CHANGELOG

Add unreleased section:

```markdown
## Unreleased

### Fixed
- Restored compile correctness after streaming tracker refactor.
- Fixed `server start` daemon mode dropping startup options.
- Fixed JSON mode to emit structured errors.
- Fixed quiet mode to avoid dashboards/tables.
- Made `--shell-policy` a typed CLI enum.
- Aligned CLI docs with v2 command tree.
```

### 11.5 Validation

```bash
rg -n "opencode2api (serve|start|status|stop|restart|logs)" README.md docs CLAUDE.md
```

Old commands may remain only in a backward compatibility section.

### 11.6 Acceptance Criteria

```text
1. Primary examples use `server ...` commands.
2. Legacy commands are documented only as aliases.
3. Output mode behavior matches code.
4. No docs claim behavior not implemented.
```

---

## 12. Phase 7 — Security Review

### 12.1 Goal

Ensure the CLI fixes do not weaken security.

### 12.2 Review points

#### Public bind + auth

Confirm `BridgeConfig::validate_security()` still blocks:

```text
host = 0.0.0.0 without auth
host = 0.0.0.0 with unrestricted shell
```

#### Shell policy

Confirm default remains:

```text
disabled
```

Confirm invalid CLI value does not silently run.

#### JSON errors

Ensure JSON errors do not include secrets:

```text
BRIDGE_AUTH_TOKEN
TAVILY_API_KEY
EXA_API_KEY
SERPER_API_KEY
SEARXNG_API_KEY
OpenCode API keys
```

#### Logs

Do not print API keys in server logs, CLI logs, or startup banner.

If key-like values are currently in `bridge.log`, do not commit the log file.

#### Proxy protection

Confirm `proxy restart` and `proxy purge` still only modify primary ports:

```text
40001
40002
40003
```

Confirm warm standby remains protected:

```text
40004
40005
```

### 12.3 Validation

```bash
cargo test --locked config::tests::test_public_bind_without_auth_rejected
cargo test --locked config::tests::test_public_bind_unrestricted_shell_rejected
cargo test --locked proxy
```

If exact test names differ, list tests first:

```bash
cargo test --locked -- --list | rg 'public|auth|shell|proxy'
```

### 12.4 Acceptance Criteria

```text
1. No unsafe public bind regression.
2. No shell unrestricted regression.
3. No secret leakage in new CLI output.
4. Protected proxy ports remain protected.
```

---

## 13. Phase 8 — Full Verification Gates

Run all gates after implementation.

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Run project verification scripts:

```bash
./scripts/verify.sh --list
./scripts/verify.sh --phase 1
./scripts/verify.sh --phase 2
./scripts/verify.sh --phase 3
./scripts/verify.sh --phase 7
```

If the script supports all phases safely:

```bash
./scripts/verify.sh
```

Expected final state:

```text
fmt: pass
check: pass
test: pass
clippy -D warnings: pass
verification phase 1/2/3/7: pass
```

---

## 14. Manual Smoke Test Matrix

### 14.1 Help and parse

```bash
opencode2api --help
opencode2api server --help
opencode2api server start --help
opencode2api proxy --help
opencode2api doctor --help
```

Expected:

```text
Primary command tree appears.
Legacy aliases do not appear in top-level help.
Allowed values for --shell-policy appear.
```

### 14.2 Server lifecycle

```bash
opencode2api server stop || true
opencode2api server start --model cli-smoke-model --shell-policy disabled
opencode2api server status
opencode2api server logs | tail -n 80
opencode2api server stop
```

Expected:

```text
server starts as daemon.
status reports running.
logs show cli-smoke-model.
server stops cleanly.
```

### 14.3 Foreground mode

Use timeout to avoid hanging smoke test:

```bash
timeout 5s opencode2api server start -f --port 49999 --model foreground-smoke-model || true
```

Expected:

```text
Server starts and receives SIGTERM from timeout.
No panic.
```

### 14.4 JSON mode

```bash
opencode2api --json server status | python3 -m json.tool
opencode2api --json server config | python3 -m json.tool
opencode2api --json env | python3 -m json.tool
opencode2api --json doctor | python3 -m json.tool
opencode2api --json proxy ps | python3 -m json.tool
```

Expected:

```text
All outputs parse as JSON.
No ANSI escape codes.
```

### 14.5 Quiet mode

```bash
opencode2api --quiet server status
opencode2api --quiet doctor
opencode2api --quiet proxy ps
```

Expected:

```text
No dashboards.
No tables.
No decorative headings.
```

### 14.6 Invalid inputs

```bash
opencode2api server start --shell-policy typo
echo $?

opencode2api server start --tls-cert ./missing.pem
echo $?
```

Expected:

```text
clap parse error or validation error before runtime startup.
No server bind attempt.
```

### 14.7 Completion

```bash
opencode2api completion bash > /tmp/opencode2api.bash
bash -n /tmp/opencode2api.bash
```

Expected:

```text
Generated completion is syntactically valid.
Nested server/proxy commands complete.
Legacy alias behavior matches the chosen Phase 4 decision.
```

---

## 15. Risk Register

### Risk 1 — Streaming refactor changes SSE ordering

Severity: High

Mitigation:

```text
Add/retain tests for thinking->text transitions and text->tool_use transitions.
Run streaming integration tests if available.
Avoid changing event semantics beyond replacing old state variables with tracker API.
```

### Risk 2 — Daemon propagation accidentally logs secrets

Severity: High

Mitigation:

```text
Pass secret-like CLI args to child only when needed.
Do not print full child command if it includes API keys.
If debug logging command args is needed, redact secret flags.
```

Secret flags:

```text
--tavily-api-key
--exa-api-key
--serper-api-key
--searxng-api-key
BRIDGE_AUTH_TOKEN
```

### Risk 3 — JSON error contract breaks existing scripts expecting stderr

Severity: Medium

Mitigation:

```text
Only enforce JSON object output when --json is explicitly set.
Human mode remains unchanged except better consistency.
Document JSON error schema.
```

### Risk 4 — Completion hidden alias removal annoys legacy users

Severity: Low/Medium

Mitigation:

```text
Keep aliases parseable.
Only remove from generated suggestions if implementing Option A.
Mention aliases in docs backward compatibility section.
```

### Risk 5 — Tests become flaky due to daemon/process state

Severity: Medium

Mitigation:

```text
Prefer unit tests for command construction.
Use temp HOME/runtime directory for lifecycle tests.
Use random ports.
Use timeout for foreground server smoke tests.
Avoid requiring Docker for basic CLI tests.
```

---

## 16. Rollback Plan

If Phase 0 streaming fix becomes risky:

```bash
git checkout -- src/opencode/forward.rs src/stream_tracker.rs src/lib.rs
```

Then restore the last known compile-good implementation from `HEAD` or the previous release tag.

If CLI refactor becomes risky after Phase 0:

```bash
git checkout -- src/cli.rs src/main.rs src/supervisor.rs src/output.rs src/config.rs
```

Then reapply only the minimal daemon propagation fix.

Always preserve current dirty diff before rollback:

```bash
git diff > /tmp/opencode2api-rollback-safety.diff
```

---

## 17. Suggested Commit Breakdown

```text
commit 1: fix(stream): complete SseBlockTracker migration in forward.rs
commit 2: test(stream): add tracker lifecycle regression tests
commit 3: fix(cli): propagate server start options to daemon child
commit 4: fix(cli): standardize json and quiet output contracts
commit 5: fix(cli): type shell-policy and validate TLS pairs
commit 6: fix(cli): align completion behavior with public command tree
commit 7: test(cli): add CLI v2 regression tests
commit 8: docs(cli): update CLI reference and changelog
commit 9: chore: run fmt/clippy cleanup
```

Do not squash until review is complete; separate commits make regression hunting easier.

---

## 18. Final Definition of Done

The fix is complete only when all are true:

```text
1. cargo fmt --check passes.
2. cargo check --locked passes.
3. cargo test --locked passes.
4. cargo clippy --locked --all-targets -- -D warnings passes.
5. CLI daemon mode preserves all supported startup args.
6. JSON mode emits only valid JSON for success and error paths.
7. Quiet mode emits compact script-friendly output.
8. Invalid shell-policy fails before startup.
9. Completion behavior matches documented decision.
10. docs/cli.md uses CLI v2 as the primary command tree.
11. CHANGELOG documents the fixes.
12. No secret-bearing logs are committed.
13. Warm-standby proxy protection remains intact.
```

---

## 19. Quick Command Checklist

Use this checklist during implementation:

```bash
# Build baseline
cargo check --locked

# Format and tests
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings

# CLI parser
cargo run -- --help
cargo run -- server start --help
cargo run -- server start --shell-policy typo

# JSON
cargo run -- --json server status | python3 -m json.tool
cargo run -- --json doctor | python3 -m json.tool
cargo run -- --json proxy ps | python3 -m json.tool

# Quiet
cargo run -- --quiet server status
cargo run -- --quiet doctor
cargo run -- --quiet proxy ps

# Daemon propagation
cargo run -- server stop || true
cargo run -- server start --model cli-propagation-test --shell-policy disabled
sleep 1
cargo run -- server logs | tail -n 80 | grep 'cli-propagation-test'
cargo run -- server stop

# Completion
cargo run -- completion bash > /tmp/opencode2api.bash
bash -n /tmp/opencode2api.bash

# Docs drift
rg -n "opencode2api (serve|start|status|stop|restart|logs)" README.md docs CLAUDE.md
```
