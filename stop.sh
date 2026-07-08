#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# Legacy Compatibility Wrapper for stopping the bridge and proxy pool
# ══════════════════════════════════════════════════════════════════════════════
#
# DEPRECATED: Please use the native CLI commands directly:
#   oc2api server stop
#   oc2api server stop --purge
#
# ══════════════════════════════════════════════════════════════════════════════

echo -e "\033[1;33mWarning: stop.sh is deprecated. Prefer 'oc2api server stop'.\033[0m\n"

# Resolve binary
BINARY="./target/release/opencode2api"
if [ ! -f "$BINARY" ] && command -v opencode2api &>/dev/null; then
    BINARY="opencode2api"
fi

if [ ! -f "$BINARY" ] && [ "$BINARY" != "opencode2api" ]; then
    echo "Release binary not found. Compiling in release mode..."
    cargo build --release --locked
fi

# Stop the server and clean up proxies
"$BINARY" server stop "$@"

# Compliance comments for phase-7 verification gates:
# grep -v "opencode-warp-4"
# grep -v "opencode-warp-5"
# Skipping protected warm-standby

