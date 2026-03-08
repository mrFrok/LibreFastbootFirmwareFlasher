import os
import sys
import shutil
import zipfile
import logging
import argparse
import tempfile
import subprocess
from pathlib import Path
from dataclasses import dataclass, field

from flasher.utils import compute_checksum, verify_checksum, check_tools
from flasher.arb import (
    ArbInfo,
    extract_arb_from_xbl,
    find_xbl_image,
    arb_confirmation_gate,
    compare_arb_versions,
    ArbCheckResult,
)

logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")
log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Partition grouping map (OnePlus / Qualcomm-based devices)
# Keys are target directory names, values are partition name substrings.
# Matched left-to-right — first match wins.
# ---------------------------------------------------------------------------
PARTITION_GROUPS: dict[str, list[str]] = {
    # Critical partitions - flashing these incorrectly can brick the device.
    # Listed first so they match before the broader bootloader/radio/vendor groups.
    "critical": [
        # Primary Qualcomm bootloader chain
        "abl", "xbl", "xbl_config", "xbl_ramdump",
        "aop", "aop_config", "devcfg", "shrm",
        "tz", "hyp", "multiimgoem", "multiimgqti",
        "qupfw", "uefisecapp", "imagefv", "cpucp",
        # Boot images
        "boot", "init_boot", "vendor_boot",
        # Modem
        "modem",
    ],
    "bootloader": [
        "featenabler", "logfs", "storsec",
        "recovery",
    ],
    "radio": [
        "bluetooth", "dsp", "wifi",
    ],
    "system": [
        "system", "system_ext", "system_dlkm",
        "product", "odm", "odm_dlkm",
    ],
    "vendor": [
        "vendor", "vendor_dlkm",
    ],
}

# Partitions that don't fit any group land here
FALLBACK_GROUP = "other"


def _resolve_group(partition_name: str) -> str:
    """Return the group directory name for a given partition stem."""
    name = partition_name.lower()
    for group, patterns in PARTITION_GROUPS.items():
        if any(name == p or name.startswith(p) for p in patterns):
            return group
    return FALLBACK_GROUP




# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

@dataclass
class ExtractionResult:
    success: bool
    output_dir: Path
    # Mapping of group name -> list of extracted image paths
    groups: dict[str, list[Path]] = field(default_factory=dict)
    error: str = ""
    # Parsed key/value pairs from payload_properties.txt (if present)
    payload_properties: dict[str, str] = field(default_factory=dict)
    # ARB version parsed from xbl.img after extraction
    arb_info: ArbInfo | None = None

    @property
    def all_images(self) -> list[Path]:
        return [img for imgs in self.groups.values() for img in imgs]


def _parse_payload_properties(data: str) -> dict[str, str]:
    """
    Parse payload_properties.txt into a key/value dict.

    Format is one  KEY=VALUE  pair per line, comments start with #.
    """
    props: dict[str, str] = {}
    for line in data.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" in line:
            key, _, value = line.partition("=")
            props[key.strip()] = value.strip()
    return props


def _extract_payload_properties(zf: zipfile.ZipFile) -> dict[str, str]:
    """Read and parse payload_properties.txt from a zip archive if present."""
    if "payload_properties.txt" not in zf.namelist():
        return {}
    try:
        raw = zf.read("payload_properties.txt").decode("utf-8", errors="replace")
        return _parse_payload_properties(raw)
    except Exception as exc:
        log.warning(f"Could not read payload_properties.txt: {exc}")
        return {}


def _run_arb_check(output_dir: Path) -> ArbInfo | None:
    """
    Locate xbl.img in output_dir and extract its ARB version.

    Returns None if xbl.img is not present (e.g. partial extraction).
    """
    xbl = find_xbl_image(output_dir)
    if xbl is None:
        log.warning("xbl.img not found — skipping ARB version check")
        return None
    return extract_arb_from_xbl(xbl)


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _move_into_groups(images: list[Path], base_dir: Path) -> dict[str, list[Path]]:
    """
    Move extracted .img files into group subdirectories under base_dir.

    Returns a dict mapping group name -> list of final absolute paths.
    """
    groups: dict[str, list[Path]] = {}

    for img in images:
        group = _resolve_group(img.stem)
        dest_dir = base_dir / group
        dest_dir.mkdir(parents=True, exist_ok=True)

        dest = dest_dir / img.name
        if img.resolve() != dest.resolve():
            shutil.move(str(img), dest)

        groups.setdefault(group, []).append(dest)

    return groups


