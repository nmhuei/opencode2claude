#!/usr/bin/env bash

# Stop script for OpenCode2Claude Bridge

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

PID_FILE=".bridge.pids"

echo -e "${BLUE}=== Stopping OpenCode2Claude Processes ===${NC}"

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
    echo -e "Checking for any running bridge.py or opencode serve processes..."
    
    # Fallback to pkill for clean termination of defaults
    pkill -f "opencode2claude" && echo -e "Terminated running opencode2claude bridge"
    pkill -f "opencode serve" && echo -e "Terminated running opencode serve daemon"
    
    echo -e "${GREEN}✓ Cleanup complete.${NC}"
fi

# Stop and remove Docker Proxy Pool containers if running
if command -v docker &> /dev/null && docker info &>/dev/null; then
    containers=$(docker ps -a --format '{{.Names}}' | grep "^opencode-warp-")
    if [ -n "$containers" ]; then
        echo -e "${BLUE}Stopping and removing multi-agent SOCKS5 proxy pool containers...${NC}"
        for container_name in $containers; do
            echo -e "Stopping and removing container: $container_name"
            docker rm -f "$container_name" >/dev/null &
        done
        wait # Wait for all background tasks to finish
        echo -e "${GREEN}✓ Stopped and removed proxy pool containers.${NC}"
    fi
fi

# Inform about WARP CLI status if it is connected
if command -v warp-cli &> /dev/null; then
    warp_status=$(warp-cli status 2>/dev/null)
    if echo "$warp_status" | grep -q "Connected"; then
        echo -e "\n${YELLOW}Note: Cloudflare WARP is still connected on host.${NC}"
        echo -e "To disconnect host WARP, run: ${YELLOW}warp-cli disconnect${NC}"
    fi
fi
