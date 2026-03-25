# LFFF GUI — Slint Desktop Interface

## Project structure

```
lfff/
├── Cargo.toml              # workspace root
├── lib/                    # library crate (business logic)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── utils.rs
│       ├── arb.rs
│       ├── device.rs
│       ├── extractor.rs
│       ├── downloader.rs
│       ├── flasher.rs
│       └── deps.rs
├── cli/                    # CLI binary (clap)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs
└── gui/                    # GUI binary (Slint)
    ├── Cargo.toml
    ├── build.rs
    ├── ui/
    │   └── main.slint
    └── src/
        └── main.rs
```

## Setup from existing LFFF codebase

```bash
cd ~/path/to/lfff

# Create sub-crate directories
mkdir -p lib/src cli/src

# Move library sources (everything except main.rs)
mv src/lib.rs src/utils.rs src/arb.rs src/device.rs \
   src/extractor.rs src/downloader.rs src/flasher.rs \
   src/deps.rs lib/src/

# Move CLI entry point
mv src/main.rs cli/src/main.rs
rmdir src

# Extract GUI from the downloaded archive
tar xzf lfff-gui-project.tar.gz
mv lfff-gui-project/gui ./gui
rm -rf lfff-gui-project

# Replace root Cargo.toml with the workspace version
cat > Cargo.toml << 'EOF'
[workspace]
members = ["lib", "cli", "gui"]
resolver = "2"
EOF
```

## Cargo.toml for each crate

### lib/Cargo.toml

Take your existing `[package]` and `[dependencies]` sections,
remove `[[bin]]`, and add `[lib]`:

```toml
[package]
name = "lfff-lib"
version = "0.1.0"
edition = "2021"

[lib]
name = "lfff_lib"

[dependencies]
# Keep all existing deps: reqwest, zip, serde_json, etc.
```

### cli/Cargo.toml

```toml
[package]
name = "lfff-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "lfff"
path = "src/main.rs"

[dependencies]
lfff-lib = { path = "../lib" }
# clap, colored, etc.
```

### gui/Cargo.toml

Already provided. Uncomment the lfff-lib dependency:

```toml
[dependencies]
slint = "1.10"
lfff-lib = { path = "../lib" }
```

## Fix imports after splitting

In `cli/src/main.rs`, replace all `crate::` references:

```rust
// Before:
use crate::device;
use crate::flasher;

// After:
use lfff_lib::device;
use lfff_lib::flasher;
```

In `lib/src/lib.rs`, make modules public:

```rust
pub mod utils;
pub mod arb;
pub mod device;
pub mod extractor;
pub mod downloader;
pub mod flasher;
pub mod deps;
```

## Build and run

```bash
# Run GUI
cargo run -p lfff-gui

# Run CLI (same as before)
cargo run -p lfff-cli

# Build everything
cargo build --workspace

# Release build
cargo build --workspace --release
# Binaries: target/release/lfff  target/release/lfff-gui
```

## Integration TODOs

### 1. Add progress callback to flasher

```rust
// lib/src/flasher.rs
pub fn flash(
    firmware_dir: &Path,
    device: &Device,
    options: &FlashOptions,
    on_progress: impl Fn(&str, f32),
) -> Result<()> {
    for (i, img) in images.iter().enumerate() {
        fastboot_flash(&img.partition, &img.path)?;
        on_progress(&img.partition, (i + 1) as f32 / images.len() as f32);
    }
    Ok(())
}
```

### 2. Add rfd for native file dialog

```toml
# gui/Cargo.toml
[dependencies]
rfd = "0.15"
```

### 3. Tokio in worker thread (if lib uses async)

```toml
# gui/Cargo.toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread"] }
```

```rust
// In worker_thread():
let rt = tokio::runtime::Runtime::new().unwrap();

UiCommand::DownloadFirmware { url } => {
    rt.block_on(async {
        lfff_lib::downloader::download(&url, |frac| {
            tx.send(WorkerMsg::Progress {
                fraction: frac,
                partition: "download".into(),
            }).ok();
        }).await
    }).ok();
}
```

### 4. Cancellation via AtomicBool

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

let cancel = Arc::new(AtomicBool::new(false));
// Pass clone to worker, check in flasher loop:
//   if cancel.load(Ordering::Relaxed) { return Err(Error::Cancelled); }
```

## Licensing

Slint is available under GPL-3.0 for open-source projects —
compatible with the LFFF license.