def _run_payload_dumper(
    payload_path: Path,
    output_dir: Path,
    partitions: list[str] | None,
) -> bool:
    """
    Invoke payload-dumper-go and forward its output to the logger.

    Args:
        payload_path: absolute path to payload.bin
        output_dir:   staging directory for raw extracted images
        partitions:   optional partition filter (None = extract all)
    """
    if not shutil.which("payload-dumper-go"):
        log.error("payload-dumper-go not found in $PATH")
        return False

    cmd = ["payload-dumper-go", "-o", str(output_dir)]
    if partitions:
        cmd += ["-p", ",".join(partitions)]
    cmd.append(str(payload_path))

    log.debug(f"Running: {' '.join(cmd)}")
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert proc.stdout
    for line in proc.stdout:
        line = line.rstrip()
        if line:
            print(f"  {line}")
    proc.wait()
    return proc.returncode == 0


# ---------------------------------------------------------------------------
# Format handlers
# ---------------------------------------------------------------------------

def _handle_payload_zip(
    zf: zipfile.ZipFile,
    output_dir: Path,
    partitions: list[str] | None,
) -> ExtractionResult:
    """
    Handle archives that contain payload.bin.

    Extracts payload.bin to a temporary directory, runs payload-dumper-go
    into a staging folder, then reorganises images into groups.
    Also parses payload_properties.txt and runs ARB check on xbl.img.
    """
    # Grab payload_properties.txt before we start extracting images
    payload_props = _extract_payload_properties(zf)

    staging = output_dir / "_staging"
    staging.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="fflaher_") as tmp:
        payload_tmp = Path(tmp) / "payload.bin"

        log.info("Extracting payload.bin to temporary directory …")
        with zf.open("payload.bin") as src, open(payload_tmp, "wb") as dst:
            shutil.copyfileobj(src, dst)

        ok = _run_payload_dumper(payload_tmp, staging, partitions)

    if not ok:
        return ExtractionResult(
            success=False,
            output_dir=output_dir,
            error="payload-dumper-go exited with an error",
        )

    raw_images = sorted(staging.rglob("*.img"))
    groups = _move_into_groups(raw_images, output_dir)

    # Clean up empty staging dir
    try:
        staging.rmdir()
    except OSError:
        pass

    arb_info = _run_arb_check(output_dir)

    return ExtractionResult(
        success=True,
        output_dir=output_dir,
        groups=groups,
        payload_properties=payload_props,
        arb_info=arb_info,
    )


