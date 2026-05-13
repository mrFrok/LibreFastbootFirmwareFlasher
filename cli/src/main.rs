//! LFFF CLI — LibreFastbootFirmwareFlasher entry point.
//!
//! Subcommands: deps, download, extract, devices, arb, flash, flash-partition

use std::path::PathBuf;
use std::process;

use clap::{Command, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};

/// LibreFastbootFirmwareFlasher — extract, check, and flash Android firmware.
#[derive(Parser)]
#[command(name = "lfff", version, about, long_about = None)]
struct Cli {
    /// Enable debug logging
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
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

    /// Generate shell completion script
    Completions {
        /// Shell to generate completions for
        shell: Shell,
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

// ---------------------------------------------------------------------------
// Subcommand handlers
// ---------------------------------------------------------------------------

fn cmd_devices(check: bool, serial: Option<&str>) -> i32 {
    use lfff_lib::device::{
        list_adb_devices, list_fastboot_devices, print_check_report, run_pre_flash_checks,
    };

    println!("\n── Connected devices ────────────────────────────────────");

    let fb_serials = list_fastboot_devices();
    let adb_serials = list_adb_devices();

    if fb_serials.is_empty() && adb_serials.is_empty() {
        println!("  No devices found via fastboot or adb.");
        println!("  Make sure USB debugging or fastboot mode is enabled.");
        println!("────────────────────────────────────────────────────────\n");
        return 1;
    }

    for s in &fb_serials {
        println!("  fastboot : {}", s);
    }
    for s in &adb_serials {
        println!("  adb      : {}", s);
    }
    println!("────────────────────────────────────────────────────────\n");

    if check {
        println!("Running pre-flash checks …\n");
        let result = run_pre_flash_checks(serial);
        print_check_report(&result);
        return if result.ready() { 0 } else { 1 };
    }

    0
}

fn cmd_extract(
    zip: &PathBuf,
    output: Option<&PathBuf>,
    partitions: Option<&str>,
    checksum: Option<&str>,
    list_only: bool,
) -> i32 {
    use lfff_lib::extractor::{extract_firmware, get_firmware_name, print_extraction_result};

    if !zip.exists() {
        println!("✗ File not found: {}", zip.display());
        return 1;
    }

    if list_only {
        // List zip contents
        if let Ok(file) = std::fs::File::open(zip) {
            if let Ok(mut archive) = zip::ZipArchive::new(file) {
                println!(
                    "\nContents of {}:",
                    zip.file_name().unwrap_or_default().to_string_lossy()
                );
                let mut names: Vec<String> = (0..archive.len())
                    .filter_map(|i| {
                        archive.by_index(i).ok().map(|e| {
                            let size_mb = e.size() as f64 / 1024.0 / 1024.0;
                            format!("  {:<55} {:>8.1} MB", e.name(), size_mb)
                        })
                    })
                    .collect();
                names.sort();
                for line in names {
                    println!("{}", line);
                }
            }
        }
        return 0;
    }

    let output_dir = match output {
        Some(dir) => dir.clone(),
        None => {
            let name = get_firmware_name(zip);
            let default = std::env::current_dir()
                .unwrap_or_default()
                .join("firmwares")
                .join(&name);
            println!("  Firmware : {}", name);
            let raw =
                lfff_lib::utils::prompt(&format!("  Output directory [{}]", default.display()), "");
            if raw.is_empty() {
                default
            } else {
                PathBuf::from(raw)
            }
        }
    };

    let parts: Option<Vec<String>> =
        partitions.map(|p| p.split(',').map(|s| s.trim().to_string()).collect());

    let result = extract_firmware(zip, &output_dir, checksum, parts.as_deref());

    print_extraction_result(&result);
    if result.success { 0 } else { 1 }
}

fn cmd_flash(
    source: &lfff_lib::flasher::FirmwareSource,
    serial: Option<&str>,
    dry_run: bool,
    skip_xbl_abl: bool,
    skip_preloader: bool,
) -> i32 {
    use lfff_lib::flasher::{
        collect_images, collect_images_from_source, is_mediatek_build,
        print_summary, run_flash_session_with_log, FlashProgress,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let dir = source.path();
    if !dir.is_dir() {
        println!("✗ Not a directory: {}", dir.display());
        return 1;
    }

    println!("\n── Dependency check ─────────────────────────────────────");
    let ok = lfff_lib::utils::require_tools(&["fastboot"]);
    println!("────────────────────────────────────────────────────────\n");
    if !ok {
        println!("✗ fastboot is required for flashing. Aborting.");
        return 1;
    }

    // Check for Mediatek firmware — warn about preloader
    let images = if source.is_source() {
        collect_images_from_source(dir)
    } else {
        collect_images(dir)
    };

    let mut skip_preloader = skip_preloader;

    if !images.is_empty() && is_mediatek_build(&images) {
        if dry_run {
            println!("⚠  Mediatek firmware detected (preloader found).");
            println!("   Use --skip-preloader to exclude it during actual flashing.");
        } else if !skip_preloader {
            println!("\n⚠  Mediatek firmware detected (preloader found).");
            println!("   Flashing preloader on Mediatek devices is risky and may brick your device.");
            println!("   It is recommended to skip the preloader unless you know what you are doing.\n");
            print!("Skip preloader? [Y/n/a] (Y=skip, n=flash preloader, a=abort): ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            match input.trim().to_lowercase().as_str() {
                "" | "y" | "yes" => {
                    println!("→ Skipping preloader.");
                    skip_preloader = true;
                }
                "n" | "no" => {
                    println!("→ Will flash preloader.");
                }
                "a" | "abort" => {
                    println!("Aborted by user.");
                    return 1;
                }
                _ => {
                    println!("→ Skipping preloader (default).");
                    skip_preloader = true;
                }
            }
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let on_log = |msg: String| println!("{}", msg);
    let on_progress = |p: FlashProgress| {
        if p.done == p.total {
            println!("  ✓ {} (slot {}): {}/{} done", p.partition, p.slot, p.done, p.total);
        }
    };

    let session = run_flash_session_with_log(
        source,
        serial,
        dry_run,
        skip_xbl_abl,
        skip_preloader,
        false,
        cancel,
        &on_log,
        &on_progress,
    );

    print_summary(&session);

    if session.critical_failed().is_empty() {
        0
    } else {
        1
    }
}

fn cmd_flash_partition(
    image: Option<&str>,
    firmware_dir: Option<&PathBuf>,
    partition: Option<&str>,
    slot: Option<&str>,
    no_ab: bool,
    dry_run: bool,
    serial: Option<&str>,
) -> i32 {
    use lfff_lib::flasher::{collect_images, run_flash_single};

    let image_path: PathBuf = match image {
        Some(img) => {
            let p = PathBuf::from(img);
            if p.extension().map(|e| e == "img").unwrap_or(false) && p.exists() {
                p
            } else {
                // Treat as partition name, look up in firmware-dir
                let part_name = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                match firmware_dir {
                    Some(dir) => {
                        let images = collect_images(dir);
                        match images.get(&part_name) {
                            Some(path) => path.clone(),
                            None => {
                                let available: Vec<_> = images.keys().collect();
                                println!(
                                    "✗ Partition '{}' not found in {}",
                                    part_name,
                                    dir.display()
                                );
                                println!("  Available: {:?}", available);
                                return 1;
                            }
                        }
                    }
                    None => {
                        println!(
                            "✗ '{}' is not a .img file and --firmware-dir is not set.",
                            img
                        );
                        return 1;
                    }
                }
            }
        }
        None => {
            println!("✗ Provide an image path or partition name with --firmware-dir.");
            return 1;
        }
    };

    let slots: Option<Vec<String>> = if let Some(s) = slot {
        Some(s.split(',').map(|x| x.trim().to_lowercase()).collect())
    } else if no_ab {
        Some(vec![String::new()])
    } else {
        None
    };

    let session = run_flash_single(&image_path, partition, slots.as_deref(), serial, dry_run);

    if session.critical_failed().is_empty() {
        0
    } else {
        1
    }
}

fn cmd_arb(
    xbl: Option<&PathBuf>,
    firmware_dir: Option<&PathBuf>,
    device: bool,
    serial: Option<&str>,
) -> i32 {
    use lfff_lib::arb::{
        arb_confirmation_gate, compare_arb_versions, extract_arb_from_xbl, find_xbl_config,
        get_device_arb_version,
    };

    let xbl_path = match (xbl, firmware_dir) {
        (Some(path), _) => path.clone(),
        (None, Some(dir)) => match find_xbl_config(dir) {
            Some(p) => p,
            None => {
                println!("✗ xbl_config.img not found in the given firmware directory.");
                return 1;
            }
        },
        _ => {
            println!("✗ Provide either --xbl <path> or --firmware-dir <dir>.");
            return 1;
        }
    };

    let firmware_arb = extract_arb_from_xbl(&xbl_path);
    println!("\n  Firmware  : {}", firmware_arb);

    if device {
        let (device_arb, method) = get_device_arb_version(serial);
        println!("  Device    : {}", device_arb);
        let result = compare_arb_versions(&firmware_arb, &device_arb);
        arb_confirmation_gate(&result, &method.to_string());
    } else if firmware_arb.enforced() {
        println!("  ⚠  Hard ARB is ACTIVE on this firmware.");
    } else {
        println!("  ✓  Hard ARB is not enforced (version = 0).");
    }

    0
}

fn cmd_deps(check: bool, tools: &[String]) -> i32 {
    use lfff_lib::deps::install_dependencies;

    let tool_list = if tools.is_empty() { None } else { Some(tools) };
    let report = install_dependencies(tool_list, check);

    if report.all_ok() { 0 } else { 1 }
}

fn cmd_download(url: &str, output: Option<&PathBuf>, connections: u32) -> i32 {
    use lfff_lib::downloader::download_firmware;

    println!("\n── Firmware download ────────────────────────────────────");
    let result = download_firmware(url, output.map(|p| p.as_path()), connections);

    if !result.success {
        println!("\n✗ Download failed: {}", result.error);
        return 1;
    }

    println!("\n✓ Download complete.");
    if let Some(ref path) = result.output_path {
        println!("  Saved to: {}", path.display());
        println!("\n  Next step:");
        println!("    lfff extract \"{}\"", path.display());
    }
    println!("────────────────────────────────────────────────────────");
    0
}

// ---------------------------------------------------------------------------
// Welcome screen
// ---------------------------------------------------------------------------

fn print_welcome() {
    println!();
    println!("  ██╗     ███████╗███████╗███████╗");
    println!("  ██║     ██╔════╝██╔════╝██╔════╝");
    println!("  ██║     █████╗  █████╗  █████╗  ");
    println!("  ██║     ██╔══╝  ██╔══╝  ██╔══╝  ");
    println!("  ███████╗██║     ██║     ██║     ");
    println!("  ╚══════╝╚═╝     ╚═╝     ╚═╝     ");
    println!();
    println!("  LibreFastbootFirmwareFlasher  v{}", env!("CARGO_PKG_VERSION"));
    println!("  Flash Android firmware via fastboot — free, open, no bloat.");
    println!();
    println!("  Quick start:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  1.  lfff deps                       install tools");
    println!("  2.  lfff download <url>              download OTA zip");
    println!("  3.  lfff extract firmware.zip        unpack images");
    println!("  4.  lfff devices --check             verify setup");
    println!("  5.  lfff flash ./firmwares/<dir>     flash device");
    println!();
    println!("  Commands:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  deps              install & verify external tools");
    println!("  download          download OTA firmware zip");
    println!("  extract           extract .zip into partition images");
    println!("  devices           list devices, run pre-flash checks");
    println!("  arb               compare Anti-Rollback version");
    println!("  flash             flash full firmware (A/B, super)");
    println!("  flash-partition   flash a single partition by name");
    println!("  completions       generate shell completion script");
    println!();
    println!("  Tab completion:");
    println!("    lfff completions bash   > /etc/bash_completion.d/lfff");
    println!("    lfff completions zsh    > /usr/share/zsh/site-functions/_lfff");
    println!("    lfff completions fish   > ~/.config/fish/completions/lfff.fish");
    println!("    lfff completions powershell >> $PROFILE");
    println!();
    println!("  Links:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  GitHub     https://github.com/mrFrok/LibreFastbootFirmwareFlasher");
    println!("  Telegram   https://t.me/gt3neo5hub");
    println!("  Author     https://t.me/mrFrok228");
    println!();
    println!("  -v / --verbose    debug output");
    println!("  <command> --help  command help");
    println!();
    println!("  OnePlus · OPPO · Realme · Qualcomm A/B · Dynamic partitions");
    println!();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();

    // Setup logging
    let log_level = if cli.verbose { "debug" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
        .format_timestamp(None)
        .init();

    let exit_code = match cli.command {
        None => {
            print_welcome();
            0
        }

        Some(Commands::Deps { check, ref tools }) => cmd_deps(check, tools),

        Some(Commands::Download {
            ref url,
            ref output,
            connections,
        }) => cmd_download(url, output.as_ref(), connections),

        Some(Commands::Extract {
            ref zip,
            ref output,
            ref partitions,
            ref checksum,
            list,
        }) => cmd_extract(
            zip,
            output.as_ref(),
            partitions.as_deref(),
            checksum.as_deref(),
            list,
        ),

        Some(Commands::Devices { check, ref serial }) => cmd_devices(check, serial.as_deref()),

        Some(Commands::Arb {
            ref xbl,
            ref firmware_dir,
            device,
            ref serial,
        }) => cmd_arb(
            xbl.as_ref(),
            firmware_dir.as_ref(),
            device,
            serial.as_deref(),
        ),

        Some(Commands::Flash {
            ref firmware_dir,
            ref source,
            ref serial,
            dry_run,
            skip_xbl_abl,
            skip_preloader,
        }) => {
            if let Some(dir) = firmware_dir {
                cmd_flash(&lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()), serial.as_deref(), dry_run, skip_xbl_abl, skip_preloader)
            } else if let Some(dir) = source {
                cmd_flash(&lfff_lib::flasher::FirmwareSource::SourceBuild(dir.clone()), serial.as_deref(), dry_run, skip_xbl_abl, skip_preloader)
            } else {
                println!("✗ Specify a firmware directory or --source DIR");
                eprintln!("Usage: lfff flash <DIR>  or  lfff flash --source <DIR>");
                1
            }
        },

        Some(Commands::Completions { shell }) => {
            let mut cmd: Command = Cli::command();
            generate(shell, &mut cmd, "lfff", &mut std::io::stdout());
            0
        }

        Some(Commands::FlashPartition {
            ref image,
            ref firmware_dir,
            ref partition,
            ref slot,
            no_ab,
            dry_run,
            ref serial,
        }) => cmd_flash_partition(
            image.as_deref(),
            firmware_dir.as_ref(),
            partition.as_deref(),
            slot.as_deref(),
            no_ab,
            dry_run,
            serial.as_deref(),
        ),
    };

    process::exit(exit_code);
}
