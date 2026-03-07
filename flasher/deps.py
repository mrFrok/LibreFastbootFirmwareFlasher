"""
deps.py - Automatic dependency installer for LibreFastbootFirmwareFlasher.

Handles:
  - android-tools (fastboot + adb)   via system package manager
  - aria2c                            via system package manager
  - payload-dumper-go                 via GitHub releases (not in distro repos)

Supported package managers: pacman, apt, dnf, zypper, emerge, brew
"""

import os
import re
import sys
import json
import shutil
import logging
import platform
import subprocess
import tempfile
import urllib.request
from pathlib import Path
from dataclasses import dataclass, field

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

# Tools available in standard repos, keyed by package manager
_PKG_MANAGER_PACKAGES: dict[str, dict[str, str]] = {
    "pacman": {
        "fastboot": "android-tools",
        "adb":      "android-tools",
        "aria2c":   "aria2",
    },
    "apt": {
        "fastboot": "android-tools-fastboot",
        "adb":      "android-tools-adb",
        "aria2c":   "aria2",
    },
    "dnf": {
        "fastboot": "android-tools",
        "adb":      "android-tools",
        "aria2c":   "aria2",
    },
    "zypper": {
        "fastboot": "android-tools",
        "adb":      "android-tools",
        "aria2c":   "aria2",
    },
    "emerge": {
        "fastboot": "dev-util/android-tools",
        "adb":      "dev-util/android-tools",
        "aria2c":   "net-misc/aria2",
    },
    "brew": {
        "fastboot": "android-platform-tools",
        "adb":      "android-platform-tools",
        "aria2c":   "aria2",
    },
}

# Install commands per package manager
_INSTALL_CMDS: dict[str, list[str]] = {
    "pacman": ["pacman", "-S", "--noconfirm"],
    "apt":    ["apt-get", "install", "-y"],
    "dnf":    ["dnf",     "install", "-y"],
    "zypper": ["zypper",  "install", "-y"],
    "emerge": ["emerge"],
    "brew":   ["brew",    "install"],
}

# payload-dumper-go GitHub release info
_PDG_REPO    = "ssut/payload-dumper-go"
_PDG_BINARY  = "payload-dumper-go"
_PDG_INSTALL = Path("/usr/local/bin/payload-dumper-go")


# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

@dataclass
class DepResult:
    tool: str
    already_installed: bool = False
    installed: bool = False
    skipped: bool = False
    error: str = ""

    @property
    def ok(self) -> bool:
        return self.already_installed or self.installed


@dataclass
class DepsReport:
    results: list[DepResult] = field(default_factory=list)

    @property
    def all_ok(self) -> bool:
        return all(r.ok for r in self.results)

    @property
    def failed(self) -> list[DepResult]:
        return [r for r in self.results if not r.ok and not r.skipped]


# ---------------------------------------------------------------------------
# Package manager detection
# ---------------------------------------------------------------------------

def _detect_pkg_manager() -> str | None:
    """Return the name of the available package manager, or None."""
    for pm in ("pacman", "apt", "apt-get", "dnf", "zypper", "emerge", "brew"):
        if shutil.which(pm):
            # Normalise apt-get -> apt
            return "apt" if pm == "apt-get" else pm
    return None


def _needs_sudo(pm: str) -> bool:
    return pm != "brew" and os.geteuid() != 0


# ---------------------------------------------------------------------------
# payload-dumper-go installer (GitHub releases)
# ---------------------------------------------------------------------------

def _pdg_pick_asset(assets: list[dict]) -> dict | None:
    """
    Pick the correct release asset for the current platform.

    Asset naming convention: payload-dumper-go_<version>_<os>_<arch>.tar.gz
    Examples:
      payload-dumper-go_1.3.0_linux_amd64.tar.gz
      payload-dumper-go_1.3.0_darwin_amd64.tar.gz
      payload-dumper-go_1.3.0_windows_amd64.zip
    """
    machine = platform.machine().lower()
    system  = platform.system().lower()  # linux / darwin / windows

    arch_map = {
        "x86_64":  "amd64",
        "aarch64": "arm64",
        "arm64":   "arm64",
        "armv7l":  "arm",
    }
    arch = arch_map.get(machine, machine)

    # Score assets: must match both OS and arch
    for asset in assets:
        name = asset["name"].lower()
        if system in name and arch in name:
            return asset

    return None


