"""
main.py — LibreFastbootFirmwareFlasher
Entry point that ties together the extractor, flasher, and device diagnostics.

Usage:
    python main.py extract <firmware.zip>
    python main.py flash   <firmware_dir>
    python main.py devices
"""

import sys
import shutil
import logging
import argparse
from pathlib import Path

try:
    import argcomplete
    _ARGCOMPLETE = True
except ImportError:
    _ARGCOMPLETE = False

# ---------------------------------------------------------------------------
# Logging setup
# ---------------------------------------------------------------------------

def _setup_logging(verbose: bool) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(levelname)s: %(message)s",
    )


# ---------------------------------------------------------------------------
# Dependency check
# ---------------------------------------------------------------------------

_REQUIRED_TOOLS = {
    "fastboot":          "android-tools (apt / brew / pacman)",
    "payload-dumper-go": "https://github.com/ssut/payload-dumper-go",
}


def _check_tools(required: list[str]) -> bool:
    """
    Verify that required external binaries are available in $PATH.
    Prints a table of results and returns False if anything is missing.
    """
    missing = False
    for tool in required:
        found = shutil.which(tool) is not None
        status = "✓" if found else "✗"
        hint   = "" if found else f"  →  install via {_REQUIRED_TOOLS.get(tool, 'unknown')}"
        print(f"  {status}  {tool}{hint}")
        if not found:
            missing = True
    return not missing


# ---------------------------------------------------------------------------
# Subcommand: devices
# ---------------------------------------------------------------------------

def cmd_devices(args: argparse.Namespace) -> int:
    """
    Detect connected devices and run pre-flash diagnostics.
    Useful for verifying the setup before extracting or flashing.
    """
    from flasher.device import (
        list_fastboot_devices,
        list_adb_devices,
        run_pre_flash_checks,
    )

    print("\n── Connected devices ────────────────────────────────────")

    fastboot_serials = list_fastboot_devices()
    adb_serials      = list_adb_devices()

    if not fastboot_serials and not adb_serials:
        print("  No devices found via fastboot or adb.")
        print("  Make sure USB debugging or fastboot mode is enabled.")
        print("────────────────────────────────────────────────────────\n")
        return 1

    for s in fastboot_serials:
        print(f"  fastboot : {s}")
    for s in adb_serials:
        print(f"  adb      : {s}")
    print("────────────────────────────────────────────────────────\n")

    if args.check:
        print("Running pre-flash checks …\n")
        from flasher.device import _print_check_report
        check = run_pre_flash_checks(serial=args.serial)
        _print_check_report(check)
        return 0 if check.ready else 1

    return 0


# ---------------------------------------------------------------------------
# Subcommand: extract
# ---------------------------------------------------------------------------

def cmd_extract(args: argparse.Namespace) -> int:
    """
    Extract a firmware .zip archive into a grouped directory structure.
    Runs ARB check and prints payload_properties.txt summary at the end.
    """
    from flasher.extractor import extract_firmware, _ask_output_dir, _print_result

    zip_path: Path = args.zip.resolve()

    if not zip_path.exists():
        print(f"✗ File not found: {zip_path}")
        return 1

    print("\n── Dependency check ─────────────────────────────────────")
    _check_tools(["payload-dumper-go"])
    print("────────────────────────────────────────────────────────\n")

    output_dir: Path = args.output if args.output else _ask_output_dir(zip_path)
    partitions = args.partitions.split(",") if args.partitions else None

    result = extract_firmware(
        zip_path=zip_path,
        output_dir=output_dir,
        expected_checksum=args.checksum,
        partitions=partitions,
    )

    _print_result(result)
    return 0 if result.success else 1


# ---------------------------------------------------------------------------
# Subcommand: flash
# ---------------------------------------------------------------------------

