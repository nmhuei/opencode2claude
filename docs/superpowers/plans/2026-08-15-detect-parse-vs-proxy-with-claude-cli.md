# Detect Parse vs Proxy with Claude Code CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect, with real Claude Code CLI probes, whether the current failure belongs to the parse/protocol-mapping layer, the proxy/egress layer, the Claude CLI/config layer, or upstream opencode/account behavior.

**Architecture:** The investigation treats `opencode2api` as two separable components: parse/protocol mapping and proxy/egress routing. Every diagnostic runs through Claude Code CLI first, then uses bridge REST snapshots and direct controls only to classify where the failing boundary is. A secondary direct-egress bridge on port `4010` isolates parse from proxy while preserving the same Claude Code CLI client path.

**Tech Stack:** Rust `opencode2api`, Claude Code CLI `2.1.233`, local bridge endpoints `127.0.0.1:4000` and `127.0.0.1:4010`, Bash, Python 3, `jq` optional but not required.

## Global Constraints

- Do not print API keys, dashboard tokens, REST tokens, or auth file contents.
- Do not edit production config except creating a temporary diagnostic config under `artifacts/`.
- Do not conclude “quota hết” unless direct opencode and upstream response evidence prove it.
- Every classification must cite one real probe output: Claude CLI output, bridge response, REST proxy snapshot, or debug log.
- Primary diagnostic must use Claude Code CLI, not only `curl`.
- Keep proxy privacy intent intact: do not enable direct fallback on the main proxy server.
- The main managed server is `http://127.0.0.1:4000`.
- The temporary direct-egress server, if started, is `http://127.0.0.1:4010` and must be stopped at the end.

---

## File Structure

**Create:**
- `artifacts/claude-cli-layer-detect-$RUN_ID/` — all diagnostic outputs for one run.
- `artifacts/claude-cli-layer-detect-$RUN_ID/direct-egress.toml` — temporary direct-egress bridge config.
- `artifacts/claude-cli-layer-detect-$RUN_ID/*.json` — structured outputs from Claude CLI, bridge probes, and REST snapshots.
- `artifacts/claude-cli-layer-detect-$RUN_ID/*.debug.log` — Claude CLI debug logs.
- `artifacts/claude-cli-layer-detect-$RUN_ID/*.stderr` — stderr from CLI probes.
- `artifacts/claude-cli-layer-detect-$RUN_ID/classification.md` — final classification report.

**Read only:**
- `/home/light/GitHub/opencode2claude/.env` — read tokens internally only; never echo values.
- `/home/light/.claude/settings.json` — inspect only if Claude CLI behaves differently from direct bridge probes.
- `src/handlers/messages.rs` — parse/validation boundary.
- `src/opencode/mapper/request.rs` — Anthropic → OpenAI mapping boundary.
- `src/opencode/retry/execute.rs` — route selection and retry boundary.
- `src/proxy_pool/*` — proxy health/identity/circuit boundary.

---

### Task 1: Prepare a Secret-Safe Diagnostic Workspace

**Files:**
- Create: `artifacts/claude-cli-layer-detect-$RUN_ID/`
- Create: `artifacts/claude-cli-layer-detect-$RUN_ID/env-summary.txt`

**Interfaces:**
- Consumes: local binaries `claude`, `opencode2api`, `opencode2api-serve`, `opencode`.
- Produces: `RUN_ID`, `ART`, and a redacted baseline environment used by all later tasks.

- [ ] **Step 1: Create artifact directory and record binary versions**

```bash
cd /home/light/GitHub/opencode2claude
export RUN_ID="$(date +%Y%m%d-%H%M%S)"
export ART="$PWD/artifacts/claude-cli-layer-detect-$RUN_ID"
mkdir -p "$ART"

{
  echo "RUN_ID=$RUN_ID"
  echo "ART=$ART"
  echo "claude=$(command -v claude || true)"
  claude --version 2>/dev/null || true
  echo "opencode2api=$(command -v opencode2api || true)"
  opencode2api --version 2>/dev/null || true
  echo "opencode2api-serve=$(command -v opencode2api-serve || true)"
  opencode2api-serve --version 2>/dev/null || true
  echo "opencode=$(command -v opencode || true)"
  opencode --version 2>/dev/null || true
} | tee "$ART/env-summary.txt"
```

Expected:

```text
claude 2.1.233 or newer installed
opencode2api installed
opencode2api-serve installed
opencode installed
```

