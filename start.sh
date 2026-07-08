#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# Legacy Compatibility Wrapper for starting the bridge and proxy pool
# ══════════════════════════════════════════════════════════════════════════════
#
# DEPRECATED: Please use the native CLI commands directly:
#   oc2api server start
#
# ══════════════════════════════════════════════════════════════════════════════

echo -e "\033[1;33mWarning: start.sh is deprecated. Prefer 'oc2api server start'.\033[0m\n"

# Resolve binary
BINARY="./target/release/opencode2api"
if [ ! -f "$BINARY" ] && command -v opencode2api &>/dev/null; then
    BINARY="opencode2api"
fi

if [ ! -f "$BINARY" ] && [ "$BINARY" != "opencode2api" ]; then
    echo "Release binary not found. Compiling in release mode..."
    cargo build --release --locked
fi

# Start the bridge (with proxy bootstrap built-in)
"$BINARY" server start "$@"

# Simple argument parsing to extract custom port for exporting
PORT=4000
while [[ $# -gt 0 ]]; do
    case $1 in
        -p|--port)
            PORT="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

# Export environment variables if sourced
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    export ANTHROPIC_API_KEY="opencode-bridge"
    export ANTHROPIC_BASE_URL="http://127.0.0.1:${PORT}/v1"
    echo -e "\n\033[0;32m✓ Environment variables exported to your active terminal session!\033[0m"
    echo -e "  export ANTHROPIC_API_KEY=\"$ANTHROPIC_API_KEY\""
    echo -e "  export ANTHROPIC_BASE_URL=\"$ANTHROPIC_BASE_URL\""
    echo -e "\nYou can now run \033[0;32mclaude\033[0m directly."
fi
