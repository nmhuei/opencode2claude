#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# Local Installation Script for opencode2api
# ══════════════════════════════════════════════════════════════════════════════
#
# Compiles the local code in release mode and copies the binary to a directory
# in your PATH (~/.local/bin, /usr/local/bin, or ~/.cargo/bin).
#
# Usage:
#   ./install-local.sh
# ══════════════════════════════════════════════════════════════════════════════
set -euo pipefail

# ── Colors ──
BOLD=$'\033[1m'
NC=$'\033[0m'
GREEN=$'\033[0;32m'
BLUE=$'\033[0;34m'
YELLOW=$'\033[1;33m'
RED=$'\033[0;31m'

info() { printf '%s::%s %s\n' "${BLUE}" "${NC}" "$*"; }
ok()   { printf '%sOK%s  %s\n' "${GREEN}" "${NC}" "$*"; }
warn() { printf '%sWARN%s %s\n' "${YELLOW}" "${NC}" "$*"; }
err()  { printf '%sERR%s  %s\n' "${RED}" "${NC}" "$*"; }

# 1. Build project in release mode
info "Compiling opencode2api in release mode..."
cargo build --release

if [ ! -f "target/release/opencode2api" ]; then
    err "Compilation failed. Target binary not found."
    exit 1
fi

# 2. Determine installation directory
INSTALL_DIR=""
USE_SUDO=false

if [ -d "$HOME/.local/bin" ]; then
    INSTALL_DIR="$HOME/.local/bin"
elif [ -d "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
    if [ ! -w "/usr/local/bin" ]; then
        USE_SUDO=true
    fi
else
    # Fallback to Cargo binary path
    INSTALL_DIR="$HOME/.cargo/bin"
fi

# 3. Copy binaries
info "Installing to ${BOLD}${INSTALL_DIR}/${NC}..."
mkdir -p "$INSTALL_DIR"

if [ "$USE_SUDO" = true ]; then
    if command -v sudo >/dev/null 2>&1; then
        sudo cp target/release/opencode2api "$INSTALL_DIR/"
        sudo cp target/release/oc2api "$INSTALL_DIR/"
        sudo cp target/release/o2a "$INSTALL_DIR/"
        sudo cp target/release/opencode2api-serve "$INSTALL_DIR/"
        sudo chmod +x "$INSTALL_DIR/opencode2api"
        sudo chmod +x "$INSTALL_DIR/oc2api"
        sudo chmod +x "$INSTALL_DIR/o2a"
        sudo chmod +x "$INSTALL_DIR/opencode2api-serve"
    else
        err "Cannot write to ${INSTALL_DIR} and 'sudo' is not available."
        exit 1
    fi
else
    cp target/release/opencode2api "$INSTALL_DIR/"
    cp target/release/oc2api "$INSTALL_DIR/"
    cp target/release/o2a "$INSTALL_DIR/"
    cp target/release/opencode2api-serve "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/opencode2api"
    chmod +x "$INSTALL_DIR/oc2api"
    chmod +x "$INSTALL_DIR/o2a"
    chmod +x "$INSTALL_DIR/opencode2api-serve"
fi

# 4. Verify installation
case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*)
        ok "Installation successful! ${BOLD}oc2api${NC} is now available globally."
        ;;
    *)
        warn "Installation successful, but ${BOLD}${INSTALL_DIR}${NC} is not in your PATH."
        info "Add it: export PATH=\"${INSTALL_DIR}:\$PATH\""
        ;;
esac
