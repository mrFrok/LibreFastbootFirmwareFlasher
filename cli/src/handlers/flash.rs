use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_lib::arb::ArbVerdict;
use lfff_lib::flasher::{
    FastbootdStatus, FirmwareSource, FlashOptions, FlashProgress, collect_images,
    collect_images_from_source, fastbootd_status, is_preloader, is_xbl_abl,
    run_flash_session_with_log,
};

use crate::output::{
    interactive, offer_wipe_and_reboot, print_check_report, print_summary, prompt, require_tools,
};

/// Ask the user which flash method to use. Flashing never starts without an
/// explicit choice — there is no platform auto-detection.
/// Returns the method as `--method` spells it.
fn prompt_flash_method() -> Option<&'static str> {
    eprintln!(
        "\n{}",
        "── Flash method ─────────────────────────────────────────".dimmed()
    );
    eprintln!("  Select the flash method for your device:");
    eprintln!();
    eprintln!(
        "  [1] Snapdragon (Qualcomm) — everything in fastbootd, the phone switches slots; ARB check"
    );
    eprintln!("  [2] MediaTek              — everything flashed in fastbootd, xbl/abl skipped");
    eprintln!(
        "  [3] Snapdragon via bootloader — the old way, modem through the bootloader (not recommended)"
    );
    eprintln!("  [q] Abort");

    loop {
        let c = prompt("\n  Choice", "");
        match c.as_str() {
            "1" => return Some("snapdragon"),
            "2" => return Some("mtk"),
            "3" => return Some("snapdragon-bootloader"),
            "q" | "Q" => return None,
            _ => eprintln!("  Enter 1, 2, 3 or q"),
        }
    }
}

/// ARB confirmation gate for Snapdragon firmware. Must be confirmed BEFORE
/// the final flash confirmation. Returns false when the user aborts.
fn arb_gate(dir: &std::path::Path) -> bool {
    eprintln!(
        "\n{}",
        "── ARB (Anti-Rollback) warning ──────────────────────────".dimmed()
    );
    match lfff_lib::arb::firmware_arb(dir) {
        ArbVerdict::Known(v) if v > 0 => {
            eprintln!(
                "  {}",
                format!(
                    "Firmware ARB = {}. Flashing will permanently raise the anti-rollback counter.",
                    v
                )
                .yellow()
            );
            eprintln!(
                "  {}",
                "You will NOT be able to downgrade to firmware with a lower ARB afterwards."
                    .dimmed()
            );
        }
        ArbVerdict::Known(_) => {
            eprintln!(
                "  {}",
                "Firmware ARB = 0, but the device's current ARB level cannot be verified.".yellow()
            );
            eprintln!(
                "  {}",
                "If the device already has ARB > 0, this firmware will not boot.".dimmed()
            );
        }
        ArbVerdict::Unparsed => {
            eprintln!(
                "  {}",
                "Firmware ARB version is unknown (xbl_config could not be parsed).".yellow()
            );
        }
        ArbVerdict::NotFound => {
            eprintln!(
                "  {}",
                "xbl_config.img not found — firmware ARB version is unknown.".yellow()
            );
        }
    }
    let ans = prompt(
        "\n  Type YES to confirm the ARB warning, anything else to abort",
        "",
    );
    ans == "YES"
}