- [ ] **Step 2: Define a reusable redactor script**

```bash
cat > "$ART/redact.py" <<'PY'
import re
import sys

patterns = [
    re.compile(r'(sk-[A-Za-z0-9_-]{8,})'),
    re.compile(r'(Bearer\s+)[A-Za-z0-9._~+/=-]{8,}', re.I),
    re.compile(r'("(?:api[_-]?key|token|secret|authorization)"\s*:\s*")[^"]+', re.I),
    re.compile(r'((?:api[_-]?key|token|secret|authorization)\s*=\s*)[^\s]+', re.I),
]

for line in sys.stdin:
    out = line
    for pat in patterns:
        if pat.pattern.startswith('("'):
            out = pat.sub(r'\1[REDACTED]', out)
        elif pat.pattern.startswith('(('):
            out = pat.sub(r'\1[REDACTED]', out)
        elif 'Bearer' in pat.pattern:
            out = pat.sub(r'\1[REDACTED]', out)
        else:
            out = pat.sub('[REDACTED]', out)
    sys.stdout.write(out)
PY
```

Expected: file exists and can redact logs:

```bash
printf 'Authorization: Bearer abcdefgh123456\n' | python3 "$ART/redact.py"
```

Expected output:

```text
Authorization: Bearer [REDACTED]
```

---

### Task 2: Capture Main Bridge Status and Proxy Snapshot

**Files:**
- Create: `server-status.json`
- Create: `rest-status.json` or `rest-status.error.txt`
- Create: `rest-proxies.json` or `rest-proxies.error.txt`
- Create: `rest-metrics.json` or `rest-metrics.error.txt`

**Interfaces:**
- Consumes: main bridge on `127.0.0.1:4000`.
- Produces: current egress readiness and proxy node state before any Claude CLI test.

- [ ] **Step 1: Capture managed server status**

```bash
opencode2api server status --json \
  | tee "$ART/server-status.json"
```

Expected if server is running:

```json
{"status":"running","endpoint":"http://127.0.0.1:4000"}
```

If status is not running, classification is immediately:

```text
Layer: service lifecycle, not parse/proxy.
Evidence: opencode2api server status is not running.
```

- [ ] **Step 2: Capture REST status/proxy/metrics without printing tokens**

```bash
python3 > "$ART/rest-capture.raw.txt" 2>&1 <<'PY'
from pathlib import Path
import json
import urllib.error
import urllib.request

root = Path.home() / 'GitHub' / 'opencode2claude'
art = Path(__import__('os').environ['ART'])
env_path = root / '.env'
keys = {}
if env_path.exists():
    for raw in env_path.read_text(errors='replace').splitlines():
        line = raw.strip()
        if not line or line.startswith('#') or '=' not in line:
            continue
        k, v = line.split('=', 1)
        keys[k.strip()] = v.strip().strip('"').strip("'")

token = (
    keys.get('BRIDGE_REST_API_TOKEN')
    or keys.get('REST_API_TOKEN')
    or keys.get('DASHBOARD_ADMIN_TOKEN')
    or keys.get('BRIDGE_DASHBOARD_TOKEN')
)
headers = {}
if token:
    headers['Authorization'] = 'Bearer ' + token

for path, name in [
    ('/api/v1/status', 'rest-status.json'),
    ('/api/v1/proxies', 'rest-proxies.json'),
    ('/api/v1/metrics', 'rest-metrics.json'),
]:
    req = urllib.request.Request('http://127.0.0.1:4000' + path, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = resp.read().decode('utf-8', 'replace')
            (art / name).write_text(body)
            print(path, 'HTTP', resp.status, 'saved', name)
    except urllib.error.HTTPError as e:
        body = e.read().decode('utf-8', 'replace')
        (art / (name + '.error.txt')).write_text(f'HTTP {e.code}\n{body}')
        print(path, 'HTTP_ERROR', e.code, 'saved', name + '.error.txt')
    except Exception as e:
        (art / (name + '.error.txt')).write_text(type(e).__name__ + ': ' + str(e))
        print(path, 'ERROR', type(e).__name__, 'saved', name + '.error.txt')
PY
python3 "$ART/redact.py" < "$ART/rest-capture.raw.txt" | tee "$ART/rest-capture.txt"
```

Expected if token is available:

```text
/api/v1/status HTTP 200 saved rest-status.json
/api/v1/proxies HTTP 200 saved rest-proxies.json
```

