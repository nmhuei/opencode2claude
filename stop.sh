#!/usr/bin/env bash

# Stop script for OpenCode2Claude Bridge
# Usage:
#   ./stop.sh          — Stop bridge + daemon, pause proxy containers (fast restart)
#   ./stop.sh --purge  — Stop everything and remove proxy containers entirely

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

PID_FILE=".bridge.pids"
PURGE=false

# Parse flags
for arg in "$@"; do
    case $arg in
        --purge) PURGE=true ;;
    esac
done

echo -e "${BLUE}=== Stopping OpenCode2Claude Processes ===${NC}"

# Stop systemd units if running
if systemctl --user is-active --quiet opencode-bridge 2>/dev/null; then
    echo -e "Stopping systemd unit: opencode-bridge"
    systemctl --user stop opencode-bridge 2>/dev/null
fi
if systemctl --user is-active --quiet opencode-serve 2>/dev/null; then
    echo -e "Stopping systemd unit: opencode-serve"
    systemctl --user stop opencode-serve 2>/dev/null
fi

if [ -f "$PID_FILE" ]; then
    while read -r pid; do
        if kill -0 "$pid" 2>/dev/null; then
            echo -e "Terminating process: $pid"
            kill "$pid" 2>/dev/null
            sleep 0.2
            # Force kill if still active
            if kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null
            fi
        else
            echo -e "Process $pid already stopped."
        fi
    done < "$PID_FILE"
    rm -f "$PID_FILE"
    echo -e "${GREEN}✓ Cleaned up all registered bridge processes.${NC}"
else
    echo -e "${YELLOW}No active bridge process registration file found (.bridge.pids).${NC}"
    echo -e "Checking for any running opencode2claude or opencode serve processes..."
    
    # Fallback to pkill for clean termination of defaults
    pkill -f "opencode2claude" && echo -e "Terminated running opencode2claude bridge"
    pkill -f "opencode serve" && echo -e "Terminated running opencode serve daemon"
    
    echo -e "${GREEN}✓ Cleanup complete.${NC}"
fi

# Catch any rogue process still holding the bridge port (regardless of PID file)
BRIDGE_PORT=${BRIDGE_PORT:-4000}
if nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
    rogue_pid=$(lsof -ti :"$BRIDGE_PORT" 2>/dev/null)
    if [ -n "$rogue_pid" ]; then
        echo -e "${YELLOW}Found process ($rogue_pid) still on port ${BRIDGE_PORT}. Force killing...${NC}"
        kill "$rogue_pid" 2>/dev/null
        sleep 0.3
        if kill -0 "$rogue_pid" 2>/dev/null; then
            kill -9 "$rogue_pid" 2>/dev/null
        fi
        sleep 0.2
        if nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
            echo -e "${RED}⚠ Port ${BRIDGE_PORT} is still occupied. You may need to kill it manually: lsof -i :${BRIDGE_PORT}${NC}"
        else
            echo -e "${GREEN}✓ Port ${BRIDGE_PORT} is now free.${NC}"
        fi
    fi
fi

# Handle Docker Proxy Pool containers
if command -v docker &>/dev/null && docker info &>/dev/null; then
    containers=$(docker ps -a --format '{{.Names}}' | grep "^opencode-warp-" | grep -v "opencode-warp-4$" | grep -v "opencode-warp-5$" || true)
    standby_containers=$(docker ps -a --format '{{.Names}}' | grep -E "^opencode-warp-(4|5)$" || true)
    if [ -n "$standby_containers" ]; then
        echo -e "${YELLOW}Skipping protected warm-standby proxies: 40004, 40005${NC}"
    fi
    # Also clean up stray WARP containers not in the numbered pool (e.g. deprecated warp-external on 40010)
    stray_containers=$(docker ps -a --format '{{.Names}}' | grep "^warp-external$" 2>/dev/null || true)
    if [ -n "$stray_containers" ]; then
        echo -e "${YELLOW}Cleaning up deprecated WARP container(s) (no longer in pool)...${NC}"
        for container_name in $stray_containers; do
            echo -e "  Removing container: $container_name"
            docker rm -f "$container_name" >/dev/null 2>&1 &
        done
        wait
    fi
    if [ -n "$containers" ]; then
        if [ "$PURGE" = true ]; then
            echo -e "${BLUE}Purging proxy pool containers (full removal)...${NC}"
            for container_name in $containers; do
                echo -e "  Removing container: $container_name"
                docker rm -f "$container_name" >/dev/null &
            done
            wait
            echo -e "${GREEN}✓ Proxy pool containers removed. Next start will create fresh containers.${NC}"
        else
            echo -e "${BLUE}Pausing proxy pool containers (preserving WARP registration)...${NC}"
            for container_name in $containers; do
                echo -e "  Stopping container: $container_name"
                docker stop -t 5 "$container_name" >/dev/null &
            done
            wait
            echo -e "${GREEN}✓ Proxy containers stopped. They will resume quickly on next start.${NC}"
            echo -e "  ${YELLOW}Tip: Use './stop.sh --purge' to fully remove containers.${NC}"
        fi
    fi
fi

# Inform about WARP CLI status if it is connected
if command -v warp-cli &>/dev/null; then
    warp_status=$(warp-cli status 2>/dev/null)
    if echo "$warp_status" | grep -q "Connected"; then
        echo -e "\n${YELLOW}Note: Cloudflare WARP is still connected on host.${NC}"
        echo -e "To disconnect host WARP, run: ${YELLOW}warp-cli disconnect${NC}"
    fi
fi
