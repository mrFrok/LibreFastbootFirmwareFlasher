"""
arb.py - Anti-Rollback (ARB) version checker for Qualcomm-based OnePlus / OPPO devices.

Parses xbl_config.img directly in Python using the same algorithm as arbextract:
  https://github.com/koaaN/arbextract

Algorithm (from arbextract.c):
  1. Parse ELF64 header -> locate program headers
  2. Find the last PT_NULL segment with filesz > 0 (HASH segment)
  3. Scan HASH segment for a Hash Table Segment Header
  4. Jump to OEM Metadata at header_off + 36 + common_sz + qti_sz
  5. Read: major (4B), minor (4B), arb (4B)

ARB == 0: hard ARB not enforced (safe).
ARB  > 0: hard ARB active - flashing a lower version will brick the device.
"""

import re
import struct
import logging
from pathlib import Path
from dataclasses import dataclass

from flasher.utils import run_cmd

log = logging.getLogger(__name__)

_ELF_MAGIC  = b"\x7fELF"
_ELFCLASS64 = 2
_EI_CLASS   = 4
_PT_NULL    = 0


# ---------------------------------------------------------------------------
# Result dataclass
# ---------------------------------------------------------------------------

@dataclass
class ArbInfo:
    version: int | None
    source: str = ""
    oem_major: int | None = None
    oem_minor: int | None = None

    @property
    def enforced(self) -> bool:
        return self.version is not None and self.version > 0

    def __str__(self) -> str:
        if self.version is None:
            return "ARB version: unknown"
        if self.version == 0:
            return "ARB version: 0 (hard ARB not enforced)"
        return f"ARB version: {self.version} (hard ARB ACTIVE)"


# ---------------------------------------------------------------------------
# Native Python ELF parser - mirrors arbextract.c logic exactly
# ---------------------------------------------------------------------------