fn confirm_fastbootd_status(serial: Option<&str>) -> bool {
    match fastbootd_status(serial) {
        FastbootdStatus::Fastbootd => true,
        FastbootdStatus::UserspaceFastboot => {
            eprintln!(
                "\n{}",
                "── Fastbootd label warning ──────────────────────────────".dimmed()
            );
            eprintln!(
                "  {}",
                "Device reports userspace fastboot, but it is listed as plain 'fastboot'.".yellow()
            );
            eprintln!("  {}", "This is a known OnePlus / OPPO / Realme issue: fastbootd may be usable even when the mode label does not say fastbootd.".dimmed());
            eprintln!(
                "  {}",
                "Continuing will flash dynamic partitions from this userspace fastboot session."
                    .dimmed()
            );
            let ans = prompt("\n  Continue anyway? (continue/stop)", "stop");
            ans.eq_ignore_ascii_case("continue")
        }
        FastbootdStatus::BootloaderFastboot | FastbootdStatus::UnknownFastboot => {
            eprintln!(
                "\n{}",
                "── Fastbootd warning ────────────────────────────────────".dimmed()
            );
            eprintln!(
                "  {}",
                "Device is not confirmed as fastbootd/userspace fastboot.".yellow()
            );
            eprintln!("  {}", "Dynamic partition flashing may fail unless your device supports flashing them from this mode.".dimmed());
            let ans = prompt("\n  Continue anyway? (continue/stop)", "stop");
            ans.eq_ignore_ascii_case("continue")
        }
        FastbootdStatus::NotFound => {
            eprintln!(
                "{} {}",
                "✗".red().bold(),
                "No fastboot device found. Aborting.".red()
            );
            false
        }
    }
}

