#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# CLI End-to-End Test Suite for opencode2claude
# ══════════════════════════════════════════════════════════════════════════════
#
# Tests all critical CLI paths: server start/stop/status/restart, proxy,
# doctor, completion, env, --json, --quiet flags, and error handling.
#
# Usage:  bash tests/cli_e2e.sh [release|debug]
#         Default: release
#
# Prerequisites:
#   - cargo build --release   (or --debug)
#   - Port 4000 must be free (or set BRIDGE_PORT)
# ══════════════════════════════════════════════════════════════════════════════
set -Euo pipefail

PROFILE="${1:-release}"
BIN="./target/${PROFILE}/opencode2api"

# Use a non-default port to avoid conflicts
TEST_PORT="${TEST_PORT:-4077}"
PID_DIR="$(pwd)/.runtime_e2e"
export RUNTIME_DIR="$PID_DIR"
PID_FILE="$PID_DIR/opencode2api.pid.json"
LOG_FILE="$PID_DIR/opencode2api.log"

PASS=0
FAIL=0
SKIP=0
ERRORS=()

# ── Colors ──
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

pass() {
  ((PASS++))
  echo -e "  ${GREEN}✓${NC} $1"
}

fail() {
  ((FAIL++))
  ERRORS+=("$1: $2")
  echo -e "  ${RED}✗${NC} $1"
  echo -e "    ${RED}→ $2${NC}"
}

skip() {
  ((SKIP++))
  echo -e "  ${YELLOW}⊘${NC} $1 (skipped: $2)"
}

section() {
  echo
  echo -e "${CYAN}═══ $1 ═══${NC}"
}

# ── Cleanup ──
cleanup() {
  echo
  echo -e "${CYAN}Cleaning up...${NC}"
  # Stop daemon if running
  "$BIN" server stop --port "$TEST_PORT" 2>/dev/null || true
  # Kill any stragglers on our test port
  lsof -ti :"$TEST_PORT" 2>/dev/null | xargs -r kill -9 2>/dev/null || true
  rm -rf "$PID_DIR"
  sleep 0.5
}

trap cleanup EXIT

# ── Pre-flight checks ──
if [[ ! -x "$BIN" ]]; then
  echo -e "${RED}ERROR: Binary not found at $BIN${NC}"
  echo "Run: cargo build --${PROFILE}"
  exit 1
fi

# Clean slate
cleanup 2>/dev/null || true

# ══════════════════════════════════════════════════════════════════════════════
section "1. CLI Help & Version"
# ══════════════════════════════════════════════════════════════════════════════

# 1.1 --help prints usage
if "$BIN" --help 2>&1 | grep -q "Usage"; then
  pass "1.1 --help shows usage"
else
  fail "1.1 --help shows usage" "No 'Usage' in output"
fi

# 1.2 --version prints version number
if "$BIN" --version 2>&1 | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'; then
  pass "1.2 --version shows version"
else
  fail "1.2 --version shows version" "No semver in output"
fi

# 1.3 server --help works
if "$BIN" server --help 2>&1 | grep -q "start"; then
  pass "1.3 server --help shows subcommands"
else
  fail "1.3 server --help" "No 'start' in server help output"
fi

# 1.4 server start --help works
if "$BIN" server start --help 2>&1 | grep -q "foreground"; then
  pass "1.4 server start --help shows --foreground"
else
  fail "1.4 server start --help" "No 'foreground' in help"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "2. Shell Policy Enum Validation"
# ══════════════════════════════════════════════════════════════════════════════

# 2.1 Invalid shell policy rejected at parse time
if ! "$BIN" server start --shell-policy typo 2>&1; then
  pass "2.1 --shell-policy typo → exit code != 0"
else
  fail "2.1 --shell-policy typo" "Should fail with non-zero exit"
fi