def extract_arb_from_xbl_config(xbl_config_path: Path) -> ArbInfo:
    """
    Parse xbl_config.img and return its ARB version.

    Mirrors the algorithm from arbextract.c:
      1. Validate ELF64 magic
      2. Iterate program headers in reverse, find last PT_NULL with filesz > 0
      3. Scan that segment for the Hash Table header
      4. Read OEM metadata: major, minor, arb
    """
    path = Path(xbl_config_path)

    if not path.exists():
        log.warning(f"xbl_config.img not found: {path}")
        return ArbInfo(version=None, source="file not found")

    try:
        data = path.read_bytes()
    except OSError as exc:
        log.error(f"Cannot read {path}: {exc}")
        return ArbInfo(version=None, source=f"read error: {exc}")

    if len(data) < 64:
        return ArbInfo(version=None, source="file too small for ELF header")

    if data[:4] != _ELF_MAGIC or data[_EI_CLASS] != _ELFCLASS64:
        return ArbInfo(version=None, source="not a valid ELF64 file")

    e_phoff   = struct.unpack_from("<Q", data, 0x20)[0]
    e_phentsz = struct.unpack_from("<H", data, 0x36)[0]
    e_phnum   = struct.unpack_from("<H", data, 0x38)[0]

    log.debug(f"ELF64: e_phoff={e_phoff:#x} e_phentsz={e_phentsz} e_phnum={e_phnum}")

    # Find last PT_NULL segment with filesz > 0 (HASH segment)
    hash_off  = 0
    hash_size = 0

    for i in range(e_phnum - 1, -1, -1):
        ph_start = e_phoff + i * e_phentsz
        if ph_start + 56 > len(data):
            continue
        p_type   = struct.unpack_from("<I", data, ph_start)[0]
        p_offset = struct.unpack_from("<Q", data, ph_start + 8)[0]
        p_filesz = struct.unpack_from("<Q", data, ph_start + 32)[0]

        if p_type == _PT_NULL and p_filesz > 0:
            hash_off  = p_offset
            hash_size = p_filesz
            log.debug(f"HASH segment: offset={hash_off:#x} size={hash_size:#x}")
            break

    if not hash_size:
        return ArbInfo(version=None, source="HASH segment not found in ELF")

    if hash_off + hash_size > len(data):
        return ArbInfo(version=None, source="HASH segment extends beyond file")

    seg = data[hash_off: hash_off + hash_size]

    # Scan for Hash Table Segment Header
    # Fields: version(4) common_sz(4) qti_sz(4) oem_sz(4) hash_tbl_sz(4) ... (36B total)
    header_off = None

    for off in range(0, min(0x1000, len(seg) - 36), 4):
        version, common_sz, qti_sz, oem_sz, hash_tbl_sz = struct.unpack_from(
            "<IIIII", seg, off
        )
        if not (1 <= version <= 10):
            continue
        if common_sz > 0x1000 or oem_sz > 0x4000 or hash_tbl_sz > 0x4000:
            continue
        if off + 36 + common_sz + qti_sz + oem_sz > len(seg):
            continue
        header_off = off
        log.debug(
            f"Hash table header at seg+{off:#x}: "
            f"ver={version} common={common_sz} qti={qti_sz} oem={oem_sz}"
        )
        break

    if header_off is None:
        return ArbInfo(version=None, source="hash table header not found in HASH segment")

    # Read OEM metadata at: header_off + 36 + common_sz + qti_sz
    common_sz = struct.unpack_from("<I", seg, header_off + 4)[0]
    qti_sz    = struct.unpack_from("<I", seg, header_off + 8)[0]
    oem_off   = header_off + 36 + common_sz + qti_sz

    if oem_off + 12 > len(seg):
        return ArbInfo(version=None, source="OEM metadata offset out of bounds")

    oem_major, oem_minor, arb = struct.unpack_from("<III", seg, oem_off)

    log.info(f"OEM Metadata Major={oem_major} Minor={oem_minor} ARB={arb} (from {path.name})")

    return ArbInfo(
        version=arb,
        source=f"xbl_config ELF OEM metadata ({path.name})",
        oem_major=oem_major,
        oem_minor=oem_minor,
    )


# ---------------------------------------------------------------------------
# Convenience locators
# ---------------------------------------------------------------------------

def find_xbl_config(search_dir: Path) -> Path | None:
    """Locate xbl_config.img (or xbl_config_a/b.img) under search_dir."""
    for name in ("xbl_config.img", "xbl_config_a.img", "xbl_config_b.img"):
        hit = next(search_dir.rglob(name), None)
        if hit:
            return hit
    return None


def find_xbl_image(search_dir: Path) -> Path | None:
    """Alias for find_xbl_config - kept for backward compatibility."""
    return find_xbl_config(search_dir)


def extract_arb_from_xbl(xbl_path: Path) -> ArbInfo:
    """Backward-compat alias - routes to extract_arb_from_xbl_config."""
    if xbl_path.stem.startswith("xbl_config"):
        return extract_arb_from_xbl_config(xbl_path)
    xbl_config = find_xbl_config(xbl_path.parent)
    if xbl_config:
        return extract_arb_from_xbl_config(xbl_config)
    return ArbInfo(version=None, source="xbl_config.img not found next to xbl.img")


# ---------------------------------------------------------------------------
# Device ARB version — dump xbl_config from device via fastboot
# ---------------------------------------------------------------------------

