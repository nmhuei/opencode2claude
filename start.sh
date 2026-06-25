#!/usr/bin/env bash

# Startup script for Claude to OpenCode API Bridge

# Color formatting
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0;0m' # No Color

echo -e "${BLUE}=== Starting OpenCode2Claude Setup ===${NC}"

# Check if opencode is installed
if ! command -v opencode &> /dev/null; then
    echo -e "${YELLOW}Warning: 'opencode' command not found in PATH. Make sure it is installed.${NC}"
fi

# Check if opencode serve is already running
if curl -s http://127.0.0.1:4096/doc > /dev/null; then
    echo -e "${GREEN}✓ OpenCode Daemon is already running on port 4096.${NC}"
else
    echo -e "${BLUE}Starting OpenCode serve daemon in background...${NC}"
    opencode serve --port 4096 --hostname 127.0.0.1 > opencode_serve.log 2>&1 &
    OP_PID=$!
    echo -e "${GREEN}✓ Started OpenCode serve daemon (PID: $OP_PID). Logs routed to opencode_serve.log${NC}"
    sleep 1
fi

# Start the python bridge
echo -e "${BLUE}Starting Python API Bridge...${NC}"
python3 bridge.py
