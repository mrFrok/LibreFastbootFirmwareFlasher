<div align="center">

![LFFF](logo.svg)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-orange.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Python](https://img.shields.io/badge/Python-3.10%2B-orange)](https://python.org)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-orange)](https://github.com/mrFrok/LibreFastbootFirmwareFlasher)

**Free, open-source firmware flasher for Android A/B devices via fastboot.**  
No proprietary tools. No Windows required. No telemetry.

[Installation](#installation) · [Quick Start](#quick-start) · [Commands](#commands) · [Supported Devices](#supported-devices) · [Development](#development)

</div>

---

## About

LFFF is a command-line tool for flashing Android firmware on **OnePlus / OPPO / Realme** devices with Qualcomm SoC and A/B partition layout. It handles the full flash pipeline — extraction, pre-flash checks, Anti-Rollback protection, dynamic super partitions, A/B slot management and modem flashing — all from a single CLI.

> **Windows?** Check out the Windows version by [NeFeroN](https://t.me/NeFeroN) who helped during development.  
> **Community:** [t.me/gt3neo5hub](https://t.me/gt3neo5hub)

---

## Features

| | |
|---|---|
| 🔥 **Full firmware flash** | Non-super + super (dynamic) partitions, modem, full bootloader chain |
| 🔄 **A/B slot management** | Non-super → both slots; super → active slot only (with super wipe) |
| 🛡️ **Anti-Rollback protection** | Reads ARB from `xbl.img`, compares against device, warns on downgrade |
| 📊 **Live progress bar** | Single animated bar with elapsed time across the entire session |
| 🔧 **Interactive error recovery** | On failure: retry / reboot to correct mode / abort |
| ✅ **Pre-flash checks** | Device detection, cable speed, battery level, bootloader unlock |
| 🎯 **Single-partition flash** | Flash any partition by name with `flash-partition` |
| 📦 **Firmware extraction** | Unpacks OTA `.zip` via `payload-dumper-go` |
| 📥 **Firmware download** | Downloads OTA zips via `aria2c` with resume support |
| 🔩 **Dependency installer** | Auto-downloads `fastboot`, `aria2c`, `payload-dumper-go` |
| 💻 **Linux + macOS** | Works natively on both platforms |

---

## Requirements

| Tool | Purpose | Auto-install |
|------|---------|:---:|
| `fastboot` | Flashing interface | ✓ |
| `adb` | Device detection & reboot | ✓ |
| `payload-dumper-go` | OTA zip extraction | ✓ |
| `aria2c` | Multi-connection download | ✓ |
| `python3 + pip3` | Python | x | 

```bash
lfff deps   # installs everything automatically
```

---

## Installation

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
chmod +x install.sh
./install.sh
```

The installer detects your environment, builds a standalone binary via `cx_Freeze`, and installs it to `/usr/local/bin/lfff` (or `~/.local/bin/lfff` on atomic/immutable distros).

| Flag | Short | Description |
|------|-------|-------------|
| `--prebuilt`  | `-p` | Skip build, install existing `dist/lfff/` |
| `--update`    | `-u` | Checkout latest stable release tag and reinstall |
| `--nightly`   | `-n` | Checkout HEAD of main branch (may be unstable) |
| `--reinstall` | `-i` | Uninstall and reinstall from scratch |
| `--uninstall` | `-r` | Remove lfff from the system |

------

## Quick Start

```bash
# 1. Install external tools
lfff deps

# 2. Download firmware (skip if you already have the zip)
lfff download https://...

# 3. Extract firmware zip
lfff extract CPH2653_11.F.85_2850_202601060236.zip

# 4. Run pre-flash diagnostics
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
lfff flash ./firmwares/RMX3709_11.H.38 -s R5CT20    # specific device
lfff flash ./firmwares/RMX3709_11.H.38 --dry-run    # preview only
```

**Flash flow:**

```
[1] Choose where device is now: system / bootloader / fastbootd
      ↓
[Stage 1 — fastbootd]
  Non-super partitions  →  slot A  +  slot B
  Super/dynamic parts   →  active slot only  (super wiped first)
      ↓
[Stage 2 — bootloader]
  modem  →  slot A  +  slot B
      ↓
[Offer userdata wipe]  →  reboot to system
```

On error: **retry** / **reboot to correct mode + retry** / **abort**

---

### `lfff flash-partition <partition>`

Flash a single partition by name.

```bash
lfff flash-partition boot --firmware-dir ./firmwares/RMX3709
lfff flash-partition vendor_boot --firmware-dir ./firmwares/RMX3709 --slot a
lfff flash-partition modem --firmware-dir ./firmwares/RMX3709 --no-ab
```

---

### `lfff extract <zip>`

Extract OTA firmware zip into individual `.img` files.

```bash
lfff extract firmware.zip
lfff extract firmware.zip -o ./firmwares/my_build
lfff extract firmware.zip --list               # list contents only
lfff extract firmware.zip --checksum <sha256>  # verify before extracting
```

---

### `lfff devices [--check]`

List connected devices or run full pre-flash diagnostics.

```bash
lfff devices           # list fastboot devices
lfff devices --check   # cable speed, battery, unlock status
```

---

### `lfff arb`

Compare Anti-Rollback version between firmware and device.

```bash
lfff arb --firmware-dir ./firmwares/RMX3709
lfff arb --xbl ./firmwares/RMX3709/xbl.img --device
```

---

### `lfff download <url>`

Download firmware with multi-connection resume support.

```bash
lfff download https://example.com/firmware.zip
lfff download https://example.com/firmware.zip -o ./firmwares -c 8
```

---

### `lfff deps [--check] [TOOL ...]`

Install or verify external dependencies.

```bash
lfff deps              # install all missing
lfff deps --check      # check status
lfff deps fastboot     # install specific tool
```

---

## Supported Devices

Tested on Qualcomm A/B devices:

| Device | Model | Status |
|--------|-------|--------|
| Realme GT Neo 5 | RMX3709 | ✅ Working |

Other OnePlus / OPPO / Realme devices with Qualcomm SoC and A/B layout should work.  
If your device works (or doesn't) — open an issue!

---

## Development

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher

make install   # create venv, install deps
make build     # build binary → dist/lfff/lfff
make rebuild   # clean + build
make test      # run test suite
make lint      # run ruff linter
make clean     # remove build artifacts
```

**Project structure:**

```
LibreFastbootFirmwareFlasher/
├── main.py              # CLI entrypoint, argument parsing, subcommands
├── setup.py             # cx_Freeze build config
├── pyproject.toml       # project metadata, pytest/ruff config
├── Makefile             # build, test, clean targets
├── install.sh           # installer (Linux + macOS)
├── logo.svg             # project logo
└── flasher/
    ├── __init__.py      # public API
    ├── arb.py           # Anti-Rollback version parser (ELF)
    ├── deps.py          # dependency installer
    ├── device.py        # device detection, pre-flash checks
    ├── downloader.py    # firmware download via aria2c
    ├── extractor.py     # OTA zip extraction via payload-dumper-go
    ├── flasher.py       # flash orchestrator, progress bar, session
    └── utils.py         # shared subprocess helpers
```

---

## Links

| | |
|---|---|
| ✈️ Author | [t.me/mrFrok228](https://t.me/mrFrok228) |
| 👥 Community | [t.me/gt3neo5hub](https://t.me/gt3neo5hub) |
| 🪟 Windows version | [NeFeroN](https://t.me/NeFeroN) |

---

## License

[GNU GPL v3](LICENSE) — free to use, modify and distribute.  
Derivative works must remain open source.

---

## Star History

If LFFF saved you time or a bricked device — a ⭐ goes a long way!

<a href="https://www.star-history.com/?repos=mrFrok%2FLibreFastbootFirmwareFlasher&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/image?repos=mrFrok/LibreFastbootFirmwareFlasher&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/image?repos=mrFrok/LibreFastbootFirmwareFlasher&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/image?repos=mrFrok/LibreFastbootFirmwareFlasher&type=date&legend=top-left" />
 </picture>
</a>

