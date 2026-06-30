#!/usr/bin/env bash

# Legacy compatibility wrapper for OpenCode2Claude Bridge
#
# DEPRECATED: This script is a compatibility wrapper. Prefer the CLI supervisor:
#   opencode2claude start   → Start bridge daemon
#   opencode2claude status  → Check status
#   opencode2claude stop    → Stop bridge
#   opencode2claude proxy status  → Proxy pool status
#
# This wrapper provides:
#   1. Docker proxy pool bootstrap (if Docker available)
#   2. Bridge daemon start via supervisor CLI
#   3. Environment variable export (when sourced)
#
# Does NOT:
#   - Require OpenCode CLI
#   - Start opencode serve daemon
#   - Compile the binary
#   - Manage .bridge.pids directly

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Configurations (Overridable via environment variables)
BRIDGE_PORT=${BRIDGE_PORT:-4000}
OPENCODE_MODEL=${OPENCODE_MODEL:-""}

# Determine if script is being sourced or run directly
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    is_sourced=true
else
    is_sourced=false
fi

echo -e "${YELLOW}start.sh is a legacy compatibility wrapper. Prefer 'opencode2claude start'.${NC}"
echo -e ""

# Check binary exists
BINARY="./target/release/opencode2claude"
if [ ! -f "$BINARY" ]; then
    # Check if it's on PATH
    if command -v opencode2claude &>/dev/null; then
        BINARY="opencode2claude"
    else
        echo -e "${YELLOW}Binary not found at target/release/opencode2claude or in PATH.${NC}"
        echo -e "Building..."
        cargo build --release --locked || {
            echo -e "${RED}Build failed.${NC}"
            if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
        }
    fi
fi

# ══════════════════════════════════════════════════════════════════════
# Auto-detect Cloudflare WARP proxy settings or spin up Docker proxy pool
# ══════════════════════════════════════════════════════════════════════
BRIDGE_ALL_PROXY=""
BRIDGE_NO_PROXY=""