def _fetch_latest_pdg_release() -> dict | None:
    """Fetch latest release metadata from GitHub API."""
    import ssl
    url = f"https://api.github.com/repos/{_PDG_REPO}/releases/latest"
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "lfff/0.1"})
        with urllib.request.urlopen(req, timeout=15, context=ctx) as resp:
            return json.loads(resp.read())
    except Exception as exc:
        log.error(f"GitHub API request failed: {exc}")
        return None


def _pdg_is_runnable() -> bool:
    """Return True if the installed payload-dumper-go actually executes on this platform."""
    binary = shutil.which(_PDG_BINARY)
    if not binary:
        return False
    try:
        proc = subprocess.run(
            [binary, "--help"],
            capture_output=True,
            timeout=5,
        )
        # Any exit code is fine as long as the OS could exec it.
        # "cannot execute binary file" -> OSError / non-zero with specific stderr.
        return b"cannot execute" not in proc.stderr and proc.returncode != 126
    except (OSError, subprocess.TimeoutExpired):
        return False


def _install_payload_dumper_go() -> DepResult:
    """Download and install payload-dumper-go from GitHub releases."""
    result = DepResult(tool="payload-dumper-go")

    if _pdg_is_runnable():
        result.already_installed = True
        return result

    if shutil.which(_PDG_BINARY):
        print("  ⚠  payload-dumper-go found but not runnable on this platform — reinstalling ...")

    print("  Fetching latest payload-dumper-go release from GitHub ...")
    release = _fetch_latest_pdg_release()
    if not release:
        result.error = "Could not fetch release info from GitHub"
        return result

    tag     = release.get("tag_name", "unknown")
    assets  = release.get("assets", [])

    asset = _pdg_pick_asset(assets)

    if not asset:
        available = [a["name"] for a in assets]
        result.error = (
            f"No matching asset for this platform "
            f"({platform.system().lower()}/{platform.machine().lower()}). "
            f"Available: {available}. "
            f"Install manually from https://github.com/{_PDG_REPO}/releases"
        )
        return result

    dl_url = asset["browser_download_url"]
    print(f"  Downloading {asset['name']} ({tag}) ...")

    try:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            archive  = tmp_path / asset["name"]

            # Download
            import ssl
            ctx = ssl.create_default_context()
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx))
            with opener.open(dl_url) as resp, open(archive, "wb") as out:
                shutil.copyfileobj(resp, out)

            # Extract
            if asset["name"].endswith(".tar.gz"):
                import tarfile
                with tarfile.open(archive) as tf:
                    tf.extractall(tmp_path)
            elif asset["name"].endswith(".zip"):
                import zipfile
                with zipfile.ZipFile(archive) as zf:
                    zf.extractall(tmp_path)

            # Find the binary
            binary = next(tmp_path.rglob("payload-dumper-go*"), None)
            if binary is None or not binary.is_file():
                result.error = "payload-dumper-go binary not found in archive"
                return result

            binary.chmod(0o755)

            # Install
            dest = _PDG_INSTALL
            try:
                shutil.copy2(binary, dest)
                dest.chmod(0o755)
                print(f"  Installed to {dest}")
            except PermissionError:
                # Try with sudo
                proc = subprocess.run(
                    ["sudo", "cp", str(binary), str(dest)],
                    capture_output=True
                )
                if proc.returncode != 0:
                    # Fall back to ~/.local/bin
                    local_bin = Path.home() / ".local" / "bin"
                    local_bin.mkdir(parents=True, exist_ok=True)
                    dest = local_bin / "payload-dumper-go"
                    shutil.copy2(binary, dest)
                    dest.chmod(0o755)
                    print(f"  Installed to {dest}  (add ~/.local/bin to PATH if needed)")
                else:
                    subprocess.run(["sudo", "chmod", "755", str(dest)])
                    print(f"  Installed to {dest}")

    except Exception as exc:
        result.error = f"Installation failed: {exc}"
        return result

    result.installed = True
    return result


# ---------------------------------------------------------------------------
# System package installer
# ---------------------------------------------------------------------------

