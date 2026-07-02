use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_lib::flasher::{
    collect_images, collect_images_from_source, is_preloader, is_xbl_abl,
    run_flash_session_with_log, FirmwareSource, FlashOptions, FlashProgress,
};

use crate::output::{offer_wipe_and_reboot, print_check_report, print_summary, prompt, require_tools};

/// Ask the user which flash method to use. Flashing never starts without an
/// explicit choice — there is no platform auto-detection.
fn prompt_flash_method() -> Option<bool> {
    println!("\n{}", "── Flash method ─────────────────────────────────────────".dimmed());
    println!("  Select the flash method for your device:");
    println!();
    println!("  [1] Snapdragon (Qualcomm) — modem flashed in bootloader mode, ARB check");
    println!("  [2] MediaTek              — everything flashed in fastbootd, xbl/abl skipped");
    println!("  [q] Abort");

    loop {
        let c = prompt("\n  Choice", "");
        match c.as_str() {
            "1" => return Some(false),
            "2" => return Some(true),
            "q" | "Q" => return None,
            _ => println!("  Enter 1, 2 or q"),
        }
    }
}

/// ARB confirmation gate for Snapdragon firmware. Must be confirmed BEFORE
/// the final flash confirmation. Returns false when the user aborts.
fn arb_gate(dir: &std::path::Path) -> bool {
    println!("\n{}", "── ARB (Anti-Rollback) warning ──────────────────────────".dimmed());
    match lfff_lib::arb::find_xbl_config(dir) {
        Some(xbl) => {
            let arb = lfff_lib::arb::extract_arb_from_xbl(&xbl);
            match arb.version {
                Some(v) if v > 0 => {
                    println!(
                        "  {}",
                        format!("Firmware ARB = {}. Flashing will permanently raise the anti-rollback counter.", v)
                            .yellow()
                    );
                    println!("  {}", "You will NOT be able to downgrade to firmware with a lower ARB afterwards.".dimmed());
                }
                Some(_) => {
                    println!("  {}", "Firmware ARB = 0, but the device's current ARB level cannot be verified.".yellow());
                    println!("  {}", "If the device already has ARB > 0, this firmware will not boot.".dimmed());
                }
                None => {
                    println!("  {}", "Firmware ARB version is unknown (xbl_config could not be parsed).".yellow());
                }
            }
        }
        None => {
            println!("  {}", "xbl_config.img not found — firmware ARB version is unknown.".yellow());
        }
    }
    let ans = prompt("\n  Type YES to confirm the ARB warning, anything else to abort", "");
    ans == "YES"
}

