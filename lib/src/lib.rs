//! LFFF — Libre Fastboot Firmware Flasher
//!
//! Free, open-source firmware flasher for Android A/B devices via fastboot.
//! Supports OnePlus / OPPO / Realme devices with Qualcomm SoC.
//!
//! Library crate — used by the CLI binary and the Slint GUI.

pub mod arb;
pub mod deps;
pub mod device;
pub mod downloader;
pub mod errors;
pub mod extractor;
pub mod file_ops;
pub mod flash_history;
pub mod flasher;
pub mod utils;
