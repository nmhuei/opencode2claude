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

# Auto-detect Cloudflare WARP proxy settings or spin up Docker proxy pool
BRIDGE_ALL_PROXY=""
BRIDGE_NO_PROXY=""

if command -v docker &> /dev/null && docker info &>/dev/null; then
    echo -e "${GREEN}✓ Docker is running. Automating SOCKS5 proxy pool setup for multi-agent support...${NC}"
    PROXY_POOL_SIZE=${PROXY_POOL_SIZE:-5}
    PROXY_PORTS=()
    for ((idx=0; idx<PROXY_POOL_SIZE; idx++)); do
        PROXY_PORTS+=($((40001 + idx)))
    done
    BRIDGE_PROXIES_LIST=()
    any_new_created=false

    for i in "${!PROXY_PORTS[@]}"; do
        port=${PROXY_PORTS[$i]}
        container_name="opencode-warp-$((i+1))"
        BRIDGE_PROXIES_LIST+=("socks5://127.0.0.1:$port")

        if docker ps -a --format '{{.Names}}' | grep -q "^${container_name}$"; then
            if ! docker ps --format '{{.Names}}' | grep -q "^${container_name}$"; then
                echo -e "  Starting stopped proxy container: ${YELLOW}${container_name}${NC} on port ${port}..."
                docker start "$container_name" >/dev/null
            fi
        else
            echo -e "  Creating new WARP proxy container: ${YELLOW}${container_name}${NC} on port ${port}..."
            docker run -d \
                --name "$container_name" \
                --restart always \
                --cap-add=NET_ADMIN \
                --sysctl net.ipv4.conf.all.src_valid_mark=1 \
                -p "$port":9091 \
                ghcr.io/mon-ius/docker-warp-socks:latest >/dev/null
            any_new_created=true
        fi
    done

    BRIDGE_PROXIES=$(IFS=,; echo "${BRIDGE_PROXIES_LIST[*]}")
    export BRIDGE_PROXIES

    # Function to verify proxy connectivity
    verify_proxies() {
        local all_ok=true
        for i in "${!PROXY_PORTS[@]}"; do
            port=${PROXY_PORTS[$i]}
            container_name="opencode-warp-$((i+1))"
            echo -n "  Verifying proxy ${container_name} (port ${port})..."
            local ok=false
            for attempt in $(seq 1 12); do
                if curl -s -o /dev/null -w '' -x socks5h://127.0.0.1:$port --max-time 5 https://cloudflare.com/cdn-cgi/trace 2>/dev/null; then
                    echo -e " ${GREEN}✓ Online${NC}"
                    ok=true
                    break
                fi
                echo -n "."
                sleep 5
            done
            if [ "$ok" = false ]; then
                echo -e " ${RED}✗ Failed (container may still be initializing)${NC}"
                all_ok=false
            fi
        done
        if [ "$all_ok" = false ]; then
            echo -e "${YELLOW}  ⚠ Some proxies failed verification. They may come online shortly.${NC}"
        else
            echo -e "${GREEN}  ✓ All proxies verified and online!${NC}"
        fi
    }

    if [ "$any_new_created" = true ]; then
        echo -e "${YELLOW}  Waiting 10 seconds for Cloudflare WARP registration...${NC}"
        sleep 10
    fi
    verify_proxies
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
