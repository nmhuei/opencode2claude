#!/usr/bin/env bash
# ══════════════════════════════════════════════════════════════════════════════
# Local Uninstall Script for opencode2api CLI aliases
# ══════════════════════════════════════════════════════════════════════════════
#
# Removes locally installed CLI binaries from common PATH directories:
#   opencode2api, oc2api, o2a, opencode2api-serve; also removes legacy opencode2claude if present
#
# Usage:
#   ./uninstall-local.sh              # remove from common local install dirs
#   ./uninstall-local.sh --dry-run    # show what would be removed
#   ./uninstall-local.sh --all-path   # also scan every directory in PATH
#
# The script removes only files/symlinks whose basename matches one of the known
# CLI aliases. It never removes directories.
# ══════════════════════════════════════════════════════════════════════════════
set -euo pipefail

NC=$'\033[0m'
GREEN=$'\033[0;32m'
BLUE=$'\033[0;34m'
YELLOW=$'\033[1;33m'
RED=$'\033[0;31m'

info() { printf '%s::%s %s\n' "${BLUE}" "${NC}" "$*"; }
ok()   { printf '%sOK%s  %s\n' "${GREEN}" "${NC}" "$*"; }
warn() { printf '%sWARN%s %s\n' "${YELLOW}" "${NC}" "$*"; }
err()  { printf '%sERR%s  %s\n' "${RED}" "${NC}" "$*"; }

DRY_RUN=false
ALL_PATH=false

for arg in "$@"; do
    case "$arg" in
        --dry-run|-n) DRY_RUN=true ;;
        --all-path) ALL_PATH=true ;;
        --help|-h)
            sed -n '1,22p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            err "Unknown argument: $arg"
            echo "Usage: ./uninstall-local.sh [--dry-run] [--all-path]"
            exit 2
            ;;
    esac
done

HOOK_BEGIN='# >>> opencode2api shell integration >>>'
HOOK_END='# <<< opencode2api shell integration <<<'

remove_shell_hook_file() {
    local rc="$1"
    [ -f "$rc" ] || return 0
    grep -Fq "$HOOK_BEGIN" "$rc" || return 0
    if [ "$DRY_RUN" = true ]; then
        printf '  would remove shell integration from %s\n' "$rc"
        return 0
    fi

    local tmp
    tmp="$(mktemp "${rc}.opencode2api.XXXXXX")"
    awk -v begin="$HOOK_BEGIN" -v end="$HOOK_END" '
        $0 == begin { skip = 1; next }
        $0 == end { skip = 0; next }
        !skip { print }
    ' "$rc" > "$tmp"
    mv "$tmp" "$rc"
    printf '  removed shell integration from %s\n' "$rc"
}

BINARIES=(opencode2claude opencode2api oc2api o2a opencode2api-serve)

DIRS=()
add_dir() {
    local dir="$1"
    [ -n "$dir" ] || return 0
    [ -d "$dir" ] || return 0
    local existing
    for existing in "${DIRS[@]:-}"; do
        [ "$existing" = "$dir" ] && return 0
    done
    DIRS+=("$dir")
}

if [ -n "${OPENCODE2API_UNINSTALL_DIRS:-}" ]; then
    IFS=':' read -r -a configured_dirs <<< "$OPENCODE2API_UNINSTALL_DIRS"
    for dir in "${configured_dirs[@]}"; do
        add_dir "$dir"
    done
else
    add_dir "$HOME/.local/bin"
    add_dir "$HOME/.cargo/bin"
    add_dir "/usr/local/bin"
fi

if [ "$ALL_PATH" = true ]; then
    IFS=':' read -r -a path_dirs <<< "${PATH:-}"
    for dir in "${path_dirs[@]}"; do
        # Never uninstall from this checkout's Cargo build output. Those are
        # build artifacts, not globally installed stale commands.
        case "$dir" in
            */target/debug|*/target/release) continue ;;
        esac
        add_dir "$dir"
    done
fi

removed=0
found=0
needs_sudo=false

# Also clean stale Cargo build artifacts in this checkout unless an isolated
# test/operator scope explicitly disables it.
if [ "${OPENCODE2API_SKIP_BUILD_ARTIFACTS:-false}" != "true" ]; then
    for stale in opencode2claude oc2api o2a; do
        for dir in target/release target/debug; do
            path="$dir/$stale"
            if [ -e "$path" ] || [ -L "$path" ]; then
                found=$((found + 1))
                if [ "$DRY_RUN" = true ]; then
                    printf '  would remove %s\n' "$path"
                else
                    rm -f "$path"
                    printf '  removed %s\n' "$path"
                    removed=$((removed + 1))
                fi
            fi
        done
    done
fi

info "Scanning install directories..."
for dir in "${DIRS[@]}"; do
    for bin in "${BINARIES[@]}"; do
        path="$dir/$bin"
        if [ -e "$path" ] || [ -L "$path" ]; then
            found=$((found + 1))
            if [ "$DRY_RUN" = true ]; then
                printf '  would remove %s\n' "$path"
                continue
            fi

            if [ -w "$dir" ]; then
                rm -f "$path"
                printf '  removed %s\n' "$path"
                removed=$((removed + 1))
            else
                if command -v sudo >/dev/null 2>&1; then
                    sudo rm -f "$path"
                    printf '  removed %s\n' "$path"
                    removed=$((removed + 1))
                else
                    needs_sudo=true
                    warn "Cannot remove ${path}: directory is not writable and sudo is unavailable."
                fi
            fi
        fi
    done
done

remove_shell_hook_file "${ZDOTDIR:-$HOME}/.zshrc"
remove_shell_hook_file "$HOME/.bashrc"

if [ "$DRY_RUN" = true ]; then
    ok "Dry run complete. ${found} matching file(s) found."
else
    ok "Uninstall complete. Removed ${removed} file(s)."
fi

if [ "$needs_sudo" = true ]; then
    warn "Some files remain because elevated permissions are required. Re-run with sudo or remove them manually."
fi

info "Current command resolution:"
for bin in opencode2api oc2api o2a; do
    resolved="$(command -v "$bin" 2>/dev/null || true)"
    if [ -n "$resolved" ]; then
        warn "${bin} still resolves to ${resolved}"
        printf '       run: type -a %s\n' "$bin"
    else
        printf '  %s: not found\n' "$bin"
    fi
done
