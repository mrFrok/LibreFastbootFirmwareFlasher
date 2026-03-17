#!/usr/bin/env bash
# LFFF installer — downloads prebuilt binary from GitHub Releases
# Works on any Linux (including immutable distros) and macOS
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash
#   wget -qO- https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash

set -euo pipefail

REPO="mrFrok/LibreFastbootFirmwareFlasher"
BINARY="lfff"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}$*${NC}"; }
ok()    { echo -e "${GREEN}✓${NC} $*"; }
warn()  { echo -e "${YELLOW}⚠${NC} $*"; }
err()   { echo -e "${RED}✗${NC} $*" >&2; exit 1; }

# Detect OS and arch
detect_platform() {
    local os arch
    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m)"

    case "$os" in
        linux*)  os="linux" ;;
        darwin*) os="macos" ;;
        *)       err "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             err "Unsupported architecture: $arch" ;;
    esac

    echo "${os}-${arch}"
}

# Find best install directory
find_install_dir() {
    # Try /usr/local/bin first (needs root)
    if [ -w /usr/local/bin ]; then
        echo "/usr/local/bin"
        return
    fi

    # Try ~/.local/bin (no root needed, works on immutable distros)
    local local_bin="$HOME/.local/bin"
    mkdir -p "$local_bin"
    echo "$local_bin"
}

# Fetch latest release tag from GitHub
get_latest_version() {
    local url="https://api.github.com/repos/${REPO}/releases/latest"
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[^"]*"\([^"]*\)".*/\1/'
    elif command -v wget &>/dev/null; then
        wget -qO- "$url" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[^"]*"\([^"]*\)".*/\1/'
    else
        err "curl or wget required"
    fi
}

main() {
    echo
    echo -e "${BOLD}LFFF Installer${NC}"
    echo -e "LibreFastbootFirmwareFlasher"
    echo

    # Detect platform
    local platform
    platform="$(detect_platform)"
    info "Platform: $platform"

    # Get latest version
    info "Fetching latest release..."
    local version
    version="$(get_latest_version)"
    [ -z "$version" ] && err "Could not determine latest version"
    ok "Latest version: $version"

    # Build download URL
    local asset="lfff-${platform}.tar.gz"
    local url="https://github.com/${REPO}/releases/download/${version}/${asset}"

    # Download
    local tmp
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    info "Downloading $asset..."
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" -o "$tmp/$asset"
    else
        wget -qO "$tmp/$asset" "$url"
    fi
    ok "Downloaded"

    # Extract
    tar xzf "$tmp/$asset" -C "$tmp"
    [ -f "$tmp/$BINARY" ] || err "Binary not found in archive"
    chmod +x "$tmp/$BINARY"

    # Install
    local install_dir
    install_dir="$(find_install_dir)"

    if [ "$install_dir" = "/usr/local/bin" ]; then
        if [ "$(id -u)" -eq 0 ]; then
            cp "$tmp/$BINARY" "$install_dir/$BINARY"
        else
            info "Installing to $install_dir (requires sudo)..."
            sudo cp "$tmp/$BINARY" "$install_dir/$BINARY"
        fi
    else
        cp "$tmp/$BINARY" "$install_dir/$BINARY"
    fi

    ok "Installed to $install_dir/$BINARY"

    # Check PATH
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
        echo
        warn "$install_dir is not in your PATH"
        echo "  Add to your shell config:"
        echo -e "  ${BOLD}export PATH=\"$install_dir:\$PATH\"${NC}"
        echo
    fi

    # Verify
    if command -v lfff &>/dev/null; then
        echo
        ok "lfff is ready! Run ${BOLD}lfff deps${NC} to install external tools."
    fi

    echo
}

main "$@"
