#!/bin/sh
#
# install.sh - Install opencode2claude
#
# Auto-detects OS + arch, downloads the correct pre-built binary from GitHub
# releases, and installs it to /usr/local/bin (with sudo if needed) or
# ~/.local/bin as fallback.
#
# Usage
#   curl -fsSL https://raw.githubusercontent.com/nmhuei/opencode2claude/main/install.sh | sh
#   sh install.sh
#
# Environment variables
#   OPENCODE2CLAUDE_VERSION  Version tag to install (default: latest)
#   OPENCODE2CLAUDE_BINDIR   Install directory (default: auto-detect)
#

set -eu

# ── Metadata ──────────────────────────────────────────────────────────
REPO_OWNER="nmhuei"
REPO_NAME="opencode2claude"
REPO="${REPO_OWNER}/${REPO_NAME}"
PROJECT="opencode2claude"
GITHUB="https://github.com/${REPO}"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"

# ── Terminal colours (disabled when stdout is not a tty) ──────────────
if [ -t 1 ]; then
    BOLD='\033[1m'
    NC='\033[0m'
    GREEN='\033[0;32m'
    BLUE='\033[0;34m'
    YELLOW='\033[1;33m'
    RED='\033[0;31m'
    CYAN='\033[0;36m'
else
    BOLD=''; NC=''; GREEN=''; BLUE='';
    YELLOW=''; RED=''; CYAN=''
fi

# ── Logging helpers ───────────────────────────────────────────────────
info()   { printf "${BLUE}::${NC} %s\n" "$*"; }
ok()     { printf "${GREEN}OK${NC}  %s\n" "$*"; }
warn()   { printf "${YELLOW}WARN${NC} %s\n" "$*"; }
err()    { printf "${RED}ERR${NC}  %s\n" "$*"; }
header() { printf "${BOLD}%s${NC}\n" "$*"; }

# ── Cleanup handler ───────────────────────────────────────────────────
cleanup() {
    if [ -n "${tmpdir:-}" ] && [ -d "$tmpdir" ]; then
        rm -rf "$tmpdir"
    fi
}
trap cleanup EXIT INT TERM

# ══════════════════════════════════════════════════════════════════════
#  Platform detection
# ══════════════════════════════════════════════════════════════════════
detect_platform() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os_alias="linux"  ;;
        Darwin) os_alias="macos"  ;;
        *)
            err "Unsupported OS: ${os}"
            err "${PROJECT} currently supports Linux and macOS only."
            exit 1
            ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_alias="amd64" ;;
        aarch64|arm64) arch_alias="arm64" ;;
        *)
            err "Unsupported architecture: ${arch}"
            err "Supported architectures: x86_64, aarch64 (arm64)"
            exit 1
            ;;
    esac

    # Validate that a pre-built binary exists for this combination
    case "${os_alias}-${arch_alias}" in
        linux-amd64|macos-amd64|macos-arm64) ;;
        *)
            err "No pre-built binary for ${os_alias}-${arch_alias}"
            echo ""
            err "Available platforms:"
            err "  Linux    x86_64"
            err "  macOS    x86_64, arm64"
            echo ""
            err "For other platforms try: cargo install ${PROJECT}"
            exit 1
            ;;
    esac

    binary="${PROJECT}-${os_alias}-${arch_alias}"
}

# ══════════════════════════════════════════════════════════════════════
#  Download-tool detection
# ══════════════════════════════════════════════════════════════════════
find_download_tool() {
    if command -v curl >/dev/null 2>&1; then
        dl() { curl -fL -sS "$1" -o "$2"; }
    elif command -v wget >/dev/null 2>&1; then
        dl() { wget -q -O "$2" "$1"; }
    else
        err "Neither curl nor wget is available."
        err "Install curl or wget and try again."
        exit 1
    fi
}

# ══════════════════════════════════════════════════════════════════════
#  Version helpers
# ══════════════════════════════════════════════════════════════════════
fetch_latest_version() {
    # May fail due to rate-limiting or network — caller handles empty return.
    curl -fsS "$API_URL" 2>/dev/null |
        grep '"tag_name"' |
        sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' || true
}

get_installed_version() {
    if command -v opencode2claude >/dev/null 2>&1; then
        opencode2claude --version 2>/dev/null || printf ''
    fi
}

# ══════════════════════════════════════════════════════════════════════
#  Interactive confirmation
# ══════════════════════════════════════════════════════════════════════
confirm() {
    prompt="$1"
    default="$2"          # "yes" or "no"

    # Non-interactive — use default
    if [ ! -t 0 ]; then
        [ "$default" = "yes" ]
        return
    fi

    printf "  %s " "$prompt"
    reply=""
    read -r reply < /dev/tty 2>/dev/null || reply=""

    case "$reply" in
        y|Y|yes|Yes) return 0 ;;
        n|N|no|No)   return 1 ;;
        "")
            [ "$default" = "yes" ]
            return
            ;;
        *) return 1 ;;
    esac
}

# ══════════════════════════════════════════════════════════════════════
#  Install-directory selection
# ══════════════════════════════════════════════════════════════════════
choose_install_dir() {
    # 1. Env-var override
    if [ -n "${OPENCODE2CLAUDE_BINDIR:-}" ]; then
        installdir="$OPENCODE2CLAUDE_BINDIR"
        use_sudo=false
        mkdir -p "$installdir"
        return
    fi

    # 2. /usr/local/bin — with sudo when needed
    if [ -d /usr/local/bin ]; then
        if [ -w /usr/local/bin ]; then
            installdir="/usr/local/bin"
            use_sudo=false
        elif command -v sudo >/dev/null 2>&1; then
            installdir="/usr/local/bin"
            use_sudo=true
        else
            installdir="${HOME}/.local/bin"
            use_sudo=false
        fi
    else
        installdir="${HOME}/.local/bin"
        use_sudo=false
    fi

    mkdir -p "$installdir"

    # 3. Warn if the chosen directory is not on PATH
    case ":${PATH:-}:" in
        *":${installdir}:"*) ;;
        *)
            warn "${installdir} is not in your PATH"
            info  "Add it: export PATH=\"${installdir}:\$PATH\""
            ;;
    esac
}

