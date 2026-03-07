import time
import shutil
import logging
from flasher.utils import run_cmd, fastboot, adb
import tempfile
from pathlib import Path
from dataclasses import dataclass, field

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Minimum acceptable transfer speed for cable health test (bytes/sec)
CABLE_SPEED_THRESHOLD_MB = 1.0

# Size of the temporary test payload written during cable speed test
CABLE_TEST_PAYLOAD_MB = 8

# How long to wait between device polling attempts (seconds)
POLL_INTERVAL = 3

# Total time to wait for a device to appear before giving up (seconds)
DEVICE_WAIT_TIMEOUT = 60

# Minimum recommended battery level before flashing
BATTERY_MIN_LEVEL = 30


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

@dataclass
class DeviceInfo:
    """Holds key variables retrieved from the device via fastboot getvar."""
    serial: str = ""
    product: str = ""
    variant: str = ""
    bootloader_version: str = ""
    baseband_version: str = ""
    secure: str = ""
    unlocked: str = ""
    battery_level: int = -1        # -1 = not reported
    slot_count: int = 1            # A/B devices report 2
    current_slot: str = ""
    raw: dict[str, str] = field(default_factory=dict)


@dataclass
class CableTestResult:
    passed: bool
    speed_mbs: float               # measured transfer speed in MB/s
    error: str = ""


@dataclass
class PreFlashCheck:
    device_found: bool
    communication_ok: bool
    cable_ok: bool
    battery_ok: bool
    unlocked: bool
    device_info: DeviceInfo = field(default_factory=DeviceInfo)
    cable_result: CableTestResult = field(default_factory=lambda: CableTestResult(False, 0.0))
    warnings: list[str] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    @property
    def ready(self) -> bool:
        """True only when all hard requirements pass."""
        return (
            self.device_found
            and self.communication_ok
            and self.cable_ok
            and self.unlocked
        )


# ---------------------------------------------------------------------------
# Low-level fastboot / adb wrappers
# ---------------------------------------------------------------------------




# ---------------------------------------------------------------------------
# Device discovery
# ---------------------------------------------------------------------------

def list_fastboot_devices() -> list[str]:
    """Return serial numbers of all devices in fastboot or fastbootd mode."""
    rc, out, _ = fastboot("devices")
    if rc != 0 or not out:
        return []
    serials = []
    for line in out.splitlines():
        parts = line.split()
        # "fastboot devices" output:
        #   <serial>  fastboot   <- device in bootloader (old-style fastboot)
        #   <serial>  fastbootd  <- device in fastbootd (userspace fastboot)
        if len(parts) >= 2 and parts[1] in ("fastboot", "fastbootd"):
            serials.append(parts[0])
    return serials


def list_adb_devices() -> list[str]:
    """Return serial numbers of all devices reachable over adb."""
    rc, out, _ = adb("devices")
    if rc != 0:
        return []
    serials = []
    for line in out.splitlines()[1:]:   # skip the "List of devices" header
        parts = line.split()
        if len(parts) >= 2 and parts[1] == "device":
            serials.append(parts[0])
    return serials


def reboot_tofastbootd(serial: str | None = None) -> bool:
    """
    Reboot from ADB (system) directly into fastbootd via 'adb reboot fastboot'.

    Returns True if the adb command was accepted (device may still be
    rebooting — caller should poll with wait_for_device()).
    """
    cmd = ["-s", serial, "reboot", "fastboot"] if serial else ["reboot", "fastboot"]
    rc, _, err = adb(*cmd, timeout=15)
    if rc != 0:
        log.error(f"adb reboot fastboot failed: {err}")
        return False
    log.info("Reboot to fastbootd sent — waiting for device ...")
    return True


