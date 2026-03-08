"""
flasher.flasher — core flash orchestrator for LibreFastbootFirmwareFlasher.

Public API:
    run_flash_session(firmware_dir, serial, dry_run) -> FlashSession
    run_flash_single(image_path, partition, slots, serial, dry_run) -> FlashSession
    flash_partition(image_path, partition, slot, serial) -> FlashResult
    FlashSession, FlashResult
"""

import sys
import time
import logging
import subprocess
import threading
from enum import Enum
from pathlib import Path
from dataclasses import dataclass, field

from flasher.device import run_pre_flash_checks
from flasher.arb import (
    find_xbl_image,
    extract_arb_from_xbl,
    get_device_arb_version,
    compare_arb_versions,
    arb_confirmation_gate,
)

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Partitions flashed in bootloader mode (everything else in fastbootd)
BOOTLOADER_MODE_PARTITIONS: frozenset[str] = frozenset({"modem"})

# Dynamic partitions inside super — flash to ACTIVE slot only
SUPER_PARTITIONS: frozenset[str] = frozenset({
    "system", "system_ext", "system_dlkm",
    "product",
    "odm", "odm_dlkm",
    "vendor", "vendor_dlkm",
    "my_bigball", "my_carrier", "my_engineering", "my_heytap",
    "my_manifest", "my_product", "my_region", "my_stock",
})

# Critical partitions — failure aborts flash immediately
CRITICAL_PARTITIONS: frozenset[str] = frozenset({
    "abl", "xbl", "xbl_config", "xbl_ramdump",
    "aop", "aop_config", "devcfg", "shrm",
    "tz", "hyp", "multiimgoem", "multiimgqti",
    "qupfw", "uefisecapp", "imagefv", "cpucp",
    "boot", "init_boot", "vendor_boot",
    "modem",
})

SLOTS: tuple[str, ...] = ("a", "b")
REBOOT_SETTLE  = 2
REBOOT_TIMEOUT = 90
POLL_INTERVAL  = 3


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

class DeviceMode(Enum):
    SYSTEM     = "system"
    BOOTLOADER = "bootloader"
    FASTBOOTD  = "fastbootd"
    UNKNOWN    = "unknown"


@dataclass
class FlashResult:
    partition: str
    slot:      str
    success:   bool
    error:     str   = ""
    duration_s: float = 0.0


@dataclass
class FlashSession:
    firmware_dir: Path
    results:  list[FlashResult] = field(default_factory=list)
    serial:   str | None = None
    aborted:  bool = False
    dry_run:  bool = False

    @property
    def failed(self) -> list[FlashResult]:
        return [r for r in self.results if not r.success]

    @property
    def succeeded(self) -> list[FlashResult]:
        return [r for r in self.results if r.success]

    @property
    def critical_failed(self) -> list[FlashResult]:
        return [r for r in self.failed if r.partition in CRITICAL_PARTITIONS]


# ---------------------------------------------------------------------------
# Low-level subprocess helpers
# ---------------------------------------------------------------------------

def _run(cmd: list[str], timeout: int = 60) -> tuple[int, str, str]:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout.strip(), r.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", f"Timed out after {timeout}s"
    except FileNotFoundError:
        return -1, "", f"Binary not found: {cmd[0]}"


def _fastboot(*args: str, timeout: int = 60) -> tuple[int, str, str]:
    return _run(["fastboot", *args], timeout=timeout)


def _adb(*args: str, timeout: int = 30) -> tuple[int, str, str]:
    return _run(["adb", *args], timeout=timeout)


# ---------------------------------------------------------------------------
# Device mode detection
# ---------------------------------------------------------------------------

def detect_mode(serial: str | None = None) -> DeviceMode:
    """Detect device mode via fastboot devices, then adb devices."""
    serial_args = ["-s", serial] if serial else []

    rc, out, _ = _fastboot(*serial_args, "devices")
    if rc == 0:
        for line in out.splitlines():
            parts = line.split()
            if len(parts) < 2:
                continue
            if serial and parts[0] != serial:
                continue
            if parts[1] == "fastbootd":
                return DeviceMode.FASTBOOTD
            if parts[1] == "fastboot":
                return DeviceMode.BOOTLOADER

    rc, out, _ = _adb(*serial_args, "devices")
    if rc == 0:
        for line in out.splitlines()[1:]:
            parts = line.split()
            if len(parts) >= 2 and parts[1] == "device":
                if serial is None or parts[0] == serial:
                    return DeviceMode.SYSTEM

    return DeviceMode.UNKNOWN