Classification rules from this task:

```text
If egress.ready=false and Claude CLI later fails with egress/proxy wording:
  suspect proxy/egress layer.

If egress.ready=true and Claude CLI later fails with invalid request / mapping / HTTP 400:
  suspect parse/protocol-mapping layer or upstream request-shape compatibility.

If REST is 401 but /v1/messages works:
  REST auth unavailable only; do not classify parse/proxy from REST alone.
```

---

### Task 3: Run a Real Claude Code CLI Baseline Through Main Proxy Bridge

**Files:**
- Create: `claude-main-minimal.json`
- Create: `claude-main-minimal.stderr`
- Create: `claude-main-minimal.debug.log`
- Create: `claude-main-minimal.debug.redacted.log`
- Create: `claude-main-minimal.check.txt`

**Interfaces:**
- Consumes: main bridge `http://127.0.0.1:4000`.
- Produces: first real Claude Code CLI evidence.

- [ ] **Step 1: Run minimal Claude CLI prompt through the main bridge**

```bash
ANTHROPIC_BASE_URL="http://127.0.0.1:4000" \
ANTHROPIC_API_KEY="opencode-bridge" \
ANTHROPIC_AUTH_TOKEN="" \
OPENAI_BASE_URL="" \
OPENAI_API_KEY="" \
OPENCODE_API_KEY="" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format json \
  --debug-file "$ART/claude-main-minimal.debug.log" \
  --model claude-opus-5 \
  'Reply with exactly: cli-main-ok' \
  > "$ART/claude-main-minimal.json" \
  2> "$ART/claude-main-minimal.stderr"

python3 "$ART/redact.py" < "$ART/claude-main-minimal.debug.log" > "$ART/claude-main-minimal.debug.redacted.log" || true
python3 "$ART/redact.py" < "$ART/claude-main-minimal.stderr" > "$ART/claude-main-minimal.stderr.redacted" || true
```

Expected pass condition:

```bash
python3 - <<'PY' | tee "$ART/claude-main-minimal.check.txt"
from pathlib import Path
import json, os, sys
p = Path(os.environ['ART']) / 'claude-main-minimal.json'
try:
    data = json.loads(p.read_text(errors='replace'))
    text = json.dumps(data, ensure_ascii=False)
except Exception:
    text = p.read_text(errors='replace')
print('contains_cli_main_ok=', 'cli-main-ok' in text)
print('contains_429=', '429' in text or 'rate_limit' in text.lower())
print('contains_egress=', 'egress' in text.lower() or 'proxy' in text.lower() or 'unique healthy proxy' in text.lower())
print('contains_invalid_request=', 'invalid request' in text.lower() or 'invalid_request' in text.lower())
PY
```

Classification rules:

```text
Pass with cli-main-ok:
  main Claude CLI path works now; continue stress tests to find intermittent or edge-case layer.

Fail with no HTTP request in debug log:
  Claude CLI/config/auth layer, before bridge.

Fail with invalid request body / JSON parse / messages field error:
  parse/handler layer.

Fail with unique healthy proxy / egress unavailable / proxy pool wording:
  proxy/egress layer.

Fail with upstream status 400 after model fallback:
  parse mapping may have produced provider-incompatible payload; inspect effective_json/history.

Fail with upstream 429/quota body after a selected route was used:
  upstream/provider/account/rate limit evidence, not pure proxy selection.
```

---

### Task 4: Run the Same Claude CLI Prompt Against a Temporary Direct-Egress Bridge

**Files:**
- Create: `direct-egress.toml`
- Create: `direct-server.log`
- Create: `direct-server.pid`
- Create: `claude-direct-minimal.json`
- Create: `claude-direct-minimal.stderr`
- Create: `claude-direct-minimal.debug.log`
- Create: `claude-direct-minimal.check.txt`

**Interfaces:**
- Consumes: temporary server `127.0.0.1:4010`, same Claude CLI, same model, same prompt.
- Produces: decisive parse-vs-proxy isolation.

- [ ] **Step 1: Create direct-egress config**

```bash
cat > "$ART/direct-egress.toml" <<'EOF'
host = "127.0.0.1"
bridge_port = 4010
model = "opencode/deepseek-v4-flash-free"
egress_mode = "direct"
max_body_size = 0
EOF
```

- [ ] **Step 2: Start direct-egress server on port 4010**

