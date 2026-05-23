use std::sync::atomic::AtomicBool;
use std::sync::Arc;

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
        None,
        cancel,
        String::new(),
        &on_log,
        &on_progress,
        &|_, _, _| lfff_lib::flasher::FailureAction::Abort,
    );

    print_summary(&session);

    if session.critical_failed().is_empty() {
        0
    } else {
        1
    }
}
