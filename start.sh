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
        fi
    done < "$PID_FILE"
    rm -f "$PID_FILE"
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

# Check bridge port
if nc -z 127.0.0.1 "$BRIDGE_PORT" 2>/dev/null; then
    echo -e "${RED}Error: Port ${BRIDGE_PORT} is already in use by another process.${NC}"
    if [ "$is_sourced" = true ]; then return 1; else exit 1; fi
fi

# Auto-detect Cloudflare WARP proxy settings
BRIDGE_ALL_PROXY=""
BRIDGE_NO_PROXY=""
if command -v warp-cli &> /dev/null; then
    warp_settings=$(warp-cli settings list 2>/dev/null || warp-cli settings 2>/dev/null)
    if echo "$warp_settings" | grep -q "WarpProxy"; then
        echo -e "${GREEN}✓ Cloudflare WARP Proxy support detected.${NC}"
        BRIDGE_ALL_PROXY="socks5://127.0.0.1:40000"
        BRIDGE_NO_PROXY="localhost,127.0.0.1"
        echo -e "  Routing bridge traffic via ${YELLOW}socks5://127.0.0.1:40000${NC} (Other terminal commands remain unaffected)"
    fi
fi

# Start the Rust bridge in the background
echo -e "${BLUE}Starting Rust API Bridge on port ${BRIDGE_PORT} in background...${NC}"
export BRIDGE_PORT
export OPENCODE_PORT
export OPENCODE_MODEL

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
