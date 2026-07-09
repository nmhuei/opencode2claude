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
cargo build --release --bin opencode2api --bin opencode2api-serve

BINARIES=(opencode2api opencode2api-serve)
ALIASES=(oc2api o2a)
STALE_BINARIES=(opencode2claude oc2api o2a)

# Cargo does not delete old binary artifacts after target names are removed.
# If target/release is in PATH, stale artifacts can shadow the installed symlinks.
for stale in "${STALE_BINARIES[@]}"; do
    rm -f "target/release/${stale}" "target/debug/${stale}"
done

for bin in "${BINARIES[@]}"; do
    if [ ! -f "target/release/${bin}" ]; then
        err "Compilation failed. Target binary not found: ${bin}"
        exit 1
    fi
done

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
        for bin in "${BINARIES[@]}"; do
            sudo cp "target/release/${bin}" "$INSTALL_DIR/"
            sudo chmod +x "$INSTALL_DIR/${bin}"
        done
        for alias in "${ALIASES[@]}"; do
            sudo rm -f "$INSTALL_DIR/${alias}"
            sudo ln -s "opencode2api" "$INSTALL_DIR/${alias}"
        done
    else
        err "Cannot write to ${INSTALL_DIR} and 'sudo' is not available."
        exit 1
    fi
else
    for bin in "${BINARIES[@]}"; do
        cp "target/release/${bin}" "$INSTALL_DIR/"
        chmod +x "$INSTALL_DIR/${bin}"
    done
    for alias in "${ALIASES[@]}"; do
        rm -f "$INSTALL_DIR/${alias}"
        ln -s "opencode2api" "$INSTALL_DIR/${alias}"
    done
fi

# 4. Verify installation
case ":${PATH:-}:" in
    *":${INSTALL_DIR}:"*)
        ok "Installation successful! ${BOLD}opencode2api${NC} installed; ${BOLD}oc2api${NC} and ${BOLD}o2a${NC} are symlink aliases."
        for bin in opencode2api oc2api o2a; do
            resolved="$(command -v "$bin" 2>/dev/null || true)"
            if [ -n "$resolved" ] && [ "$resolved" != "$INSTALL_DIR/$bin" ]; then
                warn "${bin} resolves to ${resolved}, not ${INSTALL_DIR}/${bin}. Check PATH order if you still see stale behavior."
            fi
        done
        for alias in "${ALIASES[@]}"; do
            if [ ! -L "$INSTALL_DIR/${alias}" ]; then
                warn "${alias} is not a symlink. Re-run ./uninstall-local.sh --all-path then ./install-local.sh if stale behavior remains."
            fi
        done
        case ":${PATH:-}:" in
            *"target/release"*|*"target/debug"*)
                warn "Your PATH contains a Cargo target directory. It can shadow installed CLI aliases with stale build artifacts. Prefer ${INSTALL_DIR} before target/*, or remove target/* from PATH."
                ;;
        esac
        ;;
    *)
        warn "Installation successful, but ${BOLD}${INSTALL_DIR}${NC} is not in your PATH."
        info "Add it: export PATH=\"${INSTALL_DIR}:\$PATH\""
        ;;
esac
