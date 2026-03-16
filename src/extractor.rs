//! Android firmware extractor — supports payload.bin and raw .img archives.
//!
//! Automatically detects the archive format and extracts images into grouped
//! subdirectories: critical/, bootloader/, radio/, system/, vendor/, other/.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use log::{debug, info};

use crate::arb::{ArbInfo, extract_arb_from_xbl, find_xbl_config};
use crate::utils::verify_sha256;

// ---------------------------------------------------------------------------
// Partition grouping (OnePlus / Qualcomm)
// ---------------------------------------------------------------------------

const PARTITION_GROUPS: &[(&str, &[&str])] = &[
    (
        "critical",
        &[
            "abl",
            "xbl",
            "xbl_config",
            "xbl_ramdump",
            "aop",
            "aop_config",
            "devcfg",
            "shrm",
            "tz",
            "hyp",
            "multiimgoem",
            "multiimgqti",
            "qupfw",
            "uefisecapp",
            "imagefv",
            "cpucp",
            "boot",
            "init_boot",
            "vendor_boot",
            "modem",
        ],
    ),
    (
        "bootloader",
        &["featenabler", "logfs", "storsec", "recovery"],
    ),
    ("radio", &["bluetooth", "dsp", "wifi"]),
    (
        "system",
        &[
            "system",
            "system_ext",
            "system_dlkm",
            "product",
            "odm",
            "odm_dlkm",
        ],
    ),
    ("vendor", &["vendor", "vendor_dlkm"]),
];

fn resolve_group(partition: &str) -> &'static str {
    let name = partition.to_lowercase();
    for (group, patterns) in PARTITION_GROUPS {
        if patterns.iter().any(|&p| name == p || name.starts_with(p)) {
            return group;
        }
    }
    "other"
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExtractionResult {
    pub success: bool,
    pub output_dir: PathBuf,
    pub groups: HashMap<String, Vec<PathBuf>>,
    pub error: String,
    pub payload_properties: HashMap<String, String>,
    pub arb_info: Option<ArbInfo>,
}

impl ExtractionResult {
    pub fn fail(dir: &Path, err: &str) -> Self {
        Self {
            success: false,
            output_dir: dir.to_path_buf(),
            groups: HashMap::new(),
            error: err.to_string(),
            payload_properties: HashMap::new(),
            arb_info: None,
        }
    }
    pub fn all_images(&self) -> Vec<PathBuf> {
        self.groups.values().flatten().cloned().collect()
    }
}

fn parse_payload_properties(data: &str) -> HashMap<String, String> {
    data.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            l.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn move_into_groups(images: &[PathBuf], base: &Path) -> Result<HashMap<String, Vec<PathBuf>>> {
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for img in images {
        let stem = img.file_stem().unwrap_or_default().to_string_lossy();
        let group = resolve_group(&stem);
        let dest_dir = base.join(group);
        fs::create_dir_all(&dest_dir)?;
        let dest = dest_dir.join(img.file_name().unwrap());
        if img.canonicalize().ok() != dest.canonicalize().ok() {
            fs::rename(img, &dest)
                .or_else(|_| {
                    fs::copy(img, &dest)?;
                    fs::remove_file(img)?;
                    Ok::<(), io::Error>(())
                })
                .with_context(|| format!("Failed to move {}", img.display()))?;
        }
        groups.entry(group.to_string()).or_default().push(dest);
    }
    Ok(groups)
}

/// Run payload_dumper (Rust) or payload-dumper-go to extract images.
///
/// payload_dumper accepts ZIP files directly (no need to unpack payload.bin first).
/// payload-dumper-go requires a raw payload.bin path.
fn run_payload_dumper(input: &Path, output: &Path, partitions: Option<&[String]>) -> bool {
    let (tool, images_flag) = if which::which("payload_dumper").is_ok() {
        ("payload_dumper", "-i")
    } else if which::which("payload-dumper-go").is_ok() {
        ("payload-dumper-go", "-p")
    } else {
        log::error!("No payload dumper found in $PATH. Install: cargo install payload_dumper");
        return false;
    };

    let mut cmd = Command::new(tool);
    cmd.arg("-o").arg(output);
    if let Some(parts) = partitions {
        if !parts.is_empty() {
            cmd.arg(images_flag).arg(parts.join(","));
        }
    }
    cmd.arg(input);
    debug!("Running: {:?}", cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Check if payload_dumper (Rust version) is available.
/// It supports ZIP input directly, so we can skip payload.bin extraction.
fn has_payload_dumper_rust() -> bool {
    which::which("payload_dumper").is_ok()
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                result.extend(walkdir(&p));
            } else {
                result.push(p);
            }
        }
    }
    result
}

