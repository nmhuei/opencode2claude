#!/usr/bin/env bash
set -Eeuo pipefail

source "$ROOT_DIR/scripts/lib/common.sh" 2>/dev/null || true

# ── Port helpers ──
pick_free_port() {
  local port
  for port in {49152..65535}; do
    # Use bash's built-in /dev/tcp to check if port is listening
    if ! (: </dev/tcp/127.0.0.1/$port) 2>/dev/null; then
      echo "$port"
      return 0
    fi
  done
  error "No free port found after checking 16384 ports"
  return 1
}

# ── HTTP helpers ──
wait_for_http() {
  local url="$1"
  local timeout="${2:-10}"
  local interval="${3:-0.5}"
  local waited=0
  local max_iterations
  max_iterations=$(awk "BEGIN { printf \"%d\", $timeout / $interval }" 2>/dev/null || echo "$timeout")
  while [[ "$waited" -lt "$max_iterations" ]]; do
    if curl -sf "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$interval"
    waited=$((waited + 1))
  done
  return 1
}

# ── PID helpers ──
pid_alive() {
  local pid="$1"
  kill -0 "$pid" 2>/dev/null
}

wait_for_pid_exit() {
  local pid="$1"
  local timeout="${2:-5}"
  local elapsed=0
  while pid_alive "$pid" && [[ "$elapsed" -lt "$timeout" ]]; do
    sleep 1
    elapsed=$((elapsed + 1))
  done
  ! pid_alive "$pid"
}
