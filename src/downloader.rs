//! OTA firmware downloader for OnePlus / OPPO / Realme devices.
//!
//! Resolves OTA download links (including 4PDA redirects) to direct CDN URLs
//! and downloads them via aria2c for maximum speed.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::utils::{require_tools, run_cmd};
use log::{error, info};

const OTA_HEADERS: &[&str] = &[
    "userId: oplus-ota|16002018",
    "User-Agent: okhttp/4.9.2",
    "Accept: application/json",
];

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub success: bool,
    pub url: String,
    pub cdn_url: String,
    pub output_path: Option<PathBuf>,
    pub error: String,
}

impl DownloadResult {
    fn fail(url: &str, error: &str) -> Self {
        Self {
            success: false,
            url: url.into(),
            cdn_url: String::new(),
            output_path: None,
            error: error.into(),
        }
    }
}

/// Unwrap 4PDA redirect to get real OTA endpoint.
fn extract_real_url(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        if parsed
            .host_str()
            .map(|h| h.contains("4pda.to"))
            .unwrap_or(false)
        {
            for (k, v) in parsed.query_pairs() {
                if k == "u" {
                    info!("Unwrapped 4PDA redirect -> {}", v);
                    return v.to_string();
                }
            }
        }
    }
    url.to_string()
}

/// Follow OTA server 302 redirect to get CDN URL.
fn resolve_cdn(url: &str) -> Option<String> {
    let mut parts: Vec<String> = vec![
        "curl".into(),
        "-s".into(),
        "-o".into(),
        "/dev/null".into(),
        "-D".into(),
        "-".into(),
        "--max-redirs".into(),
        "0".into(),
    ];
    for h in OTA_HEADERS {
        parts.push("-H".into());
        parts.push(h.to_string());
    }
    parts.push(url.into());
    let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
    let r = run_cmd(&refs, 30);
    for line in r.stdout.lines() {
        if line.to_lowercase().starts_with("location:") {
            let cdn = line.split_once(':').map(|(_, v)| v.trim().to_string())?;
            info!("CDN URL resolved: {}", cdn);
            return Some(cdn);
        }
    }
    error!("Could not find Location header in response");
    None
}

/// Resolve OTA link and download firmware via aria2c.
pub fn download_firmware(url: &str, output_dir: Option<&Path>, connections: u32) -> DownloadResult {
    if !require_tools(&["aria2c", "curl"]) {
        return DownloadResult::fail(url, "Missing tools: aria2c, curl");
    }

    let real_url = extract_real_url(url);
    println!("\n  OTA endpoint : {}", real_url);
    println!("  Resolving CDN link ...");

    let cdn_url = match resolve_cdn(&real_url) {
        Some(c) => c,
        None => return DownloadResult::fail(&real_url, "Failed to resolve CDN URL"),
    };
    println!("  CDN URL      : {}", cdn_url);

    // Show link expiry if available
    if let Ok(parsed) = url::Url::parse(&cdn_url) {
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        if let Some(ts) = params.get("e") {
            if let Ok(ts) = ts.parse::<u64>() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if ts > now {
                    println!(
                        "  Link expires : in {}h {}m",
                        (ts - now) / 3600,
                        ((ts - now) % 3600) / 60
                    );
                } else {
                    println!("  Link expires : EXPIRED");
                }
            }
        }
    }

    let mut cmd = Command::new("aria2c");
    cmd.arg(format!("-x{}", connections))
        .arg(format!("-s{}", connections))
        .arg("-k")
        .arg("1M")
        .arg("--file-allocation=none")
        .arg("--console-log-level=notice");
    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir).ok();
        cmd.arg("-d").arg(dir);
    }
    cmd.arg(&cdn_url);

    println!("\n  Starting download ({} connections) ...\n", connections);
    let status = match cmd.status() {
        Ok(s) => s,
        Err(e) => {
            return DownloadResult {
                success: false,
                url: real_url,
                cdn_url,
                output_path: None,
                error: format!("aria2c: {}", e),
            };
        }
    };
    if !status.success() {
        return DownloadResult {
            success: false,
            url: real_url,
            cdn_url,
            output_path: None,
            error: format!("aria2c exited {}", status.code().unwrap_or(-1)),
        };
    }

    let filename = cdn_url
        .split('/')
        .last()
        .unwrap_or("firmware.zip")
        .split('?')
        .next()
        .unwrap_or("firmware.zip");
    let out = output_dir
        .map(|d| d.join(filename))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(filename));
    DownloadResult {
        success: true,
        url: real_url,
        cdn_url,
        output_path: if out.exists() { Some(out) } else { None },
        error: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_extract_4pda() {
        assert_eq!(
            extract_real_url("https://4pda.to/goto/?u=https%3A%2F%2Fexample.com%2Ffw.zip"),
            "https://example.com/fw.zip"
        );
    }
    #[test]
    fn test_extract_direct() {
        assert_eq!(
            extract_real_url("https://example.com/fw.zip"),
            "https://example.com/fw.zip"
        );
    }
}
