# LFFF — LibreFastbootFirmwareFlasher

```
 ██╗     ███████╗███████╗███████╗
 ██║     ██╔════╝██╔════╝██╔════╝
 ██║     █████╗  █████╗  █████╗
 ██║     ██╔══╝  ██╔══╝  ██╔══╝
 ███████╗██║     ██║     ██║
 ╚══════╝╚═╝     ╚═╝     ╚═╝
```

**Free, open-source firmware flasher for Android A/B devices via fastboot.**  
No proprietary tools, no Windows required, no telemetry.

Designed for **OnePlus / OPPO / Realme** devices on Qualcomm platforms.  
Supports dynamic (super) partitions, Anti-Rollback checks, and full A/B slot management.

> Windows version by [NeFeroN](https://t.me/NeFeroN)  
> Community group: [t.me/gt3neo5hub](https://t.me/gt3neo5hub)

---

## Features

- **Full firmware flash** — non-super and super (dynamic) partitions, modem, bootloader chain
- **A/B slot management** — non-super partitions flashed to both slots; super to active slot only
- **Anti-Rollback (ARB) protection** — reads ARB version from `xbl.img`, compares against device, warns before downgrade
- **Live progress bar** — single animated bar with elapsed time across the entire flash session
- **Interactive error recovery** — on failure: retry, reboot to correct mode and retry, or abort
- **Pre-flash checks** — device detection, cable speed test, battery level, bootloader unlock status
- **Single-partition flash** — flash any individual partition by name
- **Firmware extraction** — unpacks OTA `.zip` files via `payload-dumper-go`
- **Firmware download** — downloads OTA zips via `aria2c` with resume support
- **Dependency installer** — auto-downloads `fastboot`, `aria2c`, `payload-dumper-go`
- **Tab completion** — shell autocomplete via `argcomplete`
- Works on **Linux** and **macOS**

---

## Requirements

| Tool | Purpose |
|------|---------|
| `fastboot` | Flashing interface |
| `adb` | Device detection / reboot commands |
| `payload-dumper-go` | Extracting `.zip` OTA firmware |
| `aria2c` | Multi-connection firmware download |

Install all at once:
```bash
lfff deps
```

---

## Installation

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
chmod +x install.sh
./install.sh
```

The installer will:
1. Check system dependencies (`python3 ≥ 3.10`, `make`)
2. Build a standalone binary via `cx_Freeze`
3. Install the bundle to `/usr/local/lib/lfff/`
4. Create a launcher at `/usr/local/bin/lfff`

**Pre-built binary (skip build step):**
```bash
./install.sh --prebuilt
```

**Uninstall:**
```bash
./install.sh --uninstall
```

---

## Quick Start

```bash
# 1. Install external tools
lfff deps

# 2. Download firmware (optional — skip if you already have the zip)
lfff download https://...

# 3. Extract firmware zip
lfff extract CPH2653_11.F.85_2850_202601060236.zip

# 4. Check device connectivity, cable, battery, unlock
lfff devices --check

# 5. Flash
lfff flash ./firmwares/CPH2653_11.F.85_2850
```

---

## Commands

### `lfff flash <firmware_dir>`
Flash a full extracted firmware directory.

```bash
lfff flash ./firmwares/RMX3709_11.H.38
lfff flash ./firmwares/RMX3709_11.H.38 -s R5CT20   # target specific device
lfff flash ./firmwares/RMX3709_11.H.38 --dry-run   # preview without flashing
```

**Flash stages:**
1. Asks where the device currently is (system / bootloader / fastbootd)
2. **Stage 1 — fastbootd:** flashes all partitions except modem
   - Non-super partitions → both slots (a + b)
   - Super/dynamic partitions → active slot only (after wiping super)
3. **Stage 2 — bootloader:** flashes modem to both slots
4. Offers userdata wipe (`fastboot -w`) on full success
5. Reboots to system

---

### `lfff flash-partition <partition> [--firmware-dir DIR]`
Flash a single partition by name.

```bash
lfff flash-partition boot --firmware-dir ./firmwares/RMX3709
lfff flash-partition vendor_boot --firmware-dir ./firmwares/RMX3709 --slot a
lfff flash-partition modem --firmware-dir ./firmwares/RMX3709 --no-ab
```

---

### `lfff extract <zip> [-o DIR]`
Extract OTA firmware zip into individual `.img` files.

```bash
lfff extract firmware.zip
lfff extract firmware.zip -o ./firmwares/my_build
lfff extract firmware.zip --list              # list contents without extracting
lfff extract firmware.zip --checksum <sha256> # verify before extracting
```

---

### `lfff devices [--check]`
List connected fastboot devices or run full pre-flash diagnostics.

```bash
lfff devices           # list devices
lfff devices --check   # cable speed, battery, unlock status
```

---

### `lfff arb`
Check Anti-Rollback version of firmware vs device.

```bash
lfff arb --firmware-dir ./firmwares/RMX3709
lfff arb --xbl ./firmwares/RMX3709/xbl.img --device
```

---

### `lfff download <url>`
Download firmware with resume support.

```bash
lfff download https://example.com/firmware.zip
lfff download https://example.com/firmware.zip -o ./firmwares -c 8
```

---

### `lfff deps [--check] [TOOL ...]`
Install or verify external dependencies.

```bash
lfff deps              # install all missing tools
lfff deps --check      # check what's installed
lfff deps fastboot     # install only fastboot
```

---

## Supported Devices

Tested on **OnePlus / OPPO / Realme** devices with Qualcomm SoC and A/B partition layout.

Known working:
- RMX3709 (Realme GT3 Neo 5)
- CPH2653 (OnePlus ...)

Support for other A/B Qualcomm devices is likely but untested.  
If your device works (or doesn't), open an issue!

---

## Development

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
make install     # create venv, install deps
make build       # build binary → dist/lfff/lfff
make test        # run test suite
make lint        # run ruff linter
make clean       # remove build artifacts
```

**Project structure:**
```
LibreFastbootFirmwareFlasher/
├── main.py              # CLI entrypoint, subcommand handlers
├── setup.py             # cx_Freeze build config
├── pyproject.toml       # project metadata, pytest/ruff config
├── Makefile
├── install.sh           # installer script (Linux + macOS)
└── flasher/
    ├── __init__.py
    ├── arb.py           # Anti-Rollback version parser
    ├── deps.py          # dependency installer
    ├── device.py        # device detection, pre-flash checks
    ├── downloader.py    # firmware download via aria2c
    ├── extractor.py     # OTA zip extraction via payload-dumper-go
    ├── flasher.py       # flash orchestrator, progress bar, session logic
    └── utils.py         # shared subprocess helpers
```

---

## Links

- GitHub: [github.com/mrFrok/LibreFastbootFirmwareFlasher](https://github.com/mrFrok/LibreFastbootFirmwareFlasher)
- Author: [t.me/mrFrok228](https://t.me/mrFrok228)
- Community: [t.me/gt3neo5hub](https://t.me/gt3neo5hub)
- Windows version by NeFeroN: [t.me/NeFeroN](https://t.me/NeFeroN)

---

## License

[MIT](LICENSE)
