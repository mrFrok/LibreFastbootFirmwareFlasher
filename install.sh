#!/usr/bin/env bash
# LFFF installer — downloads prebuilt binaries from GitHub Releases
# Works on any Linux (including immutable distros) and macOS
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --cli-only
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --gui-only
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --version v1.2.3
#   curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --uninstall

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
INSTALL_GUI=true
UNINSTALL=false
VERSION=""

for arg in "$@"; do
    case "$arg" in
        --cli-only)  INSTALL_GUI=false ;;
        --gui-only)  INSTALL_CLI=false ;;
        --uninstall) UNINSTALL=true ;;
        --version=*) VERSION="${arg#*=}" ;;
        --reinstall) : ;;
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

# Remove one file with optional sudo
remove_file() {
    local path="$1"
    if [ -f "$path" ]; then
        if [ -w "$(dirname "$path")" ]; then
            rm -f "$path" && ok "Removed $path"
        else
            sudo rm -f "$path" && ok "Removed $path"
        fi
    fi
}

uninstall() {
    echo
    echo -e "${BOLD}LFFF Uninstaller${NC}"
    echo

    local dirs=("/usr/local/bin" "/usr/bin" "$HOME/.local/bin")
    for dir in "${dirs[@]}"; do
        remove_file "$dir/lfff"
        remove_file "$dir/lfff-gui"
    done

    # Remove desktop entry and icon on Linux
    if [ "$(uname -s)" = "Linux" ]; then
        remove_file "$HOME/.local/share/applications/lfff-gui.desktop"
        remove_file "$HOME/.local/share/icons/hicolor/scalable/apps/lfff-gui.svg"
        command -v update-desktop-database &>/dev/null && \
            update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
        command -v gtk-update-icon-cache &>/dev/null && \
            gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
    fi

    echo
    ok "LFFF uninstalled"
    echo
}

# Download and install one binary
install_binary() {
    local binary="$1"
    local asset_name="$2"
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

    local binary_path="$tmp/$binary"
    local is_app_bundle=false
    if [ ! -f "$binary_path" ]; then
        # macOS .app bundle
        binary_path="$tmp/LFFF.app/Contents/MacOS/$binary"
        if [ -f "$binary_path" ]; then
            is_app_bundle=true
        else
            err "Binary '$binary' not found in archive"
        fi
    fi
    chmod +x "$binary_path"

    if [ "$install_dir" = "/usr/local/bin" ] && [ "$(id -u)" -ne 0 ]; then
        info "Installing to $install_dir (requires sudo)..."
        sudo cp "$binary_path" "$install_dir/$binary"
    else
        cp "$binary_path" "$install_dir/$binary"
    fi

    ok "Installed $binary to $install_dir/$binary"

    # On macOS, offer to copy the .app bundle to /Applications
    if $is_app_bundle; then
        local app_src="$tmp/LFFF.app"
        local app_dst="/Applications/LFFF.app"
        if [ -d "$app_src" ] && [ ! -e "$app_dst" ]; then
            echo
            info "LFFF.app bundle found in archive."
            if command -v sudo &>/dev/null; then
                if sudo cp -r "$app_src" "$app_dst" 2>/dev/null; then
                    ok "Copied LFFF.app to /Applications"
                else
                    warn "Could not copy to /Applications. To install manually:"
                    echo "  sudo cp -r '$tmp/LFFF.app' /Applications"
                fi
            else
                warn "To use LFFF from Launchpad, copy the app bundle:"
                echo "  sudo cp -r '$tmp/LFFF.app' /Applications"
            fi
            echo
        fi
    fi
}

# Suggest shell PATH update
suggest_path() {
    local dir="$1"
    local rc
    case "${SHELL:-}" in
        */zsh)  rc="$HOME/.zshrc" ;;
        */fish) rc="$HOME/.config/fish/config.fish" ;;
        */bash) rc="$HOME/.bashrc" ;;
        *)      rc="$HOME/.profile" ;;
    esac
    echo
    warn "$dir is not in your PATH"
    echo "  Add to $rc:"
    if [[ "$SHELL" == *fish ]]; then
        echo -e "  ${BOLD}fish_add_path $dir${NC}"
    else
        echo -e "  ${BOLD}export PATH=\"$dir:\$PATH\"${NC}"
    fi
    echo "  Then run: source $rc"
    echo
}

main() {
    echo
    echo -e "${BOLD}LFFF Installer${NC}"
    echo -e "LibreFastbootFirmwareFlasher"
    echo

    local platform
    platform="$(detect_platform)"
    info "Platform: $platform"

    if [ -z "$VERSION" ]; then
        info "Fetching latest release..."
        VERSION="$(get_latest_version)"
        [ -z "$VERSION" ] && err "Could not determine latest version"
    fi
    ok "Version: $VERSION"

    local install_dir
    install_dir="$(find_install_dir)"

    if $INSTALL_CLI; then
        install_binary "lfff" "lfff-${platform}.tar.gz" "$VERSION" "$install_dir"
    fi

    if $INSTALL_GUI; then
        install_binary "lfff-gui" "lfff-gui-${platform}.tar.gz" "$VERSION" "$install_dir"

        # Install .desktop entry and icon on Linux
        if [ "$(uname -s)" = "Linux" ]; then
            local desktop_dir="$HOME/.local/share/applications"
            local icon_dir="$HOME/.local/share/icons/hicolor/scalable/apps"
            mkdir -p "$desktop_dir" "$icon_dir"

            local raw="https://raw.githubusercontent.com/${REPO}/${VERSION}"
            info "Installing desktop entry..."
            if command -v curl &>/dev/null; then
                curl -fsSL "$raw/lfff-gui.desktop" -o "$desktop_dir/lfff-gui.desktop" || \
                    warn "Could not download desktop entry (version tag may not exist yet, try main)"
                curl -fsSL "$raw/lfff-gui.svg" -o "$icon_dir/lfff-gui.svg" || \
                    warn "Could not download icon"
            else
                wget -qO "$desktop_dir/lfff-gui.desktop" "$raw/lfff-gui.desktop" || true
                wget -qO "$icon_dir/lfff-gui.svg" "$raw/lfff-gui.svg" || true
            fi

            # Replace Exec= with absolute path
            local gui_bin
            gui_bin="$(command -v lfff-gui 2>/dev/null || echo "$install_dir/lfff-gui")"
            sed -i "s|^Exec=.*|Exec=$gui_bin|" "$desktop_dir/lfff-gui.desktop" 2>/dev/null || true

            command -v gtk-update-icon-cache &>/dev/null && \
                gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
            command -v update-desktop-database &>/dev/null && \
                update-desktop-database "$desktop_dir" 2>/dev/null || true

            ok "Desktop entry installed"
        fi
    fi

    # PATH warning
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$install_dir"; then
        suggest_path "$install_dir"
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

if $UNINSTALL; then
    uninstall
else
    main
fi