def cmd_flash(args: argparse.Namespace) -> int:
    """
    Flash an extracted firmware directory to a connected device.

    Expects the directory layout produced by the extract subcommand:
        firmware_dir/
            bootloader/   *.img
            radio/        *.img
            system/       *.img
            vendor/       *.img
            other/        *.img
    """
    from flasher.flasher import run_flash_session, _print_summary

    firmware_dir: Path = args.firmware_dir.resolve()

    if not firmware_dir.is_dir():
        print(f"✗ Not a directory: {firmware_dir}")
        return 1

    print("\n── Dependency check ─────────────────────────────────────")
    ok = _check_tools(["fastboot"])
    print("────────────────────────────────────────────────────────\n")
    if not ok:
        print("✗ fastboot is required for flashing. Aborting.")
        return 1

    session = run_flash_session(
        firmware_dir=firmware_dir,
        serial=args.serial,
        dry_run=args.dry_run,
    )

    _print_summary(session)
    return 0 if not session.critical_failed else 1


def cmd_flash_partition(args: argparse.Namespace) -> int:
    """Flash a single .img file to a specific partition."""
    from flasher.flasher import run_flash_single, _collect_images

    # Resolve image path — either direct file or lookup by name in firmware-dir
    image_path: Path | None = None

    if args.image:
        p = Path(args.image)
        if p.suffix.lower() == ".img" and p.exists():
            # Direct path to .img file
            image_path = p.resolve()
        else:
            # Treat as partition name, look up in firmware-dir
            part_name_arg = p.name.lower().removesuffix(".img")
            if args.firmware_dir:
                images = _collect_images(Path(args.firmware_dir))
                image_path = images.get(part_name_arg)
                if image_path is None:
                    available = sorted(images.keys())
                    print(f"✗ Partition '{part_name_arg}' not found in {args.firmware_dir}")
                    print(f"  Available: {', '.join(available)}")
                    return 1
            else:
                print(f"✗ '{args.image}' is not a .img file and --firmware-dir is not set.")
                print(f"  Use: lfff flash-partition <name> --firmware-dir <dir>")
                print(f"    or: lfff flash-partition <path/to/file.img>")
                return 1
    elif args.firmware_dir:
        print("✗ Specify a partition name or image path.")
        print(f"  Example: lfff flash-partition boot --firmware-dir {args.firmware_dir}")
        return 1
    else:
        print("✗ Provide an image path or partition name with --firmware-dir.")
        return 1

    # Parse slots
    if args.slot:
        slots = [s.strip().lower() for s in args.slot.split(",")]
    elif args.no_ab:
        slots = [""]
    else:
        slots = None  # default: both a and b

    session = run_flash_single(
        image_path=image_path,
        partition=args.partition or None,
        slots=slots,
        serial=args.serial,
        dry_run=args.dry_run,
    )

    return 0 if not session.critical_failed else 1


# ---------------------------------------------------------------------------
# Subcommand: list
# ---------------------------------------------------------------------------

def cmd_list(args: argparse.Namespace) -> int:
    """List contents of a firmware .zip without extracting."""
    from flasher.extractor import _cmd_list
    _cmd_list(args.zip)
    return 0


# ---------------------------------------------------------------------------
# Subcommand: arb
# ---------------------------------------------------------------------------

def cmd_arb(args: argparse.Namespace) -> int:
    """
    Standalone ARB version check.

    Can compare a firmware directory against a connected device,
    or just print the ARB version embedded in a given xbl.img.
    """
    from flasher.arb import (
        find_xbl_image,
        extract_arb_from_xbl,
        get_device_arb_version,
        compare_arb_versions,
        arb_confirmation_gate,
    )

    # Resolve xbl source
    if args.xbl:
        xbl_path = Path(args.xbl).resolve()
    elif args.firmware_dir:
        xbl_path = find_xbl_image(Path(args.firmware_dir).resolve())
        if xbl_path is None:
            print("✗ xbl.img not found in the given firmware directory.")
            return 1
    else:
        print("✗ Provide either --xbl <path> or --firmware-dir <dir>.")
        return 1

    firmware_arb = extract_arb_from_xbl(xbl_path)
    print(f"\n  Firmware  : {firmware_arb}")

    if args.device:
        device_arb = get_device_arb_version(serial=args.serial)
        print(f"  Device    : {device_arb}")
        result = compare_arb_versions(firmware_arb, device_arb)
        arb_confirmation_gate(result)
    else:
        if firmware_arb.enforced:
            print("  ⚠  Hard ARB is ACTIVE on this firmware.")
        else:
            print("  ✓  Hard ARB is not enforced (version = 0).")

    return 0