def get_active_slot(serial: str | None = None) -> str:
    """Return current active slot ('a' or 'b'). Defaults to 'a'."""
    serial_args = ["-s", serial] if serial else []
    _, out, err = _fastboot(*serial_args, "getvar", "current-slot", timeout=10)
    for line in (out + err).lower().splitlines():
        if "current-slot:" in line:
            slot = line.split("current-slot:")[-1].strip()
            if slot in ("a", "b"):
                return slot
    log.warning("Could not detect active slot — defaulting to 'a'")
    return "a"


# ---------------------------------------------------------------------------
# Reboot helpers
# ---------------------------------------------------------------------------

def _wait_for_fastbootd(serial: str | None, timeout: int = REBOOT_TIMEOUT) -> bool:
    """Poll until fastboot devices reports the device as 'fastbootd'."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _, out, _ = _fastboot("devices")
        for line in out.splitlines():
            parts = line.split()
            if len(parts) < 2:
                continue
            if serial and parts[0] != serial:
                continue
            if parts[1] == "fastbootd":
                return True
        remaining = int(deadline - time.monotonic())
        log.info(f"Waiting for fastbootd ... ({remaining}s)")
        time.sleep(POLL_INTERVAL)
    return False


def _wait_for_fastboot(serial: str | None, timeout: int = REBOOT_TIMEOUT) -> bool:
    """Poll until any fastboot device appears."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _, out, _ = _fastboot(*(["-s", serial] if serial else []), "devices")
        if out.strip():
            return True
        remaining = int(deadline - time.monotonic())
        log.info(f"Waiting for bootloader ... ({remaining}s)")
        time.sleep(POLL_INTERVAL)
    return False


def enter_bootloader(serial: str | None) -> bool:
    """Reboot from fastbootd into bootloader."""
    log.info("Rebooting to bootloader ...")
    rc, _, err = _fastboot(*(["-s", serial] if serial else []), "reboot", "bootloader")
    if rc != 0:
        log.error(f"fastboot reboot bootloader failed: {err}")
        return False
    time.sleep(REBOOT_SETTLE)
    return _wait_for_fastboot(serial)


# ---------------------------------------------------------------------------
# Progress bar
# ---------------------------------------------------------------------------

def _progress(done: int, total: int, partition: str, slot: str, elapsed: float) -> None:
    pct    = int(done / total * 100) if total else 0
    bar_w  = 24
    filled = int(bar_w * done / total) if total else 0
    bar    = "█" * filled + "░" * (bar_w - filled)
    mins, secs = divmod(int(elapsed), 60)
    time_s = f"{mins}m{secs:02d}s" if mins else f"{secs}s"
    label  = f"{partition}_{slot}" if slot else partition
    print(f"\r  [{bar}] {pct:>3}%  {done}/{total}  {time_s}  {label:<28}", end="", flush=True)


def _flash_with_progress(
    image_path: Path,
    partition:  str,
    slot:       str,
    serial:     str | None,
    done:       int,
    total:      int,
    flash_start: float,
) -> FlashResult:
    """Flash partition in background thread while animating progress bar."""
    result_box: list[FlashResult] = []

    def _worker():
        result_box.append(flash_partition(image_path, partition, slot, serial))

    root_logger = logging.getLogger()
    old_level   = root_logger.level
    root_logger.setLevel(logging.CRITICAL)  # silence logs during animation

    t = threading.Thread(target=_worker, daemon=True)
    t.start()
    while t.is_alive():
        _progress(done, total, partition, slot, time.monotonic() - flash_start)
        time.sleep(0.15)

    root_logger.setLevel(old_level)
    _progress(done + 1, total, partition, slot, time.monotonic() - flash_start)
    return result_box[0]