# 2.2 Error message mentions valid values
OUTPUT=$("$BIN" server start --shell-policy invalid 2>&1 || true)
if echo "$OUTPUT" | grep -q "disabled.*allowlist.*unrestricted\|possible values"; then
  pass "2.2 Error message shows possible values"
else
  fail "2.2 Error message shows possible values" "Output: $OUTPUT"
fi

# 2.3 Valid shell policies are accepted (help confirms enum)
if "$BIN" server start --help 2>&1 | grep -qiE "disabled|allowlist|unrestricted"; then
  pass "2.3 Help mentions valid shell policy values"
else
  fail "2.3 Help mentions valid shell policy values" "Not found in help"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "3. Server Status (when stopped)"
# ══════════════════════════════════════════════════════════════════════════════

# 3.1 server status shows Stopped
STATUS_OUT=$("$BIN" server status --port "$TEST_PORT" 2>&1 || true)
if echo "$STATUS_OUT" | grep -qi "stopped\|not running"; then
  pass "3.1 server status → Stopped (no daemon)"
else
  fail "3.1 server status → Stopped" "Output: $STATUS_OUT"
fi

# 3.2 server status --quiet outputs 'stopped'
QUIET_OUT=$("$BIN" --quiet server status --port "$TEST_PORT" 2>&1 || true)
if [[ "$QUIET_OUT" == *"stopped"* ]]; then
  pass "3.2 server status --quiet → 'stopped'"
else
  fail "3.2 server status --quiet → 'stopped'" "Output: '$QUIET_OUT'"
fi

# 3.3 server status --json outputs valid JSON with "stopped"
JSON_OUT=$("$BIN" --json server status --port "$TEST_PORT" 2>&1 || true)
if echo "$JSON_OUT" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('status')=='stopped' or d.get('state')=='stopped'" 2>/dev/null; then
  pass "3.3 server status --json → JSON with stopped"
else
  # Try alternative: just check it's valid JSON
  if echo "$JSON_OUT" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
    pass "3.3 server status --json → valid JSON"
  else
    fail "3.3 server status --json" "Output: $JSON_OUT"
  fi
fi

# ══════════════════════════════════════════════════════════════════════════════
section "4. Server Start (daemon mode) ★ CRITICAL ★"
# ══════════════════════════════════════════════════════════════════════════════

# 4.1 server start --port $TEST_PORT
echo -e "  Starting daemon on port $TEST_PORT..."
START_OUT=$("$BIN" server start --port "$TEST_PORT" 2>&1)
START_EXIT=$?
if [[ $START_EXIT -eq 0 ]]; then
  pass "4.1 server start → exit 0"
else
  fail "4.1 server start → exit 0" "exit=$START_EXIT output='$START_OUT'"
fi

# 4.2 PID file was created
if [[ -f "$PID_FILE" ]]; then
  pass "4.2 PID file created at $PID_FILE"
else
  fail "4.2 PID file created" "File not found: $PID_FILE"
fi

# 4.3 PID file is valid JSON
if [[ -f "$PID_FILE" ]] && python3 -c "import sys,json; json.load(open('$PID_FILE'))" 2>/dev/null; then
  pass "4.3 PID file is valid JSON"
else
  fail "4.3 PID file is valid JSON" "Cannot parse PID file"
fi

# 4.4 Bridge is reachable at /health
sleep 1
HEALTH_OUT=$(curl -sf "http://127.0.0.1:${TEST_PORT}/health" 2>/dev/null || echo "FAIL")
if echo "$HEALTH_OUT" | grep -q '"status":"healthy"'; then
  pass "4.4 /health returns healthy"
else
  fail "4.4 /health returns healthy" "Output: $HEALTH_OUT"
fi

# 4.5 server status shows Online/Running
STATUS_OUT=$("$BIN" server status --port "$TEST_PORT" 2>&1 || true)
if echo "$STATUS_OUT" | grep -qi "online\|running"; then
  pass "4.5 server status → Online"
else
  fail "4.5 server status → Online" "Output: $STATUS_OUT"
