#!/usr/bin/env bash
# =============================================================================
# install.sh — LibreFastbootFirmwareFlasher installer
# =============================================================================
# Usage:
#   ./install.sh                      — auto-detect environment, build and install
#   ./install.sh --prebuilt  | -p     — install pre-built dist/lfff/ (skip build)
#   ./install.sh --update    | -u     — pull latest changes and reinstall
#   ./install.sh --uninstall | -r     — remove lfff from the system
#   ./install.sh --help      | -h     — show this help
# =============================================================================

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
BINARY_NAME="lfff"
DIST_DIR="dist/lfff"
REPO_URL="https://github.com/mrFrok/LibreFastbootFirmwareFlasher"
INSTALL_DIR=""
LIB_DIR=""
USE_SUDO=1

# ── Colours ───────────────────────────────────────────────────────────────────
if [ -t 1 ]; then
    R="\033[0m"; BOLD="\033[1m"
    O="\033[38;5;208m";   G="\033[38;5;78m"
    RED="\033[38;5;203m"; GR="\033[38;5;244m"
    YEL="\033[38;5;220m"; CYN="\033[38;5;117m"
else
    R=""; BOLD=""; O=""; G=""; RED=""; GR=""; YEL=""; CYN=""
fi

ok()   { echo -e "  ${G}✓${R}  $*"; }
fail() { echo -e "  ${RED}✗${R}  $*" >&2; }
info() { echo -e "  ${GR}·${R}  $*"; }
warn() { echo -e "  ${YEL}⚠${R}  $*"; }
hdr()  { echo -e "\n${BOLD}${O}── $* ${R}"; }

# Pretty error block — shows what went wrong and what to do
die() {
    local msg="$1"
    local hint="${2:-}"
    echo -e "\n${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}" >&2
    echo -e "  ${RED}${BOLD}✗  Installation failed${R}" >&2
    echo -e "  ${RED}${msg}${R}" >&2
    if [ -n "$hint" ]; then
        echo -e "" >&2
        echo -e "  ${CYN}What to do:${R}" >&2
        echo -e "  ${hint}" >&2
    fi
    echo -e "${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}" >&2
    echo -e "" >&2
    exit 1
}

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
        --prebuilt|-p)  MODE="prebuilt" ;;
        --uninstall|-r) MODE="uninstall" ;;
        --update|-u)    MODE="update" ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo ""
            echo "  (no flag)              build from source and install"
            echo "  --prebuilt,  -p        install existing dist/lfff/ without building"
            echo "  --update,    -u        pull latest changes from git and reinstall"
            echo "  --uninstall, -r        remove lfff from the system"
            echo "  --help,      -h        show this help"
            exit 0 ;;
        *) die "Unknown argument: $arg" "Run: $0 --help" ;;
    esac
done

# ── OS / distro detection ─────────────────────────────────────────────────────
hdr "Detecting environment"

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

    case "${DISTRO}" in
        silverblue|kinoite|sericea|onyx|bazzite|aurora|bluefin|\
        nixos|guix|vanillaos|carbonos|steamos)
            ATOMIC=1 ;;
    esac

    if [ "$ATOMIC" -eq 0 ] && [ -d /ostree ]; then ATOMIC=1; fi
    if [ "$ATOMIC" -eq 0 ] && [ -f /etc/NIXOS ]; then ATOMIC=1; fi
fi

# ── Decide install paths ──────────────────────────────────────────────────────
if [ "$PLATFORM" = "macos" ]; then
    INSTALL_DIR="/usr/local/bin"
    LIB_DIR="/usr/local/lib/lfff"
    USE_SUDO=1
    ok "macOS → ${INSTALL_DIR}"

elif [ "$ATOMIC" -eq 1 ]; then
    INSTALL_DIR="${HOME}/.local/bin"
    LIB_DIR="${HOME}/.local/lib/lfff"
    USE_SUDO=0
    warn "Atomic/immutable distro (${DISTRO:-unknown}) — installing to user home"
    info "Install dir: ${INSTALL_DIR}"

    if [[ ":$PATH:" != *":${HOME}/.local/bin:"* ]]; then
        warn "${HOME}/.local/bin is not in PATH yet"
        info "After install, add it:"
        info "  ${BOLD}echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc${R}"
        info "  (or ~/.zshrc / ~/.config/fish/config.fish)"
    fi