if command -v docker &>/dev/null && docker info &>/dev/null; then
    echo -e "${GREEN}✓ Docker is running. Automating SOCKS5 proxy pool setup for multi-agent support...${NC}"
    PRIMARY_POOL_SIZE=${PRIMARY_POOL_SIZE:-3}
    STANDBY_POOL_SIZE=${STANDBY_POOL_SIZE:-2}
    # Guard: total ports must stay within 40001-40005 range.
    # Port 40010 is deprecated and must never be generated.
    total_pool=$((PRIMARY_POOL_SIZE + STANDBY_POOL_SIZE))
    if [ "$total_pool" -gt 5 ]; then
      echo -e "${RED}Error: total pool size ($total_pool) exceeds allowed maximum of 5 (40001-40005). Port 40010 is deprecated.${NC}"
      echo -e "Set PRIMARY_POOL_SIZE=3 and STANDBY_POOL_SIZE=2 for the standard 2-tier configuration."
      if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
    fi
    if [ "$PRIMARY_POOL_SIZE" -lt 1 ] || [ "$STANDBY_POOL_SIZE" -lt 0 ]; then
      echo -e "${RED}Error: pool sizes must be positive (primary >= 1, standby >= 0).${NC}"
      if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
    fi
    PROXY_PORTS=()
    PRIMARY_PORTS=()
    STANDBY_PORTS=()
    for ((idx=0; idx<PRIMARY_POOL_SIZE; idx++)); do
        port=$((40001 + idx))
        PROXY_PORTS+=("$port")
        PRIMARY_PORTS+=("$port")
    done
    for ((idx=0; idx<STANDBY_POOL_SIZE; idx++)); do
        port=$((40001 + PRIMARY_POOL_SIZE + idx))
        PROXY_PORTS+=("$port")
        STANDBY_PORTS+=("$port")
    done
    BRIDGE_PROXIES_LIST=()
    new_container_count=0
    resumed_count=0
    already_running_count=0
    migrated_count=0

    # Fast entrypoint: skip WARP registration if config is already cached in volume
    FAST_ENTRYPOINT='if [ -f /etc/sing-box/config.json ]; then exec sing-box -c /etc/sing-box/config.json run; else exec /run/entrypoint.sh rws-cli-v6; fi'

    # ── Phase 1: Create/start all containers in parallel ──
    #   Uses Docker named volumes to persist WARP config across restarts.
    #   On first run: entrypoint registers with WARP (~20s), writes config to volume.
    #   On subsequent starts: cached config found → sing-box starts instantly (~1s).
    container_pids=()
    for i in "${!PROXY_PORTS[@]}"; do
        port=${PROXY_PORTS[$i]}
        container_name="opencode-warp-$((i+1))"
        volume_name="opencode-warp-config-$((i+1))"
        BRIDGE_PROXIES_LIST+=("socks5://127.0.0.1:$port")

        (
            _create_container() {
                docker run -d \
                    --name "$container_name" \
                    --restart always \
                    --cap-add=NET_ADMIN \
                    --sysctl net.ipv4.conf.all.src_valid_mark=1 \
                    -v "${volume_name}:/etc/sing-box" \
                    -p "$port":9091 \
                    --entrypoint /bin/sh \
                    ghcr.io/mon-ius/docker-warp-socks:latest \
                    -c "$FAST_ENTRYPOINT" >/dev/null 2>&1
            }

            _has_volume() {
                docker inspect --format '{{range .Mounts}}{{.Name}} {{end}}' "$container_name" 2>/dev/null | grep -q "$volume_name"
            }

            if docker ps --format '{{.Names}}' | grep -q "^${container_name}$"; then
                if _has_volume; then
                    # Already running with volume — nothing to do
                    echo "RUNNING" > "/tmp/.opencode_proxy_state_${port}"
                else
                    # Old container without volume — migrate
                    docker stop "$container_name" >/dev/null 2>&1
                    docker rm "$container_name" >/dev/null 2>&1
                    _create_container
                    echo "MIGRATED" > "/tmp/.opencode_proxy_state_${port}"
                fi
            elif docker ps -a --format '{{.Names}}' | grep -q "^${container_name}$"; then
                if _has_volume; then
                    # Stopped with cached config — fast resume!
                    docker start "$container_name" >/dev/null 2>&1
                    echo "RESUMED" > "/tmp/.opencode_proxy_state_${port}"
                else
                    # Old container without volume — migrate
                    docker rm "$container_name" >/dev/null 2>&1
                    _create_container
                    echo "MIGRATED" > "/tmp/.opencode_proxy_state_${port}"
                fi
            else
                # Brand new
                _create_container
                echo "NEW" > "/tmp/.opencode_proxy_state_${port}"
            fi
        ) &
        container_pids+=($!)
    done
    for pid in "${container_pids[@]}"; do
        wait "$pid"
    done

    # Collect container states
    for port in "${PROXY_PORTS[@]}"; do
        state=$(cat "/tmp/.opencode_proxy_state_${port}" 2>/dev/null || echo "UNKNOWN")
        rm -f "/tmp/.opencode_proxy_state_${port}"
        case "$state" in
            NEW)       ((new_container_count++)) ;;
            MIGRATED)  ((migrated_count++)) ;;
            RESUMED)   ((resumed_count++)) ;;
            RUNNING)   ((already_running_count++)) ;;
        esac
    done
    if [ "$already_running_count" -gt 0 ]; then
        echo -e "  ${GREEN}${already_running_count} container(s) already running${NC}"
    fi
    if [ "$resumed_count" -gt 0 ]; then
        echo -e "  ${GREEN}Resumed ${resumed_count} stopped container(s) (WARP cached — instant start)${NC}"
    fi
    if [ "$migrated_count" -gt 0 ]; then
        echo -e "  ${YELLOW}Migrated ${migrated_count} container(s) to volume-cached mode (one-time WARP registration)${NC}"
    fi
    if [ "$new_container_count" -gt 0 ]; then
        echo -e "  ${YELLOW}Created ${new_container_count} new container(s) (WARP registration required)${NC}"
    fi

    # Export 2-tier proxy pool:
    #   BRIDGE_PRIMARY_PROXIES — managed proxies 40001-40003
    #   BRIDGE_WARM_STANDBY_PROXIES — protected proxies 40004-40005
    BRIDGE_PRIMARY_PROXIES=$(IFS=,; echo "${BRIDGE_PROXIES_LIST[*]:0:PRIMARY_POOL_SIZE}")
    BRIDGE_WARM_STANDBY_PROXIES=$(IFS=,; echo "${BRIDGE_PROXIES_LIST[*]:PRIMARY_POOL_SIZE:STANDBY_POOL_SIZE}")
    export BRIDGE_PRIMARY_PROXIES
    export BRIDGE_WARM_STANDBY_PROXIES

    # ── Phase 2: Verify all proxies in parallel ──
    # verify_proxies [max_attempts] [sleep_interval] [label]
    # Sets FAILED_INDICES array with indices of failed proxies from VERIFY_PORTS
    FAILED_INDICES=()
    VERIFY_PORTS=("${PROXY_PORTS[@]}")

    verify_proxies() {
        local verify_dir max_attempts sleep_interval label
        verify_dir=$(mktemp -d)
        max_attempts=${1:-8}
        sleep_interval=${2:-3}
        label=${3:-""}

        echo -e "  ${BLUE}Verifying ${#VERIFY_PORTS[@]} proxy(ies) in parallel${label}...${NC}"

        for i in "${!VERIFY_PORTS[@]}"; do
            (
                port=${VERIFY_PORTS[$i]}
                for attempt in $(seq 1 "$max_attempts"); do
                    if curl -s -o /dev/null -w '' -x "socks5h://127.0.0.1:$port" --max-time 5 https://cloudflare.com/cdn-cgi/trace 2>/dev/null; then
                        echo "OK" > "${verify_dir}/port_${port}"
                        exit 0
                    fi
                    sleep "$sleep_interval"
                done
                echo "FAIL" > "${verify_dir}/port_${port}"
            ) &
        done
        wait

        # Collect results
        FAILED_INDICES=()
        local all_ok=true
        for i in "${!VERIFY_PORTS[@]}"; do
            port=${VERIFY_PORTS[$i]}
            # Find original index in PROXY_PORTS for display
            local orig_idx=$i
            for j in "${!PROXY_PORTS[@]}"; do
                if [ "${PROXY_PORTS[$j]}" = "$port" ]; then
                    orig_idx=$j
                    break
                fi
            done
            container_name="opencode-warp-$((orig_idx+1))"
            result=$(cat "${verify_dir}/port_${port}" 2>/dev/null || echo "FAIL")
            if [ "$result" = "OK" ]; then
                echo -e "  ${GREEN}✓${NC} ${container_name} (port ${port}) — Online"
            else
                echo -e "  ${RED}✗${NC} ${container_name} (port ${port}) — Failed"
                FAILED_INDICES+=("$orig_idx")
                all_ok=false
            fi
        done
        rm -rf "$verify_dir"

        if [ "$all_ok" = true ]; then
            echo -e "${GREEN}  ✓ All proxies verified and online!${NC}"
        fi
    }

    # Smart wait based on container state
    needs_registration=$(( new_container_count + migrated_count ))
    if [ "$needs_registration" -gt 0 ]; then
        echo -e "${YELLOW}  Waiting 20 seconds for Cloudflare WARP registration (${needs_registration} new/migrated)...${NC}"
        sleep 20
    elif [ "$resumed_count" -gt 0 ]; then
        # Cached config → sing-box starts in ~1s, no fixed sleep needed
        echo -e "  Cached WARP config detected — skipping wait..."
    fi

    # First verification pass (15 retries × 2s = 30s max per proxy, all parallel)
    # With cached config, proxies typically pass on 1st-2nd retry (~2-4s)
    verify_proxies 15 2

    # ── Phase 3: Auto-recover failed proxies ──
    if [ ${#FAILED_INDICES[@]} -gt 0 ]; then
        echo -e "\n  ${YELLOW}Recovering ${#FAILED_INDICES[@]} failed proxy(ies) — restarting containers...${NC}"

        # Restart failed containers in parallel
        for idx in "${FAILED_INDICES[@]}"; do
            port=${PROXY_PORTS[$idx]}
            container_name="opencode-warp-$((idx+1))"
            echo -e "  Restarting ${container_name}..."
            docker restart "$container_name" >/dev/null 2>&1 &
        done
        wait

        echo -e "  Waiting 15 seconds for WARP reconnection..."
        sleep 15

        # Re-verify only the failed proxies
        VERIFY_PORTS=()
        for idx in "${FAILED_INDICES[@]}"; do
            VERIFY_PORTS+=("${PROXY_PORTS[$idx]}")
        done
        verify_proxies 10 3 " (retry)"

        if [ ${#FAILED_INDICES[@]} -gt 0 ]; then
            echo -e "${YELLOW}  ⚠ ${#FAILED_INDICES[@]} proxy(ies) still offline. Bridge will route around them.${NC}"
        fi

        # Restore full ports list
        VERIFY_PORTS=("${PROXY_PORTS[@]}")
    fi

    echo -e "  Proxies in pool (primary): ${YELLOW}$BRIDGE_PRIMARY_PROXIES${NC}"
    echo -e "  Proxies in pool (standby): ${YELLOW}$BRIDGE_WARM_STANDBY_PROXIES${NC}"
else
    if command -v warp-cli &> /dev/null; then
        warp_settings=$(warp-cli settings list 2>/dev/null || warp-cli settings 2>/dev/null)
        if echo "$warp_settings" | grep -q "WarpProxy"; then
            echo -e "${GREEN}✓ Cloudflare WARP Proxy support detected on host.${NC}"
            BRIDGE_ALL_PROXY="socks5://127.0.0.1:40000"
            BRIDGE_NO_PROXY="localhost,127.0.0.1"
            echo -e "  Routing bridge traffic via ${YELLOW}socks5://127.0.0.1:40000${NC} (Other terminal commands remain unaffected)"
        fi
    fi
fi

# Start the Rust bridge via CLI supervisor
echo -e "${BLUE}Starting Rust API Bridge on port ${BRIDGE_PORT} in background...${NC}"
$BINARY start --port "$BRIDGE_PORT" ${OPENCODE_MODEL:+-m "$OPENCODE_MODEL"} > /dev/null 2>&1
BRIDGE_PID=$!
echo -e "${GREEN}✓ Started Rust API Bridge (PID: $BRIDGE_PID). Use 'opencode2claude status' to check.${NC}"

# Export the variables so they are active in the sourced terminal
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_BASE_URL="http://127.0.0.1:${BRIDGE_PORT}/v1"
[ -n "$OPENCODE_MODEL" ] && export OPENCODE_MODEL

echo -e "\n${GREEN}✓ Setup completed successfully!${NC}"
if [ "$is_sourced" = true ]; then
    echo -e "Environment variables set in current session:"
    echo -e "  ${YELLOW}export ANTHROPIC_API_KEY=\"$ANTHROPIC_API_KEY\"${NC}"
    echo -e "  ${YELLOW}export ANTHROPIC_BASE_URL=\"$ANTHROPIC_BASE_URL\"${NC}"
    echo -e "  ${YELLOW}export OPENCODE_MODEL=\"$OPENCODE_MODEL\"${NC}"
    echo -e "\nYou can now run ${GREEN}claude${NC} directly in this terminal window."
else
    echo -e "${YELLOW}To use Claude Code in your active terminal, copy and run these commands:${NC}"
    echo -e "\nexport ANTHROPIC_API_KEY=\"$ANTHROPIC_API_KEY\""
    echo -e "export ANTHROPIC_BASE_URL=\"$ANTHROPIC_BASE_URL\""
    echo -e "export OPENCODE_MODEL=\"$OPENCODE_MODEL\"\n"
fi
echo -e "To stop the bridge later, run: ${YELLOW}./stop.sh${NC} or ${YELLOW}opencode2claude stop${NC}"

if [ "$is_sourced" = false ]; then
    echo -e "\n${BLUE}Press Ctrl+C to stop the bridge.${NC}"
    trap 'echo -e "\n${YELLOW}Stopping bridge...${NC}"; '"$BINARY"' stop > /dev/null 2>&1; exit 0' SIGINT SIGTERM
    wait
fi
