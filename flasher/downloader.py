"""
downloader.py - OTA firmware downloader for OnePlus / OPPO devices.

Resolves OTA download links (including 4PDA redirects) to direct CDN URLs
and downloads them via aria2c for maximum speed.

Requires: aria2c  (sudo pacman -S aria2 / sudo apt install aria2)
"""

import urllib.parse
import logging
from pathlib import Path
from dataclasses import dataclass

from flasher.utils import run_cmd, require_tools

log = logging.getLogger(__name__)

_OTA_HEADERS = [
    "userId: oplus-ota|16002018",
    "User-Agent: okhttp/4.9.2",
    "Accept: application/json",
]


# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

@dataclass
class DownloadResult:
    success: bool
    url: str = ""
    cdn_url: str = ""
    output_path: Path | None = None
    error: str = ""


# ---------------------------------------------------------------------------
# Link resolution
# ---------------------------------------------------------------------------

def _extract_real_url(url: str) -> str:
    """Unwrap 4PDA redirect to get the real OTA endpoint."""
    parsed = urllib.parse.urlparse(url)
    if "4pda.to" in parsed.netloc:
        qs = urllib.parse.parse_qs(parsed.query)
        if "u" in qs:
            real = urllib.parse.unquote(qs["u"][0])
            log.info(f"Unwrapped 4PDA redirect -> {real}")
            return real
    return url


def _resolve_cdn(url: str) -> str | None:
    """
    Follow the OTA downloadCheck endpoint to get the real CDN URL.

    The server returns HTTP 302 with the CDN link in Location header.
    Uses curl to avoid pulling in requests as a dependency.
    """
    # Build curl command with custom headers, follow no redirects, dump headers only
    cmd = ["curl", "-s", "-o", "/dev/null", "-D", "-", "--max-redirs", "0"]
    for h in _OTA_HEADERS:
        cmd += ["-H", h]
    cmd.append(url)

    rc, stdout, stderr = run_cmd(cmd, timeout=30)

    if rc not in (0, 6, 7):  # curl exits non-zero on redirect when --max-redirs 0
        # Check if we got a Location header anyway (some curl versions)
        pass

    for line in stdout.splitlines():
        if line.lower().startswith("location:"):
            cdn = line.split(":", 1)[1].strip()
            log.info(f"CDN URL resolved: {cdn}")
            return cdn

    log.error(f"Could not find Location header in response:\n{stdout[:500]}")
    return None


# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------

def download_firmware(
    url: str,
    output_dir: Path | None = None,
    connections: int = 16,
) -> DownloadResult:
    """
    Resolve an OTA link and download the firmware via aria2c.

    Args:
        url:         OTA link (direct or 4PDA redirect).
        output_dir:  Directory to save the file (default: cwd).
        connections: Number of parallel connections for aria2c.

    Returns:
        DownloadResult dataclass.
    """
    if not require_tools("aria2c", "curl"):
        return DownloadResult(success=False, url=url,
                              error="Missing required tools: aria2c, curl")

    # Step 1: unwrap 4PDA redirect if needed
    real_url = _extract_real_url(url)

    print(f"\n  OTA endpoint : {real_url}")

    # Step 2: resolve to CDN
    print("  Resolving CDN link ...")
    cdn_url = _resolve_cdn(real_url)
    if not cdn_url:
        return DownloadResult(success=False, url=real_url,
                              error="Failed to resolve CDN URL (no Location header in response)")

    print(f"  CDN URL      : {cdn_url}")

    # Step 3: download via aria2c
    cmd = [
        "aria2c",
        f"-x{connections}",
        f"-s{connections}",
        "-k", "1M",
        "--file-allocation=none",
        "--console-log-level=notice",
    ]
    if output_dir:
        output_dir = Path(output_dir)
        output_dir.mkdir(parents=True, exist_ok=True)
        cmd += ["-d", str(output_dir)]

    cmd.append(cdn_url)

    print(f"\n  Starting download ({connections} connections) ...\n")

    # Run aria2c interactively so the user sees the progress bar
    import subprocess
    proc = subprocess.run(cmd)

    if proc.returncode != 0:
        return DownloadResult(
            success=False, url=real_url, cdn_url=cdn_url,
            error=f"aria2c exited with code {proc.returncode}",
        )

    # Try to figure out where aria2c saved the file
    filename = cdn_url.split("/")[-1].split("?")[0]
    output_path = (Path(output_dir) / filename) if output_dir else Path.cwd() / filename

    return DownloadResult(
        success=True,
        url=real_url,
        cdn_url=cdn_url,
        output_path=output_path if output_path.exists() else None,
    )