def _fastboot_dump(partition: str, dest: Path, serial: str | None = None) -> bool:
    """
    Dump a partition from a connected device to a local file.

    Tries two methods:
      1. fastboot dump_partition  (older fastboot / device-side command)
      2. fastboot fetch           (newer fastboot >= 31)

    Returns True on success.
    """
    base_cmd = ["fastboot"] + (["-s", serial] if serial else [])

    # Method 1: dump_partition
    cmd = base_cmd + ["dump_partition", partition, str(dest)]
    rc, out, err = run_cmd(cmd, timeout=60)
    if rc == 0 and dest.exists() and dest.stat().st_size > 0:
        log.debug(f"dump_partition succeeded for {partition}")
        return True

    # Method 2: fetch (fastboot >= 31)
    cmd = base_cmd + ["fetch", partition, str(dest)]
    rc, out, err = run_cmd(cmd, timeout=60)
    if rc == 0 and dest.exists() and dest.stat().st_size > 0:
        log.debug(f"fetch succeeded for {partition}")
        return True

    log.warning(f"Could not dump {partition}: {err or out}")
    return False


def dump_xbl_config_from_device(
    serial: str | None = None,
    dest_dir: Path | None = None,
) -> Path | None:
    """
    Dump xbl_config_a from the connected device via fastboot.

    Tries slots in order: xbl_config_a, xbl_config_b, xbl_config.
    Returns the path to the dumped file, or None on failure.
    """
    import tempfile
    out_dir = dest_dir or Path(tempfile.mkdtemp(prefix="lfff_arb_"))

    for part in ("xbl_config_a", "xbl_config_b", "xbl_config"):
        dest = out_dir / f"{part}.img"
        log.info(f"Attempting to dump {part} from device ...")
        if _fastboot_dump(part, dest, serial):
            return dest

    return None


def get_device_arb_version(serial: str | None = None) -> tuple["ArbInfo", str]:
    """
    Read ARB version from the connected device.

    Strategy:
      1. Dump xbl_config_a via fastboot dump_partition / fetch
      2. Parse the ELF OEM metadata (same as firmware check)
      3. Fallback: fastboot getvar anti-rollback-version

    Returns (ArbInfo, method_used).
    """
    # ── Step 1: try dumping xbl_config from device ───────────────────────
    dumped = dump_xbl_config_from_device(serial)
    if dumped:
        arb = extract_arb_from_xbl_config(dumped)
        if arb.version is not None:
            arb.source = f"dumped {dumped.name} from device"
            log.info(f"Device ARB via dump: {arb.version}")
            try:
                dumped.unlink()
            except OSError:
                pass
            return arb, "dump"

    # ── Step 2: fallback to fastboot getvar ──────────────────────────────
    log.info("xbl_config dump failed or unreadable — falling back to fastboot getvar")
    cmd = ["fastboot"] + (["-s", serial] if serial else []) + ["getvar", "anti-rollback-version"]
    rc, stdout, stderr = run_cmd(cmd, timeout=15)

    if rc != -1:
        output = stdout + stderr
        for var in ("anti-rollback-version", "version-anti-rollback"):
            match = re.search(rf"{re.escape(var)}:\s*(\d+)", output, re.IGNORECASE)
            if match:
                version = int(match.group(1))
                log.info(f"Device ARB version (fastboot getvar): {version}")
                return ArbInfo(version=version, source=f"fastboot getvar {var}"), "getvar"

    return ArbInfo(version=None, source="could not read ARB from device"), "failed"


# ---------------------------------------------------------------------------
# Comparison
# ---------------------------------------------------------------------------

@dataclass
class ArbCheckResult:
    firmware_arb: ArbInfo
    device_arb: ArbInfo
    safe: bool
    warning: str = ""
    detail: str = ""