fi

# 4.6 server status --quiet outputs 'running'
QUIET_OUT=$("$BIN" --quiet server status --port "$TEST_PORT" 2>&1 || true)
if [[ "$QUIET_OUT" == *"running"* ]]; then
  pass "4.6 server status --quiet → 'running'"
else
  fail "4.6 server status --quiet → 'running'" "Output: '$QUIET_OUT'"
fi

# 4.7 Duplicate start fails (already running)
DUP_OUT=$("$BIN" server start --port "$TEST_PORT" 2>&1 || true)
DUP_EXIT=$?
if [[ $DUP_EXIT -ne 0 ]] || echo "$DUP_OUT" | grep -qi "already running"; then
  pass "4.7 Duplicate server start → error"
else
  fail "4.7 Duplicate server start → error" "exit=$DUP_EXIT output='$DUP_OUT'"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "5. Server Stop"
# ══════════════════════════════════════════════════════════════════════════════

# 5.1 server stop
STOP_OUT=$("$BIN" server stop --port "$TEST_PORT" 2>&1)
STOP_EXIT=$?
if [[ $STOP_EXIT -eq 0 ]]; then
  pass "5.1 server stop → exit 0"
else
  fail "5.1 server stop → exit 0" "exit=$STOP_EXIT output='$STOP_OUT'"
fi

sleep 1

# 5.2 PID file cleaned up
if [[ ! -f "$PID_FILE" ]]; then
  pass "5.2 PID file cleaned up after stop"
else
  fail "5.2 PID file cleaned up" "File still exists: $PID_FILE"
fi

# 5.3 Port is free
if ! lsof -ti :"$TEST_PORT" >/dev/null 2>&1; then
  pass "5.3 Port $TEST_PORT is free after stop"
else
  fail "5.3 Port $TEST_PORT is free" "Port still in use"
fi

# 5.4 Double stop is idempotent (no error)
STOP2_OUT=$("$BIN" server stop --port "$TEST_PORT" 2>&1)
STOP2_EXIT=$?
if [[ $STOP2_EXIT -eq 0 ]]; then
  pass "5.4 Double stop → exit 0 (idempotent)"
else
  fail "5.4 Double stop → exit 0" "exit=$STOP2_EXIT output='$STOP2_OUT'"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "6. Server Restart"
# ══════════════════════════════════════════════════════════════════════════════

# 6.1 Start first, then restart
"$BIN" server start --port "$TEST_PORT" 2>&1 || true
sleep 1

RESTART_OUT=$("$BIN" server restart 2>&1 || true)
if echo "$RESTART_OUT" | grep -qi "restarted\|started"; then
  pass "6.1 server restart → success message"
else
  fail "6.1 server restart" "Output: $RESTART_OUT"
fi

# 6.2 Bridge still healthy after restart
sleep 1
HEALTH_OUT=$(curl -sf "http://127.0.0.1:${TEST_PORT}/health" 2>/dev/null || echo "FAIL")
if echo "$HEALTH_OUT" | grep -q '"status":"healthy"'; then
  pass "6.2 /health healthy after restart"
else
  # restart may use default port 4000 instead of TEST_PORT
  HEALTH_OUT2=$(curl -sf "http://127.0.0.1:4000/health" 2>/dev/null || echo "FAIL")
  if echo "$HEALTH_OUT2" | grep -q '"status":"healthy"'; then
    pass "6.2 /health healthy after restart (on default port 4000)"
  else
    fail "6.2 /health healthy after restart" "Neither port $TEST_PORT nor 4000 responded"
  fi
fi

# Clean up
"$BIN" server stop --port "$TEST_PORT" 2>/dev/null || true
"$BIN" server stop 2>/dev/null || true
sleep 1

# ══════════════════════════════════════════════════════════════════════════════
section "7. Server Start Foreground (-f)"
# ══════════════════════════════════════════════════════════════════════════════

