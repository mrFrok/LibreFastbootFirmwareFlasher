use std::path::PathBuf;

use clap::{ArgGroup, Parser, Subcommand, ValueEnum};

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

/// Where `lfff reboot` sends the phone.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum RebootTo {
    System,
    Recovery,
    Bootloader,
    Fastbootd,
}

/// An A/B slot.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Slot {
    A,
    B,
}

/// Shells `lfff completion` can emit a script for.
#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell")]
    PowerShell,
    Elvish,
    Nushell,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Generate a shell completion script
    Completion {
        /// Shell to generate the script for
        #[arg(value_name = "SHELL")]
        shell: CompletionShell,
    },
    /// Install and verify external dependencies
    Deps {
        /// Only check, do not install anything
        #[arg(long)]
        check: bool,

        /// Specific tools to install (default: all)
        #[arg(value_name = "TOOL")]
        tools: Vec<String>,
    },

    /// Find official firmware for an OPPO, OnePlus or realme phone
    #[command(name = "find-ota")]
    FindOta {
        /// Model code, e.g. CPH2747 or RMX3709 (Settings → About phone)
        model: String,

        /// Comma-separated regions: GL, EU, IN, SG, RU, TR, TH, CN, or all
        #[arg(short, long, value_name = "LIST")]
        region: Option<String>,

        /// Comma-separated channels (default: A,C,F,H)
        #[arg(short, long, value_name = "LIST")]
        channel: Option<String>,

        /// Print only the newest build's link
        #[arg(long)]
        latest: bool,

        /// Print the builds as JSON
        #[arg(long)]
        json: bool,

        /// Show what's new in the newest build
        #[arg(long)]
        changes: bool,
    },

    /// Download firmware via OTA link (supports 4PDA redirects)
    Download {
        /// OTA download link or 4PDA redirect URL; "-" reads it from stdin
        url: String,

        /// Directory to save the firmware (default: current directory)
        #[arg(short, long, value_name = "DIR")]
        output: Option<PathBuf>,

        /// Number of parallel connections for aria2c
        #[arg(short, long, default_value = "16")]
        connections: u32,

        /// Expected MD5 of the archive (find-ota prints it); a mismatch
        /// fails the download
        #[arg(long, value_name = "MD5")]
        md5: Option<String>,
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
    #[command(group(
        ArgGroup::new("arb_what")
            .required(true)
            .multiple(true)
            .args(["xbl", "firmware_dir", "device"])
    ))]
    Arb {
        /// Direct path to xbl_config.img
        #[arg(long, value_name = "XBL_IMG", group = "arb_src")]
        xbl: Option<PathBuf>,

        /// Extracted firmware directory (xbl_config.img located automatically)
        #[arg(long, value_name = "DIR", group = "arb_src")]
        firmware_dir: Option<PathBuf>,

        /// Read the ARB installed on the phone (adb, needs root). With
        /// --xbl or --firmware-dir, also compares the two
        #[arg(long)]
        device: bool,

        /// Target a specific device by serial number (with --device)
        #[arg(short, long, value_name = "SERIAL", requires = "device")]
        serial: Option<String>,
    },

    /// Flash an extracted firmware directory (or source build with --source)
    Flash {
        /// Path to the extracted firmware directory
        #[arg(required_unless_present = "source")]
        firmware_dir: Option<PathBuf>,

        /// Android source build output directory (skips ARB check)
        #[arg(long, value_name = "DIR", conflicts_with = "firmware_dir")]
        source: Option<PathBuf>,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,

        /// Flash method: 'snapdragon' (Qualcomm: everything in fastbootd, the
        /// modem into the other slot after switching to it), 'mtk' (MediaTek),
        /// or 'snapdragon-bootloader' (Qualcomm, the old way: the modem through
        /// the bootloader; not recommended, some phones cannot reach it).
        /// 'snapdragon-fastbootd' is the same as 'snapdragon'.
        /// Prompted interactively if omitted.
        #[arg(
            long,
            value_name = "METHOD",
            value_parser = ["snapdragon", "mtk", "snapdragon-bootloader", "snapdragon-fastbootd"]
        )]
        method: Option<String>,

        /// Detect images and run checks without flashing
        #[arg(long)]
        dry_run: bool,

        /// Skip xbl and abl partitions during flashing
        #[arg(long)]
        skip_xbl_abl: bool,

        /// Skip preloader partition during flashing
        #[arg(long)]
        skip_preloader: bool,

        /// Keep super's layout even when the slot being flashed has no
        /// partitions (they are then created outside their group, and the
        /// next system updates need the OTA repair)
        #[arg(long)]
        no_rebuild_super: bool,
    },

    /// Flash a single .img file to a specific partition
    #[command(name = "flash-partition")]
    FlashPartition {
        /// Path to .img file, or partition name (requires --firmware-dir)
        image: String,

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

    /// Reboot the phone (through adb or fastboot, whichever sees it)
    Reboot {
        /// Where to reboot to
        #[arg(value_enum, default_value = "system")]
        to: RebootTo,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,
    },

    /// Make an A/B slot the active one (fastboot or fastbootd)
    #[command(name = "set-slot")]
    SetSlot {
        /// The slot to boot from next
        #[arg(value_enum)]
        slot: Slot,

        /// Target a specific device by serial number
        #[arg(short, long, value_name = "SERIAL")]
        serial: Option<String>,
    },
}