```bash
BRIDGE_ENV_PATH="$HOME/GitHub/opencode2claude/.env" \
opencode2api-serve \
  --config "$ART/direct-egress.toml" \
  --host 127.0.0.1 \
  --port 4010 \
  --model opencode/deepseek-v4-flash-free \
  > "$ART/direct-server.log" \
  2>&1 &
echo $! > "$ART/direct-server.pid"

python3 - <<'PY'
import time, urllib.request
for i in range(40):
    try:
        urllib.request.urlopen('http://127.0.0.1:4010/health/live', timeout=1)
        print('direct_server_ready=true')
        break
    except Exception:
        time.sleep(0.25)
else:
    print('direct_server_ready=false')
    raise SystemExit(1)
PY
```

Expected:

```text
direct_server_ready=true
```

- [ ] **Step 3: Run the same Claude CLI prompt through direct-egress bridge**

```bash
ANTHROPIC_BASE_URL="http://127.0.0.1:4010" \
ANTHROPIC_API_KEY="opencode-bridge" \
ANTHROPIC_AUTH_TOKEN="" \
OPENAI_BASE_URL="" \
OPENAI_API_KEY="" \
OPENCODE_API_KEY="" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format json \
  --debug-file "$ART/claude-direct-minimal.debug.log" \
  --model claude-opus-5 \
  'Reply with exactly: cli-direct-ok' \
  > "$ART/claude-direct-minimal.json" \
  2> "$ART/claude-direct-minimal.stderr"

python3 "$ART/redact.py" < "$ART/claude-direct-minimal.debug.log" > "$ART/claude-direct-minimal.debug.redacted.log" || true
python3 "$ART/redact.py" < "$ART/claude-direct-minimal.stderr" > "$ART/claude-direct-minimal.stderr.redacted" || true

python3 - <<'PY' | tee "$ART/claude-direct-minimal.check.txt"
from pathlib import Path
import json, os
p = Path(os.environ['ART']) / 'claude-direct-minimal.json'
try:
    data = json.loads(p.read_text(errors='replace'))
    text = json.dumps(data, ensure_ascii=False)
except Exception:
    text = p.read_text(errors='replace')
print('contains_cli_direct_ok=', 'cli-direct-ok' in text)
print('contains_429=', '429' in text or 'rate_limit' in text.lower())
print('contains_invalid_request=', 'invalid request' in text.lower() or 'invalid_request' in text.lower())
print('contains_upstream=', 'upstream' in text.lower())
PY
```

Classification matrix:

```text
Main proxy fails, direct-egress passes:
  proxy/egress layer is the problem.

Main proxy passes, direct-egress fails:
  direct-egress config/upstream/auth environment problem; do not blame parse.

Both fail with same invalid request/mapping error:
  parse/protocol mapping layer is the problem.

Both fail with upstream quota/payment/provider body:
  upstream/account/provider layer is the problem.

Both pass:
  current minimal path is healthy; continue Task 5–7 for parse edge cases and proxy intermittency.
```

---

### Task 5: Stress Parse Layer With Real Claude CLI Structured Output

**Files:**
- Create: `claude-main-schema.json`
- Create: `claude-main-schema.stderr`
- Create: `claude-main-schema.debug.log`
- Create: `claude-direct-schema.json`
- Create: `claude-direct-schema.stderr`
- Create: `claude-direct-schema.debug.log`
- Create: `schema-compare.check.txt`

**Interfaces:**
- Consumes: main proxy endpoint and direct-egress endpoint.
- Produces: evidence for `output_config`, `response_format`, JSON schema mapping, and parse compatibility.

- [ ] **Step 1: Run schema prompt through main proxy endpoint**

```bash
SCHEMA='{"type":"object","properties":{"ok":{"type":"boolean"},"marker":{"type":"string","const":"schema-main-ok"}},"required":["ok","marker"],"additionalProperties":false}'

ANTHROPIC_BASE_URL="http://127.0.0.1:4000" \
ANTHROPIC_API_KEY="opencode-bridge" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format json \
  --json-schema "$SCHEMA" \
  --debug-file "$ART/claude-main-schema.debug.log" \
  --model claude-opus-5 \
  'Return JSON only with ok=true and marker="schema-main-ok".' \
  > "$ART/claude-main-schema.json" \
  2> "$ART/claude-main-schema.stderr"
```

- [ ] **Step 2: Run schema prompt through direct-egress endpoint**

