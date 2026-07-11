#!/usr/bin/env bash
# Deterministic CLI end-to-end gate. It uses an isolated direct-egress config
# and never mutates Docker/WARP resources.
set -Eeuo pipefail

PROFILE="${1:-debug}"
BIN="./target/${PROFILE}/opencode2api"
ROOT_DIR="$(pwd)"
TEST_ROOT="$(mktemp -d -t opencode2api-cli-e2e.XXXXXX)"
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
assert_contains "version shows semver" "0.4.2" "$BIN" --version
assert_contains "server help exposes lifecycle" "restart" "$BIN" server --help
assert_contains "proxy help exposes dry-run" "--dry-run" "$BIN" proxy restart --help
assert_failure "invalid subcommand exits non-zero" "$BIN" server nonexistent
assert_failure "invalid shell policy exits non-zero" "$BIN" server start --shell-policy invalid
assert_json "stopped status JSON" "d.get('status') == 'stopped' or d.get('state') == 'stopped'" "$BIN" --json server status --port "$TEST_PORT"
assert_json "safe config JSON" "d['bridge_port'] == $TEST_PORT and d['auth_enabled'] is False" "$BIN" --json server config
assert_json "environment JSON" "isinstance(d, dict)" "$BIN" --json env
assert_json "doctor JSON" "isinstance(d, dict)" "$BIN" --json doctor
assert_contains "bash completion generated" "opencode2api" "$BIN" completion bash
assert_contains "zsh completion generated" "opencode2api" "$BIN" completion zsh

section "Non-destructive proxy commands"
assert_json "proxy list returns JSON array" "isinstance(d, list)" "$BIN" --json proxy ps
assert_json "proxy logs returns JSON array" "isinstance(d, list)" "$BIN" --json proxy logs
assert_json "proxy restart dry-run plans three actions" "len(d) == 3 and all(x['dry_run'] for x in d)" "$BIN" --json proxy restart --dry-run
assert_json "proxy purge dry-run plans six actions" "len(d) == 6 and all(x['dry_run'] for x in d)" "$BIN" --json proxy purge --yes --dry-run

section "Config initialization and migration surface"
assert_success "init creates config" "$BIN" init --output "$INIT_FILE"
[[ -f "$INIT_FILE" ]] && pass "generated config exists" || fail "generated config exists" "missing $INIT_FILE"
assert_failure "init refuses overwrite without force" "$BIN" init --output "$INIT_FILE"
assert_success "init force overwrites" "$BIN" init --output "$INIT_FILE" --force

section "Managed daemon lifecycle"
assert_success "daemon starts in direct mode" "$BIN" server start --port "$TEST_PORT" --config "$CONFIG_FILE" --no-proxy
[[ -f "$PID_FILE" ]] && pass "PID file created" || fail "PID file created" "missing $PID_FILE"
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
[[ ! -f "$PID_FILE" ]] && pass "PID file removed after stop" || fail "PID file removed after stop" "still exists"
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
assert_contains "legacy status alias remains compatible" "deprecated" "$BIN" status --port "$TEST_PORT"
assert_contains "legacy stop alias remains compatible" "deprecated" "$BIN" stop --port "$TEST_PORT"

section "Summary"
printf 'Passed: %d  Failed: %d\n' "$PASS" "$FAIL"
if [[ $FAIL -ne 0 ]]; then
  printf 'Failures:\n'
  printf '  - %s\n' "${ERRORS[@]}"
  exit 1
fi
printf 'All deterministic CLI E2E checks passed.\n'
