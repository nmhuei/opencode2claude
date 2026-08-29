#!/usr/bin/env bash
# Deterministic CLI end-to-end gate. It uses an isolated direct-egress config
# and never mutates Docker/WARP resources.
set -Eeuo pipefail

PROFILE="${1:-debug}"
BIN="./target/${PROFILE}/opencode2api"
EXPECTED_VERSION="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/opencode2api-cli-e2e.XXXXXX")"
RUNTIME_DIR="$TEST_ROOT/runtime"
CONFIG_FILE="$TEST_ROOT/opencode2api.toml"
INIT_FILE="$TEST_ROOT/generated.toml"
TEST_PORT="${TEST_PORT:-$(python3 - <<'PY'
import socket
s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()
PY
)}"
PID_FILE="$RUNTIME_DIR/opencode2api.pid.json"
FG_PID=""
PASS=0
FAIL=0
ERRORS=()
CAPTURE_OUTPUT=""
CAPTURE_CODE=0

pass() { PASS=$((PASS+1)); printf '  ✓ %s\n' "$1"; }
fail() { FAIL=$((FAIL+1)); ERRORS+=("$1: $2"); printf '  ✗ %s — %s\n' "$1" "$2"; }
section() { printf '\n=== %s ===\n' "$1"; }

capture() {
  set +e
  CAPTURE_OUTPUT=$("$@" 2>&1)
  CAPTURE_CODE=$?
  set -e
}

assert_success() {
  local name="$1"; shift
  capture "$@"
  if [[ $CAPTURE_CODE -eq 0 ]]; then pass "$name"; else fail "$name" "exit=$CAPTURE_CODE output=$CAPTURE_OUTPUT"; fi
}

assert_failure() {
  local name="$1"; shift
  capture "$@"
  if [[ $CAPTURE_CODE -ne 0 ]]; then pass "$name"; else fail "$name" "unexpected success: $CAPTURE_OUTPUT"; fi
}

assert_contains() {
  local name="$1" needle="$2"; shift 2
  capture "$@"
  if [[ $CAPTURE_CODE -eq 0 && "$CAPTURE_OUTPUT" == *"$needle"* ]]; then
    pass "$name"
  else
    fail "$name" "exit=$CAPTURE_CODE missing='$needle' output=$CAPTURE_OUTPUT"
  fi
}

assert_json() {
  local name="$1" expression="$2"; shift 2
  capture "$@"
  if [[ $CAPTURE_CODE -eq 0 ]] && printf '%s' "$CAPTURE_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert ($expression)" 2>/dev/null; then
    pass "$name"
  else
    fail "$name" "exit=$CAPTURE_CODE invalid/unexpected JSON: $CAPTURE_OUTPUT"
  fi
}

assert_json_exit() {
  local name="$1" expected_code="$2" expression="$3"; shift 3
  capture "$@"
  if [[ $CAPTURE_CODE -eq $expected_code ]] && printf '%s' "$CAPTURE_OUTPUT" | python3 -c "import json,sys; d=json.load(sys.stdin); assert ($expression)" 2>/dev/null; then
    pass "$name"
  else
    fail "$name" "exit=$CAPTURE_CODE expected=$expected_code invalid/unexpected JSON: $CAPTURE_OUTPUT"
  fi
}

assert_contains_exit() {
  local name="$1" expected_code="$2" needle="$3"; shift 3
  capture "$@"
  if [[ $CAPTURE_CODE -eq $expected_code && "$CAPTURE_OUTPUT" == *"$needle"* ]]; then
    pass "$name"
  else
    fail "$name" "exit=$CAPTURE_CODE expected=$expected_code missing='$needle' output=$CAPTURE_OUTPUT"
  fi
}

wait_http() {
  local path="$1" expected="$2"
  for _ in $(seq 1 100); do
    local code
    code=$(curl -sS -o "$TEST_ROOT/http-body" -w '%{http_code}' "http://127.0.0.1:${TEST_PORT}${path}" 2>/dev/null || true)
    if [[ "$code" == "$expected" ]]; then return 0; fi
    sleep 0.1
  done
  return 1
}

