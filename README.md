<div align="center">

![LFFF](logo.svg)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-orange.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-orange)](https://github.com/mrFrok/LibreFastbootFirmwareFlasher)
[![Releases](https://img.shields.io/github/v/release/mrFrok/LibreFastbootFirmwareFlasher)](https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases)

**Free, open-source firmware flasher for Android A/B devices via fastboot.**  
CLI + GUI — single static binary, no Python, no bloat.

[Installation](#installation) · [Quick Start](#quick-start) · [Features](#features) · [Commands](#commands) · [Supported Devices](#supported-devices) · [Development](#development)

</div>

---

## About

LFFF flashes full firmware on **OnePlus / OPPO / Realme** devices with Qualcomm SoC and A/B partition layout. Handles the entire pipeline — extraction, pre-flash checks, Anti-Rollback protection, dynamic super partitions, A/B slot management, modem flashing.

Two flavours:
- **`lfff`** — CLI, scriptable, works over SSH
- **`lfff-gui`** — native desktop GUI built with [Slint](https://slint.dev)

> **Windows?** Check out the Windows version by [NeFeroN](https://t.me/NeFeroN).  
> **Community:** [t.me/gt3neo5hub](https://t.me/gt3neo5hub)

---

## What's New in v2.0

| | |
|---|---|
| 🏗️ **Source build support** | Flash directly from `out/target/product/*` — no zip extraction needed |
| 🎨 **Custom Material Design 3** | Full dark/light theme, proper hover states, animated transitions |
| ⏭️ **Per-partition skip on error** | Skip a failed partition and continue flashing |
| 🧹 **Smarter image filtering** | Auto-filters debug/test images, `dtb`, `vendor_ramdisk`, `vendor-bootconfig` in source builds |
| 🚫 **No more vendored themes** | Custom MD3 components — smaller, faster, no external theme deps |
| ⚡ **Slint 1.16.1** | Latest stable rendering engine |
| 🔧 **Better error reporting** | Each failure shown clearly with retry/skip/abort options |

---

## Features

| | |
|---|---|
| 🖥️ **Native GUI** | Desktop app with live log, progress bar, device info, download & extract, no electron and web bloat |
| 🔥 **Full firmware flash** | Non-super + super (dynamic) partitions, modem, bootloader chain |
| 🔄 **A/B slot management** | Non-super → both slots; super → active slot only (super wiped first) |
| 🛡️ **Anti-Rollback protection** | Reads ARB from `xbl_config.img` ELF64 — warns before raising the counter |
| 📊 **Live progress** | Animated progress bar with elapsed time |
| 🔧 **Error recovery** | Retry / reboot to correct mode / skip partition / abort |
| ✅ **Pre-flash checks** | Device detection, cable speed, battery level, bootloader unlock |
| 🎯 **Single-partition flash** | Flash any partition by name |
| 📦 **Firmware extraction** | Unpacks OTA `.zip` via `payload_dumper` |
| 📥 **Firmware download** | Downloads OTA zips via `aria2c` with 4PDA redirect support |
| 🔩 **Dependency installer** | Auto-installs `fastboot`, `aria2c`, `payload_dumper` |
| 💻 **Linux + macOS** | x86_64 and aarch64 |
| 🦀 **Written in Rust** | Fast, safe, no garbage collector |

---

## Requirements

| Tool | Purpose | Auto-install |
|------|---------|:---:|
| `fastboot` | Flashing interface | ✓ |
| `adb` | Device detection & reboot | ✓ |
| `payload_dumper` | OTA zip extraction | ✓ |
| `aria2c` | Multi-connection download | ✓ |

```bash
lfff deps   # installs everything automatically
```

---

## Installation

### Any Linux / macOS (one-liner)

```bash
# Install CLI and GUI
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash

# Install only GUI
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --gui-only

# Install only CLI
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --cli-only
```

Works on immutable distros (Fedora Silverblue, NixOS, SteamOS) — installs to `~/.local/bin`.

### Homebrew (macOS / Linux)

```bash
brew tap mrFrok/lfff
brew install lfff
```

### Arch Linux (AUR)

```bash
yay -S lfff        # Build from source
yay -S lfff-bin    # Prebuilt binary
```

### From GitHub Releases

Download from [Releases](https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases):

```bash
# CLI
tar xzf lfff-linux-x86_64.tar.gz
sudo cp lfff /usr/local/bin/

# GUI
tar xzf lfff-gui-linux-x86_64.tar.gz
sudo cp lfff-gui /usr/local/bin/
```

### From source

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
cargo build --release -p lfff-cli     # CLI
cargo build --release -p lfff-gui     # GUI
```

---

## Uninstall

```bash
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --uninstall
```

---

## Quick Start

### GUI

```bash
lfff-gui
```

Navigate the sidebar: **Download** → **Flash All** → **Flash Partition**.

### CLI

```bash
# 1. Install external tools
lfff deps

# 2. Download firmware (or use your own zip)
lfff download https://...

# 3. Extract
lfff extract CPH2653_11.F.85_2850_202501010000.zip

# 4. Diagnostics
lfff devices --check

# 5. Flash full firmware
lfff flash ./firmwares/CPH2653_11.F.85_2850

# Or flash from Android source build directory
lfff flash --source /out/target/product/senna
```

---

## Commands

### `lfff flash <firmware_dir>`

Full firmware flash with A/B slot management.

```bash
lfff flash ./firmwares/RMX3709_11.H.38
lfff flash ./firmwares/RMX3709_11.H.38 -s R5CT20      # specific device
lfff flash ./firmwares/RMX3709_11.H.38 --dry-run      # preview only
lfff flash --source /out/target/product/senna           # source build dir
```

**Flash flow:**

```
[Stage 1 — fastbootd]
  Non-super partitions  →  slot A  +  slot B
  Super/dynamic parts   →  active slot only (super wiped first)
      ↓
[Stage 2 — bootloader]
  modem  →  slot A  +  slot B
      ↓
[Offer: reboot to system  /  wipe data + reboot]
```

On error: **retry** / **reboot** / **skip partition** / **abort**

---

### `lfff flash-partition <name>`

```bash
lfff flash-partition boot --firmware-dir ./firmwares/RMX3709
lfff flash-partition vendor_boot --firmware-dir ./firmwares/RMX3709 --slot a
lfff flash-partition modem --firmware-dir ./firmwares/RMX3709 --no-ab
```

---

### `lfff extract <zip>`

```bash
lfff extract firmware.zip
lfff extract firmware.zip -o ./firmwares/my_build
lfff extract firmware.zip --list               # list contents only
lfff extract firmware.zip --checksum <sha256>  # verify before extracting
```

---

### `lfff devices [--check]`

```bash
lfff devices           # list fastboot + adb devices
lfff devices --check   # cable speed, battery, unlock status
```

---

### `lfff arb`

Compare Anti-Rollback version between firmware and device.

```bash
lfff arb --firmware-dir ./firmwares/RMX3709
lfff arb --xbl ./firmwares/RMX3709/critical/xbl_config.img --device
```

ARB levels:
- `ARB = 0` — hard ARB not enforced (safe)
- `ARB > 0` — flashing **permanently raises the counter** — downgrade bricks the device

---

### `lfff download <url>`

```bash
lfff download https://example.com/firmware.zip
lfff download "https://4pda.to/redirector/..." -o ./firmwares -c 8
```

---

### `lfff deps [--check] [TOOL ...]`

```bash
lfff deps              # install all missing
lfff deps --check      # check status only
lfff deps fastboot     # install specific tool
```

---

## Supported Devices

Tested on Qualcomm A/B devices:

| Device | Model | Status |
|--------|-------|--------|
| Realme GT Neo 5 | RMX3709 | ✅ Working |

Other OnePlus / OPPO / Realme devices with Qualcomm SoC and A/B layout should work.

---

## Development

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
cargo build              # debug build (all crates)
cargo build --release    # optimized build
cargo check              # type-check without building
cargo test               # run tests
```

### Project structure

```
LibreFastbootFirmwareFlasher/
├── Cargo.toml               # workspace
├── README.md
├── LICENSE
├── logo.svg
├── lfff-gui.desktop         # desktop entry for GUI
├── lfff-gui.svg             # app icon
├── install.sh               # universal installer
├── .github/
│   └── workflows/
│       └── release.yml      # CI: builds CLI + GUI on tag push
├── lib/                     # shared library
│   └── src/
│       ├── lib.rs
│       ├── arb.rs           # Anti-Rollback ELF64 parser
│       ├── deps.rs          # dependency installer
│       ├── device.rs        # device discovery, pre-flash checks
│       ├── downloader.rs    # OTA download via aria2c
│       ├── extractor.rs     # firmware extraction
│       ├── flasher.rs       # flash orchestrator, A/B slots
│       └── utils.rs         # subprocess helpers, sha256
├── cli/                     # CLI binary
│   └── src/
│       └── main.rs
└── gui/                     # GUI binary (Slint)
    ├── build.rs
    ├── src/
    │   └── main.rs
    └── ui/
        └── main.slint
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

This project uses [Slint](https://github.com/slint-ui/slint) (GNU GPL v3), a declarative GUI toolkit.  
Uses [Material Design 3](https://m3.material.io) icons and design system under the Apache 2.0 license.

---

## Star History

If LFFF saved you time or a bricked device — a ⭐ goes a long way!

<a href="https://www.star-history.com/#mrFrok/LibreFastbootFirmwareFlasher&Date">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=mrFrok/LibreFastbootFirmwareFlasher&type=Date&theme=dark" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=mrFrok/LibreFastbootFirmwareFlasher&type=Date" />
   <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=mrFrok/LibreFastbootFirmwareFlasher&type=Date" />
 </picture>
</a>