# ---------------------------------------------------------------------------
# Core flash primitive
# ---------------------------------------------------------------------------

def flash_partition(
    image_path: Path,
    partition:  str,
    slot:       str,
    serial:     str | None = None,
) -> FlashResult:
    """
    Flash image_path to partition on slot using 'fastboot --slot <s> flash <p>'.
    Using --slot avoids the double-suffix bug (e.g. abl_a_a).
    """
    cmd = [
        *(["-s", serial] if serial else []),
        "--slot", slot,
        "flash", partition, str(image_path),
    ]
    start = time.monotonic()
    rc, out, err = _fastboot(*cmd, timeout=300)
    duration = time.monotonic() - start

    if rc == 0:
        return FlashResult(partition=partition, slot=slot, success=True, duration_s=duration)

    error_msg = err or out or f"fastboot exited with code {rc}"
    return FlashResult(partition=partition, slot=slot, success=False,
                       error=error_msg, duration_s=duration)


# ---------------------------------------------------------------------------
# Colour helpers (auto-disabled when stdout is not a TTY)
# ---------------------------------------------------------------------------

def _c(code: str) -> str:
    """Return ANSI escape code only when stdout is a TTY."""
    import sys
    return code if sys.stdout.isatty() else ""

def _RED()    -> str: return _c("[38;5;203m")
def _ORANGE() -> str: return _c("[38;5;208m")
def _GREEN()  -> str: return _c("[38;5;78m")
def _YELLOW() -> str: return _c("[38;5;220m")
def _CYAN()   -> str: return _c("[38;5;117m")
def _GRAY()   -> str: return _c("[38;5;244m")
def _BOLD()   -> str: return _c("[1m")
def _R()      -> str: return _c("[0m")


# ---------------------------------------------------------------------------
# Error reporting
# ---------------------------------------------------------------------------

def _report_failure(result: FlashResult) -> None:
    is_critical = result.partition in CRITICAL_PARTITIONS
    err_lower   = (result.error or "").lower()

    R = _R(); RED = _RED(); ORANGE = _ORANGE()
    YELLOW = _YELLOW(); CYAN = _CYAN(); BOLD = _BOLD()

    print()
    print(f"{RED}{'━' * 60}{R}")
    print(f"  {RED}{BOLD}✗  FAILED{R}  {BOLD}{result.partition}_{result.slot}{R}")
    print(f"  {GRAY()}{result.error}{R}")
    print()

    if "resize" in err_lower or "not enough space" in err_lower:
        print(f"  {ORANGE}Cause:{R} Dynamic partition resize failed.")
        print(f"  {CYAN}Fix  :{R} Make sure the device is in {BOLD}fastbootd{R} and retry.")
        print(f"         {GRAY()}fastboot reboot fastboot{R}")
    elif "does not exist" in err_lower or "not found" in err_lower:
        print(f"  {ORANGE}Cause:{R} Partition not present on this device.")
        print(f"  {CYAN}Fix  :{R} This image may not be compatible with your device variant.")
    elif "permission denied" in err_lower or "not allowed" in err_lower:
        print(f"  {ORANGE}Cause:{R} Bootloader is locked.")
        print(f"  {CYAN}Fix  :{R} {BOLD}fastboot flashing unlock{R}")
    elif "timeout" in err_lower:
        print(f"  {ORANGE}Cause:{R} USB timeout.")
        print(f"  {CYAN}Fix  :{R} Try a different cable or USB 3.0 port.")
    else:
        print(f"  {ORANGE}Possible causes:{R}")
        print(f"    {GRAY()}•{R} Faulty USB cable — try a different one")
        print(f"    {GRAY()}•{R} Bootloader is locked  →  {BOLD}fastboot flashing unlock{R}")
        print(f"    {GRAY()}•{R} Corrupted image — re-download the firmware")
        print(f"    {GRAY()}•{R} Low battery during flash")

    if is_critical:
        print()
        print(f"  {RED}{BOLD}⚠  CRITICAL partition{R}{RED} — do NOT reboot or unplug until resolved.{R}")
    print(f"{RED}{'━' * 60}{R}")
    print()