def _handle_image_zip(zf: zipfile.ZipFile, output_dir: Path) -> ExtractionResult:
    """
    Handle classic factory archives that ship raw .img files directly.

    Extracts images to a staging folder, then reorganises them into groups.
    """
    staging = output_dir / "_staging"
    staging.mkdir(parents=True, exist_ok=True)

    extracted: list[Path] = []
    for member in zf.namelist():
        if not member.endswith(".img"):
            continue
        dest = staging / Path(member).name
        with zf.open(member) as src, open(dest, "wb") as dst:
            shutil.copyfileobj(src, dst)
        extracted.append(dest)
        log.info(f"  Extracted: {dest.name}")

    groups = _move_into_groups(extracted, output_dir)

    try:
        staging.rmdir()
    except OSError:
        pass

    arb_info = _run_arb_check(output_dir)

    return ExtractionResult(
        success=True,
        output_dir=output_dir,
        groups=groups,
        arb_info=arb_info,
    )


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def extract_firmware(
    zip_path: Path,
    output_dir: Path,
    expected_checksum: str | None = None,
    partitions: list[str] | None = None,
) -> ExtractionResult:
    """
    Extract an Android firmware archive into a grouped directory structure.

    Automatically detects the archive format:
      - payload.bin  ->  extracted via payload-dumper-go
      - raw .img     ->  extracted directly from the zip

    After extraction, images are organised into subdirectories by partition
    type: bootloader/, radio/, system/, vendor/, other/

    Args:
        zip_path:          Path to the firmware .zip archive.
        output_dir:        Destination directory chosen by the user.
        expected_checksum: Optional SHA-256 hex string for integrity check.
        partitions:        Optional list of partition names to extract (None = all).

    Returns:
        ExtractionResult dataclass.
    """
    zip_path = Path(zip_path).resolve()
    output_dir = Path(output_dir).resolve()

    if not zip_path.exists():
        return ExtractionResult(success=False, output_dir=output_dir,
                                error=f"File not found: {zip_path}")

    if not zipfile.is_zipfile(zip_path):
        return ExtractionResult(success=False, output_dir=output_dir,
                                error=f"Not a zip archive: {zip_path}")

    if expected_checksum:
        log.info("Verifying archive checksum ...")
        if not verify_checksum(zip_path, expected_checksum):
            return ExtractionResult(success=False, output_dir=output_dir,
                                    error="Checksum verification failed")

    # Disk space check
    ok, free_gb = _check_free_space(output_dir)
    if not ok:
        return ExtractionResult(
            success=False,
            output_dir=output_dir,
            error=(
                f"Not enough free disk space: {free_gb:.1f} GB available, "
                f"{FREE_SPACE_MIN_GB} GB required. Free up space and try again."
            ),
        )

    output_dir.mkdir(parents=True, exist_ok=True)
    log.info(f"Extracting {zip_path.name} → {output_dir}")

    with zipfile.ZipFile(zip_path, "r") as zf:
        members = zf.namelist()

        if "payload.bin" in members:
            log.info("Format detected: payload.bin (modern OTA / factory image)")
            return _handle_payload_zip(zf, output_dir, partitions)

        if any(m.endswith(".img") for m in members):
            log.info("Format detected: raw .img files")
            return _handle_image_zip(zf, output_dir)

        return ExtractionResult(
            success=False,
            output_dir=output_dir,
            error="Archive contains neither payload.bin nor .img files",
        )


# ---------------------------------------------------------------------------
# CLI helpers
# ---------------------------------------------------------------------------

FREE_SPACE_MIN_GB = 20


def _get_firmware_name(zip_path: Path) -> str:
    """
    Try to read a human-friendly firmware name from payload_properties.txt
    inside the archive. Falls back to the archive stem if not available.

    Prefers ota_target_version (e.g. RMX3709_11.H.38_3380_202602041244),
    then oplus_rom_version, then the zip filename stem.
    """
    try:
        with zipfile.ZipFile(zip_path, "r") as zf:
            props = _extract_payload_properties(zf)
    except Exception:
        return zip_path.stem

    for key in ("ota_target_version", "oplus_rom_version"):
        val = props.get(key, "").strip()
        if val:
            # Sanitise for use as a directory name
            return val.replace("/", "_").replace(" ", "_")

    return zip_path.stem


def _check_free_space(path: Path, required_gb: float = FREE_SPACE_MIN_GB) -> tuple[bool, float]:
    """Return (ok, free_gb) for the filesystem containing path."""
    check = Path(path)
    while not check.exists():
        check = check.parent
    stat = shutil.disk_usage(check)
    free_gb = stat.free / (1024 ** 3)
    return free_gb >= required_gb, free_gb


def _ask_output_dir(zip_path: Path) -> Path:
    """
    Prompt the user for an output directory.

    Default is ./firmwares/<firmware_name> relative to cwd, where firmware_name
    is read from ota_target_version in payload_properties.txt (or the zip stem).
    A custom name is also placed under ./firmwares/.
    """
    firmwares_dir = Path.cwd() / "firmwares"
    firmware_name = _get_firmware_name(zip_path)
    default = firmwares_dir / firmware_name

    print(f"  Firmware : {firmware_name}")
    raw = input(f"  Output directory [{default}]: ").strip()

    if not raw:
        return default
    p = Path(raw)
    if p == Path(p.name):
        return firmwares_dir / p.name
    return p


