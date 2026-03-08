#!/usr/bin/env bash
# =============================================================================
# install.sh — LibreFastbootFirmwareFlasher installer
# =============================================================================
# Usage:
#   ./install.sh              — auto-detect environment, build and install
#   ./install.sh --prebuilt   — install pre-built dist/lfff/ (skip build)
#   ./install.sh --uninstall  — remove lfff from the system
# =============================================================================

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
BINARY_NAME="lfff"
DIST_DIR="dist/lfff"
REPO_URL="https://github.com/mrFrok/LibreFastbootFirmwareFlasher"

# Install paths — resolved after distro detection
INSTALL_DIR=""
LIB_DIR=""
USE_SUDO=1

# ── Colours ───────────────────────────────────────────────────────────────────
if [ -t 1 ]; then
    R="\033[0m"; BOLD="\033[1m"
    O="\033[38;5;208m"; G="\033[38;5;78m"
    RED="\033[38;5;203m"; GR="\033[38;5;244m"
    YEL="\033[38;5;220m"
else
    R=""; BOLD=""; O=""; G=""; RED=""; GR=""; YEL=""
fi

ok()   { echo -e "  ${G}✓${R}  $*"; }
err()  { echo -e "  ${RED}✗${R}  $*" >&2; }
info() { echo -e "  ${GR}·${R}  $*"; }
warn() { echo -e "  ${YEL}⚠${R}  $*"; }
hdr()  { echo -e "\n${BOLD}${O}$*${R}"; }
die()  { err "$*"; exit 1; }

# ── Banner ────────────────────────────────────────────────────────────────────
echo -e "
${O} ██╗     ${R}${O}███████╗${R}${O}███████╗${R}${O}███████╗${R}
${O} ██║     ${R}${O}██╔════╝${R}${O}██╔════╝${R}${O}██╔════╝${R}
${O} ██║     ${R}${O}█████╗  ${R}${O}█████╗  ${R}${O}█████╗  ${R}
${O} ██║     ${R}${O}██╔══╝  ${R}${O}██╔══╝  ${R}${O}██╔══╝  ${R}
${O} ███████╗${R}${O}██║     ${R}${O}██║     ${R}${O}██║     ${R}
${O} ╚══════╝${R}${O}╚═╝     ${R}${O}╚═╝     ${R}${O}╚═╝     ${R}

  ${BOLD}LibreFastbootFirmwareFlasher${R}  installer
  ${GR}${REPO_URL}${R}
"

# ── Parse args ────────────────────────────────────────────────────────────────
MODE="build"
for arg in "$@"; do
    case "$arg" in
        --prebuilt)  MODE="prebuilt" ;;
        --uninstall) MODE="uninstall" ;;
        --help|-h)
            echo "Usage: $0 [--prebuilt] [--uninstall]"
            echo "  (no flag)    build from source, then install"
            echo "  --prebuilt   install existing dist/lfff/ without building"
            echo "  --uninstall  remove lfff from the system"
            exit 0 ;;
        *) die "Unknown argument: $arg" ;;
    esac
done

# ── OS / distro detection ─────────────────────────────────────────────────────
hdr "Detecting environment ..."

OS="$(uname -s)"
case "$OS" in
    Darwin) PLATFORM="macos" ;;
    Linux)  PLATFORM="linux" ;;
    *)      PLATFORM="unknown" ;;
esac

ATOMIC=0
DISTRO=""

if [ "$PLATFORM" = "linux" ]; then
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        DISTRO="${ID:-}"
    fi

    # 1. Known atomic distro IDs
    case "${DISTRO}" in
        silverblue|kinoite|sericea|onyx|bazzite|aurora|bluefin|\
        nixos|guix|vanillaos|carbonos|steamos)
            ATOMIC=1 ;;
    esac

    # 2. ostree deployment layout (Fedora Silverblue, Bazzite etc.)
    if [ "$ATOMIC" -eq 0 ] && [ -d /ostree ]; then
        ATOMIC=1
    fi

    # 3. NixOS-specific marker
    if [ "$ATOMIC" -eq 0 ] && [ -f /etc/NIXOS ]; then
        ATOMIC=1
    fi

    # NOTE: We intentionally do NOT test if /usr is writable — that check
    # produces false positives on distros like CachyOS that mount /usr
    # read-only for performance/safety but are not immutable.