else
    INSTALL_DIR="/usr/local/bin"
    LIB_DIR="/usr/local/lib/lfff"
    USE_SUDO=1
    ok "Linux → ${INSTALL_DIR}"
fi

maybe_sudo() {
    if [ "$USE_SUDO" -eq 1 ]; then sudo "$@"; else "$@"; fi
}

# ── Uninstall ─────────────────────────────────────────────────────────────────
if [ "$MODE" = "uninstall" ]; then
    hdr "Uninstalling lfff"
    REMOVED=0
    for try_bin in "/usr/local/bin/${BINARY_NAME}" "${HOME}/.local/bin/${BINARY_NAME}"; do
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
        echo -e "\n${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}"
        echo -e "  ${G}${BOLD}✓  Uninstall complete${R}"
        echo -e "${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}\n"
    fi
    exit 0
fi

# ── Update ───────────────────────────────────────────────────────────────────
if [ "$MODE" = "update" ]; then
    hdr "Updating lfff"

    # Must be inside a git repo
    if ! git rev-parse --git-dir &>/dev/null; then
        die             "Not a git repository."             "Clone the repo first:\n  ${BOLD}git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher${R}"
    fi

    CURRENT=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    info "Current version: ${CURRENT}"

    info "Fetching latest changes ..."
    if ! git fetch origin 2>&1 | sed 's/^/     /'; then
        die             "git fetch failed."             "Check your internet connection and try again."
    fi

    BEHIND=$(git rev-list HEAD..origin/$(git branch --show-current) --count 2>/dev/null || echo "0")

    if [ "$BEHIND" -eq 0 ]; then
        echo -e "
${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}"
        echo -e "  ${G}${BOLD}✓  Already up to date${R}  ${GR}(${CURRENT})${R}"
        echo -e "${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}
"
        exit 0
    fi

    info "${BEHIND} new commit(s) available — pulling ..."
    if ! git pull --ff-only 2>&1 | sed 's/^/     /'; then
        die             "git pull failed."             "You may have local changes. Stash or reset them:\n  ${BOLD}git stash && ./install.sh --update${R}"
    fi

    NEW=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
    ok "Updated: ${CURRENT} → ${NEW}"

    info "Rebuilding and reinstalling ..."
    MODE="build"
fi

# ── Check project root ────────────────────────────────────────────────────────
[ -f "main.py" ] || die \
    "main.py not found in current directory." \
    "Run this script from the project root:\n  ${BOLD}cd LibreFastbootFirmwareFlasher && ./install.sh${R}"

# ── Dependency check ─────────────────────────────────────────────────────────
hdr "Checking system dependencies"

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
                    python3|pip3) echo "sudo pacman -S python python-pip" ;;
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

MISSING_DEPS=0
missing_list=""
need_cmd() {
    if command -v "$1" &>/dev/null; then
        ok "$1  ${GR}($(command -v "$1"))${R}"
    else
        fail "$1 not found"
        info "  → $(install_hint "$1")"
        MISSING_DEPS=1
        missing_list="${missing_list} $1"
    fi
}

need_cmd python3
need_cmd pip3
need_cmd make

if [ "$PLATFORM" = "macos" ] && ! command -v brew &>/dev/null; then
    warn "Homebrew not found — recommended for easy dependency management"
    BREW_URL='https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh'
    info "Install: /bin/bash -c \"\$(curl -fsSL $BREW_URL)\""
fi

if [ "$MISSING_DEPS" -ne 0 ]; then
    die \
        "Missing required tools:${missing_list}" \
        "Install them using the commands shown above, then re-run:\n  ${BOLD}./install.sh${R}"
fi

# Python version check
PY_VER=$(python3 -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
PY_MAJOR=$(echo "$PY_VER" | cut -d. -f1)
PY_MINOR=$(echo "$PY_VER" | cut -d. -f2)
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 10 ]; }; then
    die \
        "Python 3.10+ required, found $PY_VER" \
        "Install a newer Python:\n  ${BOLD}$(install_hint python3)${R}"