fn check_free_space(path: &Path) -> (bool, f64) {
    let mut check = path.to_path_buf();
    while !check.exists() {
        if let Some(p) = check.parent() {
            check = p.to_path_buf();
        } else {
            break;
        }
    }
    let r = crate::utils::run_cmd(&["df", "-B1", &check.to_string_lossy()], 5);
    if r.success() {
        if let Some(line) = r.stdout.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let Ok(avail) = parts[3].parse::<u64>() {
                    let gb = avail as f64 / (1024.0 * 1024.0 * 1024.0);
                    return (gb >= 20.0, gb);
                }
            }
        }
    }
    (true, 0.0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract firmware archive into grouped directory structure.
pub fn extract_firmware(
    zip_path: &Path,
    output_dir: &Path,
    checksum: Option<&str>,
    partitions: Option<&[String]>,
) -> ExtractionResult {
    let zip_path = match zip_path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return ExtractionResult::fail(
                output_dir,
                &format!("File not found: {}", zip_path.display()),
            );
        }
    };

    if let Some(expected) = checksum {
        info!("Verifying archive checksum ...");
        match verify_sha256(&zip_path, expected) {
            Ok(true) => {}
            Ok(false) => return ExtractionResult::fail(output_dir, "Checksum verification failed"),
            Err(e) => return ExtractionResult::fail(output_dir, &format!("Checksum error: {}", e)),
        }
    }

    let (ok, free_gb) = check_free_space(output_dir);
    if !ok {
        return ExtractionResult::fail(
            output_dir,
            &format!("Not enough disk space: {:.1} GB available", free_gb),
        );
    }

    fs::create_dir_all(output_dir).ok();
    info!(
        "Extracting {} → {}",
        zip_path.file_name().unwrap_or_default().to_string_lossy(),
        output_dir.display()
    );

    let file = match fs::File::open(&zip_path) {
        Ok(f) => f,
        Err(e) => return ExtractionResult::fail(output_dir, &format!("Cannot open: {}", e)),
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(e) => return ExtractionResult::fail(output_dir, &format!("Not a zip: {}", e)),
    };

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|e| e.name().to_string()))
        .collect();
    let has_payload = names.iter().any(|n| n == "payload.bin");
    let has_img = names.iter().any(|n| n.ends_with(".img"));

    // Parse payload_properties.txt if present
    let props = if names.iter().any(|n| n == "payload_properties.txt") {
        archive
            .by_name("payload_properties.txt")
            .ok()
            .and_then(|mut e| {
                let mut buf = String::new();
                io::Read::read_to_string(&mut e, &mut buf).ok()?;
                Some(parse_payload_properties(&buf))
            })
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    drop(archive);

    if has_payload {
        info!("Format: payload.bin (modern OTA)");
        let staging = output_dir.join("_staging");
        fs::create_dir_all(&staging).ok();

        let ok = if has_payload_dumper_rust() {
            // payload_dumper (Rust) accepts ZIP directly — no need to extract payload.bin
            info!("Using payload_dumper with ZIP input (no unzipping needed)");
            run_payload_dumper(&zip_path, &staging, partitions)
        } else {
            // payload-dumper-go needs raw payload.bin — extract it first
            info!("Extracting payload.bin for payload-dumper-go ...");
            let tmp = std::env::temp_dir().join("lfff_extract");
            fs::create_dir_all(&tmp).ok();
            let payload_tmp = tmp.join("payload.bin");
            let f = fs::File::open(&zip_path).unwrap();
            let mut z = zip::ZipArchive::new(f).unwrap();
            {
                let mut e = z.by_name("payload.bin").unwrap();
                let mut o = fs::File::create(&payload_tmp).unwrap();
                io::copy(&mut e, &mut o).unwrap();
            }
            let result = run_payload_dumper(&payload_tmp, &staging, partitions);
            let _ = fs::remove_file(&payload_tmp);
            let _ = fs::remove_dir(&tmp);
            result
        };

        if !ok {
            return ExtractionResult::fail(output_dir, "payload dumper failed");
        }
        let imgs: Vec<PathBuf> = walkdir(&staging)
            .into_iter()
            .filter(|p| p.extension().map(|e| e == "img").unwrap_or(false))
            .collect();
        let groups = match move_into_groups(&imgs, output_dir) {
            Ok(g) => g,
            Err(e) => return ExtractionResult::fail(output_dir, &e.to_string()),
        };
        let _ = fs::remove_dir(&staging);
        let arb = find_xbl_config(output_dir).map(|p| extract_arb_from_xbl(&p));
        return ExtractionResult {
            success: true,
            output_dir: output_dir.to_path_buf(),
            groups,
            error: String::new(),
            payload_properties: props,
            arb_info: arb,
        };
    }

    if has_img {
        info!("Format: raw .img files");
        let staging = output_dir.join("_staging");
        fs::create_dir_all(&staging).ok();
        let f = fs::File::open(&zip_path).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut extracted = Vec::new();
        for i in 0..z.len() {
            let mut e = z.by_index(i).unwrap();
            let name = e.name().to_string();
            if !name.ends_with(".img") {
                continue;
            }
            let fname = Path::new(&name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let dest = staging.join(&fname);
            let mut o = fs::File::create(&dest).unwrap();
            io::copy(&mut e, &mut o).unwrap();
            info!("  Extracted: {}", fname);
            extracted.push(dest);
        }
        let groups = match move_into_groups(&extracted, output_dir) {
            Ok(g) => g,
            Err(e) => return ExtractionResult::fail(output_dir, &e.to_string()),
        };
        let _ = fs::remove_dir(&staging);
        let arb = find_xbl_config(output_dir).map(|p| extract_arb_from_xbl(&p));
        return ExtractionResult {
            success: true,
            output_dir: output_dir.to_path_buf(),
            groups,
            error: String::new(),
            payload_properties: HashMap::new(),
            arb_info: arb,
        };
    }

    ExtractionResult::fail(
        output_dir,
        "Archive contains neither payload.bin nor .img files",
    )
}

/// Try to read firmware name from payload_properties.txt inside the archive.
pub fn get_firmware_name(zip_path: &Path) -> String {
    let fallback = || {
        zip_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    };
    let f = match fs::File::open(zip_path) {
        Ok(f) => f,
        Err(_) => return fallback(),
    };
    let mut z = match zip::ZipArchive::new(f) {
        Ok(a) => a,
        Err(_) => return fallback(),
    };
    let props = z
        .by_name("payload_properties.txt")
        .ok()
        .and_then(|mut e| {
            let mut buf = String::new();
            io::Read::read_to_string(&mut e, &mut buf).ok()?;
            Some(parse_payload_properties(&buf))
        })
        .unwrap_or_default();
    for key in &["ota_target_version", "oplus_rom_version"] {
        if let Some(v) = props.get(*key) {
            let v = v.trim();
            if !v.is_empty() {
                return v.replace('/', "_").replace(' ', "_");
            }
        }
    }
    fallback()
}

/// Print extraction result summary to stdout.
pub fn print_extraction_result(result: &ExtractionResult) {
    if !result.success {
        println!("\n✗ Extraction failed: {}", result.error);
        return;
    }
    println!("\n✓ Extracted to: {}", result.output_dir.display());
    let mut sorted: Vec<_> = result.groups.iter().collect();
    sorted.sort_by_key(|(k, _)| (*k).clone());
    for (group, images) in &sorted {
        println!("\n  {}/", group);
        let mut imgs: Vec<&PathBuf> = images.iter().collect();
        imgs.sort();
        for img in imgs {
            let mb = fs::metadata(img)
                .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                .unwrap_or(0.0);
            println!(
                "    {:<45} {:>7.1} MB",
                img.file_name().unwrap_or_default().to_string_lossy(),
                mb
            );
        }
    }
    println!("\n  Total: {} image(s)", result.all_images().len());
    if let Some(arb) = &result.arb_info {
        println!("\n── ARB ──────────────────────────────────────────────────");
        println!("  {}", arb);
        if arb.enforced() {
            println!("  ⚠  Hard ARB is ACTIVE on this firmware.");
        }
        println!("────────────────────────────────────────────────────────");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_resolve_group() {
        assert_eq!(resolve_group("xbl_config"), "critical");
        assert_eq!(resolve_group("bluetooth"), "radio");
        assert_eq!(resolve_group("unknown_xyz"), "other");
    }
    #[test]
    fn test_parse_props() {
        let p = parse_payload_properties("FILE_HASH=abc\n# comment\nFILE_SIZE=123");
        assert_eq!(p.get("FILE_HASH"), Some(&"abc".to_string()));
        assert!(!p.contains_key("# comment"));
    }
}