```bash
SCHEMA='{"type":"object","properties":{"ok":{"type":"boolean"},"marker":{"type":"string","const":"schema-direct-ok"}},"required":["ok","marker"],"additionalProperties":false}'

ANTHROPIC_BASE_URL="http://127.0.0.1:4010" \
ANTHROPIC_API_KEY="opencode-bridge" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format json \
  --json-schema "$SCHEMA" \
  --debug-file "$ART/claude-direct-schema.debug.log" \
  --model claude-opus-5 \
  'Return JSON only with ok=true and marker="schema-direct-ok".' \
  > "$ART/claude-direct-schema.json" \
  2> "$ART/claude-direct-schema.stderr"
```

- [ ] **Step 3: Compare schema results**

```bash
python3 - <<'PY' | tee "$ART/schema-compare.check.txt"
from pathlib import Path
import os
art = Path(os.environ['ART'])
for name, marker in [
    ('claude-main-schema.json', 'schema-main-ok'),
    ('claude-direct-schema.json', 'schema-direct-ok'),
]:
    text = (art / name).read_text(errors='replace') if (art / name).exists() else ''
    print(name, 'marker_present=', marker in text)
    print(name, 'invalid_request=', 'invalid_request' in text.lower() or 'invalid request' in text.lower())
    print(name, 'response_format_error=', 'response_format' in text.lower() or 'grammar' in text.lower())
PY
```

Classification rules:

```text
Both main and direct fail with response_format/grammar/invalid_request:
  parse/protocol-mapping layer, likely schema/output_config compatibility.

Main fails with egress/proxy wording but direct passes:
  proxy/egress layer.

Both pass:
  schema parse path is healthy.
```

---

### Task 6: Stress Tool-Use Parse With a Safe Claude CLI Bash Echo Probe

**Files:**
- Create: `claude-main-tool.stream.jsonl`
- Create: `claude-main-tool.stderr`
- Create: `claude-main-tool.debug.log`
- Create: `claude-direct-tool.stream.jsonl`
- Create: `claude-direct-tool.stderr`
- Create: `claude-direct-tool.debug.log`
- Create: `tool-compare.check.txt`

**Interfaces:**
- Consumes: Claude Code CLI tools through the bridge.
- Produces: evidence for tool schema forwarding, tool_use/tool_result mapping, and follow-up turns.

- [ ] **Step 1: Run safe tool-use prompt through main proxy endpoint**

```bash
ANTHROPIC_BASE_URL="http://127.0.0.1:4000" \
ANTHROPIC_API_KEY="opencode-bridge" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format stream-json \
  --include-partial-messages \
  --permission-mode bypassPermissions \
  --allowedTools 'Bash(echo *)' \
  --debug-file "$ART/claude-main-tool.debug.log" \
  --model claude-opus-5 \
  'Use Bash exactly once to run: echo cli-tool-main-ok. Then answer with exactly cli-tool-main-ok.' \
  > "$ART/claude-main-tool.stream.jsonl" \
  2> "$ART/claude-main-tool.stderr"
```

- [ ] **Step 2: Run safe tool-use prompt through direct-egress endpoint**

```bash
ANTHROPIC_BASE_URL="http://127.0.0.1:4010" \
ANTHROPIC_API_KEY="opencode-bridge" \
claude -p \
  --bare \
  --no-session-persistence \
  --output-format stream-json \
  --include-partial-messages \
  --permission-mode bypassPermissions \
  --allowedTools 'Bash(echo *)' \
  --debug-file "$ART/claude-direct-tool.debug.log" \
  --model claude-opus-5 \
  'Use Bash exactly once to run: echo cli-tool-direct-ok. Then answer with exactly cli-tool-direct-ok.' \
  > "$ART/claude-direct-tool.stream.jsonl" \
  2> "$ART/claude-direct-tool.stderr"
```

- [ ] **Step 3: Compare tool probe results**

```bash
python3 - <<'PY' | tee "$ART/tool-compare.check.txt"
from pathlib import Path
import os
art = Path(os.environ['ART'])
for name, marker in [
    ('claude-main-tool.stream.jsonl', 'cli-tool-main-ok'),
    ('claude-direct-tool.stream.jsonl', 'cli-tool-direct-ok'),
]:
    text = (art / name).read_text(errors='replace') if (art / name).exists() else ''
    print(name, 'marker_present=', marker in text)
    print(name, 'tool_use_seen=', 'tool_use' in text or 'Bash' in text)
    print(name, 'tool_result_seen=', 'tool_result' in text or marker in text)
    print(name, 'malformed_tool=', 'malformed' in text.lower() or 'tool' in text.lower() and 'invalid' in text.lower())
    print(name, 'egress_error=', 'egress' in text.lower() or 'unique healthy proxy' in text.lower() or 'proxy' in text.lower())
PY
```