def compare_arb_versions(firmware_arb: ArbInfo, device_arb: ArbInfo) -> ArbCheckResult:
    fw  = firmware_arb.version
    dev = device_arb.version

    if fw == 0:
        return ArbCheckResult(
            firmware_arb=firmware_arb, device_arb=device_arb, safe=True,
            detail=(
                "This firmware has ARB = 0 (hard ARB not enforced).\n"
                "  However, if your device has previously had a firmware with ARB > 0 installed,\n"
                "  the bootloader fuse may already be set — flashing this could still be risky.\n"
                "  Check your current firmware version before proceeding."
            ),
        )

    if fw is None or dev is None:
        unknown = []
        if fw  is None: unknown.append("firmware")
        if dev is None: unknown.append("device")
        return ArbCheckResult(
            firmware_arb=firmware_arb, device_arb=device_arb, safe=False,
            warning=f"Could not determine ARB version for: {', '.join(unknown)}.",
            detail="Proceed only if you are sure the firmware is not a downgrade.",
        )

    if fw == dev:
        return ArbCheckResult(
            firmware_arb=firmware_arb, device_arb=device_arb, safe=True,
            detail=f"ARB versions match ({fw}). Safe to flash.",
        )

    if fw > dev:
        return ArbCheckResult(
            firmware_arb=firmware_arb, device_arb=device_arb, safe=False,
            warning=(
                f"Firmware ARB ({fw}) is HIGHER than device ARB ({dev}).\n"
                f"  After flashing, rolling back to any firmware with ARB < {fw} "
                f"will be IMPOSSIBLE."
            ),
            detail="You can still flash, but downgrading afterwards will not be possible.",
        )

    return ArbCheckResult(
        firmware_arb=firmware_arb, device_arb=device_arb, safe=False,
        warning=(
            f"DANGER: Firmware ARB ({fw}) is LOWER than device ARB ({dev}).\n"
            f"  Flashing this firmware WILL BRICK the device.\n"
            f"  The bootloader fuse is already at {dev} and cannot be lowered."
        ),
        detail="Do NOT flash unless you fully understand the consequences.",
    )


# ---------------------------------------------------------------------------
# Interactive confirmation gate
# ---------------------------------------------------------------------------

def arb_confirmation_gate(result: ArbCheckResult, device_method: str = "") -> bool:
    import sys
    tty = sys.stdout.isatty()
    def c(code): return code if tty else ""
    R      = c("[0m");  BOLD  = c("[1m")
    GREEN  = c("[38;5;78m");  RED   = c("[38;5;203m")
    YELLOW = c("[38;5;220m"); GRAY  = c("[38;5;244m")
    CYAN   = c("[38;5;117m"); ORANGE = c("[38;5;208m")

    fw  = result.firmware_arb
    dev = result.device_arb

    print()
    print(f"{GRAY}── ARB (Anti-Rollback) check {'─' * 33}{R}")

    fw_ver = str(fw.version) if fw.version is not None else f"{YELLOW}unknown{R}"
    print(f"  {GRAY}Firmware :{R}  ARB {BOLD}{fw_ver}{R}  {GRAY}({fw.source}){R}")

    if device_method != "none":
        method_label = {
            "dump":   f"{GRAY}(dumped xbl_config from device){R}",
            "getvar": f"{GRAY}(fastboot getvar){R}",
            "failed": f"{YELLOW}(could not read from device){R}",
        }.get(device_method, "")
        dev_ver = str(dev.version) if dev.version is not None else f"{YELLOW}unknown{R}"
        print(f"  {GRAY}Device   :{R}  ARB {BOLD}{dev_ver}{R}  {method_label}")

    if result.safe:
        if fw.version == 0:
            # ARB=0 is safe but worth a warning
            print()
            for line in result.detail.splitlines():
                print(f"  {YELLOW}{line.strip()}{R}")
            print(f"{GRAY}{'─' * 60}{R}")
            print()
            answer = input(f"  {BOLD}Understood, continue? (yes / no):{R} ").strip().lower()
            return answer == "yes"
        print(f"  {GREEN}✓{R}  {result.detail}")
        print(f"{GRAY}{'─' * 60}{R}")
        return True

    print()
    for line in result.warning.splitlines():
        print(f"  {ORANGE if 'HIGHER' in line else RED}{line}{R}")
    if result.detail:
        print(f"  {GRAY}{result.detail}{R}")
    print(f"{GRAY}{'─' * 60}{R}")
    print()
    answer = input(f"  {BOLD}Type YES to proceed anyway, anything else to abort:{R} ").strip()
    return answer == "YES"
