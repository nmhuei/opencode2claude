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
    pkill -f "python3 bridge.py" && echo -e "Terminated running bridge.py"
    pkill -f "opencode serve" && echo -e "Terminated running opencode serve daemon"
    
    echo -e "${GREEN}✓ Cleanup complete.${NC}"
fi