pub fn run(
    source: &FirmwareSource,
    serial: Option<&str>,
    method: Option<&str>,
    dry_run: bool,
    skip_xbl_abl: bool,
    skip_preloader: bool,
) -> i32 {
    let dir = source.path();
    if !dir.is_dir() {
        eprintln!("{} {}", "✗".red().bold(), format!("Not a directory: {}", dir.display()).red());
        return 1;
    }

    println!("\n{}", "── Dependency check ─────────────────────────────────────".dimmed());
    let ok = require_tools(&["fastboot"]);
    println!("{}\n", "────────────────────────────────────────────────────────".dimmed());
    if !ok {
        eprintln!("{} {}", "✗".red().bold(), "fastboot is required for flashing. Aborting.".red());
        return 1;
    }

    let images = if source.is_source() {
        collect_images_from_source(dir)
    } else {
        collect_images(dir)
    };
    if images.is_empty() {
        eprintln!("{} {}", "✗".red().bold(), format!("No .img files found in {}", dir.display()).red());
        return 1;
    }

    // -- Mandatory flash-method selection (no auto-detection) --
    let as_mediatek = match method {
        Some("mtk") => true,
        Some(_) => false,
        None => match prompt_flash_method() {
            Some(v) => v,
            None => {
                println!("{}", "Aborted by user.".yellow());
                return 1;
            }
        },
    };

    // Warn on an apparent method/firmware mismatch, but respect the user's choice.
    let has_preloader = images.keys().any(|k| is_preloader(k));
    let has_xbl = images.keys().any(|k| is_xbl_abl(k));
    if !as_mediatek && has_preloader {
        println!(
            "{} {}",
            "⚠".yellow().bold(),
            "preloader.img found — this firmware looks like MediaTek, but Snapdragon was selected.".yellow()
        );
    }
    if as_mediatek && has_xbl {
        println!(
            "{} {}",
            "⚠".yellow().bold(),
            "xbl/abl images found — this firmware looks like Qualcomm, but MediaTek was selected.".yellow()
        );
    }

    // MediaTek: xbl/abl never exist on the device — always exclude them.
    let skip_xbl_abl = skip_xbl_abl || as_mediatek;

    let mut skip_preloader = skip_preloader;
    if as_mediatek && has_preloader {
        if dry_run {
            println!("{} {}", "⚠".yellow().bold(), "MediaTek firmware contains preloader.img.".yellow());
            println!("   {}", "Use --skip-preloader to exclude it during actual flashing.".dimmed());
        } else if !skip_preloader {
            println!("\n{} {}", "⚠".yellow().bold(), "Firmware contains preloader.img.".yellow());
            println!("   {}", "Flashing preloader via fastboot is risky and may brick your device.".red());
            println!("   {}\n", "It is recommended to skip the preloader unless you know what you are doing.".dimmed());
            print!("{} ", "Skip preloader? [Y/n/a] (Y=skip, n=flash preloader, a=abort):".cyan());
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            match input.trim().to_lowercase().as_str() {
                "n" | "no" => {
                    println!("{} {}", "→".yellow(), "Will flash preloader.".yellow());
                }
                "a" | "abort" => {
                    println!("{}", "Aborted by user.".yellow());
                    return 1;
                }
                _ => {
                    println!("{} {}", "→".green(), "Skipping preloader.".green());
                    skip_preloader = true;
                }
            }
        }
    }

    if !dry_run {
        // -- Pre-flash device checks (device present, unlocked, battery, cable) --
        let check = lfff_lib::device::run_pre_flash_checks(serial);
        print_check_report(&check);
        if !check.ready() {
            eprintln!("{} {}", "✗".red().bold(), "Pre-flash checks failed. Aborting.".red());
            return 1;
        }

        // -- Snapdragon: ARB confirmation FIRST, final confirmation AFTER --
        if !as_mediatek && !source.is_source() && !arb_gate(dir) {
            println!("{}", "Aborted by user (ARB check).".yellow());
            return 1;
        }

        // -- Final confirmation --
        println!("\n{}", "── Final warning ────────────────────────────────────────".dimmed());
        println!("  {}", "All partitions will be overwritten. This action is irreversible.".red());
        println!("  {}", "The device must be in fastbootd mode.".dimmed());
        let ans = prompt("\n  Type FLASH to start flashing, anything else to abort", "");
        if ans != "FLASH" {
            println!("{}", "Aborted by user.".yellow());
            return 1;
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let on_log = |msg: String| println!("{}", msg);

    // Progress bar for flash operations
    let pb = Arc::new(ProgressBar::new(0));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("#>-")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ "),
    );
    let pb2 = pb.clone();
    let on_progress = move |p: FlashProgress| {
        if p.total > 0 {
            if pb2.length() == Some(0) || pb2.length().is_none() {
                pb2.set_length(p.total as u64);
            }
            pb2.set_position(p.done as u64);
            pb2.set_message(format!("{}_{}", p.partition, p.slot));
        }
        if p.done == p.total && p.total > 0 {
            pb2.finish_with_message("Done");
        }
    };

    let session = run_flash_session_with_log(
        source,
        serial,
        &FlashOptions {
            dry_run,
            skip_xbl_abl,
            skip_preloader,
            as_mediatek: Some(as_mediatek),
            skip_partitions: String::new(),
        },
        cancel,
        &on_log,
        &on_progress,
        &|_, _, _| lfff_lib::flasher::FailureAction::Abort,
    );

    if dry_run {
        return 0;
    }

    print_summary(&session);

    if !session.aborted && session.failed().is_empty() {
        offer_wipe_and_reboot(&session);
        println!("\n{}", "✓ Flash completed successfully".green().bold());
        0
    } else if session.aborted {
        eprintln!("\n{}", "✗ Flash aborted by user".red().bold());
        1
    } else {
        eprintln!("\n{}", "✗ Flash completed with errors — see failed partitions above".red().bold());
        1
    }
}