# ---------------------------------------------------------------------------
# On-error interactive handler
# ---------------------------------------------------------------------------

def _on_flash_error(
    result:      FlashResult,
    serial:      str | None,
    target_mode: DeviceMode = DeviceMode.FASTBOOTD,
) -> bool:
    """
    Ask user what to do after a flash failure.

    target_mode controls what [2] reboots into:
      DeviceMode.FASTBOOTD  — fastboot reboot fastboot  (normal partitions)
      DeviceMode.BOOTLOADER — fastboot reboot bootloader (modem / stage-2)

    Returns True to retry, False to abort.
    """
    _report_failure(result)

    if target_mode == DeviceMode.BOOTLOADER:
        mode_label  = "bootloader"
        reboot_cmd  = ["reboot", "bootloader"]
        wait_fn     = _wait_for_fastboot
    else:
        mode_label  = "fastbootd"
        reboot_cmd  = ["reboot", "fastboot"]
        wait_fn     = _wait_for_fastbootd

    print("  What do you want to do?")
    print(f"  [1] Retry this partition now")
    print(f"  [2] Reboot to {mode_label} first, then retry"
          f"  (fastboot {' '.join(reboot_cmd)})")
    print("  [3] Abort flashing")

    while True:
        choice = input("\n  Choice: ").strip().lower()

        if choice == "3":
            return False

        if choice == "1":
            return True

        if choice == "2":
            print()
            print(f"  Rebooting to {mode_label} ...")
            serial_args = ["-s", serial] if serial else []
            rc, out, err = _fastboot(*serial_args, *reboot_cmd, timeout=30)
            if rc != 0:
                print(f"  ✗ Reboot command failed: {err or out}")
                print(f"  Reboot manually, then press [1] to retry.")
                continue
            print(f"  Waiting for {mode_label} ...")
            if wait_fn(serial):
                print(f"  ✓ Device is in {mode_label} — retrying ...")
                return True
            else:
                print(f"  ✗ Device did not enter {mode_label}.")
                print("  Try rebooting manually, then press [1] to retry.")

        else:
            print("  Enter 1, 2 or 3")


# ---------------------------------------------------------------------------
# Super partition wipe
# ---------------------------------------------------------------------------

def wipe_super(serial: str | None, super_images: dict) -> None:
    """
    For each dynamic partition being flashed:
      1. Delete <n>_a, <n>_b and COW snapshots
      2. Recreate <n>_a and <n>_b with size=0
    """
    serial_args = ["-s", serial] if serial else []
    names = list(super_images.keys())
    print(f"  Preparing {len(names)} super partition(s) ...")

    for base in names:
        candidates = []
        for slot in ("a", "b"):
            part = f"{base}_{slot}"
            candidates.append(part)
            for suffix in ("-cow", "_cow", "-cow-img"):
                candidates.append(part + suffix)

        for part in candidates:
            rc, _, err = _fastboot(*serial_args, "delete-logical-partition", part, timeout=15)
            if rc != 0:
                err_s = (err or "").lower()
                if "does not exist" not in err_s and "no such" not in err_s:
                    log.warning(f"delete-logical-partition {part}: {err}")

        for slot in ("a", "b"):
            part = f"{base}_{slot}"
            rc, _, err = _fastboot(*serial_args, "create-logical-partition", part, "0", timeout=15)
            if rc != 0:
                log.warning(f"create-logical-partition {part}: {err}")

    print("  ✓ Super partitions cleared and recreated with size=0")


# ---------------------------------------------------------------------------
# Image collection
# ---------------------------------------------------------------------------

def _collect_images(firmware_dir: Path) -> dict[str, Path]:
    """
    Scan firmware_dir for .img files.
    Strips _a/_b suffix so abl_a.img -> key 'abl' (prevents abl_a_a bug).
    Shallower paths win on duplicates.
    """
    images: dict[str, Path] = {}
    for img in sorted(firmware_dir.rglob("*.img"), key=lambda p: len(p.parts)):
        stem = img.stem.lower()
        for suffix in ("_a", "_b"):
            if stem.endswith(suffix):
                stem = stem[: -len(suffix)]
                break
        if stem not in images:
            images[stem] = img
    return images