# ══════════════════════════════════════════════════════════════════════
#  Installation
# ══════════════════════════════════════════════════════════════════════
do_install() {
    tmpdir="$(mktemp -d "/tmp/${PROJECT}.XXXXXX")"

    version="${OPENCODE2CLAUDE_VERSION:-latest}"
    if [ "$version" = "latest" ]; then
        download_url="${GITHUB}/releases/latest/download/${binary}"
    else
        download_url="${GITHUB}/releases/download/${version}/${binary}"
    fi

    target="${tmpdir}/${PROJECT}"

    info "Downloading ${BOLD}${binary}${NC}..."
    if ! dl "$download_url" "$target"; then
        echo ""
        err "Binary download failed."
        return 1
    fi
    echo ""

    chmod +x "$target"

    info "Installing to ${BOLD}${installdir}${NC}..."
    if [ "$use_sudo" = true ]; then
        sudo cp "$target" "${installdir}/${PROJECT}"
        sudo chmod +x "${installdir}/${PROJECT}"
    else
        cp "$target" "${installdir}/${PROJECT}"
        chmod +x "${installdir}/${PROJECT}"
    fi

    rm -f "$target"
}

# ══════════════════════════════════════════════════════════════════════
#  Verification
# ══════════════════════════════════════════════════════════════════════
verify_install() {
    if command -v opencode2claude >/dev/null 2>&1; then
        ver="$(opencode2claude --version 2>/dev/null)"
        ok "Installed: ${ver:-${PROJECT}}"
    else
        warn "Binary installed but not found in PATH."
        info "Make sure ${installdir} is in your PATH."
    fi
}

# ══════════════════════════════════════════════════════════════════════
#  Welcome message
# ══════════════════════════════════════════════════════════════════════
print_welcome() {
    echo ""
    header "================================================"
    header "  opencode2claude installed!"
    header "================================================"
    echo ""
    printf "  ${BOLD}Quick start${NC}\n"
    echo ""
    printf "  1. Start the bridge:\n"
    printf "     ${CYAN}opencode2claude${NC}\n"
    echo ""
    printf "  2. Use Claude Code with any LLM:\n"
    printf "     ${CYAN}claude${NC}\n"
    echo ""
    printf "  3. Use a specific model:\n"
    printf "     ${CYAN}OPENCODE_MODEL=\"openai/gpt-4o\" opencode2claude${NC}\n"
    echo ""
    printf "  ${BOLD}Resources${NC}\n"
    printf "    ${GITHUB}\n"
    printf "    opencode2claude --help\n"
    echo ""
}

# ══════════════════════════════════════════════════════════════════════
#  Fallback suggestions
# ══════════════════════════════════════════════════════════════════════
suggest_fallback() {
    echo ""
    err "Binary download failed."
    echo ""
    printf "  ${BOLD}Try one of these alternatives:${NC}\n"
    echo ""
    printf "  1. Install via Cargo (requires Rust toolchain):\n"
    printf "     ${CYAN}cargo install ${PROJECT}${NC}\n"
    echo ""
    printf "  2. Run via Docker:\n"
    printf "     ${CYAN}docker pull ghcr.io/${REPO}${NC}\n"
    echo ""
    printf "  3. Build from source:\n"
    printf "     ${CYAN}git clone ${GITHUB}.git${NC}\n"
    printf "     ${CYAN}cd ${PROJECT} && cargo build --release${NC}\n"
    echo ""
}

# ══════════════════════════════════════════════════════════════════════
#  Main
# ══════════════════════════════════════════════════════════════════════
main() {
    detect_platform
    find_download_tool

    # ── Check for existing installation ──
    info "Checking for existing installation..."
    existing="$(get_installed_version)"

    if [ -n "$existing" ]; then
        echo ""
        info "Found: ${BOLD}${existing}${NC}"
        latest_tag="$(fetch_latest_version)"

        if [ -n "$latest_tag" ]; then
            printf "  Latest release: ${BOLD}%s${NC}\n" "$latest_tag"

            # Strip prefix / suffix noise for simple string comparison
            installed_ver="$(printf '%s' "$existing" | sed 's/^[^0-9]*//' | sed 's/[^0-9.]*$//')"
            latest_ver="$(printf '%s' "$latest_tag" | sed 's/^v//')"

            if [ "$installed_ver" = "$latest_ver" ]; then
                ok "Already up to date (${installed_ver})."
                echo ""
                if confirm "Reinstall the same version? [y/N]" "no"; then
                    echo ""
                else
                    echo ""
                    info "Done."
                    exit 0
                fi
            else
                echo ""
                info "Upgrade available: ${installed_ver} -> ${latest_ver}"
                if confirm "Proceed with upgrade? [Y/n]" "yes"; then
                    echo ""
                else
                    echo ""
                    info "Skipping."
                    exit 0
                fi
            fi
        else
            echo ""
            warn "Could not determine the latest release version from GitHub."
            if confirm "Proceed with installation anyway? [Y/n]" "yes"; then
                echo ""
            else
                echo ""
                info "Skipping."
                exit 0
            fi
        fi
    else
        echo ""
        info "No existing installation found."
        echo ""
    fi

    choose_install_dir

    if do_install; then
        verify_install
        print_welcome
    else
        suggest_fallback
        exit 1
    fi
}

main "$@"