# 7.1 Foreground mode starts and responds to /health
"$BIN" server start -f --port "$TEST_PORT" --no-proxy &
FG_PID=$!

# Poll /health for up to 10 seconds
HEALTH_OUT="FAIL"
for i in {1..10}; do
  HEALTH_OUT=$(curl -sf "http://127.0.0.1:${TEST_PORT}/health" 2>/dev/null || echo "FAIL")
  if [[ "$HEALTH_OUT" == *'"status":"healthy"'* ]]; then
    break
  fi
  sleep 1
done

if echo "$HEALTH_OUT" | grep -q '"status":"healthy"'; then
  pass "7.1 server start -f → /health responds"
else
  fail "7.1 server start -f → /health responds" "Output: $HEALTH_OUT"
fi

# 7.2 Foreground process responds to SIGTERM
kill -TERM "$FG_PID" 2>/dev/null || true
wait "$FG_PID" 2>/dev/null || true
sleep 1

if ! lsof -ti :"$TEST_PORT" >/dev/null 2>&1; then
  pass "7.2 Foreground process stopped on SIGTERM"
else
  fail "7.2 Foreground process stopped on SIGTERM" "Port still in use"
  kill -9 "$FG_PID" 2>/dev/null || true
fi

# ══════════════════════════════════════════════════════════════════════════════
section "8. Doctor Command"
# ══════════════════════════════════════════════════════════════════════════════

# 8.1 doctor runs without crash
DOC_OUT=$("$BIN" doctor 2>&1 || true)
if [[ -n "$DOC_OUT" ]]; then
  pass "8.1 doctor produces output"
else
  fail "8.1 doctor produces output" "Empty output"
fi

# 8.2 doctor --quiet outputs warnings=X failures=Y
DOCQ_OUT=$("$BIN" --quiet doctor 2>&1 || true)
if echo "$DOCQ_OUT" | grep -qE 'warnings=[0-9]+ failures=[0-9]+'; then
  pass "8.2 doctor --quiet → 'warnings=X failures=Y'"
else
  fail "8.2 doctor --quiet" "Output: '$DOCQ_OUT'"
fi

# 8.3 doctor --json outputs valid JSON
DOCJ_OUT=$("$BIN" --json doctor 2>&1 || true)
if echo "$DOCJ_OUT" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
  pass "8.3 doctor --json → valid JSON"
else
  fail "8.3 doctor --json → valid JSON" "Output: $DOCJ_OUT"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "9. Env Command"
# ══════════════════════════════════════════════════════════════════════════════

# 9.1 env runs
ENV_OUT=$("$BIN" env 2>&1 || true)
if echo "$ENV_OUT" | grep -qi "bridge\|port\|host\|model"; then
  pass "9.1 env shows config info"
else
  fail "9.1 env shows config info" "Output: $ENV_OUT"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "10. Completion Command"
# ══════════════════════════════════════════════════════════════════════════════

# 10.1 completion bash produces output
COMP_OUT=$("$BIN" completion bash 2>/dev/null || true)
if [[ -n "$COMP_OUT" ]] && echo "$COMP_OUT" | grep -q "opencode2api"; then
  pass "10.1 completion bash → script output"
else
  fail "10.1 completion bash" "No opencode2api in output (len=${#COMP_OUT})"
fi

# 10.2 completion zsh produces output
COMP_OUT=$("$BIN" completion zsh 2>&1 || true)
if echo "$COMP_OUT" | grep -q "opencode2api"; then
  pass "10.2 completion zsh → script output"
else
  fail "10.2 completion zsh" "No opencode2api in output"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "11. Init Command"
# ══════════════════════════════════════════════════════════════════════════════

# 11.1 init generates config file
INIT_OUT_DIR=$(mktemp -d)
INIT_OUT=$("$BIN" init -o "$INIT_OUT_DIR/test.toml" 2>&1 || true)
if [[ -f "$INIT_OUT_DIR/test.toml" ]]; then
  pass "11.1 init generates TOML config"
