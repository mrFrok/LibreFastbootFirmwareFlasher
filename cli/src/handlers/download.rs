use std::path::PathBuf;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_lib::downloader::{CancelToken, DownloadProgress, download_firmware_with_progress};

/// Only the saved file's path goes to stdout, for
/// `lfff extract "$(lfff download URL)"`; everything else is stderr.
pub fn run(url: &str, output: Option<&PathBuf>, connections: u32, md5: Option<&str>) -> i32 {
    let piped;
    let url = if url == "-" {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() || line.trim().is_empty() {
            eprintln!("{} No link on standard input.", "✗".red().bold());
            return 2;
        }
        piped = line;
        piped.trim()
    } else {
        url
    };
    eprintln!(
        "\n{}",
        "── Firmware download ────────────────────────────────────".dimmed()
    );

    use std::sync::Arc;

    let pb = Arc::new(ProgressBar::new(100));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos:>3}% {msg}")
            .unwrap()
            .progress_chars("#>-")
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ "),
    );
    pb.set_position(0);

    let cancel = CancelToken::new();
    let pb2 = pb.clone();
    let result = download_firmware_with_progress(
        url,
        output.map(|p| p.as_path()),
        connections,
        cancel.clone(),
        move |p: DownloadProgress| {
            if p.percent > 0.0 {
                pb2.set_position(p.percent as u64);
            }
            let msg = if !p.speed.is_empty() && !p.eta.is_empty() {
                let link = if p.link_expires_in.is_empty() {
                    String::new()
                } else {
                    format!("  link {}", p.link_expires_in)
                };
                format!("{}  ETA {}  {}{link}", p.speed, p.eta, p.downloaded)
            } else if !p.raw_line.is_empty() {
                p.raw_line
            } else {
                String::new()
            };
            pb2.set_message(msg);
        },
    );

    pb.finish_and_clear();

    if !result.success {
        eprintln!(
            "\n{} {}",
            "✗".red().bold(),
            format!("Download failed: {}", result.error).red()
        );
        return 1;
    }

    let Some(path) = result.output_path else {
        eprintln!(
            "\n{} {}",
            "✗".red().bold(),
            "Download reported success, but the file is not there.".red()
        );
        return 1;
    };

    if let Some(want) = md5 {
        eprintln!("  {}", "Checking MD5...".dimmed());
        match lfff_lib::utils::verify_md5(&path, want) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "\n{} {}",
                    "✗".red().bold(),
                    format!(
                        "MD5 mismatch: {} is damaged. Delete it and download again.",
                        path.display()
                    )
                    .red()
                );
                return 1;
            }
            Err(e) => {
                eprintln!(
                    "\n{} {}",
                    "✗".red().bold(),
                    format!("Could not check the MD5: {e:#}").red()
                );
                return 1;
            }
        }
    }

    eprintln!("\n{} {}", "✓".green().bold(), "Download complete.".green());
    eprintln!("\n  {}", "Next step:".dimmed());
    eprintln!(
        "    {} {}",
        "lfff extract".cyan(),
        format!("\"{}\"", path.display()).cyan()
    );
    eprintln!(
        "{}",
        "────────────────────────────────────────────────────────".dimmed()
    );
    println!("{}", path.display());
    0
}
