"""
utils.py — Shared utilities for fastboot-flasher.

Centralises subprocess execution, checksum helpers, and dependency checks
so every other module can import from one place.
"""

import hashlib
import logging
import shutil
import subprocess
from pathlib import Path

log = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Subprocess helpers
# ---------------------------------------------------------------------------

def run_cmd(cmd: list[str], timeout: int = 60) -> tuple[int, str, str]:
    """
    Run an external command and return (returncode, stdout, stderr).

    Never raises — callers decide how to handle failures.
    """
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return result.returncode, result.stdout.strip(), result.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", f"Command timed out after {timeout}s: {' '.join(cmd)}"
    except FileNotFoundError:
        return -1, "", f"Binary not found: {cmd[0]}"


def fastboot(*args: str, timeout: int = 60) -> tuple[int, str, str]:
    """Thin wrapper around run_cmd for fastboot invocations."""
    return run_cmd(["fastboot", *args], timeout=timeout)


def adb(*args: str, timeout: int = 30) -> tuple[int, str, str]:
    """Thin wrapper around run_cmd for adb invocations."""
    return run_cmd(["adb", *args], timeout=timeout)


# ---------------------------------------------------------------------------
# Integrity
# ---------------------------------------------------------------------------

def compute_checksum(file_path: Path, algorithm: str = "sha256") -> str:
    """Compute and return the hex digest of a file."""
    h = hashlib.new(algorithm)
    with open(file_path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            h.update(chunk)
    return h.hexdigest()


def verify_checksum(file_path: Path, expected: str, algorithm: str = "sha256") -> bool:
    """Return True if the file digest matches expected, False otherwise."""
    actual = compute_checksum(file_path, algorithm)
    if actual != expected:
        log.error(f"Checksum mismatch for {file_path.name}: expected {expected}, got {actual}")
        return False
    log.info(f"Checksum OK ({algorithm}): {actual}")
    return True


# ---------------------------------------------------------------------------
# Dependency checks
# ---------------------------------------------------------------------------

TOOL_INSTALL_HINTS: dict[str, str] = {
    "fastboot":          "android-tools  (apt / brew / pacman)",
    "adb":               "android-tools  (apt / brew / pacman)",
    "payload-dumper-go": "https://github.com/ssut/payload-dumper-go",
}


def check_tools(*tools: str) -> dict[str, bool]:
    """
    Check whether each tool is available in $PATH.

    Returns a dict mapping tool name -> found (bool).
    """
    return {tool: shutil.which(tool) is not None for tool in tools}


def require_tools(*tools: str) -> bool:
    """
    Print a dependency table and return False if any tool is missing.

    Intended for use at the start of a subcommand before doing real work.
    """
    results = check_tools(*tools)
    missing = False

    for tool, found in results.items():
        status = "✓" if found else "✗"
        hint   = "" if found else f"  →  {TOOL_INSTALL_HINTS.get(tool, 'not found')}"
        print(f"  {status}  {tool}{hint}")
        if not found:
            missing = True

    return not missing


# ---------------------------------------------------------------------------
# Misc
# ---------------------------------------------------------------------------

def prompt(message: str, default: str = "") -> str:
    """
    Display a prompt with an optional default value and return user input.

    Returns default if the user presses Enter without typing anything.
    """
    suffix = f" [{default}]" if default else ""
    raw = input(f"{message}{suffix}: ").strip()
    return raw if raw else default
