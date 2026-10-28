use std::path::PathBuf;

use colored::Colorize;
use lfff_lib::flasher::{collect_images, run_flash_single};

pub fn run(
    image: &str,
    firmware_dir: Option<&PathBuf>,
    partition: Option<&str>,
    slot: Option<&str>,
    no_ab: bool,
    dry_run: bool,
    serial: Option<&str>,
) -> i32 {
    let p = PathBuf::from(image);
    let image_path: PathBuf = if p.extension().map(|e| e == "img").unwrap_or(false) && p.exists() {
        p
    } else {
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
                        eprintln!(
                            "{} {}",
                            "✗".red().bold(),
                            format!("Partition '{}' not found in {}", part_name, dir.display())
                                .red()
                        );
                        eprintln!(
                            "  {} {}",
                            "Available:".dimmed(),
                            format!("{:?}", available).dimmed()
                        );
                        return 1;
                    }
                }
            }
            None => {
                eprintln!(
                    "{} {}",
                    "✗".red().bold(),
                    format!(
                        "'{}' is not a .img file and --firmware-dir is not set.",
                        image
                    )
                    .red()
                );
                return 2;
            }
        }
    };

    let slots: Option<Vec<String>> = if let Some(s) = slot {
        Some(s.split(',').map(|x| x.trim().to_lowercase()).collect())
    } else if no_ab {
        Some(vec![String::new()])
    } else {
        None
    };

    let session = run_flash_single(
        &image_path,
        partition,
        slots.as_deref(),
        serial,
        dry_run,
        &|line| eprintln!("{}", line),
    );

    // run_flash_single aborts on the first failure — diagnose it for the user.
    if let Some(failed) = session.failed().last() {
        crate::output::report_failure(failed);
    }

    // Any failed partition (critical or not) is a failure for scripting.
    // An empty result set outside dry-run means flashing never started
    // (missing image, undetectable active slot, …) — also a failure.
    if session.failed().is_empty() && (dry_run || !session.results.is_empty()) {
        0
    } else {
        1
    }
}