fi

# ── Decide install paths ──────────────────────────────────────────────────────
if [ "$PLATFORM" = "macos" ]; then
    INSTALL_DIR="/usr/local/bin"
    LIB_DIR="/usr/local/lib/lfff"
    USE_SUDO=1
    info "macOS detected → ${INSTALL_DIR}"

elif [ "$ATOMIC" -eq 1 ]; then
    INSTALL_DIR="${HOME}/.local/bin"
    LIB_DIR="${HOME}/.local/lib/lfff"
    USE_SUDO=0
    warn "Atomic/immutable distro detected (${DISTRO:-unknown}) — /usr is read-only"
    info "Installing to ${INSTALL_DIR}  (user-local, no sudo required)"

    if [[ ":$PATH:" != *":${HOME}/.local/bin:"* ]]; then
        warn "${HOME}/.local/bin is not in PATH yet"
        info "Add it after install:"
        info "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
        info "  (use ~/.zshrc or ~/.config/fish/config.fish for other shells)"
    fi

else
    INSTALL_DIR="/usr/local/bin"
    LIB_DIR="/usr/local/lib/lfff"
    USE_SUDO=1
    info "Standard Linux detected → ${INSTALL_DIR}"
fi

# ── Helper: run with or without sudo ─────────────────────────────────────────
maybe_sudo() {
    if [ "$USE_SUDO" -eq 1 ]; then
        sudo "$@"
    else
        "$@"
    fi
}