def cmd_deps(args: argparse.Namespace) -> int:
    """Check and install missing system dependencies."""
    from flasher.deps import install_dependencies, MANAGED_TOOLS

    tools = args.tools if args.tools else None
    report = install_dependencies(tools=tools, dry_run=args.check)

    return 0 if report.all_ok else 1


def cmd_download(args: argparse.Namespace) -> int:
    """
    Resolve an OTA link and download the firmware via aria2c.

    Handles 4PDA redirect links automatically.
    """
    from flasher.downloader import download_firmware

    output_dir = Path(args.output) if args.output else None

    print("\n── Firmware download ────────────────────────────────────")
    result = download_firmware(
        url=args.url,
        output_dir=output_dir,
        connections=args.connections,
    )

    if not result.success:
        print(f"\n✗ Download failed: {result.error}")
        return 1

    print("\n✓ Download complete.")
    if result.output_path:
        print(f"  Saved to: {result.output_path}")
        print(f"\n  Next step:")
        print(f"    lfff extract \"{result.output_path}\"")
    print("────────────────────────────────────────────────────────")
    return 0


# ---------------------------------------------------------------------------
# Argument parser
# ---------------------------------------------------------------------------

def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="lfff",
        description="LibreFastbootFirmwareFlasher — extract, check, and flash Android firmware",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""\
── deps ─────────────────────────────────────────────────
  %(prog)s deps                       Install all missing dependencies
  %(prog)s deps --check               Check only, do not install
  %(prog)s deps payload-dumper-go     Install only payload-dumper-go
  %(prog)s deps fastboot adb          Install only fastboot and adb

── download ─────────────────────────────────────────────
  %(prog)s download <url>             Download firmware (resolves OTA link)
  %(prog)s download <url> -o ~/dl     Save to a specific directory
  %(prog)s download <url> -c 8        Use 8 parallel connections (default: 16)
  %(prog)s download "https://4pda.to/redirector/?u=..."
                                      4PDA redirect links are unwrapped automatically

── extract ──────────────────────────────────────────────
  %(prog)s extract firmware.zip       Extract (prompts for output dir)
  %(prog)s extract firmware.zip -o ./out/oos15
                                      Extract to a specific directory
  %(prog)s extract firmware.zip --list
                                      List archive contents without extracting
  %(prog)s extract firmware.zip -p boot,vendor_boot
                                      Extract only specific partitions
  %(prog)s extract firmware.zip --checksum <sha256>
                                      Verify archive integrity before extracting

── devices ──────────────────────────────────────────────
  %(prog)s devices                    List all connected ADB/fastboot devices
  %(prog)s devices --check            Full pre-flash diagnostics (cable, battery, unlock)
  %(prog)s devices --check -s R5CT20  Diagnostics for a specific device

── arb ──────────────────────────────────────────────────
  %(prog)s arb --firmware-dir ./out/oos15
                                      Show ARB version embedded in firmware
  %(prog)s arb --xbl ./out/oos15/critical/xbl_config.img
                                      Show ARB from a specific xbl_config.img
  %(prog)s arb --firmware-dir ./out/oos15 --device
                                      Compare firmware ARB vs device fused ARB
  %(prog)s arb --firmware-dir ./out/oos15 --device -s R5CT20
                                      Same, targeting a specific device

── flash ────────────────────────────────────────────────
  %(prog)s flash ./out/oos15          Flash extracted firmware
  %(prog)s flash ./out/oos15 --dry-run
                                      Simulate flash without writing anything
  %(prog)s flash ./out/oos15 -s R5CT20
                                      Flash a specific device (multi-device setup)

