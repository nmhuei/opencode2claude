#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# Universal Uninstaller for opencode2api & opencode2claude
# ══════════════════════════════════════════════════════════════════════════════
#
# Completely and cleanly removes:
#   1. Running bridge daemon processes and services
#   2. Installed binaries (opencode2api, oc2api, o2a, opencode2claude, opencode2api-serve)
#   3. Configuration & data directories (~/.opencode2api, ~/.opencode2claude)
#   4. Shell integration hooks from ~/.bashrc, ~/.zshrc, ~/.profile, ~/.bash_profile
#   5. Temporary runtime sockets, PID files, and logs
#
# Usage:
#   ./uninstall.sh                # Interactive uninstall with confirmation
#   ./uninstall.sh --yes          # Non-interactive / unattended uninstall
#   ./uninstall.sh --dry-run      # Show what would be removed without touching anything
#   ./uninstall.sh --keep-config  # Remove binaries and hooks but keep user config & database
# ══════════════════════════════════════════════════════════════════════════════

set -euo pipefail

# ── Terminal colours ──────────────────────────────────────────────────────────
if [ -t 1 ]; then
    BOLD=$'\033[1m'
    NC=$'\033[0m'
    GREEN=$'\033[0;32m'
    BLUE=$'\033[0;34m'
    YELLOW=$'\033[1;33m'
    RED=$'\033[0;31m'
    CYAN=$'\033[0;36m'
else
    BOLD=''; NC=''; GREEN=''; BLUE='';
    YELLOW=''; RED=''; CYAN=''
fi

info()   { printf '%s::%s %s\n' "${BLUE}" "${NC}" "$*"; }
ok()     { printf '%sOK%s  %s\n' "${GREEN}" "${NC}" "$*"; }
warn()   { printf '%sWARN%s %s\n' "${YELLOW}" "${NC}" "$*"; }
err()    { printf '%sERR%s  %s\n' "${RED}" "${NC}" "$*"; }
header() { printf '%s%s%s\n' "${BOLD}" "$*" "${NC}"; }

DRY_RUN=false
ASSUME_YES=false
KEEP_CONFIG=false

for arg in "$@"; do
    case "$arg" in
        --dry-run|-n) DRY_RUN=true ;;
        --yes|-y)     ASSUME_YES=true ;;
        --keep-config) KEEP_CONFIG=true ;;
        --help|-h)
            sed -n '1,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            err "Unknown option: $arg"
            echo "Usage: ./uninstall.sh [--yes] [--dry-run] [--keep-config]"
            exit 2
            ;;
    esac
done

printf '%s\n' ""
header "================================================="
header "  opencode2api Uninstaller"
header "================================================="
printf '%s\n' ""

if [ "$DRY_RUN" = true ]; then
    warn "Running in DRY-RUN mode. No files or processes will be modified."
    printf '%s\n' ""
fi

# Confirmation prompt
if [ "$ASSUME_YES" = false ] && [ "$DRY_RUN" = false ] && [ -t 0 ]; then
    printf "Are you sure you want to uninstall opencode2api and remove its components? [y/N]: "
    read -r reply
    case "$reply" in
        y|Y|yes|Yes) ;;
        *)
            info "Uninstall aborted."
            exit 0
            ;;
    esac
    printf '%s\n' ""
fi

# ── 1. Stop running bridge processes ─────────────────────────────────────────
info "1. Stopping any running opencode2api / opencode2claude processes..."

# Try graceful CLI stop first
if command -v opencode2api >/dev/null 2>&1; then
    if [ "$DRY_RUN" = true ]; then
        info "  [dry-run] would run: opencode2api server stop"
    else
        opencode2api server stop 2>/dev/null || true
    fi
fi

# Check PID files
PID_FILES=(
    "${HOME}/.opencode2api/opencode2api.pid"
    "${HOME}/.opencode2claude/opencode2claude.pid"
    "/tmp/opencode2api.pid"
    "/tmp/opencode2claude.pid"
)

for pidfile in "${PID_FILES[@]}"; do
    if [ -f "$pidfile" ]; then
        pid="$(cat "$pidfile" 2>/dev/null || true)"
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            if [ "$DRY_RUN" = true ]; then
                info "  [dry-run] would kill daemon PID $pid from $pidfile"
            else
                kill "$pid" 2>/dev/null || true
                sleep 0.5
                kill -9 "$pid" 2>/dev/null || true
                ok "Stopped daemon process (PID: $pid)"
            fi
        fi
        [ "$DRY_RUN" = false ] && rm -f "$pidfile" 2>/dev/null || true
    fi
done

# Kill any leftover matching binaries
LEFT_PIDS="$(pgrep -f '(opencode2api|opencode2claude)' 2>/dev/null || true)"
if [ -n "$LEFT_PIDS" ]; then
    for p in $LEFT_PIDS; do
        [ "$p" = "$$" ] && continue
        if [ "$DRY_RUN" = true ]; then
            info "  [dry-run] would terminate process PID $p"
        else
            kill "$p" 2>/dev/null || true
        fi
    done
fi
ok "Bridge processes stopped."

# ── 2. Remove binaries and symlinks ──────────────────────────────────────────
info "2. Removing binaries and CLI aliases..."

