#!/usr/bin/env bash

# Robust startup script for Rust version of OpenCode2Claude Bridge
# Designed to be sourced: source start.sh

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Configurations (Overridable via environment variables)
BRIDGE_PORT=${BRIDGE_PORT:-4000}
OPENCODE_PORT=${OPENCODE_PORT:-4096}
OPENCODE_MODEL=${OPENCODE_MODEL:-"opencode/deepseek-v4-flash-free"}
PID_FILE=".bridge.pids"

# Determine if script is being sourced or run directly
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    is_sourced=true
else
    is_sourced=false
fi

echo -e "${BLUE}=== Starting OpenCode2Claude Setup ===${NC}"

# Check for opencode CLI
if ! command -v opencode &> /dev/null; then
    echo -e "${RED}Error: 'opencode' CLI command not found in PATH.${NC}"
    echo -e "Please install OpenCode first or make sure it is in your PATH."
    if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
fi

# Clean up stale pid file if it exists
if [ -f "$PID_FILE" ]; then
    echo -e "${YELLOW}Stale process file found. Cleaning up old processes...${NC}"
    while read -r pid; do
        if kill -0 "$pid" 2>/dev/null; then
            echo -e "Terminating process: $pid"
            kill "$pid" 2>/dev/null
            sleep 0.3
            # Force kill if still active
            if kill -0 "$pid" 2>/dev/null; then
                echo -e "Force killing process: $pid"
                kill -9 "$pid" 2>/dev/null
            fi
        fi
    done < "$PID_FILE"
    rm -f "$PID_FILE"
fi

# Kill any rogue opencode2claude process holding the bridge port (not tracked in PID file)
if nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
    rogue_pid=$(lsof -ti :"$BRIDGE_PORT" 2>/dev/null)
    if [ -n "$rogue_pid" ]; then
        echo -e "${YELLOW}Found untracked process ($rogue_pid) on port ${BRIDGE_PORT}. Killing it...${NC}"
        kill "$rogue_pid" 2>/dev/null
        sleep 0.5
        if kill -0 "$rogue_pid" 2>/dev/null; then
            kill -9 "$rogue_pid" 2>/dev/null
        fi
    fi

    # Wait for port to be freed (up to 5 seconds)
    echo -n "Waiting for port ${BRIDGE_PORT} to be freed..."
    for i in {1..10}; do
        if ! nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
            echo -e " ${GREEN}OK${NC}"
            break
        fi
        echo -n "."
        sleep 0.5
    done

    # Final check — abort if port is still busy
    if nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
        echo -e "\n${RED}Error: Port ${BRIDGE_PORT} is still in use after cleanup. Cannot start.${NC}"
        echo -e "Try manually: lsof -i :${BRIDGE_PORT} | kill"
        if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
    fi
fi

# Compile Rust binary if missing or code is updated
if [ ! -f "target/release/opencode2claude" ]; then
    echo -e "${BLUE}Compiling Rust bridge in release mode... (first build may take a minute)${NC}"
    cargo build --release
    if [ $? -ne 0 ]; then
        echo -e "${RED}Compilation failed.${NC}"
        if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
    fi
    echo -e "${GREEN}✓ Compilation completed successfully.${NC}"
fi

# Start opencode serve daemon if not already running
if curl -s "http://127.0.0.1:${OPENCODE_PORT}/doc" > /dev/null; then
    echo -e "${GREEN}✓ OpenCode Daemon is already listening on port ${OPENCODE_PORT}.${NC}"
else
    echo -e "${BLUE}Starting OpenCode serve daemon in background (port ${OPENCODE_PORT})...${NC}"
    opencode serve --port "$OPENCODE_PORT" --hostname 127.0.0.1 > opencode_serve.log 2>&1 &
    DAEMON_PID=$!
    disown "$DAEMON_PID" 2>/dev/null
    echo "$DAEMON_PID" >> "$PID_FILE"
    echo -e "${GREEN}✓ Started OpenCode serve daemon (PID: $DAEMON_PID). Logs routed to opencode_serve.log${NC}"
    
    # Wait for startup
    echo -n "Waiting for daemon to boot..."
    for i in {1..10}; do
        if curl -s "http://127.0.0.1:${OPENCODE_PORT}/doc" > /dev/null; then
            echo -e " ${GREEN}Ready!${NC}"
            break
        fi
        echo -n "."
        sleep 0.5
    done
fi

# ══════════════════════════════════════════════════════════════════════
# Auto-detect Cloudflare WARP proxy settings or spin up Docker proxy pool
# ══════════════════════════════════════════════════════════════════════
BRIDGE_ALL_PROXY=""
BRIDGE_NO_PROXY=""

if command -v docker &>/dev/null && docker info &>/dev/null; then
    echo -e "${GREEN}✓ Docker is running. Automating SOCKS5 proxy pool setup for multi-agent support...${NC}"
    PROXY_POOL_SIZE=${PROXY_POOL_SIZE:-5}
    PROXY_PORTS=()
    for ((idx=0; idx<PROXY_POOL_SIZE; idx++)); do
        PROXY_PORTS+=($((40001 + idx)))
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

    # Add the always-on external proxy (warp-external) to the pool
    BRIDGE_PROXIES_LIST+=("socks5://127.0.0.1:40010")

    BRIDGE_PROXIES=$(IFS=,; echo "${BRIDGE_PROXIES_LIST[*]}")
    export BRIDGE_PROXIES

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

    echo -e "  Proxies in pool: ${YELLOW}$BRIDGE_PROXIES${NC}"
    echo -e "  Requests will be dynamically load-balanced and failovered."
elif [ -n "$BRIDGE_PROXIES" ]; then
    echo -e "${GREEN}✓ Proxy Pool configuration detected via BRIDGE_PROXIES env var.${NC}"
    echo -e "  Proxies in pool: ${YELLOW}$BRIDGE_PROXIES${NC}"
    echo -e "  Requests will be balanced/failovered dynamically based on client API keys."
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

# Start the Rust bridge in the background
echo -e "${BLUE}Starting Rust API Bridge on port ${BRIDGE_PORT} in background...${NC}"
export BRIDGE_PORT
export OPENCODE_PORT
export OPENCODE_MODEL
export BRIDGE_PROXIES

nohup env ALL_PROXY="$BRIDGE_ALL_PROXY" NO_PROXY="$BRIDGE_NO_PROXY" ./target/release/opencode2claude > bridge.log 2>&1 &
BRIDGE_PID=$!
disown "$BRIDGE_PID" 2>/dev/null
echo "$BRIDGE_PID" >> "$PID_FILE"
echo -e "${GREEN}✓ Started Rust API Bridge (PID: $BRIDGE_PID). Logs routed to bridge.log${NC}"

# Export the variables so they are active in the sourced terminal
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_BASE_URL="http://127.0.0.1:${BRIDGE_PORT}/v1"
export OPENCODE_MODEL

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
echo -e "To stop the bridge and daemon later, run: ${YELLOW}./stop.sh${NC}"

if [ "$is_sourced" = false ]; then
    # If not sourced, wait to keep the foreground process alive
    echo -e "\n${BLUE}Press Ctrl+C to terminate the bridge and the background daemon.${NC}"
    trap 'echo -e "\n${YELLOW}Shutting down processes...${NC}"; kill $(cat '"$PID_FILE"') 2>/dev/null; rm -f '"$PID_FILE"'; exit 0' SIGINT SIGTERM
    wait "$BRIDGE_PID"
fi
