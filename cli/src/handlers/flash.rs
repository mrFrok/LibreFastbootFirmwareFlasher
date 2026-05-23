use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_lib::flasher::{
    collect_images, collect_images_from_source, is_mediatek_build,
    print_summary, run_flash_session_with_log, FirmwareSource, FlashProgress,
};

pub fn run(
    source: &FirmwareSource,
    serial: Option<&str>,
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
    let ok = lfff_lib::utils::require_tools(&["fastboot"]);
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

    let mut skip_preloader = skip_preloader;

    if !images.is_empty() && is_mediatek_build(&images) {
        if dry_run {
            println!("{} {}", "⚠".yellow().bold(), "Mediatek firmware detected (preloader found).".yellow());
            println!("   {}", "Use --skip-preloader to exclude it during actual flashing.".dimmed());
        } else if !skip_preloader {
            println!("\n{} {}", "⚠".yellow().bold(), "Mediatek firmware detected (preloader found).".yellow());
            println!("   {}", "Flashing preloader on Mediatek devices is risky and may brick your device.".red());
            println!("   {}\n", "It is recommended to skip the preloader unless you know what you are doing.".dimmed());
            print!("{} ", "Skip preloader? [Y/n/a] (Y=skip, n=flash preloader, a=abort):".cyan());
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            match input.trim().to_lowercase().as_str() {
                "" | "y" | "yes" => {
                    println!("{} {}", "→".green(), "Skipping preloader.".green());
                    skip_preloader = true;
                }
                "n" | "no" => {
                    println!("{} {}", "→".yellow(), "Will flash preloader.".yellow());
                }
                "a" | "abort" => {
                    println!("{}", "Aborted by user.".yellow());
                    return 1;
                }
                _ => {
                    println!("{} {}", "→".green(), "Skipping preloader (default).".green());
                    skip_preloader = true;
                }
            }
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
        if p.done == p.total {
            pb2.finish_with_message("Done");
        }
    };

    let session = run_flash_session_with_log(
        source,
        serial,
        dry_run,
        skip_xbl_abl,
        skip_preloader,
        None,
        cancel,
        String::new(),
        &on_log,
        &on_progress,
        &|_, _, _| lfff_lib::flasher::FailureAction::Abort,
    );

    print_summary(&session);

    if session.critical_failed().is_empty() {
        println!("\n{}", "✓ Flash completed successfully".green().bold());
        0
    } else {
        eprintln!("\n{}", "✗ Flash failed — see critical errors above".red().bold());
        1
    }
}
