# THIS IS TEST BUILD. ONLY FOR TEST!

# LFFF — Libre Fastboot Firmware Flasher (Rust)

Free, open-source firmware flasher for Android A/B devices via fastboot.
Single static binary — no Python, no pip, no bloat.

**Supported devices:** OnePlus · OPPO · Realme (Qualcomm SoC, A/B partition layout)

## Install

### Prerequisites

- [Rust toolchain](https://rustup.rs) (for building LFFF and installing payload_dumper)
- `fastboot` and `adb` (android-tools)
- `aria2c` (for firmware downloads)
- `payload_dumper` (for OTA extraction)

### Build from source

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
cargo build --release
```

The binary will be at `target/release/lfff`. Copy it somewhere in your `$PATH`:

```bash
sudo cp target/release/lfff /usr/local/bin/
```

### Install dependencies

LFFF can install its own dependencies:

```bash
lfff deps
```

This will install `fastboot`, `adb`, `aria2c` via your system package manager and `payload_dumper` via `cargo install payload_dumper`.

To check what's missing without installing:

```bash
lfff deps --check
```

## Quick start

```
1.  lfff deps                        # install tools
2.  lfff download <url>              # download OTA zip
3.  lfff extract firmware.zip        # unpack partition images
4.  lfff devices --check             # verify cable, battery, unlock
5.  lfff flash ./firmwares/<dir>     # flash all partitions
```

## Commands

### `lfff deps`

Install and verify external dependencies.

```bash
lfff deps                        # install all missing dependencies
lfff deps --check                # check only, do not install
lfff deps fastboot adb           # install specific tools
```

### `lfff download <url>`

Download firmware via OTA link. Supports direct URLs and 4PDA redirect links. Uses `aria2c` with 16 parallel connections.

```bash
lfff download "https://..." -o ~/Downloads
lfff download "https://4pda.to/redirector/?u=..." -c 8
```

### `lfff extract <firmware.zip>`

Extract a firmware archive into grouped partition images. Automatically detects the format:
- `payload.bin` inside ZIP → extracted via `payload_dumper` (ZIP passed directly, no unzipping needed)
- raw `.img` files → extracted directly

Images are organized into subdirectories: `critical/`, `bootloader/`, `radio/`, `system/`, `vendor/`, `other/`.

```bash
lfff extract firmware.zip                          # interactive output dir prompt
lfff extract firmware.zip -o ./out/oos15           # specify output directory
lfff extract firmware.zip -p boot,vendor_boot      # extract specific partitions only
lfff extract firmware.zip --list                   # list contents without extracting
lfff extract firmware.zip --checksum <sha256>      # verify integrity first
```

### `lfff devices`

List connected devices and run pre-flash diagnostics.

```bash
lfff devices                     # list fastboot and adb devices
lfff devices --check             # full diagnostics: cable speed, battery, unlock
lfff devices --check -s R5CT20   # target a specific device
```

Pre-flash checks include:
- Device detection (fastboot / adb)
- Communication test (`fastboot getvar all`)
- USB cable speed test (`fastboot stage`)
- Battery level check (minimum 30%)
- Bootloader unlock status

### `lfff arb`

Check Anti-Rollback (ARB) version embedded in firmware. Parses `xbl_config.img` ELF64 OEM metadata directly — same algorithm as [arbextract](https://github.com/koaaN/arbextract).

```bash
lfff arb --firmware-dir ./out/oos15                      # show firmware ARB
lfff arb --xbl ./out/oos15/critical/xbl_config.img       # from specific file
lfff arb --firmware-dir ./out/oos15 --device              # compare with device
lfff arb --firmware-dir ./out/oos15 --device -s R5CT20   # specific device
```

ARB levels:
- `ARB = 0` — hard ARB not enforced (safe)
- `ARB > 0` — hard ARB active; flashing a lower version **will brick the device**

### `lfff flash <firmware_dir>`

Flash an extracted firmware directory to a connected device.

```bash
lfff flash ./firmwares/oos15                # full flash
lfff flash ./firmwares/oos15 --dry-run      # simulate without writing
lfff flash ./firmwares/oos15 -s R5CT20      # target specific device
```

Flash stages:
1. **Pre-flash checks** — device, cable, battery, unlock
2. **ARB check** — firmware-only, warns about rollback risk
3. **Stage 1 (fastbootd)** — non-super partitions (both slots) + super/dynamic partitions (active slot only)
4. **Stage 2 (bootloader)** — modem (both slots)
5. **Summary** — wipe offer, reboot to system

On failure: interactive retry / reboot / abort menu with cause diagnosis.

### `lfff flash-partition`

Flash a single partition image.

```bash
lfff flash-partition boot --firmware-dir ./firmwares/oos15
lfff flash-partition boot --firmware-dir ./firmwares/oos15 --slot a
lfff flash-partition vbmeta --firmware-dir ./firmwares/oos15 --no-ab
lfff flash-partition ./firmwares/oos15/critical/boot.img
lfff flash-partition boot --firmware-dir ./firmwares/oos15 --dry-run
```

## Project structure

```
lfff-rust/
├── Cargo.toml
└── src/
    ├── lib.rs           # library crate root (for future GUI)
    ├── main.rs          # CLI entry point (clap)
    ├── utils.rs         # subprocess helpers, sha256, tool checks
    ├── arb.rs           # Anti-Rollback ELF64 parser
    ├── device.rs        # device discovery, pre-flash checks
    ├── extractor.rs     # firmware extraction, partition grouping
    ├── downloader.rs    # OTA download via aria2c
    ├── flasher.rs       # flash orchestrator, A/B slots, progress bar
    └── deps.rs          # dependency installer
```

Split into library + binary crates to support a potential Qt GUI in the future.

## Dependencies (Cargo)

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing (derive) |
| `anyhow` / `thiserror` | Error handling |
| `log` / `env_logger` | Logging |
| `sha2` | File checksum verification |
| `colored` | Terminal colors |
| `regex` | Parsing fastboot output |
| `which` | Finding binaries in $PATH |
| `zip` | Reading firmware archives |
| `url` | Parsing OTA/CDN URLs |
| `indicatif` | Progress bars |

## External tools

| Tool | Installed by | Purpose |
|------|-------------|---------|
| `fastboot` | `lfff deps` (system pkg manager) | Flash partitions, device communication |
| `adb` | `lfff deps` (system pkg manager) | Device detection, reboot commands |
| `aria2c` | `lfff deps` (system pkg manager) | Multi-connection firmware download |
| `payload_dumper` | `lfff deps` (`cargo install`) | OTA payload extraction |
| `curl` | Pre-installed on most systems | CDN URL resolution for downloads |

## Links

- **GitHub:** https://github.com/mrFrok/LibreFastbootFirmwareFlasher
- **Telegram group:** https://t.me/gt3neo5hub
- **Author:** https://t.me/mrFrok228

## License

GPL-3.0