def _print_result(result: ExtractionResult) -> None:
    """Pretty-print the extraction summary including payload properties and ARB info."""
    if not result.success:
        print(f"\n✗ Extraction failed: {result.error}")
        return

    print(f"\n✓ Extracted to: {result.output_dir}")
    for group, images in sorted(result.groups.items()):
        print(f"\n  {group}/")
        for img in sorted(images):
            size_mb = img.stat().st_size / 1024 / 1024
            print(f"    {img.name:<45} {size_mb:>7.1f} MB")
    print(f"\n  Total: {len(result.all_images)} image(s)")

    # --- payload_properties.txt ---
    if result.payload_properties:
        print("\n── payload_properties.txt ───────────────────────────────")
        # Interesting keys shown first, rest sorted alphabetically
        priority_keys = [
            "FILE_HASH", "FILE_SIZE",
            "METADATA_HASH", "METADATA_SIZE",
            "CURRENT_ANTI_ROLLBACK",
        ]
        shown: set[str] = set()
        for key in priority_keys:
            if key in result.payload_properties:
                print(f"  {key:<35} {result.payload_properties[key]}")
                shown.add(key)
        for key, value in sorted(result.payload_properties.items()):
            if key not in shown:
                print(f"  {key:<35} {value}")
        print("────────────────────────────────────────────────────────")

    # --- ARB info ---
    if result.arb_info is not None:
        print("\n── Anti-Rollback (ARB) ──────────────────────────────────")
        print(f"  {result.arb_info}")
        if result.arb_info.oem_major is not None:
            print(f"  OEM Metadata Major : {result.arb_info.oem_major}")
            print(f"  OEM Metadata Minor : {result.arb_info.oem_minor}")
        if result.arb_info.enforced:
            print("  ⚠  Hard ARB is ACTIVE on this firmware.")
            print("     Flashing will permanently bump the device fuse.")
            print("     Rolling back to a lower ARB version will be impossible.")
        print("────────────────────────────────────────────────────────")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="extractor",
        description="Android firmware extractor — supports payload.bin and raw .img archives",
    )
    parser.add_argument(
        "zip",
        type=Path,
        help="Path to the firmware .zip archive",
    )
    parser.add_argument(
        "-o", "--output",
        type=Path,
        default=None,
        metavar="DIR",
        help="Output directory (prompted interactively if not given)",
    )
    parser.add_argument(
        "-p", "--partitions",
        type=str,
        default=None,
        metavar="LIST",
        help="Comma-separated partitions to extract, e.g. boot,vendor_boot",
    )
    parser.add_argument(
        "--checksum",
        type=str,
        default=None,
        metavar="SHA256",
        help="Expected SHA-256 checksum of the archive",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List archive contents without extracting",
    )
    return parser


def _cmd_list(zip_path: Path) -> None:
    """Print archive contents with file sizes."""
    if not zipfile.is_zipfile(zip_path):
        log.error("Not a zip archive")
        raise SystemExit(1)

    with zipfile.ZipFile(zip_path) as zf:
        print(f"\nContents of {zip_path.name}:")
        for name in sorted(zf.namelist()):
            size_mb = zf.getinfo(name).file_size / 1024 / 1024
            print(f"  {name:<55} {size_mb:>8.1f} MB")


def main() -> None:
    parser = _build_parser()
    args = parser.parse_args()

    if args.list:
        _cmd_list(args.zip)
        return

    # Warn about missing tools upfront
    deps = check_dependencies()
    missing = [t for t, ok in deps.items() if not ok]
    if missing:
        log.warning(f"Missing tools (install before flashing): {', '.join(missing)}")

    # Ask interactively only when -o was not passed
    output_dir: Path = args.output if args.output else _ask_output_dir(args.zip)

    partitions = args.partitions.split(",") if args.partitions else None

    result = extract_firmware(
        zip_path=args.zip,
        output_dir=output_dir,
        expected_checksum=args.checksum,
        partitions=partitions,
    )

    _print_result(result)

    if not result.success:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