pub fn run(
    source: &FirmwareSource,
    serial: Option<&str>,
    method: Option<&str>,
    dry_run: bool,
    skip_xbl_abl: bool,
    skip_preloader: bool,
    rebuild_super: bool,
) -> i32 {
    let dir = source.path();
    if !dir.is_dir() {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            format!("Not a directory: {}", dir.display()).red()
        );
        return 1;
    }

    eprintln!(
        "\n{}",
        "── Dependency check ─────────────────────────────────────".dimmed()
    );
    let ok = require_tools(&["fastboot"]);
    eprintln!(
        "{}\n",
        "────────────────────────────────────────────────────────".dimmed()
    );
    if !ok {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            "fastboot is required for flashing. Aborting.".red()
        );
        return 1;
    }

    let images = if source.is_source() {
        collect_images_from_source(dir)
    } else {
        collect_images(dir)
    };
    if images.is_empty() {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            format!("No .img files found in {}", dir.display()).red()
        );
        return 1;
    }

    // Flashing is confirmed by a person, never by a pipe: without a terminal
    // only a dry run with the method on the command line can go ahead.
    if !interactive() && (!dry_run || method.is_none()) {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            if dry_run {
                "No terminal to ask the flash method: pass --method."
            } else {
                "Flashing asks for confirmation and needs a terminal; run it there (or use --dry-run --method …)."
            }
            .red()
        );
        return 2;
    }

    // -- Mandatory flash-method selection (no auto-detection) --
    let method = match method {
        Some(m) => m,
        None => match prompt_flash_method() {
            Some(m) => m,
            None => {
                eprintln!("{}", "Aborted by user.".yellow());
                return 1;
            }
        },
    };
    let as_mediatek = method == "mtk";
    // The bootloader route only when asked for by name: some phones cannot
    // reach the bootloader, and a flash stopped there can brick them.
    let modem_in_fastbootd = method != "snapdragon-bootloader";

    // Warn on an apparent method/firmware mismatch, but respect the user's choice.
    let has_preloader = images.keys().any(|k| is_preloader(k));
    let has_xbl = images.keys().any(|k| is_xbl_abl(k));
    if !as_mediatek && has_preloader {
        eprintln!(
            "{} {}",
            "⚠".yellow().bold(),
            "preloader.img found — this firmware looks like MediaTek, but Snapdragon was selected."
                .yellow()
        );
    }
    if as_mediatek && has_xbl {
        eprintln!(
            "{} {}",
            "⚠".yellow().bold(),
            "xbl/abl images found — this firmware looks like Qualcomm, but MediaTek was selected."
                .yellow()
        );
    }

    // MediaTek: xbl/abl never exist on the device — always exclude them.
    let skip_xbl_abl = skip_xbl_abl || as_mediatek;

    let mut skip_preloader = skip_preloader;
    if as_mediatek && has_preloader {
        if dry_run {
            eprintln!(
                "{} {}",
                "⚠".yellow().bold(),
                "MediaTek firmware contains preloader.img.".yellow()
            );
            eprintln!(
                "   {}",
                "Use --skip-preloader to exclude it during actual flashing.".dimmed()
            );
        } else if !skip_preloader {
            eprintln!(
                "\n{} {}",
                "⚠".yellow().bold(),
                "Firmware contains preloader.img.".yellow()
            );
            eprintln!(
                "   {}",
                "Flashing preloader via fastboot is risky and may brick your device.".red()
            );
            eprintln!(
                "   {}\n",
                "It is recommended to skip the preloader unless you know what you are doing."
                    .dimmed()
            );
            let input = prompt(
                &format!(
                    "{}",
                    "Skip preloader? [Y/n/a] (Y=skip, n=flash preloader, a=abort)".cyan()
                ),
                "",
            );
            match input.to_lowercase().as_str() {
                "n" | "no" => {
                    eprintln!("{} {}", "→".yellow(), "Will flash preloader.".yellow());
                }
                "a" | "abort" => {
                    eprintln!("{}", "Aborted by user.".yellow());
                    return 1;
                }
                _ => {
                    eprintln!("{} {}", "→".green(), "Skipping preloader.".green());
                    skip_preloader = true;
                }
            }
        }
    }

    let options = FlashOptions {
        dry_run,
        skip_xbl_abl,
        skip_preloader,
        as_mediatek: Some(as_mediatek),
        modem_in_fastbootd,
        // Nothing is copied from the phone here; with the modem in fastbootd
        // the session refuses a skipped partition instead.
        keep_images: Default::default(),
        skip_partitions: String::new(),
        rebuild_super,
    };

    if !dry_run {
        // -- Pre-flash device checks (device present, unlocked, battery, cable) --
        let check = lfff_lib::device::run_pre_flash_checks(serial);
        print_check_report(&check);
        if !check.ready() {
            eprintln!(
                "{} {}",
                "✗".red().bold(),
                "Pre-flash checks failed. Aborting.".red()
            );
            return 1;
        }

        if !confirm_fastbootd_status(serial) {
            eprintln!("{}", "Aborted by user (fastbootd check).".yellow());
            return 1;
        }

        // -- Snapdragon: ARB confirmation FIRST, final confirmation AFTER --
        if !as_mediatek && !source.is_source() && !arb_gate(dir) {
            eprintln!("{}", "Aborted by user (ARB check).".yellow());
            return 1;
        }

        // -- What will be flashed: the same selection the session makes --
        let (selected, held_back) =
            lfff_lib::flasher::select_images(images.clone(), &options, source.is_source());
        let mut names: Vec<&String> = selected.keys().collect();
        names.sort();
        eprintln!(
            "\n{}",
            "── Partitions to flash ──────────────────────────────────".dimmed()
        );
        eprintln!(
            "  {} {}",
            format!("{}:", names.len()).bold(),
            names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (name, reason) in &held_back {
            eprintln!("  {}", format!("Skipping {} ({})", name, reason).yellow());
        }

        // -- Final confirmation --
        eprintln!(
            "\n{}",
            "── Final warning ────────────────────────────────────────".dimmed()
        );
        eprintln!(
            "  {}",
            "All partitions will be overwritten. This action is irreversible.".red()
        );
        eprintln!("  {}", "The device should be in fastbootd/userspace fastboot; any warning above must be accepted.".dimmed());
        let ans = prompt(
            "\n  Type FLASH to start flashing, anything else to abort",
            "",
        );
        if ans != "FLASH" {
            eprintln!("{}", "Aborted by user.".yellow());
            return 1;
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let on_log = |msg: String| eprintln!("{}", msg);

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
        &options,
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
        eprintln!("\n{}", "✓ Flash completed successfully".green().bold());
        0
    } else if session.aborted {
        eprintln!("\n{}", "✗ Flash aborted by user".red().bold());
        1
    } else {
        eprintln!(
            "\n{}",
            "✗ Flash completed with errors — see failed partitions above"
                .red()
                .bold()
        );
        1
    }
}
