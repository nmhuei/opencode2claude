# Claude to OpenCode API Bridge

An API bridge designed to route Anthropic-format requests (specifically from tools like Claude Code or agent wrappers) to a local **OpenCode** daemon process. 

This bridge accelerates execution times significantly (~3x faster) by utilizing a running OpenCode daemon instead of starting a new process on each model call.

## Features

- **Standard endpoint mapping**: Bridges `/v1/messages` (Claude's message format) directly to `opencode run`.
- **Auto-Attach**: Automatically detects if `opencode serve` is running on port `4096` and uses `--attach http://127.0.0.1:4096` for sub-second execution startup.
- **Server-Sent Events (SSE) Streaming**: Supports real-time text streaming from OpenCode.
- **Zero dependencies**: Built entirely on Python's standard library (`http.server`, `urllib`, `subprocess`).

---

## Getting Started

### 1. Start the OpenCode Daemon Server
First, run the OpenCode daemon in the background to listen for commands:
```bash
opencode serve --port 4096 --hostname 127.0.0.1
```

### 2. Start the API Bridge
Run this python bridge to listen on port `4000`:
```bash
python3 bridge.py
```

### 3. Route Your Requests
Point your API calls (using Claude Code, custom agents, or wrappers) to:
- **Base URL**: `http://127.0.0.1:4000`
- **Model Endpoint**: `/v1/messages`

If you are using Claude Code CLI, set the `API_BASE_URL` or equivalent environment variable:
```bash
export ANTHROPIC_API_KEY="opencode-bridge" # Or any dummy value
export ANTHROPIC_API_URL="http://127.0.0.1:4000/v1"
claude
```

---

## File Structure

- `bridge.py`: The HTTP server bridging code.
- `start.sh`: A shell script to automatically launch both the daemon and the bridge.

## License
MIT
