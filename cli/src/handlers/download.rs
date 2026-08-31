use std::path::PathBuf;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_lib::downloader::{CancelToken, DownloadProgress, download_firmware_with_progress};

pub fn run(url: &str, output: Option<&PathBuf>, connections: u32) -> i32 {
    println!(
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
                format!("{}  ETA {}  {}", p.speed, p.eta, p.downloaded)
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

    println!("\n{} {}", "✓".green().bold(), "Download complete.".green());
    if let Some(ref path) = result.output_path {
        println!("  {} {}", "Saved to:".dimmed(), path.display());
        println!("\n  {}", "Next step:".dimmed());
        println!(
            "    {} {}",
            "lfff extract".cyan(),
            format!("\"{}\"", path.display()).cyan()
        );
    }
    println!(
        "{}",
        "────────────────────────────────────────────────────────".dimmed()
    );
    0
}
