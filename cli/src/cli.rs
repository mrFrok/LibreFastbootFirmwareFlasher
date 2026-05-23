use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// LibreFastbootFirmwareFlasher — extract, check, and flash Android firmware.
#[derive(Parser)]
#[command(name = "lfff", version, about, long_about = None)]
pub struct Cli {
    /// Enable debug logging
    #[arg(short, long)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Install and verify external dependencies
    Deps {
        /// Only check, do not install anything
        #[arg(long)]
        check: bool,

        /// Specific tools to install (default: all)
        #[arg(value_name = "TOOL")]
        tools: Vec<String>,
    },

    /// Download firmware via OTA link (supports 4PDA redirects)
    Download {
        /// OTA download link or 4PDA redirect URL
        url: String,

        /// Directory to save the firmware (default: current directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<PathBuf>,

        /// Number of parallel connections for aria2c
        #[arg(short, long, default_value = "16")]
        connections: u32,
    },

    /// Extract a firmware .zip archive
    Extract {
        /// Path to the firmware .zip
        zip: PathBuf,

        /// Output directory (prompted if not given)
        #[arg(short, long, value_name = "DIR")]
        output: Option<PathBuf>,

        /// Comma-separated partitions to extract
        #[arg(short, long, value_name = "LIST")]
        partitions: Option<String>,

        /// Expected SHA-256 checksum of the archive
        #[arg(long, value_name = "SHA256")]
        checksum: Option<String>,

        /// List archive contents without extracting
        #[arg(long)]
        list: bool,
    },

    /// List connected devices, run pre-flash diagnostics
    Devices {
        /// Run full pre-flash diagnostics
        #[arg(long)]
        check: bool,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,
    },

    /// Check Anti-Rollback version of a firmware
    Arb {
        /// Direct path to xbl_config.img
        #[arg(long, value_name = "XBL_IMG", group = "arb_src")]
        xbl: Option<PathBuf>,

        /// Extracted firmware directory (xbl_config.img located automatically)
        #[arg(long, value_name = "DIR", group = "arb_src")]
        firmware_dir: Option<PathBuf>,

        /// Also read ARB version from connected device and compare
        #[arg(long)]
        device: bool,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,
    },

    /// Flash an extracted firmware directory (or source build with --source)
    Flash {
        /// Path to the extracted firmware directory
        firmware_dir: Option<PathBuf>,

        /// Android source build output directory (skips ARB check)
        #[arg(long, value_name = "DIR", conflicts_with = "firmware_dir")]
        source: Option<PathBuf>,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,

        /// Detect images and run checks without flashing
        #[arg(long)]
        dry_run: bool,

        /// Skip xbl and abl partitions during flashing
        #[arg(long)]
        skip_xbl_abl: bool,

        /// Skip preloader partition during flashing
        #[arg(long)]
        skip_preloader: bool,
    },

    /// Flash a single .img file to a specific partition
    #[command(name = "flash-partition")]
    FlashPartition {
        /// Path to .img file, or partition name (requires --firmware-dir)
        image: Option<String>,

        /// Extracted firmware directory to search for the partition image
        #[arg(long, value_name = "DIR")]
        firmware_dir: Option<PathBuf>,

        /// Partition name override (default: image filename stem)
        #[arg(short, long, value_name = "NAME")]
        partition: Option<String>,

        /// Slot(s) to flash: a, b, or a,b (default: both)
        #[arg(long, value_name = "SLOT")]
        slot: Option<String>,

        /// Flash without slot suffix (for non-A/B partitions)
        #[arg(long)]
        no_ab: bool,

        /// Show what would be flashed without actually flashing
        #[arg(long)]
        dry_run: bool,

        /// Target a specific device
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,
    },
}