BIN_NAMES=(
    "opencode2api"
    "opencode2claude"
    "oc2api"
    "o2a"
    "opencode2api-serve"
)

RAW_SEARCH_DIRS=(
    "/usr/local/bin"
    "/usr/bin"
    "${HOME}/.local/bin"
    "${HOME}/.cargo/bin"
    "${HOME}/bin"
)

IFS=':' read -r -a PATH_DIRS <<< "${PATH:-}"
for pdir in "${PATH_DIRS[@]}"; do
    [ -d "$pdir" ] || continue
    case "$pdir" in
        */target/debug|*/target/release) continue ;;
    esac
    RAW_SEARCH_DIRS+=("$pdir")
done

# Deduplicate search directories
SEARCH_DIRS=()
for dir in "${RAW_SEARCH_DIRS[@]}"; do
    [ -d "$dir" ] || continue
    seen=false
    for s in "${SEARCH_DIRS[@]:-}"; do
        [ "$s" = "$dir" ] && { seen=true; break; }
    done
    [ "$seen" = false ] && SEARCH_DIRS+=("$dir")
done

removed_bins=0

for bdir in "${SEARCH_DIRS[@]}"; do
    for bname in "${BIN_NAMES[@]}"; do
        bpath="${bdir}/${bname}"
        if [ -e "$bpath" ] || [ -L "$bpath" ]; then
            if [ "$DRY_RUN" = true ]; then
                info "  [dry-run] would remove binary: $bpath"
                removed_bins=$((removed_bins + 1))
            else
                if [ -w "$bdir" ] || [ -w "$bpath" ]; then
                    rm -f "$bpath"
                    ok "Removed: $bpath"
                    removed_bins=$((removed_bins + 1))
                elif command -v sudo >/dev/null 2>&1; then
                    sudo rm -f "$bpath"
                    ok "Removed (with sudo): $bpath"
                    removed_bins=$((removed_bins + 1))
                else
                    warn "Permission denied: cannot remove $bpath (sudo not available)"
                fi
            fi
        fi
    done
done

if [ "$removed_bins" -eq 0 ]; then
    info "No installed binaries found."
fi

# ── 3. Clean Shell Integration Hooks ─────────────────────────────────────────
info "3. Cleaning shell integration hooks..."

HOOK_BEGIN='# >>> opencode2api shell integration >>>'
HOOK_END='# <<< opencode2api shell integration <<<'

RC_FILES=()
for rc in "${HOME}/.bashrc" "${HOME}/.zshrc" "${HOME}/.bash_profile" "${HOME}/.profile" "${ZDOTDIR:-$HOME}/.zshrc"; do
    [ -f "$rc" ] || continue
    seen=false
    for s in "${RC_FILES[@]:-}"; do
        [ "$s" = "$rc" ] && { seen=true; break; }
    done
    [ "$seen" = false ] && RC_FILES+=("$rc")
done

for rc in "${RC_FILES[@]}"; do
    if grep -Fq "$HOOK_BEGIN" "$rc" 2>/dev/null; then
        if [ "$DRY_RUN" = true ]; then
            info "  [dry-run] would remove shell hook from $rc"
        else
            tmp="$(mktemp "${rc}.opencode2api.XXXXXX")"
            awk -v begin="$HOOK_BEGIN" -v end="$HOOK_END" '
                $0 == begin { skip = 1; next }
                $0 == end { skip = 0; next }
                !skip { print }
            ' "$rc" > "$tmp"
            mv "$tmp" "$rc"
            ok "Removed shell hook from: $rc"
        fi
    fi
done

# ── 4. Remove Configuration, Databases, and Runtime Data ──────────────────────
info "4. Cleaning configuration, cache, and logs..."

DATA_DIRS=(
    "${HOME}/.opencode2api"
    "${HOME}/.opencode2claude"
    "${HOME}/.config/opencode2api"
    "${HOME}/.config/opencode2claude"
    "${HOME}/.local/share/opencode2api"
    "${HOME}/.local/share/opencode2claude"
)

if [ "$KEEP_CONFIG" = true ]; then
    info "Keeping configuration directories as requested (--keep-config)."
else
    for dpath in "${DATA_DIRS[@]}"; do
        if [ -d "$dpath" ]; then
            if [ "$DRY_RUN" = true ]; then
                info "  [dry-run] would delete directory: $dpath"
            else
                rm -rf "$dpath"
                ok "Deleted directory: $dpath"
            fi
        fi
    done
fi

# Clean temp debug / log files
if [ "$DRY_RUN" = true ]; then
    info "  [dry-run] would clean temporary /tmp/opencode2* files"
else
    rm -rf /tmp/opencode2api* /tmp/opencode2claude* /tmp/.opencode2api* /tmp/.opencode2claude* 2>/dev/null || true
    ok "Cleaned temporary files in /tmp."
fi

printf '%s\n' ""
header "================================================="
if [ "$DRY_RUN" = true ]; then
    header "  Dry-run complete. No changes were made."
else
    header "  opencode2api has been completely uninstalled!"
fi
header "================================================="
printf '%s\n' ""
info "To refresh your current terminal session, run:"
printf '  %s%s%s\n' "${CYAN}" "source ~/.bashrc" "${NC}"
printf '%s\n' ""
