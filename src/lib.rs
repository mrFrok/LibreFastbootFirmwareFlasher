//! LFFF — Libre Fastboot Firmware Flasher
//!
//! Free, open-source firmware flasher for Android A/B devices via fastboot.
//! Supports OnePlus / OPPO / Realme devices with Qualcomm SoC.
//!
//! Library crate — used by the CLI binary and (in the future) a Qt GUI.

pub mod arb;
pub mod deps;
pub mod device;
pub mod downloader;
pub mod extractor;
pub mod flasher;
pub mod utils;