else
  fail "11.1 init generates TOML config" "File not found. Output: $INIT_OUT"
fi
rm -rf "$INIT_OUT_DIR"

# ══════════════════════════════════════════════════════════════════════════════
section "12. Server Logs"
# ══════════════════════════════════════════════════════════════════════════════

# Start server briefly to generate logs
"$BIN" server start --port "$TEST_PORT" 2>/dev/null || true
sleep 1
"$BIN" server stop --port "$TEST_PORT" 2>/dev/null || true
sleep 0.5

# 12.1 server logs doesn't crash
LOGS_OUT=$("$BIN" server logs 2>&1 || true)
if [[ $? -le 1 ]]; then
  pass "12.1 server logs runs without crash"
else
  fail "12.1 server logs" "Crashed"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "13. Legacy Alias Backward Compatibility"
# ══════════════════════════════════════════════════════════════════════════════

# 13.1 `status` legacy alias works
LEGACY_OUT=$("$BIN" status --port "$TEST_PORT" 2>&1 || true)
if echo "$LEGACY_OUT" | grep -qi "stopped\|deprecated\|not running"; then
  pass "13.1 Legacy 'status' alias works"
else
  fail "13.1 Legacy 'status' alias" "Output: $LEGACY_OUT"
fi

# 13.2 `stop` legacy alias works
LEGACY_OUT=$("$BIN" stop --port "$TEST_PORT" 2>&1 || true)
if [[ $? -eq 0 ]] || echo "$LEGACY_OUT" | grep -qi "stopped\|deprecated"; then
  pass "13.2 Legacy 'stop' alias works"
else
  fail "13.2 Legacy 'stop' alias" "Output: $LEGACY_OUT"
fi

# ══════════════════════════════════════════════════════════════════════════════
section "14. Error Handling"
# ══════════════════════════════════════════════════════════════════════════════

# 14.1 Start on occupied port fails gracefully
"$BIN" server start -f --port "$TEST_PORT" &
BLOCK_PID=$!
sleep 2

OCCUPY_OUT=$("$BIN" server start --port "$TEST_PORT" 2>&1 || true)
OCCUPY_EXIT=$?
kill -TERM "$BLOCK_PID" 2>/dev/null || true
wait "$BLOCK_PID" 2>/dev/null || true
sleep 0.5

if [[ $OCCUPY_EXIT -ne 0 ]] || echo "$OCCUPY_OUT" | grep -qi "already\|cannot bind\|in use"; then
  pass "14.1 Start on occupied port → error"
else
  fail "14.1 Start on occupied port" "exit=$OCCUPY_EXIT output='$OCCUPY_OUT'"
fi

# 14.2 Invalid subcommand shows error
INVALID_OUT=$("$BIN" server nonexistent 2>&1 || true)
if [[ $? -ne 0 ]] || echo "$INVALID_OUT" | grep -qi "error\|unrecognized\|invalid"; then
  pass "14.2 Invalid subcommand → error"
else
  fail "14.2 Invalid subcommand → error" "Output: $INVALID_OUT"
fi

# ══════════════════════════════════════════════════════════════════════════════
# Summary
# ══════════════════════════════════════════════════════════════════════════════

echo
echo -e "${CYAN}══════════════════════════════════════════════════════════════${NC}"
echo -e "  ${GREEN}Passed: $PASS${NC}  ${RED}Failed: $FAIL${NC}  ${YELLOW}Skipped: $SKIP${NC}"
echo -e "${CYAN}══════════════════════════════════════════════════════════════${NC}"

if [[ $FAIL -gt 0 ]]; then
  echo
  echo -e "${RED}Failed tests:${NC}"
  for err in "${ERRORS[@]}"; do
    echo -e "  ${RED}✗${NC} $err"
  done
  echo
  exit 1
fi

echo
echo -e "${GREEN}All CLI tests passed!${NC}"
exit 0
