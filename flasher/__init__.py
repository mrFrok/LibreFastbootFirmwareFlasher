"""
flasher — Fastboot Firmware Flasher package.

Public API surface — import from here rather than from submodules directly:

    from flasher import extract_firmware, run_flash_session, run_pre_flash_checks
    from flasher import ExtractionResult, FlashSession, PreFlashCheck
    from flasher import extract_arb_from_xbl, compare_arb_versions
"""

from flasher.extractor import extract_firmware, ExtractionResult
from flasher.flasher import run_flash_session, FlashSession, run_flash_single
from flasher.device import run_pre_flash_checks, PreFlashCheck, DeviceInfo
from flasher.arb import (
    extract_arb_from_xbl,
    get_device_arb_version,
    compare_arb_versions,
    arb_confirmation_gate,
    ArbInfo,
    ArbCheckResult,
)

__all__ = [
    # extractor
    "extract_firmware",
    "ExtractionResult",
    # flasher
    "run_flash_session",
    "FlashSession",
    # device
    "run_pre_flash_checks",
    "PreFlashCheck",
    "DeviceInfo",
    # arb
    "extract_arb_from_xbl",
    "get_device_arb_version",
    "compare_arb_versions",
    "arb_confirmation_gate",
    "ArbInfo",
    "ArbCheckResult",
]

from flasher.downloader import download_firmware, DownloadResult
__all__ += ["download_firmware", "DownloadResult"]