# ---------------------------------------------------------------------------
# Main flash session
# ---------------------------------------------------------------------------

def run_flash_session(
    firmware_dir: Path,
    serial:   str | None = None,
    dry_run:  bool = False,
) -> FlashSession:
    """
    Full firmware flash orchestrator.

    Stages:
      1. Pre-flash checks
      2. Collect images
      3. ARB check
      4. Ask user how to reach fastbootd
      5. Stage 1: fastbootd — non-super (both slots) + super (active slot)
      6. Stage 2: bootloader — modem (both slots)
      7. Reboot to system (only on full success)
    """
    session = FlashSession(firmware_dir=firmware_dir, serial=serial, dry_run=dry_run)

    # ── Pre-flash checks ────────────────────────────────────────────────
    log.info("==> Running pre-flash checks ...")
    check = run_pre_flash_checks(serial=serial)
    if not check.ready:
        print("\n✗ Pre-flash checks failed. Aborting.\n")
        for err in check.errors:
            print(f"  ✗ {err}")
        sys.exit(1)

    serial = serial or (check.device_info.serial if check.device_info else None)
    session.serial = serial

    # ── Collect images ──────────────────────────────────────────────────
    images = _collect_images(firmware_dir)
    if not images:
        print(f"✗ No .img files found in {firmware_dir}")
        sys.exit(1)

    fastbootd_images  = {k: v for k, v in images.items() if k not in BOOTLOADER_MODE_PARTITIONS}
    bootloader_images = {k: v for k, v in images.items() if k in BOOTLOADER_MODE_PARTITIONS}

    print(f"\nFound {len(images)} image(s) to flash:")
    for name, path in sorted(images.items()):
        mode_tag = "bootloader" if name in BOOTLOADER_MODE_PARTITIONS else "fastbootd"
        crit_tag = " [CRITICAL]" if name in CRITICAL_PARTITIONS else ""
        size_mb  = path.stat().st_size / 1024 / 1024
        print(f"  {name:<30} {size_mb:>7.1f} MB  ({mode_tag}){crit_tag}")

    # ── ARB check ───────────────────────────────────────────────────────
    xbl_path = find_xbl_image(firmware_dir)
    if xbl_path:
        firmware_arb = extract_arb_from_xbl(xbl_path)
        device_arb   = get_device_arb_version(serial)
        arb_result   = compare_arb_versions(firmware_arb, device_arb)
        if not arb_confirmation_gate(arb_result):
            print("Aborted by user (ARB check).")
            sys.exit(0)
    else:
        log.warning("xbl.img not found — ARB check skipped")

    if dry_run:
        print("\n[dry-run] No partitions were flashed.")
        return session

    # ── Ask user how to reach fastbootd ─────────────────────────────────
    if fastbootd_images:
        print()
        print("── Reboot to fastbootd ──────────────────────────────────")
        print("  Where is the device right now?")
        print()
        print("  [1] In system (Android running)  → adb reboot fastboot")
        print("  [2] In bootloader                → fastboot reboot fastboot")
        print("  [3] Already in fastbootd          → skip reboot")
        print("  [q] Abort")

        while True:
            choice = input("\n  Choice: ").strip().lower()
            if choice == "q":
                print("Aborted.")
                sys.exit(0)
            if choice in ("1", "2", "3"):
                break
            print("  Enter 1, 2, 3 or q")

        if choice == "1":
            print()
            rc, _, err = _adb(*(["-s", serial] if serial else []), "reboot", "fastboot")
            if rc != 0:
                print(f"✗ adb reboot fastboot failed: {err}")
                sys.exit(1)
            print("  Waiting for fastbootd ...")
            if not _wait_for_fastbootd(serial):
                print("✗ Device did not enter fastbootd. Aborting.")
                sys.exit(1)
            print("  ✓ Device is in fastbootd")

        elif choice == "2":
            print()
            rc, out, err = _fastboot(*(["-s", serial] if serial else []), "reboot", "fastboot")
            if rc != 0:
                print(f"✗ fastboot reboot fastboot failed: {err}")
                sys.exit(1)
            print("  Waiting for fastbootd ...")
            if not _wait_for_fastbootd(serial):
                print("✗ Device did not enter fastbootd. Aborting.")
                sys.exit(1)
            print("  ✓ Device is in fastbootd")

        print("────────────────────────────────────────────────────────")

    print()
    input("Device is in fastbootd. Press Enter to begin flashing, or Ctrl+C to abort ...")

    # ── Stage 1: fastbootd ───────────────────────────────────────────────
    if fastbootd_images:
        active_slot      = get_active_slot(serial)
        super_images     = {k: v for k, v in fastbootd_images.items() if k in SUPER_PARTITIONS}
        non_super_images = {k: v for k, v in fastbootd_images.items() if k not in SUPER_PARTITIONS}

        total_ops   = len(non_super_images) * 2 + len(super_images)
        flash_start = time.monotonic()
        done_ops    = 0

        print(f"\n── Stage 1/2: fastbootd ──────────────────────────────")
        print(f"  Active slot    : {active_slot.upper()}")
        print(f"  Non-super      : {len(non_super_images)} partitions × 2 slots")
        print(f"  Super (dynamic): {len(super_images)} partitions × 1 slot (active only)")
        print(f"  Total          : {total_ops} flash operations")
        print()

        # Non-super → both slots
        for slot in SLOTS:
            for partition, image_path in sorted(non_super_images.items()):
                result = _flash_with_progress(image_path, partition, slot, serial,
                                              done_ops, total_ops, flash_start)
                session.results.append(result)
                done_ops += 1
                if not result.success:
                    print()
                    retry = _on_flash_error(result, serial)
                    if not retry:
                        session.aborted = True
                        return session
                    result = _flash_with_progress(image_path, partition, slot, serial,
                                                  done_ops - 1, total_ops, flash_start)
                    # Replace the failed result instead of appending — keeps summary clean
                    session.results[-1] = result
                    if not result.success:
                        print()
                        print("✗ Retry failed. Aborting.")
                        return session

        # Super → active slot only
        if super_images:
            print(f"\n\n── Clearing super partition ─────────────────────────────")
            wipe_super(serial, super_images)
            print()
            for partition, image_path in sorted(super_images.items()):
                result = _flash_with_progress(image_path, partition, active_slot, serial,
                                              done_ops, total_ops, flash_start)
                session.results.append(result)
                done_ops += 1
                if not result.success:
                    print()
                    retry = _on_flash_error(result, serial)
                    if not retry:
                        session.aborted = True
                        return session
                    result = _flash_with_progress(image_path, partition, active_slot, serial,
                                                  done_ops - 1, total_ops, flash_start)
                    session.results[-1] = result
                    if not result.success:
                        print()
                        print("✗ Retry failed. Aborting.")
                        return session

        elapsed = time.monotonic() - flash_start
        mins, secs = divmod(int(elapsed), 60)
        print(f"\n  ✓ Stage 1 complete in {mins}m{secs:02d}s")

    # ── Stage 2: bootloader (modem) ──────────────────────────────────────
    if bootloader_images:
        log.info("==> Rebooting to bootloader for modem flash ...")
        if not enter_bootloader(serial):
            print("✗ Could not reach bootloader. Modem was not flashed.")
            for partition in bootloader_images:
                for slot in SLOTS:
                    session.results.append(FlashResult(
                        partition=partition, slot=slot,
                        success=False, error="Could not enter bootloader mode",
                    ))
            return session

        total_ops2   = len(bootloader_images) * 2
        done_ops2    = 0
        flash_start2 = time.monotonic()
        print(f"\n── Stage 2/2: bootloader ({len(bootloader_images)} partitions × 2 slots) ──")
        print()

        for slot in SLOTS:
            for partition, image_path in sorted(bootloader_images.items()):
                result = _flash_with_progress(image_path, partition, slot, serial,
                                              done_ops2, total_ops2, flash_start2)
                session.results.append(result)
                done_ops2 += 1
                if not result.success:
                    print()
                    retry = _on_flash_error(result, serial, DeviceMode.BOOTLOADER)
                    if not retry:
                        session.aborted = True
                        return session
                    result = _flash_with_progress(image_path, partition, slot, serial,
                                                  done_ops2 - 1, total_ops2, flash_start2)
                    session.results[-1] = result
                    if not result.success:
                        print()
                        print("✗ Retry failed. Aborting.")
                        return session

        elapsed2 = time.monotonic() - flash_start2
        mins2, secs2 = divmod(int(elapsed2), 60)
        print(f"\n  ✓ Stage 2 complete in {mins2}m{secs2:02d}s")

    return session