cleanup() {
  set +e
  if [[ -n "$FG_PID" ]]; then kill -TERM "$FG_PID" 2>/dev/null; wait "$FG_PID" 2>/dev/null; fi
  "$BIN" server stop --port "$TEST_PORT" >/dev/null 2>&1
  if [[ -f "$PID_FILE" ]]; then
    local pid
    pid=$(python3 -c "import json; print(json.load(open('$PID_FILE')).get('pid',''))" 2>/dev/null)
    [[ -n "$pid" ]] && kill -TERM "$pid" 2>/dev/null
  fi
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
  echo "Binary missing: $BIN" >&2
  exit 2
fi

mkdir -p "$RUNTIME_DIR"
cat >"$CONFIG_FILE" <<EOF
schema_version = 1
port = $TEST_PORT
host = "127.0.0.1"
model = "opencode/test-model"
shell_policy = "disabled"
egress_mode = "direct"
primary_proxies = []
warm_standby_proxies = []
runtime_dir = "$RUNTIME_DIR"
docker_binary = "/nonexistent/opencode2api-docker"
dashboard_admin_token = "dashboard-test-token-strong"
rest_api_token = "rest-test-token-strong"
metrics_enabled = true
EOF

# Prevent repository .env from overriding the isolated contract.
export BRIDGE_CONFIG_PATH="$CONFIG_FILE"
export RUNTIME_DIR
export BRIDGE_PORT="$TEST_PORT"
export BRIDGE_HOST="127.0.0.1"
export OPENCODE_MODEL="opencode/test-model"
export BRIDGE_SHELL_POLICY="disabled"
export BRIDGE_EGRESS_MODE=direct
export BRIDGE_DOCKER_BINARY="/nonexistent/opencode2api-docker"
export BRIDGE_PRIMARY_PROXIES=""
export BRIDGE_WARM_STANDBY_PROXIES=""
export BRIDGE_AUTH_TOKEN=""
export DASHBOARD_ADMIN_TOKEN=""
export REST_API_TOKEN=""
export TAVILY_API_KEY=""
export EXA_API_KEY=""
export SERPER_API_KEY=""
export SEARXNG_URL=""
export SEARXNG_API_KEY=""
export NO_COLOR=1

section "CLI parsing and output contracts"
assert_contains "help shows usage" "Usage:" "$BIN" --help
assert_contains "version shows package semver" "$EXPECTED_VERSION" "$BIN" --version
assert_contains "server help exposes lifecycle" "restart" "$BIN" server --help
assert_contains "proxy help exposes dry-run" "--dry-run" "$BIN" proxy restart --help
assert_failure "invalid subcommand exits non-zero" "$BIN" server nonexistent
assert_failure "invalid shell policy exits non-zero" "$BIN" server start --shell-policy invalid
assert_json_exit "stopped status JSON" 1 "d.get('status') == 'stopped' or d.get('state') == 'stopped'" "$BIN" --json server status --port "$TEST_PORT"
assert_json "safe config JSON" "d['bridge_port'] == $TEST_PORT and d['auth_enabled'] is False" "$BIN" --json server config
assert_json "environment JSON" "isinstance(d, dict)" "$BIN" --json env
assert_json "doctor JSON" "isinstance(d, dict)" "$BIN" --json doctor
assert_contains "bash completion generated" "opencode2api" "$BIN" completion bash
assert_contains "zsh completion generated" "opencode2api" "$BIN" completion zsh

section "Utility and credential workflows"
assert_json_exit "set env explains required parent-shell hook" 1 "d.get('status') == 'shell-hook-required' and d.get('install') == 'opencode2api shell install'" "$BIN" --json set env
assert_contains "shell hook renders managed marker" "opencode2api shell integration" "$BIN" shell hook --shell bash
SHELL_RC="$TEST_ROOT/test.bashrc"
: > "$SHELL_RC"
assert_json "shell install writes only isolated rc" "d.get('status') == 'ok' and d.get('action') == 'installed' and d.get('changed') is True" "$BIN" --json shell install --shell bash --rc "$SHELL_RC"
if grep -q 'opencode2api shell integration' "$SHELL_RC"; then pass "isolated shell rc contains managed hook"; else fail "isolated shell rc contains managed hook" "managed block missing"; fi
assert_json "shell install is idempotent" "d.get('status') == 'ok' and d.get('action') == 'installed' and d.get('changed') is False" "$BIN" --json shell install --shell bash --rc "$SHELL_RC"
assert_json "shell uninstall removes isolated hook" "d.get('status') == 'ok' and d.get('action') == 'uninstalled' and d.get('changed') is True" "$BIN" --json shell uninstall --shell bash --rc "$SHELL_RC"
if ! grep -q 'opencode2api shell integration' "$SHELL_RC"; then pass "isolated shell rc no longer contains managed hook"; else fail "isolated shell rc no longer contains managed hook" "managed block remained"; fi
assert_json "api-key generate is non-persistent by default" "len(d.get('keys', [])) == 2 and d.get('saved') is False and d.get('config_path') is None and all(k.startswith('test-') for k in d['keys'])" "$BIN" --json api-key generate --count 2 --bytes 16 --prefix test-
assert_failure "api-key rejects undersized entropy" "$BIN" api-key generate --bytes 8
assert_json "dashboard stopped status is machine-readable" "d.get('running') is False and isinstance(d.get('url'), str)" "$BIN" --json dashboard status

section "Non-destructive proxy commands"
assert_json "proxy list returns JSON array" "isinstance(d, list)" "$BIN" --json proxy ps
assert_json "proxy logs returns typed JSON envelope" "isinstance(d, dict) and isinstance(d.get('logs'), list) and isinstance(d.get('errors'), list)" "$BIN" --json proxy logs
assert_json "proxy restart dry-run reflects isolated empty pool" "d.get('dry_run') is True and d.get('action') == 'restart' and d.get('ports') == []" "$BIN" --json proxy restart --dry-run
assert_json "proxy purge dry-run reflects isolated empty pool" "d.get('dry_run') is True and d.get('action') == 'purge and recreate' and d.get('ports') == []" "$BIN" --json proxy purge --yes --dry-run

section "Config initialization and migration surface"
assert_success "init creates config" "$BIN" init --output "$INIT_FILE"
if [[ -f "$INIT_FILE" ]]; then
  pass "generated config exists"
else
  fail "generated config exists" "missing $INIT_FILE"
fi
assert_failure "init refuses overwrite without force" "$BIN" init --output "$INIT_FILE"
assert_success "init force overwrites" "$BIN" init --output "$INIT_FILE" --force

section "Managed daemon lifecycle"
assert_success "daemon starts in direct mode" "$BIN" server start --port "$TEST_PORT" --config "$CONFIG_FILE" --no-proxy
if [[ -f "$PID_FILE" ]]; then
  pass "PID file created"
else
  fail "PID file created" "missing $PID_FILE"
fi
if [[ -f "$PID_FILE" ]] && python3 - "$PID_FILE" <<'PY'
import json,sys
p=json.load(open(sys.argv[1]))
assert p['pid'] > 0
assert p.get('executable')
assert p.get('start_marker')
assert p.get('instance_id')
PY
then pass "PID file contains ownership identity"; else fail "PID file contains ownership identity" "invalid PID metadata"; fi

if wait_http /health 200 && grep -q '"status":"ok"' "$TEST_ROOT/http-body"; then pass "compatibility health is live"; else fail "compatibility health is live" "no 200/ok"; fi
if wait_http /health/live 200 && grep -q '"status":"live"' "$TEST_ROOT/http-body"; then pass "liveness endpoint is live"; else fail "liveness endpoint is live" "no 200/live"; fi
if wait_http /health/ready 200 && grep -q '"status":"ready"' "$TEST_ROOT/http-body"; then pass "direct-mode readiness is ready"; else fail "direct-mode readiness is ready" "no 200/ready"; fi

assert_json "running status JSON" "d.get('status') == 'running' or d.get('state') == 'running'" "$BIN" --json server status --port "$TEST_PORT"
assert_json "dashboard status JSON" "d['running'] is True" "$BIN" --json dashboard status
assert_json "dashboard start is idempotent" "d['status'] == 'ready'" "$BIN" --json dashboard start
assert_success "duplicate start is idempotent" "$BIN" server start --port "$TEST_PORT" --config "$CONFIG_FILE" --no-proxy
assert_success "server restart succeeds" "$BIN" server restart
if wait_http /health/live 200; then pass "server is live after restart"; else fail "server is live after restart" "liveness unavailable"; fi
assert_success "server logs command succeeds" "$BIN" server logs
assert_success "daemon stops" "$BIN" server stop --port "$TEST_PORT"
if [[ ! -f "$PID_FILE" ]]; then
  pass "PID file removed after stop"
else
  fail "PID file removed after stop" "still exists"
fi
if ! curl -fsS "http://127.0.0.1:${TEST_PORT}/health/live" >/dev/null 2>&1; then pass "port no longer serves after stop"; else fail "port no longer serves after stop" "server still responding"; fi
assert_success "second stop is idempotent" "$BIN" server stop --port "$TEST_PORT"

section "Foreground lifecycle and legacy aliases"
"$BIN" server start --foreground --port "$TEST_PORT" --config "$CONFIG_FILE" --no-proxy >"$TEST_ROOT/foreground.log" 2>&1 &
FG_PID=$!
if wait_http /health/live 200; then pass "foreground server starts"; else fail "foreground server starts" "liveness unavailable"; fi
if kill -0 "$FG_PID" 2>/dev/null; then
  kill -TERM "$FG_PID"
  wait "$FG_PID"
else
  fail "foreground process remains alive until SIGTERM" "process exited before termination"
fi
FG_PID=""
if ! curl -fsS "http://127.0.0.1:${TEST_PORT}/health/live" >/dev/null 2>&1; then pass "foreground SIGTERM is graceful"; else fail "foreground SIGTERM is graceful" "server still responding"; fi
assert_contains_exit "legacy status alias remains compatible" 1 "deprecated" "$BIN" status --port "$TEST_PORT"
assert_contains "legacy stop alias remains compatible" "deprecated" "$BIN" stop --port "$TEST_PORT"

section "Summary"
printf 'Passed: %d  Failed: %d\n' "$PASS" "$FAIL"
if [[ $FAIL -ne 0 ]]; then
  printf 'Failures:\n'
  printf '  - %s\n' "${ERRORS[@]}"
  exit 1
fi
printf 'All deterministic CLI E2E checks passed.\n'
