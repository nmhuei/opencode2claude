#!/usr/bin/env bash

# Robust startup script for OpenCode2Claude Bridge
# Designed to be sourced: source start.sh

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Configurations (Overridable via environment variables)
BRIDGE_PORT=${BRIDGE_PORT:-4000}
OPENCODE_PORT=${OPENCODE_PORT:-4096}
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

# Start opencode serve daemon if not already running
if curl -s "http://127.0.0.1:${OPENCODE_PORT}/doc" > /dev/null; then
    echo -e "${GREEN}✓ OpenCode Daemon is already listening on port ${OPENCODE_PORT}.${NC}"
else
    echo -e "${BLUE}Starting OpenCode serve daemon in background (port ${OPENCODE_PORT})...${NC}"
    opencode serve --port "$OPENCODE_PORT" --hostname 127.0.0.1 > opencode_serve.log 2>&1 &
    DAEMON_PID=$!
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

# Start the python bridge in the background
echo -e "${BLUE}Starting API Bridge on port ${BRIDGE_PORT} in background...${NC}"
export BRIDGE_PORT
export OPENCODE_PORT

python3 bridge.py > bridge.log 2>&1 &
BRIDGE_PID=$!
echo "$BRIDGE_PID" >> "$PID_FILE"
echo -e "${GREEN}✓ Started API Bridge (PID: $BRIDGE_PID). Logs routed to bridge.log${NC}"

# Export the variables so they are active in the sourced terminal
export ANTHROPIC_API_KEY="opencode-bridge"
export ANTHROPIC_API_URL="http://127.0.0.1:${BRIDGE_PORT}/v1"

echo -e "\n${GREEN}✓ Setup completed successfully!${NC}"
echo -e "Environment variables set in current session:"
echo -e "  ${YELLOW}export ANTHROPIC_API_KEY=\"$ANTHROPIC_API_KEY\"${NC}"
echo -e "  ${YELLOW}export ANTHROPIC_API_URL=\"$ANTHROPIC_API_URL\"${NC}"
echo -e "\nYou can now run ${GREEN}claude${NC} directly in this terminal window."
echo -e "To stop the bridge and daemon later, run: ${YELLOW}./stop.sh${NC}"

if [ "$is_sourced" = false ]; then
    # If not sourced, wait to keep the foreground process alive
    echo -e "\n${BLUE}Press Ctrl+C to terminate the bridge and the background daemon.${NC}"
    trap 'echo -e "\n${YELLOW}Shutting down processes...${NC}"; kill $(cat '"$PID_FILE"') 2>/dev/null; rm -f '"$PID_FILE"'; exit 0' SIGINT SIGTERM
    wait "$BRIDGE_PID"
fi