Classification rules:

```text
Both endpoints fail with malformed tool/tool_result or invalid request:
  parse/protocol-mapping layer.

Only main proxy endpoint fails with egress/proxy wording:
  proxy/egress layer.

Tool prompt fails but minimal/schema pass:
  parse edge case is probably tool mapping, not base routing.
```

---

### Task 7: Confirm Upstream Account/Model Separately With opencode Direct

**Files:**
- Create: `opencode-direct.txt`
- Create: `opencode-direct.stderr`

**Interfaces:**
- Consumes: direct `opencode run`, not Claude CLI.
- Produces: control evidence for upstream account/model availability.

- [ ] **Step 1: Run opencode direct control**

```bash
env \
  -u ANTHROPIC_BASE_URL \
  -u ANTHROPIC_API_KEY \
  -u ANTHROPIC_AUTH_TOKEN \
  -u OPENAI_BASE_URL \
  -u OPENAI_API_KEY \
  -u OPENCODE_API_KEY \
  OPENCODE_DISABLE_AUTO_UPDATE=1 \
  opencode run --pure \
    -m opencode/deepseek-v4-flash-free \
    'Reply with exactly: opencode-direct-ok' \
  > "$ART/opencode-direct.txt" \
  2> "$ART/opencode-direct.stderr"
```

Expected pass:

```text
opencode-direct-ok
```

Classification rules:

```text
opencode direct fails with quota/payment/provider message:
  upstream/account/model issue exists independently of bridge.

opencode direct passes while Claude CLI direct-egress fails:
  bridge parse/mapping or bridge upstream request compatibility issue.

opencode direct passes while Claude CLI main proxy fails and Claude CLI direct-egress passes:
  proxy/egress issue.
```

---

### Task 8: Build the Final Classification Report

**Files:**
- Create: `classification.md`

**Interfaces:**
- Consumes: outputs from Tasks 2–7.
- Produces: one concise diagnosis with evidence.

- [ ] **Step 1: Generate classification from probe outputs**

```bash
cat > "$ART/classify.py" <<'PY'
from pathlib import Path
import os

art = Path(os.environ['ART'])

def read(name):
    p = art / name
    return p.read_text(errors='replace') if p.exists() else ''

main = read('claude-main-minimal.json') + read('claude-main-minimal.stderr') + read('claude-main-minimal.debug.redacted.log')
direct = read('claude-direct-minimal.json') + read('claude-direct-minimal.stderr') + read('claude-direct-minimal.debug.redacted.log')
schema = read('schema-compare.check.txt')
tool = read('tool-compare.check.txt')
opencode = read('opencode-direct.txt') + read('opencode-direct.stderr')
rest = read('rest-status.json') + read('rest-proxies.json') + read('rest-status.json.error.txt') + read('rest-proxies.json.error.txt')

def has(text, *needles):
    low = text.lower()
    return any(n.lower() in low for n in needles)

main_ok = 'cli-main-ok' in main
direct_ok = 'cli-direct-ok' in direct
opencode_ok = 'opencode-direct-ok' in opencode
main_egress = has(main, 'unique healthy proxy', 'egress temporarily unavailable', 'egress unavailable', 'proxy pool')
direct_invalid = has(direct, 'invalid request', 'invalid_request', 'response_format', 'grammar')
main_invalid = has(main, 'invalid request', 'invalid_request', 'response_format', 'grammar')
upstream_quota = has(main + direct + opencode, 'quota', 'payment required', 'credits', 'freeusagelimit', 'too many requests')

lines = []
lines.append('# Claude CLI Layer Detection Classification')
lines.append('')
lines.append(f'- main_proxy_minimal_ok: {main_ok}')
lines.append(f'- direct_egress_minimal_ok: {direct_ok}')
lines.append(f'- opencode_direct_ok: {opencode_ok}')
lines.append(f'- main_proxy_egress_error: {main_egress}')
lines.append(f'- main_parse_like_error: {main_invalid}')
lines.append(f'- direct_parse_like_error: {direct_invalid}')
lines.append(f'- upstream_quota_like_text_seen: {upstream_quota}')
lines.append('')

if main_ok and direct_ok:
    lines.append('## Classification')
    lines.append('Minimal Claude Code CLI path is healthy on both proxy and direct-egress bridges. Use schema/tool/proxy snapshots to classify edge failures.')
elif (not main_ok) and direct_ok and main_egress:
    lines.append('## Classification')
    lines.append('Proxy/egress layer problem. Same Claude Code CLI prompt passes through direct-egress bridge but fails through main proxy bridge with egress/proxy wording.')
elif (not main_ok) and (not direct_ok) and main_invalid and direct_invalid:
    lines.append('## Classification')
    lines.append('Parse/protocol-mapping layer problem. Same Claude Code CLI prompt fails through both proxy and direct-egress bridges with invalid request/mapping wording.')
elif (not opencode_ok) and upstream_quota:
    lines.append('## Classification')
    lines.append('Upstream account/model/provider problem. opencode direct control also fails with quota/payment/provider wording.')
elif (not main_ok) and (not direct_ok) and opencode_ok:
    lines.append('## Classification')
    lines.append('Bridge parse/mapping or upstream request-shape compatibility problem. opencode direct passes, but both Claude CLI bridge paths fail.')
else:
    lines.append('## Classification')
    lines.append('Inconclusive from minimal probes. Inspect redacted debug logs, REST proxy snapshot, schema compare, and tool compare outputs in this artifact directory.')

lines.append('')
lines.append('## Evidence Files')
for name in sorted(p.name for p in art.iterdir()):
    if name.endswith(('.json', '.txt', '.log', '.jsonl', '.stderr', '.md')):
        lines.append(f'- `{name}`')

(art / 'classification.md').write_text('\n'.join(lines) + '\n')
print(art / 'classification.md')
PY

python3 "$ART/classify.py"
cat "$ART/classification.md"
```

