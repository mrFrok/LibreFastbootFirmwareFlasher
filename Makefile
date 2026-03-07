# =============================================================================
# Makefile — LibreFastbootFirmwareFlasher
# =============================================================================
# Targets:
#   make install    Create venv and install dev dependencies
#   make build      Build a standalone binary via cx_Freeze  →  dist/lfff/lfff
#   make test       Run the test suite with pytest
#   make lint       Run ruff linter
#   make clean      Remove build artefacts and __pycache__ dirs
#   make cleanall   Remove build artefacts + venv
#   make help       Print this message
# =============================================================================

PYTHON   := python3
VENV     := .venv
VENV_BIN := $(VENV)/bin
PIP      := $(VENV_BIN)/pip
PYTEST   := $(VENV_BIN)/pytest
CXFREEZE := $(VENV_BIN)/cxfreeze
RUFF     := $(VENV_BIN)/ruff

BINARY_NAME := lfff
DIST_DIR    := dist

.DEFAULT_GOAL := help

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------

.PHONY: help
help:
	@echo ""
	@echo "  LibreFastbootFirmwareFlasher — available targets"
	@echo ""
	@echo "  make install    Create venv and install dev dependencies"
	@echo "  make build      Build binary bundle → $(DIST_DIR)/$(BINARY_NAME)/"
	@echo "  make test       Run test suite"
	@echo "  make lint       Run ruff linter"
	@echo "  make clean      Remove build artefacts"
	@echo "  make cleanall   Remove build artefacts + venv"
	@echo ""

# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------

$(VENV)/bin/activate:
	$(PYTHON) -m venv $(VENV)
	$(PIP) install --upgrade pip

.PHONY: install
install: $(VENV)/bin/activate
	$(PIP) install "cx_Freeze>=7.0" pytest pytest-mock argcomplete
	@[ -f README.md ] || echo "# LibreFastbootFirmwareFlasher" > README.md
	@echo ""
	@echo "  ✓ venv ready: source $(VENV)/bin/activate"
	@echo ""

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
#
# cx_Freeze produces a directory bundle rather than a single file:
#
#   dist/lfff/
#     lfff          ← the executable
#     lib/         ← frozen stdlib + flasher package
#
# To distribute, zip the entire dist/lfff/ directory.
# The executable must stay next to its lib/ folder to run correctly.
#
# PATH is explicitly prepended with VENV_BIN so that cx_Freeze finds
# patchelf (installed as a Python package) without a system-wide install.

.PHONY: build
build: install
	# Register flasher package by dropping a .pth file into the venv
	echo "$(CURDIR)" > $(VENV)/lib/$(shell $(VENV_BIN)/python -c "import sys; print('python{}.{}'.format(*sys.version_info[:2]))")/site-packages/lfff-dev.pth
	PYTHONPATH="$(CURDIR)" \
	PATH="$(CURDIR)/$(VENV_BIN):$$PATH" \
	$(VENV_BIN)/python setup.py build
	@echo ""
	@echo "  ✓ Build complete: $(DIST_DIR)/$(BINARY_NAME)/"
	@echo "  Run with: $(DIST_DIR)/$(BINARY_NAME)/$(BINARY_NAME)"
	@echo ""

.PHONY: rebuild
rebuild: clean build



.PHONY: test
test: install
	$(PYTEST) -v

# ---------------------------------------------------------------------------
# Lint
# ---------------------------------------------------------------------------

.PHONY: lint
lint: install
	$(RUFF) check main.py flasher/

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

.PHONY: clean
clean:
	rm -rf $(DIST_DIR)
	find . -type d -name "__pycache__" -exec rm -rf {} +
	find . -type d -name "*.egg-info"  -exec rm -rf {} +
	find . -type f -name "*.pyc"       -delete
	@echo "  ✓ Clean"

.PHONY: cleanall
cleanall: clean
	rm -rf $(VENV)
	@echo "  ✓ venv removed"
