use std::path::PathBuf;

use lfff_lib::downloader::download_firmware;

pub fn run(url: &str, output: Option<&PathBuf>, connections: u32) -> i32 {
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