Expected: a `classification.md` file with one of these classifications:

```text
Proxy/egress layer problem
Parse/protocol-mapping layer problem
Bridge parse/mapping or upstream request-shape compatibility problem
Upstream account/model/provider problem
Minimal Claude Code CLI path is healthy
Inconclusive
```

---

### Task 9: Stop Temporary Direct-Egress Server

**Files:**
- Read: `direct-server.pid`
- Modify: none

**Interfaces:**
- Consumes: PID file from Task 4.
- Produces: cleanup evidence.

- [ ] **Step 1: Stop only the temporary server**

```bash
if [ -f "$ART/direct-server.pid" ]; then
  PID="$(cat "$ART/direct-server.pid")"
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID"
    sleep 1
  fi
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    echo "direct server still running: $PID" | tee "$ART/cleanup-warning.txt"
  else
    echo "direct server stopped: $PID" | tee "$ART/cleanup.txt"
  fi
fi
```

Expected:

```text
direct server stopped: <pid>
```

---

## Final Decision Table

| Evidence | Conclusion |
|---|---|
| Claude CLI main proxy fails; Claude CLI direct-egress passes | Proxy/egress layer |
| Claude CLI main proxy and direct-egress both fail with invalid request/schema/tool mapping wording | Parse/protocol-mapping layer |
| Claude CLI direct-egress fails; opencode direct passes | Bridge parse/mapping/upstream request-shape compatibility |
| opencode direct also fails with quota/payment/provider text | Upstream account/model/provider |
| Claude CLI fails before any bridge request appears in debug/history | Claude CLI config/auth/client layer |
| Main proxy REST says `egress.ready=false`, proxy nodes duplicate/cooldown/degraded, Claude CLI error mentions egress/proxy | Proxy/egress confirmed |
| Minimal passes but schema/tool fails on both endpoints | Parse edge case confirmed |
| Minimal/schema/tool all pass | Current issue not reproducible; keep artifact as baseline and monitor recurrence |

## Commands to Run for One Full Pass

```bash
cd /home/light/GitHub/opencode2claude
# Execute Tasks 1 through 9 in order.
# Do not skip Task 9 cleanup.
```

## Success Criteria

- At least one Claude Code CLI probe runs through `http://127.0.0.1:4000`.
- At least one Claude Code CLI probe runs through `http://127.0.0.1:4010` direct-egress bridge.
- `classification.md` contains a conclusion backed by probe outputs.
- Temporary direct-egress server is stopped.
- No secret value appears in printed output.
