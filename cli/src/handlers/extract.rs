use std::path::PathBuf;

use colored::Colorize;
use lfff_lib::extractor::{extract_firmware, get_firmware_name};

use crate::output::{print_extraction_result, prompt};

pub fn run(
    zip: &PathBuf,
    output: Option<&PathBuf>,
    partitions: Option<&str>,
    checksum: Option<&str>,
    list_only: bool,
) -> i32 {
    if !zip.exists() {
        eprintln!("{} {}", "✗".red().bold(), format!("File not found: {}", zip.display()).red());
        return 1;
    }

    if list_only {
        let file = match std::fs::File::open(zip) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("✗ Cannot open {}: {}", zip.display(), e);
                return 1;
            }
        };
        let mut archive = match zip::ZipArchive::new(file) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("✗ Not a valid ZIP archive: {}", e);
                return 1;
            }
        };
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
            let raw = prompt(&format!("  Output directory [{}]", default.display()), "");
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