── flash-partition ──────────────────────────────────────
  %(prog)s flash-partition boot --firmware-dir ./firmwares/oos15
                                      Find boot.img in firmware dir, flash both slots
  %(prog)s flash-partition boot --firmware-dir ./firmwares/oos15 --slot a
                                      Flash boot to slot a only
  %(prog)s flash-partition vbmeta --firmware-dir ./firmwares/oos15 --no-ab
                                      Flash non-A/B partition (no slot suffix)
  %(prog)s flash-partition ./firmwares/oos15/critical/boot.img
                                      Flash directly from a .img file path
  %(prog)s flash-partition boot --firmware-dir ./firmwares/oos15 --dry-run
                                      Show what would be flashed

── typical full workflow ────────────────────────────────
  %(prog)s deps
  %(prog)s download "https://..." -o ~/Downloads
  %(prog)s extract ~/Downloads/firmware.zip
  %(prog)s devices --check
  %(prog)s flash ~/firmwares/RMX3709_11.H.38
        """,
    )

    parser.add_argument(
        "-v", "--verbose",
        action="store_true",
        help="Enable debug logging",
    )

    sub = parser.add_subparsers(dest="command", metavar="command")
    sub.required = False

    # --- devices ---
    p_devices = sub.add_parser("devices", help="List connected devices")
    p_devices.add_argument(
        "--check",
        action="store_true",
        help="Run full pre-flash diagnostics on the first found device",
    )
    p_devices.add_argument(
        "-s", "--serial",
        default=None,
        metavar="SERIAL",
        help="Target a specific device",
    )
    p_devices.set_defaults(func=cmd_devices)

    # --- extract ---
    p_extract = sub.add_parser("extract", help="Extract a firmware .zip archive")
    p_extract.add_argument("zip", type=Path, help="Path to the firmware .zip")
    p_extract.add_argument(
        "-o", "--output",
        type=Path,
        default=None,
        metavar="DIR",
        help="Output directory (prompted if not given)",
    )
    p_extract.add_argument(
        "-p", "--partitions",
        type=str,
        default=None,
        metavar="LIST",
        help="Comma-separated partitions to extract, e.g. boot,vendor_boot",
    )
    p_extract.add_argument(
        "--checksum",
        type=str,
        default=None,
        metavar="SHA256",
        help="Expected SHA-256 checksum of the archive",
    )
    p_extract.add_argument(
        "--list",
        action="store_true",
        help="List archive contents without extracting",
    )
    p_extract.set_defaults(func=lambda a: cmd_list(a) if a.list else cmd_extract(a))

    # --- flash ---
    p_flash = sub.add_parser("flash", help="Flash an extracted firmware directory")
    p_flash.add_argument(
        "firmware_dir",
        type=Path,
        help="Path to the extracted firmware directory",
    )
    p_flash.add_argument(
        "-s", "--serial",
        default=None,
        metavar="SERIAL",
        help="Target a specific device by serial number",
    )
    p_flash.add_argument(
        "--dry-run",
        action="store_true",
        help="Detect images and run checks without flashing",
    )
    p_flash.set_defaults(func=cmd_flash)

    # --- flash-partition ---
    p_fp = sub.add_parser(
        "flash-partition",
        help="Flash a single .img file to a specific partition",
    )
    p_fp.add_argument(
        "image",
        nargs="?",
        default=None,
        help="Path to .img file, or partition name (requires --firmware-dir)",
    )
    p_fp.add_argument(
        "--firmware-dir",
        type=Path,
        default=None,
        metavar="DIR",
        help="Extracted firmware directory to search for the partition image",
    )
    p_fp.add_argument(
        "-p", "--partition",
        default=None,
        metavar="NAME",
        help="Partition name override (default: image filename stem)",
    )
    p_fp.add_argument(
        "--slot",
        default=None,
        metavar="SLOT",
        help="Slot(s) to flash: a, b, or a,b (default: both a and b)",
    )
    p_fp.add_argument(
        "--no-ab",
        action="store_true",
        help="Flash without slot suffix (for non-A/B partitions like splash, vbmeta)",
    )
    p_fp.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be flashed without actually flashing",
    )
    p_fp.add_argument(
        "-s", "--serial",
        default=None,
        metavar="SERIAL",
        help="Target a specific device",
    )
    p_fp.set_defaults(func=cmd_flash_partition)

    # --- arb ---
    p_arb = sub.add_parser("arb", help="Check Anti-Rollback version of a firmware")
    arb_src = p_arb.add_mutually_exclusive_group(required=True)
    arb_src.add_argument(
        "--xbl",
        type=Path,
        metavar="XBL_IMG",
        help="Direct path to xbl.img",
    )
    arb_src.add_argument(
        "--firmware-dir",
        type=Path,
        metavar="DIR",
        help="Extracted firmware directory (xbl.img will be located automatically)",
    )
    p_arb.add_argument(
        "--device",
        action="store_true",
        help="Also read ARB version from the connected device and compare",
    )
    p_arb.add_argument(
        "-s", "--serial",
        default=None,
        metavar="SERIAL",
        help="Target a specific device by serial number",
    )
    p_arb.set_defaults(func=cmd_arb)

    # --- download ---
    p_dl = sub.add_parser("download", help="Download firmware via OTA link (supports 4PDA redirects)")
    p_dl.add_argument("url", help="OTA download link or 4PDA redirect URL")
    p_dl.add_argument(
        "-o", "--output",
        default=None,
        metavar="DIR",
        help="Directory to save the firmware (default: current directory)",
    )
    p_dl.add_argument(
        "-c", "--connections",
        type=int,
        default=16,
        metavar="N",
        help="Number of parallel connections for aria2c (default: 16)",
    )
    p_dl.set_defaults(func=cmd_download)

    # --- deps ---
    p_deps = sub.add_parser("deps", help="Check and install required dependencies")
    p_deps.add_argument(
        "--check",
        action="store_true",
        help="Only check, do not install anything",
    )
    p_deps.add_argument(
        "tools",
        nargs="*",
        metavar="TOOL",
        help="Specific tools to install (default: all). Choices: fastboot adb aria2c payload-dumper-go",
    )
    p_deps.set_defaults(func=cmd_deps)

    return parser


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Terminal colours & welcome screen
# ---------------------------------------------------------------------------

_C = {
    "reset":  "\033[0m",
    "bold":   "\033[1m",
    "dim":    "\033[2m",
    "orange": "\033[38;5;208m",
    "amber":  "\033[38;5;214m",
    "white":  "\033[97m",
    "gray":   "\033[38;5;244m",
    "dkgray": "\033[38;5;238m",
    "green":  "\033[38;5;78m",
    "cyan":   "\033[38;5;117m",
}


def _col(key: str, text: str) -> str:
    """Wrap text in ANSI colour if stdout is a TTY."""
    import sys
    if not sys.stdout.isatty():
        return text
    return _C[key] + text + _C["reset"]


def _link(url: str, text: str) -> str:
    """OSC 8 clickable hyperlink (kitty, iTerm2, GNOME Terminal, WezTerm, etc.).
    Falls back to plain text when stdout is not a TTY."""
    import sys
    if not sys.stdout.isatty():
        return text
    return f"\033]8;;{url}\033\\{text}\033]8;;\033\\"


def _print_welcome() -> None:
    import shutil
    cols = shutil.get_terminal_size((80, 24)).columns

    def O(t):  return _col("orange", t)
    def A(t):  return _col("amber",  t)
    def W(t):  return _col("white",  t)
    def G(t):  return _col("gray",   t)
    def DG(t): return _col("dkgray", t)
    def B(t):  return _col("bold",   t)
    def CY(t): return _col("cyan",   t)
    def DM(t): return _col("dim",    t)
    def R(t):  return _col("red",    t)

    # Logo: each glyph column is (orange_part, amber_part) pair
    # Hand-crafted so all 4 glyphs sit flush on the same baseline
    logo = [
        O("██╗     ") + A("███████╗") + O("███████╗") + A("███████╗"),
        O("██║     ") + A("██╔════╝") + O("██╔════╝") + A("██╔════╝"),
        O("██║     ") + A("█████╗  ") + O("█████╗  ") + A("█████╗  "),
        O("██║     ") + A("██╔══╝  ") + O("██╔══╝  ") + A("██╔══╝  "),
        O("███████╗") + A("██║     ") + O("██║     ") + A("██║     "),
        O("╚══════╝") + A("╚═╝     ") + O("╚═╝     ") + A("╚═╝     "),
    ]

    w   = min(cols - 2, 64)
    sep = DG("─" * w)

    print()
    for line in logo:
        print(" " + line)
    print()
    print(f" {B(W('LibreFastbootFirmwareFlasher'))}  {DM(G('v0.1.0'))}")
    print(f" {DM(G('Flash Android firmware via fastboot — free, open, no bloat.'))}")
    print()

    def step(n, cmd, desc):
        cmd_s = CY("lfff " + cmd)
        # pad cmd to fixed visual width (ignoring escape codes)
        pad   = max(0, 36 - len("lfff " + cmd))
        return f" {O(n)}  {cmd_s}{' ' * pad}{DM(G(desc))}"

    def cmd_row(name, desc):
        pad = max(0, 18 - len(name))
        return f" {CY(name)}{' ' * pad}{G(desc)}"

    def link_row(icon, label, url):
        # Pad based on visible label length (escape codes don't count)
        COL = 22
        pad = " " * max(0, COL - len(label))
        linked_label = _link(url, W(label))
        dim_url      = DM(G(url))
        return f" {O(icon)}  {linked_label}{pad}  {dim_url}"

    print(f" {B(W('Quick start'))}")
    print(f" {sep}")
    print(step("1.", "deps",                   "install fastboot, aria2c, payload-dumper-go"))
    print(step("2.", "download <url>",          "download OTA zip (direct or 4PDA)"))
    print(step("3.", "extract firmware.zip",    "unpack partition images from zip"))
    print(step("4.", "devices --check",         "verify cable, battery, unlock status"))
    print(step("5.", "flash ./firmwares/<dir>", "flash all partitions to device"))
    print()

    print(f" {B(W('Commands'))}")
    print(f" {sep}")
    print(cmd_row("deps",            "install & verify external tools"))
    print(cmd_row("download",        "download OTA firmware zip"))
    print(cmd_row("extract",         "extract .zip into partition images"))
    print(cmd_row("devices",         "list devices, run pre-flash checks"))
    print(cmd_row("arb",             "compare Anti-Rollback version"))
    print(cmd_row("flash",           "flash full firmware (A/B, super partition)"))
    print(cmd_row("flash-partition", "flash a single partition by name"))
    print()

    print(f" {B(W('Links'))}")
    print(f" {sep}")
    print(link_row("⌥", "GitHub",             "https://github.com/mrFrok/LibreFastbootFirmwareFlasher"))
    print(link_row("✈", "Telegram group",     "https://t.me/gt3neo5hub"))
    print(link_row("◈", "Author (mrFrok)",    "https://t.me/mrFrok228"))
    print(link_row("◈", "NeFeroN", "https://t.me/NeFeroN"))
    print()

    print(f" {DM(G('-v / --verbose'))}  {DM(G('debug output'))}   "
          f"{DM(G('<command> --help'))}  {DM(G('command help'))}")
    print()
    print(f" {DM(G('OnePlus · OPPO · Realme · Qualcomm A/B · Dynamic partitions'))}")
    print()


def main() -> None:
    parser = _build_parser()
    if _ARGCOMPLETE:
        argcomplete.autocomplete(parser)
    args = parser.parse_args()

    # No subcommand given — show welcome screen
    if not hasattr(args, "func"):
        _print_welcome()
        sys.exit(0)

    _setup_logging(args.verbose)

    try:
        exit_code = args.func(args)
    except KeyboardInterrupt:
        print("\nInterrupted.")
        sys.exit(130)

    sys.exit(exit_code or 0)


if __name__ == "__main__":
    main()
