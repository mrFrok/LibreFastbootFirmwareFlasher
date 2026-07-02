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
use tracing::{debug, info};

use crate::arb::{ArbInfo, extract_arb_from_xbl, find_xbl_config};
use crate::utils::verify_sha256;
use crate::file_ops::safe_move;

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
        let file_name = img.file_name().ok_or_else(|| anyhow::anyhow!("Image path has no filename: {}", img.display()))?;
        let dest = dest_dir.join(file_name);
        if img.canonicalize().ok() != dest.canonicalize().ok() {
            // Use safe_move which prevents symlink attacks
            safe_move(img, &dest)
                .with_context(|| format!("Failed to move {} to {}", img.display(), dest.display()))?;
        }
        groups.entry(group.to_string()).or_default().push(dest);
    }
    Ok(groups)
}

/// Run payload_dumper (Rust) or payload-dumper-go to extract images.
///
/// payload_dumper accepts ZIP files directly (no need to unpack payload.bin first).
/// payload-dumper-go requires a raw payload.bin path.
fn run_payload_dumper(
    input: &Path,
    output: &Path,
    partitions: Option<&[String]>,
    on_log: Option<&dyn Fn(String)>,
) -> bool {
    let (tool, images_flag) = if which::which("payload_dumper").is_ok() {
        ("payload_dumper", "-i")
    } else if which::which("payload-dumper-go").is_ok() {
        ("payload-dumper-go", "-p")
    } else {
        tracing::error!("No payload dumper found in $PATH. Install: cargo install payload_dumper");
        if let Some(cb) = on_log { cb("ERROR: No payload dumper found. Install payload_dumper.".into()); }
        return false;
    };

    use std::process::Stdio;

    // payload_dumper (Rust binary) uses indicatif which suppresses all output
    // when stdout/stderr are not a TTY — nothing arrives until the process
    // exits, so stdout/stderr are replayed after completion. Live "Extracted:"
    // progress comes from the GUI worker, which watches the output directory.
    let mut cmd = Command::new(tool);
    cmd.arg("-o").arg(output);
    if let Some(parts) = partitions
        && !parts.is_empty() {
            cmd.arg(images_flag).arg(parts.join(","));
        }
    cmd.arg(input);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    debug!("Running: {:?}", cmd);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(cb) = on_log { cb(format!("ERROR: failed to start {}: {}", tool, e)); }
            return false;
        }
    };

    if let Some(cb) = on_log { cb(format!("Running {}...", tool)); }
    let output_result = child.wait_with_output();

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() { break; }
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn is_noise(s: &str) -> bool {
        if s.contains("[00:") { return true; }
        if s.contains("\x1b") { return true; }
        if s == "[" || s == "]" { return true; }
        
        (s.starts_with('*') || s.starts_with('-') || s.starts_with('\\') || s.starts_with('/'))
            && s.contains("Extracting partitions")
    }

    match output_result {
        Ok(out) => {
            let stdout_bytes = out.stdout.len();
            let stderr_bytes = out.stderr.len();
            // Replay stdout then stderr line by line after the process finishes
            let mut shown = 0usize;
            for bytes in [&out.stdout, &out.stderr] {
                let text = String::from_utf8_lossy(bytes);
                for line in text.lines() {
                    let t = strip_ansi(line.trim());
                    if !t.is_empty() && !is_noise(&t)
                        && let Some(cb) = on_log { cb(t); shown += 1; }
                }
            }
            if shown == 0 {
                // payload_dumper wrote nothing useful — show a summary anyway
                if let Some(cb) = on_log {
                    cb(format!("{} finished (stdout: {} B, stderr: {} B, exit: {})",
                        tool, stdout_bytes, stderr_bytes,
                        if out.status.success() { "OK" } else { "FAIL" }));
                }
            }
            out.status.success()
        }
        Err(e) => {
            if let Some(cb) = on_log { cb(format!("ERROR: {}", e)); }
            false
        }
    }
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

