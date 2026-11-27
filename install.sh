#!/usr/bin/env bash
# LFFF installer — downloads prebuilt binaries from GitHub Releases
# Works on any Linux (including immutable distros), macOS, FreeBSD and OpenBSD
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
        linux*)   os="linux" ;;
        darwin*)  os="macos" ;;
        freebsd*) os="freebsd" ;;
        openbsd*) os="openbsd" ;;
        *)        err "Unsupported OS: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)             err "Unsupported architecture: $arch" ;;
    esac

    echo "${os}-${arch}"
}

# Find best install directory
# Termux runs on Android's Bionic libc rather than glibc, yet uname reports a
# plain "Linux" there, so it needs its own check. The CLI is a static binary
# and runs fine; the GUI has no display server to talk to.
is_termux() {
    [ -n "${TERMUX_VERSION:-}" ] || [ "$(uname -o 2>/dev/null)" = "Android" ]
}

# musl distributions (Void, Alpine) cannot run the glibc build of the GUI and
# get a separate musl one. The CLI is static, so it is the same everywhere.
is_musl() {
    [ -e "/lib/ld-musl-$(uname -m).so.1" ] || ldd --version 2>&1 | grep -qi musl
}

# Download $1 to file $2, with whichever of curl or wget is present.
fetch() {
    if command -v curl &>/dev/null; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

# sha256 of a file: sha256sum on Linux and Termux, shasum on macOS, and the
# base-system sha256 (libmd on FreeBSD, MD5(1) on OpenBSD) on the BSDs.
sha256_of() {
    if command -v sha256sum &>/dev/null; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum &>/dev/null; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v sha256 &>/dev/null; then
        sha256 -q "$1"
    fi
}

# Check a downloaded asset against the .sha256 file the release publishes
# beside it. Only the digest is compared, not the file name inside.
verify_asset() {
    local file="$1" sidecar="$2"
    local expected actual
    expected="$(cut -d' ' -f1 < "$sidecar" | tr 'A-F' 'a-f')"
    case "$expected" in
        *[!0-9a-f]* | "") err "The checksum file for $(basename "$file") is malformed" ;;
    esac
    [ "${#expected}" -eq 64 ] || err "The checksum file for $(basename "$file") is malformed"
    actual="$(sha256_of "$file")"
    [ -n "$actual" ] || err "Need sha256sum or shasum to verify the download"
    [ "$actual" = "$expected" ] || err "Checksum mismatch for $(basename "$file") — not installing it"
    ok "Checksum verified"
}

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

    # Remove desktop entry and icon on Linux and FreeBSD
    if [ "$(uname -s)" = "Linux" ] || [ "$(uname -s)" = "FreeBSD" ]; then
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
    # FreeBSD's mktemp has no GNU-style default template.
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/lfff.XXXXXX")"
    trap 'rm -rf "$tmp"; trap - RETURN' RETURN

    info "Downloading $asset_name..."
    fetch "$url" "$tmp/$asset_name" || err "Could not download $url"
    fetch "$url.sha256" "$tmp/$asset_name.sha256" || err "Could not download $url.sha256"
    ok "Downloaded"
    verify_asset "$tmp/$asset_name" "$tmp/$asset_name.sha256"

    tar xzf "$tmp/$asset_name" -C "$tmp"

    local binary_path="$tmp/$binary"
    local is_app_bundle=false
    if [ ! -f "$binary_path" ]; then
        # macOS .app bundle
        binary_path="$tmp/LibreFastbootFirmwareFlasher.app/Contents/MacOS/$binary"
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
        local app_src="$tmp/LibreFastbootFirmwareFlasher.app"
        local app_dst="/Applications/LibreFastbootFirmwareFlasher.app"
        if [ -d "$app_src" ] && [ ! -e "$app_dst" ]; then
            echo
            info "LibreFastbootFirmwareFlasher.app bundle found in archive."
            if command -v sudo &>/dev/null; then
                if sudo cp -r "$app_src" "$app_dst" 2>/dev/null; then
                    ok "Copied LibreFastbootFirmwareFlasher.app to /Applications"
                else
                    warn "Could not copy to /Applications. To install manually:"
                    echo "  sudo cp -r '$tmp/LibreFastbootFirmwareFlasher.app' /Applications"
                fi
            else
                warn "To use LibreFastbootFirmwareFlasher from Launchpad, copy the app bundle:"
                echo "  sudo cp -r '$tmp/LibreFastbootFirmwareFlasher.app' /Applications"
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

    local gui_asset="lfff-gui-${platform}.tar.gz"
    local termux=false
    if is_termux; then
        termux=true
        info "Termux detected"
        if $INSTALL_GUI; then
            $INSTALL_CLI || err "The GUI does not run on Android: Termux has no display server. Install the CLI instead (--cli-only)."
            warn "Skipping the GUI: it does not run on Android. Installing the CLI only."
            INSTALL_GUI=false
        fi
    elif [ "${platform%%-*}" = "linux" ] && is_musl; then
        info "musl libc detected — using the musl build of the GUI"
        gui_asset="lfff-gui-${platform}-musl.tar.gz"
    fi

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
        install_binary "lfff-gui" "$gui_asset" "$VERSION" "$install_dir"

        # Install .desktop entry and icon on Linux and FreeBSD (same XDG paths).
        if [ "$(uname -s)" = "Linux" ] || [ "$(uname -s)" = "FreeBSD" ]; then
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

            # Replace Exec= with absolute path. sed -i takes a backup suffix
            # on the BSDs, so write through a temporary file instead.
            local gui_bin
            gui_bin="$(command -v lfff-gui 2>/dev/null || echo "$install_dir/lfff-gui")"
            sed "s|^Exec=.*|Exec=$gui_bin|" "$desktop_dir/lfff-gui.desktop" \
                > "$desktop_dir/lfff-gui.desktop.new" 2>/dev/null &&
                mv "$desktop_dir/lfff-gui.desktop.new" "$desktop_dir/lfff-gui.desktop" || true

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
        if $termux; then
            # lfff deps does not know Termux's packages; point at them directly.
            ok "lfff is ready! Install fastboot and adb with: ${BOLD}pkg install android-tools${NC}"
        else
            ok "lfff is ready! Run ${BOLD}lfff deps${NC} to install external tools."
        fi
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
