#!/usr/bin/env bash
# Short bounded lifecycle/health soak for scheduled Tier C.
set -Eeuo pipefail
BIN="${1:-./target/release/opencode2api}"
SOAK_SECONDS="${SOAK_SECONDS:-30}"
ROOT="$(mktemp -d -t opencode2api-soak.XXXXXX)"
PORT="$(python3 - <<'PY'
import socket
s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()
PY
)"
PID=""
cleanup() {
  set +e
  [[ -n "$PID" ]] && kill -TERM "$PID" 2>/dev/null
  [[ -n "$PID" ]] && wait "$PID" 2>/dev/null
  rm -rf "$ROOT"
}
trap cleanup EXIT

cat >"$ROOT/config.toml" <<EOF
schema_version = 1
port = $PORT
host = "127.0.0.1"
egress_mode = "direct"
primary_proxies = []
warm_standby_proxies = []
runtime_dir = "$ROOT/runtime"
shell_policy = "disabled"
EOF

BRIDGE_CONFIG_PATH="$ROOT/config.toml" \
BRIDGE_PORT="$PORT" BRIDGE_HOST=127.0.0.1 BRIDGE_EGRESS_MODE=direct \
BRIDGE_PRIMARY_PROXIES='' BRIDGE_WARM_STANDBY_PROXIES='' \
BRIDGE_AUTH_TOKEN='' DASHBOARD_ADMIN_TOKEN='' REST_API_TOKEN='' \
"$BIN" serve --config "$ROOT/config.toml" --port "$PORT" >"$ROOT/server.log" 2>&1 &
PID=$!

for _ in $(seq 1 100); do
  curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null 2>&1 && break
  kill -0 "$PID" 2>/dev/null || { cat "$ROOT/server.log" >&2; exit 1; }
  sleep 0.1
done
curl -fsS "http://127.0.0.1:$PORT/health/ready" >/dev/null

# Warm connections/allocators before measuring.
for _ in $(seq 1 20); do
  curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null
  curl -fsS "http://127.0.0.1:$PORT/v1/models" >/dev/null
done
RSS_START="$(ps -o rss= -p "$PID" | tr -d ' ')"
DEADLINE=$((SECONDS + SOAK_SECONDS))
REQUESTS=0
while (( SECONDS < DEADLINE )); do
  curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null
  curl -fsS "http://127.0.0.1:$PORT/health/ready" >/dev/null
  curl -fsS "http://127.0.0.1:$PORT/v1/models" >/dev/null
  kill -0 "$PID"
  REQUESTS=$((REQUESTS + 3))
done
RSS_END="$(ps -o rss= -p "$PID" | tr -d ' ')"
GROWTH=$((RSS_END - RSS_START))
if (( GROWTH > 65536 )); then
  echo "soak-smoke: RSS grew ${GROWTH} KiB (> 65536 KiB)" >&2
  exit 1
fi

kill -TERM "$PID"
wait "$PID"
PID=""
if curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null 2>&1; then
  echo "soak-smoke: server still responds after graceful shutdown" >&2
  exit 1
fi

echo "soak-smoke: PASS requests=$REQUESTS rss_start_kib=$RSS_START rss_end_kib=$RSS_END"