fi
ok "Python $PY_VER"

# ── Build ─────────────────────────────────────────────────────────────────────
if [ "$MODE" = "build" ]; then
    hdr "Building lfff"

    info "Installing Python dependencies ..."
    if ! make install 2>&1 | grep -v "^$" | sed 's/^/     /'; then
        die \
            "Failed to install Python dependencies." \
            "Try running manually:\n  ${BOLD}make install${R}\nand check the error output above."
    fi

    info "Building binary with cx_Freeze ..."
    make build 2>&1 | grep -E "(✓|✗|error|Error|warning)" | sed 's/^/     /' || true

    if [ ! -f "${DIST_DIR}/${BINARY_NAME}" ]; then
        FOUND=$(find dist/ -name "$BINARY_NAME" -type f 2>/dev/null | head -1)
        if [ -n "$FOUND" ]; then
            DIST_DIR="$(dirname "$FOUND")"
        else
            die \
                "Build failed — binary not found in dist/" \
                "Run the build manually to see full output:\n  ${BOLD}make build${R}"
        fi
    fi
    ok "Build complete → ${DIST_DIR}/${BINARY_NAME}"
fi

# ── Verify dist ───────────────────────────────────────────────────────────────
[ -f "${DIST_DIR}/${BINARY_NAME}" ] || die \
    "Binary not found at ${DIST_DIR}/${BINARY_NAME}" \
    "Build it first:\n  ${BOLD}make build${R}\nOr use a pre-built bundle:\n  ${BOLD}./install.sh --prebuilt${R}"

# ── Install ───────────────────────────────────────────────────────────────────
hdr "Installing lfff"

info "Copying runtime bundle to ${LIB_DIR} ..."
if ! maybe_sudo mkdir -p "${LIB_DIR}" || ! maybe_sudo cp -r "${DIST_DIR}/." "${LIB_DIR}/"; then
    die \
        "Failed to copy files to ${LIB_DIR}" \
        "Check permissions or run as root:\n  ${BOLD}sudo ./install.sh${R}"
fi
ok "Bundle installed → ${LIB_DIR}"

info "Creating launcher at ${INSTALL_DIR}/${BINARY_NAME} ..."
mkdir -p "${INSTALL_DIR}"
if ! maybe_sudo tee "${INSTALL_DIR}/${BINARY_NAME}" > /dev/null << WRAPPER
#!/usr/bin/env bash
exec "${LIB_DIR}/${BINARY_NAME}" "\$@"
WRAPPER
then
    die \
        "Failed to create launcher at ${INSTALL_DIR}/${BINARY_NAME}" \
        "Check write permissions for ${INSTALL_DIR}"
fi
maybe_sudo chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
ok "Launcher created → ${INSTALL_DIR}/${BINARY_NAME}"

# ── Smoke test ────────────────────────────────────────────────────────────────
hdr "Verifying installation"
if command -v "$BINARY_NAME" &>/dev/null; then
    ok "lfff found at $(command -v "$BINARY_NAME")"
    PATH_OK=1
else
    warn "lfff installed but ${INSTALL_DIR} is not in PATH yet"
    info "Add it by running:"
    info "  ${BOLD}echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc && source ~/.bashrc${R}"
    PATH_OK=0
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo -e "\n${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}"
echo -e "  ${G}${BOLD}✓  Installation complete!${R}"
echo -e "${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}"
echo -e "  ${GR}Binary  :${R}  ${LIB_DIR}/${BINARY_NAME}"
echo -e "  ${GR}Launcher:${R}  ${INSTALL_DIR}/${BINARY_NAME}"
echo -e ""
if [ "$PATH_OK" -eq 1 ]; then
    echo -e "  Run ${O}${BOLD}lfff${R} to get started."
else
    echo -e "  After adding to PATH, run ${O}${BOLD}lfff${R} to get started."
fi
echo -e "  Uninstall: ${GR}./install.sh --uninstall${R}"
echo -e "${G}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}\n"