# ── Uninstall ─────────────────────────────────────────────────────────────────
if [ "$MODE" = "uninstall" ]; then
    hdr "Uninstalling lfff ..."
    REMOVED=0

    for try_bin in \
        "/usr/local/bin/${BINARY_NAME}" \
        "${HOME}/.local/bin/${BINARY_NAME}"; do
        if [ -f "$try_bin" ]; then
            if [[ "$try_bin" == /usr/* ]]; then sudo rm -f "$try_bin"
            else rm -f "$try_bin"; fi
            ok "Removed $try_bin"
            REMOVED=1
        fi
    done

    for try_lib in "/usr/local/lib/lfff" "${HOME}/.local/lib/lfff"; do
        if [ -d "$try_lib" ]; then
            if [[ "$try_lib" == /usr/* ]]; then sudo rm -rf "$try_lib"
            else rm -rf "$try_lib"; fi
            ok "Removed $try_lib"
            REMOVED=1
        fi
    done

    if [ "$REMOVED" -eq 0 ]; then
        info "lfff was not installed — nothing to remove."
    else
        ok "Uninstall complete."
    fi
    exit 0
fi

# ── Check we're in the project root ──────────────────────────────────────────
[ -f "main.py" ] || die "Run this script from the project root (where main.py lives)."

# ── Dependency check ─────────────────────────────────────────────────────────
hdr "Checking system dependencies ..."

install_hint() {
    local cmd="$1"
    case "$PLATFORM" in
        macos)
            case "$cmd" in
                python3) echo "brew install python  (or: https://python.org/downloads)" ;;
                pip3)    echo "comes with python — reinstall: brew install python" ;;
                make)    echo "xcode-select --install" ;;
                *)       echo "brew install $cmd" ;;
            esac ;;
        linux)
            if command -v apt &>/dev/null; then
                case "$cmd" in
                    python3) echo "sudo apt install python3" ;;
                    pip3)    echo "sudo apt install python3-pip" ;;
                    make)    echo "sudo apt install make" ;;
                    *)       echo "sudo apt install $cmd" ;;
                esac
            elif command -v pacman &>/dev/null; then
                case "$cmd" in
                    python3|pip3) echo "sudo pacman -S python pyton-pip" ;;
                    make)         echo "sudo pacman -S base-devel" ;;
                    *)            echo "sudo pacman -S $cmd" ;;
                esac
            elif command -v dnf &>/dev/null; then
                case "$cmd" in
                    python3) echo "sudo dnf install python3" ;;
                    pip3)    echo "sudo dnf install python3-pip" ;;
                    make)    echo "sudo dnf install make" ;;
                    *)       echo "sudo dnf install $cmd" ;;
                esac
            elif command -v rpm-ostree &>/dev/null; then
                echo "rpm-ostree install $cmd  (requires reboot)"
            else
                echo "install $cmd via your package manager"
            fi ;;
        *) echo "install $cmd manually" ;;
    esac
}

need_cmd() {
    if command -v "$1" &>/dev/null; then
        ok "$1  ($(command -v "$1"))"
    else
        err "$1 not found"
        info "Install with: $(install_hint "$1")"
        MISSING_DEPS=1
    fi
}

MISSING_DEPS=0
need_cmd python3
need_cmd pip3
need_cmd make

if [ "$PLATFORM" = "macos" ] && ! command -v brew &>/dev/null; then
    warn "Homebrew not found — recommended for easy dependency management."
    BREW_URL='https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh'
    info "Install: /bin/bash -c \"\$(curl -fsSL $BREW_URL)\""
fi

[ "$MISSING_DEPS" -ne 0 ] && die "Please install the missing dependencies above and re-run."

# Python version check (≥ 3.10 required for union types and match)
PY_VER=$(python3 -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
PY_MAJOR=$(echo "$PY_VER" | cut -d. -f1)
PY_MINOR=$(echo "$PY_VER" | cut -d. -f2)
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 10 ]; }; then
    die "Python 3.10+ required (found $PY_VER)"
fi
ok "Python $PY_VER"

# ── Build ─────────────────────────────────────────────────────────────────────
if [ "$MODE" = "build" ]; then
    hdr "Building lfff ..."

    info "Installing Python dependencies ..."
    make install 2>&1 | grep -v "^$" | sed 's/^/     /' || true

    info "Building binary with cx_Freeze ..."
    make build 2>&1 | grep -E "(✓|✗|error|Error|warning)" | sed 's/^/     /' || true

    if [ ! -f "${DIST_DIR}/${BINARY_NAME}" ]; then
        FOUND=$(find dist/ -name "$BINARY_NAME" -type f 2>/dev/null | head -1)
        [ -n "$FOUND" ] && DIST_DIR="$(dirname "$FOUND")" \
            || die "Build failed — binary not found in dist/"
    fi
    ok "Build complete → ${DIST_DIR}/${BINARY_NAME}"
fi

# ── Verify dist exists ────────────────────────────────────────────────────────
[ -f "${DIST_DIR}/${BINARY_NAME}" ] || \
    die "Binary not found at ${DIST_DIR}/${BINARY_NAME} — run 'make build' first or use --prebuilt."

# ── Install ───────────────────────────────────────────────────────────────────
hdr "Installing lfff ..."

info "Copying runtime bundle to ${LIB_DIR} ..."
maybe_sudo mkdir -p "${LIB_DIR}"
maybe_sudo cp -r "${DIST_DIR}/." "${LIB_DIR}/"
ok "Bundle installed  →  ${LIB_DIR}"

info "Creating launcher at ${INSTALL_DIR}/${BINARY_NAME} ..."
mkdir -p "${INSTALL_DIR}"
maybe_sudo tee "${INSTALL_DIR}/${BINARY_NAME}" > /dev/null << WRAPPER
#!/usr/bin/env bash
exec "${LIB_DIR}/${BINARY_NAME}" "\$@"
WRAPPER
maybe_sudo chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
ok "Launcher created  →  ${INSTALL_DIR}/${BINARY_NAME}"

# ── Smoke test ────────────────────────────────────────────────────────────────
hdr "Verifying installation ..."
if command -v "$BINARY_NAME" &>/dev/null; then
    ok "lfff is available at $(command -v "$BINARY_NAME")"
else
    warn "lfff installed but ${INSTALL_DIR} is not in PATH yet."
    info "Add to PATH:"
    info "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo -e "
  ${G}${BOLD}Installation complete!${R}

  Run ${O}lfff${R} to get started.
  Uninstall anytime: ${GR}./install.sh --uninstall${R}
"
