<div align="center">

![LFFF](logo.svg)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-orange.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-orange)](https://github.com/mrFrok/LibreFastbootFirmwareFlasher)

**Free, open-source firmware flasher for Android A/B devices via fastboot.**  
Single static binary — no Python, no pip, no bloat.

[Installation](#installation) · [Quick Start](#quick-start) · [Commands](#commands) · [Supported Devices](#supported-devices) · [Development](#development)

</div>

---

## About

LFFF is a command-line tool for flashing Android firmware on **OnePlus / OPPO / Realme** devices with Qualcomm SoC and A/B partition layout. It handles the full flash pipeline — extraction, pre-flash checks, Anti-Rollback protection, dynamic super partitions, A/B slot management and modem flashing — all from a single binary.

> **Windows?** Check out the Windows version by [NeFeroN](https://t.me/NeFeroN) who helped during development.  
> **Community:** [t.me/gt3neo5hub](https://t.me/gt3neo5hub)

---

## Features

| | |
|---|---|
| 🔥 **Full firmware flash** | Non-super + super (dynamic) partitions, modem, full bootloader chain |
| 🔄 **A/B slot management** | Non-super → both slots; super → active slot only (with super wipe) |
| 🛡️ **Anti-Rollback protection** | Reads ARB from `xbl_config.img` ELF64, compares against device, warns on downgrade |
| 📊 **Live progress bar** | Single animated bar with elapsed time across the entire session |
| 🔧 **Interactive error recovery** | On failure: retry / reboot to correct mode / abort |
| ✅ **Pre-flash checks** | Device detection, cable speed, battery level, bootloader unlock |
| 🎯 **Single-partition flash** | Flash any partition by name with `flash-partition` |
| 📦 **Firmware extraction** | Unpacks OTA `.zip` via `payload_dumper` (ZIP passed directly, no unzipping) |
| 📥 **Firmware download** | Downloads OTA zips via `aria2c` with 4PDA redirect support |
| 🔩 **Dependency installer** | Auto-installs `fastboot`, `aria2c`, `payload_dumper` |
| 💻 **Linux + macOS** | Single static binary, no runtime dependencies |
| 🦀 **Written in Rust** | Fast compilation, zero-cost abstractions, no garbage collector |

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

### From source

```bash
git clone https://github.com/mrFrok/LibreFastbootFirmwareFlasher
cd LibreFastbootFirmwareFlasher
cargo build --release
sudo cp target/release/lfff /usr/local/bin/
```

### From GitHub Releases

Download the prebuilt binary for your platform from [Releases](https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases):

```bash
tar xzf lfff-linux-x86_64.tar.gz
sudo cp lfff /usr/local/bin/
```

### Any Linux / macOS (one-liner)

```bash
curl -fsSL https://raw.githubusercontent.com/mrFrok/LibreFastbootFirmwareFlasher/main/install.sh | bash
```

Works on immutable distros (Fedora Silverblue, NixOS, SteamOS) — installs to ~/.local/bin.

### Homebrew (macOS / Linux)

```bash
brew tap mrFrok/lfff
brew install lfff
```

### Arch Linux (AUR)

```bash
yay -S lfff        # build from source
yay -S lfff-bin    # prebuilt binary---
```

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

Extract OTA firmware zip into individual `.img` files. `payload_dumper` accepts ZIP directly — no need to unzip first.

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

Compare Anti-Rollback version between firmware and device. Parses `xbl_config.img` ELF64 OEM metadata directly — same algorithm as [arbextract](https://github.com/koaaN/arbextract).

```bash
lfff arb --firmware-dir ./firmwares/RMX3709
lfff arb --xbl ./firmwares/RMX3709/critical/xbl_config.img --device
```

ARB levels:
- `ARB = 0` — hard ARB not enforced (safe)
- `ARB > 0` — hard ARB active; flashing a lower version **will brick the device**

---

### `lfff download <url>`

Download firmware with multi-connection resume support. Handles 4PDA redirect links automatically.

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
cargo build              # debug build
cargo build --release    # optimized build
cargo check              # type-check without building
cargo test               # run tests
```

### Project structure

```
LibreFastbootFirmwareFlasher/
├── Cargo.toml
├── README.md
├── LICENSE
├── logo.svg
├── .github/
│   └── workflows/
│       └── release.yml      # CI: cross-platform builds on tag push
└── src/
    ├── lib.rs               # library crate root (for future GUI)
    ├── main.rs              # CLI entry point (clap derive)
    ├── utils.rs             # subprocess helpers, sha256, tool checks
    ├── arb.rs               # Anti-Rollback ELF64 parser
    ├── device.rs            # device discovery, pre-flash checks
    ├── extractor.rs         # firmware extraction, partition grouping
    ├── downloader.rs        # OTA download via aria2c
    ├── flasher.rs           # flash orchestrator, A/B slots, progress bar
    └── deps.rs              # dependency installer
```

Split into library + binary crates to support a potential Qt GUI in the future.

### Cargo dependencies

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