# ---------------------------------------------------------------------------
# Single-partition flash
# ---------------------------------------------------------------------------

def run_flash_single(
    image_path: Path,
    partition:  str | None = None,
    slots:      list[str] | None = None,
    serial:     str | None = None,
    dry_run:    bool = False,
) -> FlashSession:
    """Flash a single .img file to one or both slots."""
    image_path = Path(image_path).resolve()
    session = FlashSession(firmware_dir=image_path.parent, serial=serial, dry_run=dry_run)

    if not image_path.exists():
        print(f"✗ Image not found: {image_path}")
        return session

    # Determine partition name
    part_name = partition or image_path.stem.lower()
    for suffix in ("_a", "_b"):
        if part_name.endswith(suffix):
            part_name = part_name[: -len(suffix)]
            break

    is_critical = part_name in CRITICAL_PARTITIONS
    if slots is None:
        slots = list(SLOTS)

    print(f"\n── Flash single partition: {part_name} ──────────────────")
    print(f"  Image     : {image_path.name}  ({image_path.stat().st_size / 1024**2:.1f} MB)")
    print(f"  Partition : {part_name}")
    print(f"  Slots     : {', '.join(slots) if slots != [''] else 'non-A/B'}")
    if is_critical:
        print("  ⚠  CRITICAL partition — do not unplug during flash")
    print()

    if dry_run:
        for slot in slots:
            label = f"{part_name}_{slot}" if slot else part_name
            print(f"  [dry-run] would flash: {label} <- {image_path.name}")
        print("────────────────────────────────────────────────────────")
        return session

    for slot in slots:
        label = f"{part_name}_{slot}" if slot else part_name
        print(f"  Flashing {label} ...", end=" ", flush=True)

        if slot:
            result = flash_partition(image_path, part_name, slot, serial)
        else:
            start = time.monotonic()
            rc, out, err = _fastboot(*(["-s", serial] if serial else []),
                                     "flash", part_name, str(image_path), timeout=300)
            duration = time.monotonic() - start
            result = FlashResult(partition=part_name, slot="",
                                 success=(rc == 0),
                                 error=err or out if rc != 0 else "",
                                 duration_s=duration)

        session.results.append(result)

        if result.success:
            print(f"OK  ({result.duration_s:.1f}s)")
        else:
            print("FAILED")
            _report_failure(result)
            print("────────────────────────────────────────────────────────")
            return session

    print("────────────────────────────────────────────────────────")
    print(f"  ✓ {part_name} flashed successfully")
    return session


