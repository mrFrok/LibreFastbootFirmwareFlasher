#!/usr/bin/env bash
# =============================================================================
# install.sh — LibreFastbootFirmwareFlasher installer
# =============================================================================
# Usage:
#   ./install.sh            — build from source and install to /usr/local/bin
#   ./install.sh --prebuilt — install pre-built dist/lfff/ (skip build step)
#   ./install.sh --uninstall
# =============================================================================

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
BINARY_NAME="lfff"
INSTALL_DIR="/usr/local/bin"
LIB_DIR="/usr/local/lib/lfff"
DIST_DIR="dist/lfff"
REPO_URL="https://github.com/mrFrok/LibreFastbootFirmwareFlasher"

# ── Colours ───────────────────────────────────────────────────────────────────
if [ -t 1 ]; then
    R="\033[0m"
    BOLD="\033[1m"
    O="\033[38;5;208m"
    G="\033[38;5;78m"
    RED="\033[38;5;203m"
    GR="\033[38;5;244m"
else
    R="" BOLD="" O="" G="" RED="" GR=""
fi

ok()   { echo -e "  ${G}✓${R}  $*"; }
err()  { echo -e "  ${RED}✗${R}  $*" >&2; }
info() { echo -e "  ${GR}·${R}  $*"; }
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

# ── Uninstall ─────────────────────────────────────────────────────────────────
if [ "$MODE" = "uninstall" ]; then
    hdr "Uninstalling lfff ..."
    REMOVED=0

    if [ -f "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        sudo rm -f "${INSTALL_DIR}/${BINARY_NAME}"
        ok "Removed ${INSTALL_DIR}/${BINARY_NAME}"
        REMOVED=1
    fi

    if [ -d "${LIB_DIR}" ]; then
        sudo rm -rf "${LIB_DIR}"
        ok "Removed ${LIB_DIR}"
        REMOVED=1
    fi

    if [ "$REMOVED" -eq 0 ]; then
        info "lfff was not installed — nothing to remove."
    else
        ok "Uninstall complete."
    fi
    exit 0
fi

# ── Check we're in the project root ──────────────────────────────────────────
[ -f "main.py" ] || die "Run this script from the project root (where main.py lives)."

# ── OS detection ─────────────────────────────────────────────────────────────
OS="$(uname -s)"
case "$OS" in
    Darwin) PLATFORM="macos" ;;
    Linux)  PLATFORM="linux" ;;
    *)      PLATFORM="unknown" ;;
esac
info "Platform: $OS"

# ── Dependency check ─────────────────────────────────────────────────────────
hdr "Checking system dependencies ..."

# Returns a human-readable install hint for a given command
install_hint() {
    local cmd="$1"
    case "$PLATFORM" in
        macos)
            case "$cmd" in
                python3) echo "brew install python  (or: https://python.org/downloads)" ;;
                pip3)    echo "pip3 comes with python — reinstall via brew install python" ;;
                make)    echo "xcode-select --install" ;;
                *)       echo "brew install $cmd" ;;
            esac ;;
        linux)
            # Detect package manager
            if command -v apt &>/dev/null; then
                case "$cmd" in
                    python3) echo "sudo apt install python3" ;;
                    pip3)    echo "sudo apt install python3-pip" ;;
                    make)    echo "sudo apt install make" ;;
                    *)       echo "sudo apt install $cmd" ;;
                esac
            elif command -v pacman &>/dev/null; then
                case "$cmd" in
                    python3|pip3) echo "sudo pacman -S python python-pip" ;;
                    *)            echo "sudo pacman -S $cmd" ;;
                esac
            elif command -v dnf &>/dev/null; then
                case "$cmd" in
                    python3) echo "sudo dnf install python3" ;;
                    pip3)    echo "sudo dnf install python3-pip" ;;
                    *)       echo "sudo dnf install $cmd" ;;
                esac
            else
                echo "install $cmd via your package manager"
            fi ;;
        *) echo "install $cmd manually" ;;
    esac
}

need_cmd() {
    if command -v "$1" &>/dev/null; then
        ok "$1 found ($(command -v "$1"))"
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

# On macOS, warn if Homebrew is missing (not fatal but useful)
if [ "$PLATFORM" = "macos" ] && ! command -v brew &>/dev/null; then
    info "Homebrew not found — recommended for easy dependency management."
    BREW_URL='https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh'
    info "Install: /bin/bash -c \"\$(curl -fsSL \$BREW_URL)\""
fi

if [ "$MISSING_DEPS" -ne 0 ]; then
    die "Please install the missing dependencies above and re-run the installer."
fi

# Python version check (need ≥ 3.10 for match/union types)
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
        # Sometimes cx_Freeze puts the binary at a slightly different path
        FOUND=$(find dist/ -name "$BINARY_NAME" -type f 2>/dev/null | head -1)
        [ -n "$FOUND" ] && DIST_DIR="$(dirname "$FOUND")" || die "Build failed — binary not found in dist/"
    fi
    ok "Build complete → ${DIST_DIR}/${BINARY_NAME}"
fi

# ── Verify dist exists ────────────────────────────────────────────────────────
[ -f "${DIST_DIR}/${BINARY_NAME}" ] || \
    die "Binary not found at ${DIST_DIR}/${BINARY_NAME} — run 'make build' first or use --prebuilt."

# ── Install ───────────────────────────────────────────────────────────────────
hdr "Installing lfff to ${INSTALL_DIR} ..."

# Copy the entire dist/lfff/ bundle to /usr/local/lib/lfff/
info "Copying runtime bundle to ${LIB_DIR} ..."
sudo mkdir -p "${LIB_DIR}"
sudo cp -r "${DIST_DIR}/." "${LIB_DIR}/"
ok "Bundle installed to ${LIB_DIR}"

# Create a thin wrapper in /usr/local/bin that exec's the real binary
info "Creating wrapper at ${INSTALL_DIR}/${BINARY_NAME} ..."
sudo tee "${INSTALL_DIR}/${BINARY_NAME}" > /dev/null << WRAPPER
#!/usr/bin/env bash
exec "${LIB_DIR}/${BINARY_NAME}" "\$@"
WRAPPER
sudo chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
ok "Wrapper created at ${INSTALL_DIR}/${BINARY_NAME}"

# ── Smoke test ────────────────────────────────────────────────────────────────
hdr "Verifying installation ..."
if command -v "$BINARY_NAME" &>/dev/null; then
    INSTALLED_AT=$(command -v "$BINARY_NAME")
    ok "lfff is available at ${INSTALLED_AT}"
else
    info "lfff installed but not yet in PATH."
    info "Add ${INSTALL_DIR} to your PATH:"
    info "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo -e "
  ${G}${BOLD}Installation complete!${R}

  Run ${O}lfff${R} to get started.
  Uninstall anytime: ${GR}./install.sh --uninstall${R}
"