def _install_via_pkg_manager(
    tools: list[str],
    pm: str,
) -> list[DepResult]:
    """Install a list of tools using the system package manager."""
    results = []

    # Check which are already installed
    to_install: list[tuple[str, str]] = []  # (tool, package)
    for tool in tools:
        if shutil.which(tool):
            results.append(DepResult(tool=tool, already_installed=True))
            continue
        pkg = _PKG_MANAGER_PACKAGES.get(pm, {}).get(tool)
        if pkg is None:
            results.append(DepResult(
                tool=tool, skipped=True,
                error=f"No package mapping for {tool} on {pm}"
            ))
            continue
        to_install.append((tool, pkg))

    if not to_install:
        return results

    # Deduplicate packages (adb + fastboot -> same package on some distros)
    packages = list(dict.fromkeys(pkg for _, pkg in to_install))
    tool_names = [t for t, _ in to_install]

    cmd = list(_INSTALL_CMDS[pm]) + packages
    if _needs_sudo(pm):
        cmd = ["sudo"] + cmd

    print(f"  Running: {' '.join(cmd)}")

    proc = subprocess.run(cmd)
    if proc.returncode != 0:
        for tool in tool_names:
            results.append(DepResult(
                tool=tool,
                error=f"Package manager exited with code {proc.returncode}"
            ))
    else:
        for tool in tool_names:
            ok = shutil.which(tool) is not None
            results.append(DepResult(tool=tool, installed=ok,
                                     error="" if ok else "Installed but binary not found in PATH"))

    return results


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

MANAGED_TOOLS = ["fastboot", "adb", "aria2c", "payload-dumper-go"]


def install_dependencies(
    tools: list[str] | None = None,
    dry_run: bool = False,
) -> DepsReport:
    """
    Check and install missing dependencies.

    Args:
        tools:   List of tool names to check/install. Defaults to all.
        dry_run: If True, only check and report, do not install.

    Returns:
        DepsReport with per-tool results.
    """
    if tools is None:
        tools = list(MANAGED_TOOLS)

    report = DepsReport()

    pm = _detect_pkg_manager()

    print("\n── Dependency check ─────────────────────────────────────")

    if pm is None and not dry_run:
        print("  ✗ No supported package manager found.")
        print("    Install manually: fastboot, adb, aria2c, payload-dumper-go")
        for t in tools:
            report.results.append(DepResult(tool=t, skipped=True,
                                             error="No package manager"))
        print("────────────────────────────────────────────────────────")
        return report

    if pm:
        print(f"  Package manager : {pm}")

    # Split: payload-dumper-go is always installed via GitHub
    pdg = "payload-dumper-go"
    pkg_tools = [t for t in tools if t != pdg]

    # Check / install system packages
    if pkg_tools:
        if dry_run:
            for t in pkg_tools:
                if shutil.which(t):
                    report.results.append(DepResult(tool=t, already_installed=True))
                else:
                    pkg = _PKG_MANAGER_PACKAGES.get(pm or "", {}).get(t, "unknown")
                    report.results.append(DepResult(
                        tool=t, skipped=True,
                        error=f"Would install: {pkg} via {pm}"
                    ))
        elif pm:
            report.results.extend(_install_via_pkg_manager(pkg_tools, pm))
        else:
            for t in pkg_tools:
                report.results.append(DepResult(tool=t, skipped=True,
                                                 error="No package manager"))

    # payload-dumper-go
    if pdg in tools:
        if dry_run:
            if shutil.which(pdg):
                report.results.append(DepResult(tool=pdg, already_installed=True))
            else:
                report.results.append(DepResult(
                    tool=pdg, skipped=True,
                    error=f"Would download from https://github.com/{_PDG_REPO}/releases"
                ))
        else:
            report.results.append(_install_payload_dumper_go())

    # Print summary
    print()
    for r in report.results:
        if r.already_installed:
            print(f"  ✓  {r.tool:<25} already installed")
        elif r.installed:
            print(f"  ✓  {r.tool:<25} installed")
        elif r.skipped:
            print(f"  -  {r.tool:<25} skipped  ({r.error})")
        else:
            print(f"  ✗  {r.tool:<25} FAILED   ({r.error})")

    print("────────────────────────────────────────────────────────")
    return report