/// Available bytes on the filesystem containing `path` (which must exist).
/// statvfs works on both Linux and macOS — GNU `df` flags do not.
#[cfg(unix)]
fn available_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0 {
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Require free space proportional to the archive: payload.bin plus the
/// extracted images is roughly 3× the ZIP, with a small fixed floor. A flat
/// threshold would reject tiny archives on half-full disks.
fn required_free_gb(zip_size_bytes: u64) -> f64 {
    let zip_gb = zip_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    (zip_gb * 3.0).max(0.5)
}

fn check_free_space(path: &Path, required_gb: f64) -> (bool, f64) {
    let mut check = path.to_path_buf();
    while !check.exists() {
        if let Some(p) = check.parent() {
            check = p.to_path_buf();
        } else {
            break;
        }
    }
    if let Some(avail) = available_bytes(&check) {
        let gb = avail as f64 / (1024.0 * 1024.0 * 1024.0);
        return (gb >= required_gb, gb);
    }
    // Cannot determine free space — skip the check rather than block.
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
    extract_firmware_with_log(zip_path, output_dir, checksum, partitions, None)
}

/// Same as [`extract_firmware`] but streams log lines to `on_log` for GUI display.
pub fn extract_firmware_with_log(
    zip_path: &Path,
    output_dir: &Path,
    checksum: Option<&str>,
    partitions: Option<&[String]>,
    on_log: Option<&dyn Fn(String)>,
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

    let required_gb = required_free_gb(fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0));
    let (ok, free_gb) = check_free_space(output_dir, required_gb);
    if !ok {
        return ExtractionResult::fail(
            output_dir,
            &format!(
                "Not enough disk space: {:.1} GB available, ~{:.1} GB needed for this archive",
                free_gb, required_gb
            ),
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
            run_payload_dumper(&zip_path, &staging, partitions, on_log)
        } else {
            // payload-dumper-go needs raw payload.bin — extract it first.
            // Unique per-run temp dir (auto-removed on drop): a fixed /tmp path
            // collides between concurrent runs and is open to symlink attacks.
            info!("Extracting payload.bin for payload-dumper-go ...");
            let tmp = match tempfile::Builder::new().prefix("lfff-extract-").tempdir() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Cannot create temp dir: {}", e);
                    return ExtractionResult::fail(output_dir, &format!("Cannot create temp dir: {}", e));
                }
            };
            let payload_tmp = tmp.path().join("payload.bin");

            let extract_result = (|| -> bool {
                let f = match fs::File::open(&zip_path) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("Failed to open zip for payload extraction: {}", e);
                        return false;
                    }
                };
                let mut z = match zip::ZipArchive::new(f) {
                    Ok(z) => z,
                    Err(e) => {
                        tracing::error!("Failed to open zip archive: {}", e);
                        return false;
                    }
                };
                let mut e = match z.by_name("payload.bin") {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::error!("payload.bin not found in archive: {}", e);
                        return false;
                    }
                };
                let mut o = match fs::File::create(&payload_tmp) {
                    Ok(o) => o,
                    Err(e) => {
                        tracing::error!("Failed to create temp payload.bin: {}", e);
                        return false;
                    }
                };
                if let Err(e) = io::copy(&mut e, &mut o) {
                    tracing::error!("Failed to extract payload.bin: {}", e);
                    return false;
                }
                true
            })();

            if !extract_result {
                false
            } else {
                run_payload_dumper(&payload_tmp, &staging, partitions, on_log)
            }
            // `tmp` (with payload.bin inside) is removed when it drops here.
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
        let f = match fs::File::open(&zip_path) {
            Ok(f) => f,
            Err(e) => return ExtractionResult::fail(output_dir, &format!("Cannot open zip: {}", e)),
        };
        let mut z = match zip::ZipArchive::new(f) {
            Ok(z) => z,
            Err(e) => return ExtractionResult::fail(output_dir, &format!("Invalid zip: {}", e)),
        };
        let mut extracted = Vec::new();
        for i in 0..z.len() {
            let mut e = match z.by_index(i) {
                Ok(e) => e,
                Err(e) => {
                    info!("Skipping entry {} (error: {})", i, e);
                    continue;
                }
            };
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
            let mut o = match fs::File::create(&dest) {
                Ok(o) => o,
                Err(e) => {
                    info!("Skipping {} (cannot create: {})", fname, e);
                    continue;
                }
            };
            if let Err(e) = io::copy(&mut e, &mut o) {
                info!("Skipping {} (copy error: {})", fname, e);
                let _ = fs::remove_file(&dest);
                continue;
            }
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

/// Try to read a human-friendly firmware name from inside the archive.
///
/// Strategy (first match wins):
/// 1. `META-INF/com/android/metadata` — combine `product_name` + numeric part
///    of `version_name`, e.g. `RMX3709TR` + `16.0.2.400` → `RMX3709TR_16.0.2.400`
/// 2. `payload_properties.txt` — `ota_target_version` or `oplus_rom_version`
/// 3. Fallback: ZIP file stem as before.
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

    // ── Strategy 1: META-INF/com/android/metadata ──────────────────────────
    let metadata_props = z
        .by_name("META-INF/com/android/metadata")
        .ok()
        .and_then(|mut e| {
            let mut buf = String::new();
            io::Read::read_to_string(&mut e, &mut buf).ok()?;
            Some(parse_payload_properties(&buf))
        })
        .unwrap_or_default();

    if let (Some(product), Some(version)) = (
        metadata_props.get("product_name"),
        metadata_props.get("version_name"),
    ) {
        let product = product.trim();
        // version_name looks like "RMX3709_16.0.2.400(EX01)" — extract the
        // numeric version after the last underscore, stripping any suffix in parens.
        let numeric = version
            .trim()
            .rsplit('_')
            .next()
            .unwrap_or("")
            .split('(')
            .next()
            .unwrap_or("")
            .trim();

        if !product.is_empty() && !numeric.is_empty() {
            return format!("{}_{}", product, numeric);
        }
        // Fallback within strategy 1: just product_name if version parse failed
        if !product.is_empty() {
            return product.to_string();
        }
    }

    // ── Strategy 2: payload_properties.txt ────────────────────────────────
    let payload_props = z
        .by_name("payload_properties.txt")
        .ok()
        .and_then(|mut e| {
            let mut buf = String::new();
            io::Read::read_to_string(&mut e, &mut buf).ok()?;
            Some(parse_payload_properties(&buf))
        })
        .unwrap_or_default();

    for key in &["ota_target_version", "oplus_rom_version"] {
        if let Some(v) = payload_props.get(*key) {
            let v = v.trim();
            if !v.is_empty() {
                return v.replace(['/', ' '], "_");
            }
        }
    }

    fallback()
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