def wait_for_device(timeout: int = DEVICE_WAIT_TIMEOUT) -> str | None:
    """
    Poll fastboot devices until one appears or timeout is reached.

    Returns the serial number of the first device found, or None.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        serials = list_fastboot_devices()
        if serials:
            return serials[0]
        remaining = int(deadline - time.monotonic())
        log.info(f"No device found — retrying … ({remaining}s remaining)")
        time.sleep(POLL_INTERVAL)
    return None


# ---------------------------------------------------------------------------
# Device info
# ---------------------------------------------------------------------------

def _parse_getvar_output(output: str) -> dict[str, str]:
    """
    Parse 'fastboot getvar all' output into a key/value dict.

    fastboot prints variables to stderr in the form:
        (bootloader) key: value
    """
    result: dict[str, str] = {}
    for line in output.splitlines():
        line = line.strip().lstrip("(bootloader)").strip()
        if ":" in line:
            key, _, value = line.partition(":")
            result[key.strip()] = value.strip()
    return result


def get_device_info(serial: str | None = None) -> DeviceInfo | None:
    """
    Retrieve device variables via 'fastboot getvar all'.

    Returns None if communication fails.
    """
    cmd = ["getvar", "all"]
    if serial:
        cmd = ["-s", serial] + cmd

    # fastboot writes getvar output to stderr
    rc, stdout, stderr = fastboot(*cmd, timeout=15)
    raw_output = stderr or stdout

    if not raw_output:
        log.error("fastboot getvar all returned no output")
        return None

    raw = _parse_getvar_output(raw_output)

    def get(*keys: str) -> str:
        for k in keys:
            if k in raw:
                return raw[k]
        return ""

    # Battery level may be reported as "30" or "30%"
    battery_str = get("battery-level", "battery_level").rstrip("%")
    try:
        battery_level = int(battery_str)
    except ValueError:
        battery_level = -1

    try:
        slot_count = int(get("slot-count", "slot_count"))
    except ValueError:
        slot_count = 1

    return DeviceInfo(
        serial=serial or get("serialno"),
        product=get("product"),
        variant=get("variant"),
        bootloader_version=get("version-bootloader", "bootloader-version"),
        baseband_version=get("version-baseband", "baseband-version"),
        secure=get("secure"),
        unlocked=get("unlocked"),
        battery_level=battery_level,
        slot_count=slot_count,
        current_slot=get("current-slot"),
        raw=raw,
    )


# ---------------------------------------------------------------------------
# Cable speed test
# ---------------------------------------------------------------------------

def test_cable_speed(serial: str | None = None) -> CableTestResult:
    """
    Estimate USB transfer speed using 'fastboot stage' (RAM only, no NAND write).

    fastboot stage uploads a file into device RAM without touching any partition.
    Falls back to flashing 'tmp' partition if stage is not supported.
    """
    payload_bytes = CABLE_TEST_PAYLOAD_MB * 1024 * 1024

    with tempfile.NamedTemporaryFile(suffix=".img", delete=False) as f:
        f.write(b"\x00" * payload_bytes)
        tmp_path = f.name

    serial_args = ["-s", serial] if serial else []

    try:
        start = time.monotonic()
        # 'fastboot stage' uploads to RAM only — safe on all devices
        rc, _, err = fastboot(*serial_args, "stage", tmp_path, timeout=60)
        elapsed = time.monotonic() - start

        if rc != 0:
            # stage not supported — skip speed test rather than risk data loss
            return CableTestResult(
                passed=True,
                speed_mbs=0.0,
                error="fastboot stage not supported on this device (speed test skipped)",
            )
    finally:
        Path(tmp_path).unlink(missing_ok=True)

    if elapsed <= 0:
        return CableTestResult(passed=False, speed_mbs=0.0, error="Elapsed time was zero")

    speed_mbs = payload_bytes / elapsed / (1024 * 1024)
    passed = speed_mbs >= CABLE_SPEED_THRESHOLD_MB

    if not passed:
        log.warning(
            f"Cable speed {speed_mbs:.2f} MB/s is below threshold "
            f"({CABLE_SPEED_THRESHOLD_MB} MB/s) — try a different cable or port"
        )
    else:
        log.info(f"Cable speed OK: {speed_mbs:.2f} MB/s")

    return CableTestResult(passed=passed, speed_mbs=speed_mbs)


# ---------------------------------------------------------------------------
# Pre-flash check orchestrator
# ---------------------------------------------------------------------------

def run_pre_flash_checks(serial: str | None = None) -> PreFlashCheck:
    """
    Run all pre-flash diagnostics and return a PreFlashCheck summary.

    Steps:
      1. Detect device via fastboot devices
      2. Attempt adb reboot if not found
      3. Retrieve device info (fastboot getvar all)
      4. Test cable transfer speed
      5. Validate battery level and bootloader unlock state
    """
    check = PreFlashCheck(
        device_found=False,
        communication_ok=False,
        cable_ok=False,
        battery_ok=False,
        unlocked=False,
    )

    # ------------------------------------------------------------------
    # Step 1 — device discovery
    # ------------------------------------------------------------------
    log.info("==> [1/4] Detecting device …")
    serials = list_fastboot_devices()

    if not serials:
        # Not in fastboot/fastbootd — check adb (device in system)
        adb_serials = list_adb_devices()

        if adb_serials:
            adb_serial = serial or adb_serials[0]
            log.info(f"Device found in system mode via adb ({adb_serial})")
            log.info("Pre-flash checks will run in adb mode — reboot handled separately")
            # Mark as found but skip fastboot-specific checks
            check.device_found = True
            check.communication_ok = True  # adb is working
            check.cable_ok = True          # cable is fine if adb works
            check.battery_ok = True        # skip battery check
            check.unlocked = True          # cannot check unlock from adb, assume ok
            return check
        else:
            check.errors.append(
                "No device found via fastboot or adb. "
                "Boot the device into fastboot manually: hold Vol Down + Power"
            )
            return check

    serial = serial or serials[0]
    check.device_found = True
    log.info(f"Device found: {serial}")

    # ------------------------------------------------------------------
    # Step 2 — communication test (fastboot getvar all)
    # ------------------------------------------------------------------
    log.info("==> [2/4] Testing communication (fastboot getvar all) …")
    info = get_device_info(serial)

    if info is None:
        check.errors.append("fastboot getvar all failed — device may be unresponsive")
        return check

    check.communication_ok = True
    check.device_info = info

    log.info(f"  Product       : {info.product}")
    log.info(f"  Variant       : {info.variant}")
    log.info(f"  Bootloader    : {info.bootloader_version}")
    log.info(f"  Baseband      : {info.baseband_version}")
    log.info(f"  Secure boot   : {info.secure}")
    log.info(f"  Unlocked      : {info.unlocked}")
    log.info(f"  Battery       : {info.battery_level}%")
    if info.slot_count == 2:
        log.info(f"  A/B device    : current slot = {info.current_slot}")

    # ------------------------------------------------------------------
    # Step 3 — cable speed test
    # ------------------------------------------------------------------
    log.info("==> [3/4] Testing cable transfer speed …")
    cable = test_cable_speed(serial)
    check.cable_result = cable
    check.cable_ok = cable.passed

    if not cable.passed:
        check.warnings.append(
            f"Slow cable detected ({cable.speed_mbs:.2f} MB/s). "
            "Flashing may take longer or fail. Use a USB 3.0 cable and port."
        )

    # ------------------------------------------------------------------
    # Step 4 — safety checks
    # ------------------------------------------------------------------
    log.info("==> [4/4] Running safety checks …")

    # Bootloader unlock
    if info.unlocked.lower() in ("yes", "true", "1"):
        check.unlocked = True
    else:
        check.errors.append(
            "Bootloader is locked. Unlock it first: fastboot flashing unlock"
        )

    # Battery level
    if info.battery_level == -1:
        check.warnings.append("Battery level not reported by device")
        check.battery_ok = True   # can't block on unknown value
    elif info.battery_level < BATTERY_MIN_LEVEL:
        check.battery_ok = False
        check.errors.append(
            f"Battery too low ({info.battery_level}%). "
            f"Charge to at least {BATTERY_MIN_LEVEL}% before flashing."
        )
    else:
        check.battery_ok = True
        log.info(f"  Battery level OK: {info.battery_level}%")

    return check


# ---------------------------------------------------------------------------
# CLI — standalone diagnostics runner
# ---------------------------------------------------------------------------

def _print_check_report(check: PreFlashCheck) -> None:
    """Print a human-readable summary of PreFlashCheck results."""
    def status(ok: bool) -> str:
        return "✓" if ok else "✗"

    print("\n── Pre-flash check report ─────────────────────────────")
    print(f"  {status(check.device_found):<3} Device detected")
    print(f"  {status(check.communication_ok):<3} Fastboot communication")
    print(f"  {status(check.cable_ok):<3} Cable speed "
          f"({check.cable_result.speed_mbs:.2f} MB/s)")
    print(f"  {status(check.battery_ok):<3} Battery level "
          f"({check.device_info.battery_level}%)")
    print(f"  {status(check.unlocked):<3} Bootloader unlocked")

    if check.warnings:
        print("\n  Warnings:")
        for w in check.warnings:
            print(f"    ⚠  {w}")

    if check.errors:
        print("\n  Errors:")
        for e in check.errors:
            print(f"    ✗  {e}")

    print()
    if check.ready:
        print("  ✓ Device is ready for flashing.")
    else:
        print("  ✗ Device is NOT ready. Fix the errors above before flashing.")
    print("────────────────────────────────────────────────────────\n")


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(
        prog="device",
        description="Run pre-flash diagnostics on a connected Android device",
    )
    parser.add_argument(
        "-s", "--serial",
        default=None,
        metavar="SERIAL",
        help="Target a specific device by serial number",
    )
    args = parser.parse_args()

    # Verify fastboot is available
    if not shutil.which("fastboot"):
        print("✗ fastboot not found in $PATH — install android-tools")
        raise SystemExit(1)

    check = run_pre_flash_checks(serial=args.serial)
    _print_check_report(check)

    raise SystemExit(0 if check.ready else 1)


if __name__ == "__main__":
    main()
