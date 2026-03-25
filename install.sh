#!/usr/bin/env bash
# LFFF installer — downloads prebuilt binary from GitHub Releases
# Works on any Linux (including immutable distros) and macOS
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --gui
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --both

set -euo pipefail

REPO="mrFrok/LibreFastbootFirmwareFlasher"

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

# Parse flags
INSTALL_CLI=true
INSTALL_GUI=false

for arg in "$@"; do
    case "$arg" in
        --gui)  INSTALL_CLI=false; INSTALL_GUI=true ;;
        --both) INSTALL_CLI=true;  INSTALL_GUI=true ;;
        --reinstall) : ;;  # accepted, no-op (always reinstalls)
        *) warn "Unknown flag: $arg" ;;
    esac
done

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
    if [ -w /usr/local/bin ]; then
        echo "/usr/local/bin"
        return
    fi
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

# Download and install one binary
install_binary() {
    local binary="$1"       # e.g. "lfff" or "lfff-gui"
    local asset_name="$2"   # e.g. "lfff-linux-x86_64.tar.gz"
    local version="$3"
    local install_dir="$4"

    local url="https://github.com/${REPO}/releases/download/${version}/${asset_name}"
    local tmp
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN

    info "Downloading $asset_name..."
    if command -v curl &>/dev/null; then
        curl -fsSL "$url" -o "$tmp/$asset_name"
    else
        wget -qO "$tmp/$asset_name" "$url"
    fi
    ok "Downloaded"

    tar xzf "$tmp/$asset_name" -C "$tmp"
    [ -f "$tmp/$binary" ] || err "Binary '$binary' not found in archive"
    chmod +x "$tmp/$binary"

    if [ "$install_dir" = "/usr/local/bin" ] && [ "$(id -u)" -ne 0 ]; then
        info "Installing to $install_dir (requires sudo)..."
        sudo cp "$tmp/$binary" "$install_dir/$binary"
    else
        cp "$tmp/$binary" "$install_dir/$binary"
    fi

    ok "Installed $binary to $install_dir/$binary"
}

main() {
    echo
    echo -e "${BOLD}LFFF Installer${NC}"
    echo -e "LibreFastbootFirmwareFlasher"
    echo

    local platform
    platform="$(detect_platform)"
    info "Platform: $platform"

    info "Fetching latest release..."
    local version
    version="$(get_latest_version)"
    [ -z "$version" ] && err "Could not determine latest version"
    ok "Latest version: $version"

    local install_dir
    install_dir="$(find_install_dir)"

    if $INSTALL_CLI; then
        install_binary "lfff" "lfff-${platform}.tar.gz" "$version" "$install_dir"
    fi

    if $INSTALL_GUI; then
        install_binary "lfff-gui" "lfff-gui-${platform}.tar.gz" "$version" "$install_dir"
    fi

    # PATH warning
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
        echo
        warn "$install_dir is not in your PATH"
        echo "  Add to your shell config:"
        echo -e "  ${BOLD}export PATH=\"$install_dir:\$PATH\"${NC}"
        echo
    fi

    echo
    if $INSTALL_CLI && command -v lfff &>/dev/null; then
        ok "lfff is ready! Run ${BOLD}lfff deps${NC} to install external tools."
    fi
    if $INSTALL_GUI && command -v lfff-gui &>/dev/null; then
        ok "lfff-gui is ready!"
    fi
    echo
}

main "$@"
