<div align="center">

![LFFF](logo.svg)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-orange.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-orange)](https://github.com/mrFrok/LibreFastbootFirmwareFlasher)

**Free, open-source firmware flasher for Android A/B devices via fastboot.**  
Available as a CLI tool and a native GUI — single static binary, no Python, no pip, no bloat.

[Installation](#installation) · [Quick Start](#quick-start) · [Commands](#commands) · [Supported Devices](#supported-devices) · [Development](#development)

</div>

---

## About

LFFF is a tool for flashing Android firmware on **OnePlus / OPPO / Realme** devices with Qualcomm SoC and A/B partition layout. It handles the full flash pipeline — extraction, pre-flash checks, Anti-Rollback protection, dynamic super partitions, A/B slot management and modem flashing.

Available in two flavours:
- **`lfff`** — CLI tool, scriptable, works over SSH
- **`lfff-gui`** — native desktop GUI built with [Slint](https://slint.dev)

> **Windows?** Check out the Windows version by [NeFeroN](https://t.me/NeFeroN) who helped during development.  
> **Community:** [t.me/gt3neo5hub](https://t.me/gt3neo5hub)

---

## Features

| | |
|---|---|
| 🖥️ **Native GUI** | Desktop app with live log, progress bar, device info, download & extract |
| 🔥 **Full firmware flash** | Non-super + super (dynamic) partitions, modem, full bootloader chain |
| 🔄 **A/B slot management** | Non-super → both slots; super → active slot only (with super wipe) |
| 🛡️ **Anti-Rollback protection** | Reads ARB from `xbl_config.img` ELF64 — warns before raising the counter |
| 📊 **Live progress** | Animated progress bar with elapsed time |
| 🔧 **Error recovery** | On failure: retry / reboot to correct mode / abort |
| ✅ **Pre-flash checks** | Device detection, cable speed, battery level, bootloader unlock |
| 🎯 **Single-partition flash** | Flash any partition by name |
| 📦 **Firmware extraction** | Unpacks OTA `.zip` via `payload_dumper` — ZIP passed directly, no unzipping |
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

Download prebuilt binaries from [Releases](https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases):

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
cargo build --release -p lfff       # CLI
cargo build --release -p lfff-gui   # GUI
```

---

## Uninstall
```bash
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash -s -- --uninstall
```

## Quick Start

### GUI

```bash
lfff-gui
```

Use the sidebar to navigate: **Download** → **Flash All** → **Flash Partition**.

### CLI

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
[Stage 1 — fastbootd]
  Non-super partitions  →  slot A  +  slot B
  Super/dynamic parts   →  active slot only  (super wiped first)
      ↓
[Stage 2 — bootloader]
  modem  →  slot A  +  slot B
      ↓
[Offer: reboot to system  /  wipe data + reboot]
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

Images are organized into subdirectories: `critical/`, `bootloader/`, `radio/`, `system/`, `vendor/`, `other/`.

---

### `lfff devices [--check]`

List connected devices or run full pre-flash diagnostics.

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
- `ARB = 0` — hard ARB not enforced (safe to flash)
- `ARB > 0` — hard ARB active; flashing **permanently raises the counter** — downgrade will brick the device

---

### `lfff download <url>`

Download firmware with multi-connection resume support.

```bash
lfff download https://example.com/firmware.zip
lfff download "https://4pda.to/redirector/?u=..." -o ./firmwares -c 8
```

---

### `lfff deps [--check] [TOOL ...]`

Install or verify external dependencies.

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
If your device works (or doesn't) — open an issue!

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
├── install.sh               # universal installer
├── .github/
│   └── workflows/
│       └── release.yml      # CI: builds CLI + GUI on tag push
├── lib/                     # shared library crate
│   └── src/
│       ├── lib.rs
│       ├── utils.rs         # subprocess helpers, sha256
│       ├── arb.rs           # Anti-Rollback ELF64 parser
│       ├── device.rs        # device discovery, pre-flash checks
│       ├── extractor.rs     # firmware extraction, partition grouping
│       ├── downloader.rs    # OTA download via aria2c
│       ├── flasher.rs       # flash orchestrator, A/B slots
│       └── deps.rs          # dependency installer
├── cli/                     # CLI binary crate
│   └── src/
│       └── main.rs
└── gui/                     # GUI binary crate (Slint)
    ├── build.rs
    ├── src/
    │   └── main.rs
    └── ui/
        └── main.slint
```

### Cargo dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing |
| `anyhow` | Error handling |
| `log` / `env_logger` | Logging |
| `sha2` | File checksum verification |
| `colored` | Terminal colors |
| `which` | Finding binaries in $PATH |
| `zip` | Reading firmware archives |
| `indicatif` | Progress bars |
| `slint` | GUI framework |
| `rfd` | Native file dialogs |
| `arboard` | Clipboard access |

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