# ---------------------------------------------------------------------------
# Summary + wipe
# ---------------------------------------------------------------------------

def _offer_wipe(session: FlashSession) -> None:
    print("── Format userdata ──────────────────────────────────────")
    print("  'fastboot -w' wipes ALL user data (contacts, apps, files).")
    print("  Recommended after a major version change or cross-region flash.")
    print()
    print("  ⚠  ALL DATA WILL BE PERMANENTLY ERASED.")
    print()
    answer = input("  Wipe userdata now? (yes / no): ").strip().lower()
    if answer != "yes":
        print("  Skipped. Wipe manually later: fastboot -w")
        print("────────────────────────────────────────────────────────\n")
        return
    print("  Wiping userdata ...")
    rc, out, err = _fastboot(*(["-s", session.serial] if session.serial else []),
                              "-w", timeout=120)
    if rc == 0:
        print("  ✓ Userdata wiped successfully.")
    else:
        print(f"  ✗ Wipe failed: {err or out}")
    print("────────────────────────────────────────────────────────\n")


def _print_summary(session: FlashSession) -> None:
    total  = len(session.results)
    ok     = len(session.succeeded)
    failed = len(session.failed)

    R = _R(); BOLD = _BOLD()
    RED = _RED(); GREEN = _GREEN(); ORANGE = _ORANGE()
    YELLOW = _YELLOW(); GRAY = _GRAY(); CYAN = _CYAN()

    # ── Summary table ────────────────────────────────────────────────────
    print(f"\n{GRAY}── Flash session summary {'─' * 31}{R}")
    print(f"  Total      {GRAY}:{R}  {total}")
    print(f"  {GREEN}✓ OK{R}       {GRAY}:{R}  {GREEN}{ok}{R}")
    if failed:
        print(f"  {RED}✗ Failed{R}   {GRAY}:{R}  {RED}{failed}{R}")

    if session.failed:
        print(f"\n  {RED}Failed partitions:{R}")
        for r in session.failed:
            crit = f"  {RED}[CRITICAL]{R}" if r.partition in CRITICAL_PARTITIONS else ""
            print(f"    {RED}✗{R}  {BOLD}{r.partition}_{r.slot}{R}{crit}")
            print(f"       {GRAY}{r.error}{R}")

    print()

    # ── Outcome message ──────────────────────────────────────────────────
    if not session.failed and not session.aborted:
        # ── BIG SUCCESS BANNER ───────────────────────────────────────────
        print(f"{GREEN}{'━' * 60}{R}")
        print(f"  {GREEN}{BOLD}✓  Flash complete!{R}")
        print(f"{GREEN}{'━' * 60}{R}")
        print(f"  {GRAY}Partitions flashed :{R}  {GREEN}{ok}{R}")
        elapsed_total = sum(r.duration_s for r in session.succeeded)
        mins, secs = divmod(int(elapsed_total), 60)
        time_str = f"{mins}m {secs:02d}s" if mins else f"{secs}s"
        print(f"  {GRAY}Total flash time   :{R}  {time_str}")
        print(f"{GREEN}{'━' * 60}{R}")
        print()

    elif session.critical_failed:
        print(f"{RED}{'━' * 60}{R}")
        print(f"  {RED}{BOLD}✗  Critical failure{R}")
        print(f"  {RED}One or more CRITICAL partitions failed to flash.{R}")
        print(f"  {RED}The device may not boot.{R}")
        print(f"  {YELLOW}Do NOT reboot or unplug until resolved.{R}")
        print(f"{RED}{'━' * 60}{R}")
        print()

    elif session.failed:
        print(f"{YELLOW}{'━' * 60}{R}")
        print(f"  {YELLOW}{BOLD}⚠  Flash completed with errors{R}")
        print(f"  {GRAY}Non-critical partitions failed — device should still boot.{R}")
        print(f"  {CYAN}Re-flash the failed partitions to complete the update.{R}")
        print(f"{YELLOW}{'━' * 60}{R}")
        print()

    elif session.aborted:
        print(f"{YELLOW}{'━' * 60}{R}")
        print(f"  {YELLOW}{BOLD}⚠  Flash was aborted{R}")
        print(f"{YELLOW}{'━' * 60}{R}")
        print()

    # ── Wipe offer ───────────────────────────────────────────────────────
    if not session.failed and not session.aborted:
        _offer_wipe(session)
    elif not session.critical_failed and session.failed:
        print(f"  {GRAY}Skipping userdata wipe — re-flash failed partitions first.{R}")

    # ── Reboot ───────────────────────────────────────────────────────────
    if not session.failed and not session.aborted:
        print(f"\n  {CYAN}Rebooting to system ...{R}")
        _fastboot(*(["-s", session.serial] if session.serial else []), "reboot", timeout=30)
    elif session.aborted:
        print(f"\n  {YELLOW}Flash was aborted — device not rebooted.{R}")
    else:
        print(f"\n  {RED}Flash had errors — device not rebooted. Fix issues and re-flash.{R}")
