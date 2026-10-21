//! Android firmware extractor — supports payload.bin and raw .img archives.
//!
//! Automatically detects the archive format and extracts images into grouped
//! subdirectories: critical/, bootloader/, radio/, system/, vendor/, other/.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use tracing::{debug, info};

use crate::arb::{ArbInfo, extract_arb_from_xbl, find_xbl_config};
use crate::file_ops::safe_move;
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

/// Left in every directory LFFF extracts into, so a later extraction knows it
/// may clean up after the earlier one there — and leaves alone any directory
/// it did not create, since the CLI lets the user pick any output path.
const EXTRACTION_MARKER: &str = ".lfff-extracted";

/// Written only when an extraction finished, holding the archive's
/// fingerprint. [`EXTRACTION_MARKER`] cannot say that: it is written before
/// extracting, so that even a failed run is cleaned up next time.
const COMPLETE_MARKER: &str = ".lfff-complete";

/// Name, size and modification time of an archive: enough to tell a re-picked
/// archive from a different or re-downloaded one without hashing gigabytes.
pub fn archive_fingerprint(zip_path: &Path) -> Option<String> {
    let meta = fs::metadata(zip_path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let name = zip_path.file_name()?.to_string_lossy();
    Some(format!("{}\n{}\n{}\n", name, meta.len(), mtime))
}

/// Whether the file starts like a ZIP archive. A download can end in an HTML
/// page (an expired link, a redirect page) saved under the firmware's name.
pub fn looks_like_zip(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut magic))
        .is_ok()
        && magic == *b"PK\x03\x04"
}

/// Whether `output_dir` holds a finished extraction of exactly this archive.
pub fn is_extracted(zip_path: &Path, output_dir: &Path) -> bool {
    match (
        archive_fingerprint(zip_path),
        fs::read_to_string(output_dir.join(COMPLETE_MARKER)),
    ) {
        (Some(want), Ok(have)) => want == have,
        _ => false,
    }
}

fn mark_complete(zip_path: &Path, output_dir: &Path) {
    if let Some(fp) = archive_fingerprint(zip_path) {
        fs::write(output_dir.join(COMPLETE_MARKER), fp).ok();
    }
}

/// Firmware folders LFFF extracted under `root`, and their total size.
pub fn extraction_cache(root: &Path) -> (usize, u64) {
    let mut count = 0;
    let mut bytes = 0;
    for dir in cache_dirs(root) {
        count += 1;
        bytes += walkdir(&dir)
            .iter()
            .filter_map(|p| fs::symlink_metadata(p).ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum::<u64>();
    }
    (count, bytes)
}

/// Firmware folders under `root` whose extraction finished, newest first.
pub fn finished_extractions(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = cache_dirs(root)
        .into_iter()
        .filter_map(|d| {
            let t = fs::metadata(d.join(COMPLETE_MARKER))
                .ok()?
                .modified()
                .ok()?;
            Some((t, d))
        })
        .collect();
    dirs.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    dirs.into_iter().map(|(_, d)| d).collect()
}

/// Deletes the firmware folders LFFF extracted under `root`. A folder without
/// [`EXTRACTION_MARKER`] was not made by LFFF and is never touched.
pub fn clear_extraction_cache(root: &Path) -> io::Result<()> {
    for dir in cache_dirs(root) {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn cache_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) && p.join(EXTRACTION_MARKER).is_file()
        })
        .collect()
}

/// Before extracting into `dir` again, drop the `.img` files a previous LFFF
/// extraction left in its own group folders. Flashing collects every image
/// under the directory, so a leftover — say a `persist.img` from an earlier,
/// untrusted archive — would otherwise be flashed alongside the new firmware.
///
/// Only acts on directories carrying [`EXTRACTION_MARKER`], and only on `.img`
/// files directly inside LFFF's own group folders; nothing else is touched.
fn clear_previous_extraction(dir: &Path) -> io::Result<()> {
    if !dir.join(EXTRACTION_MARKER).is_file() {
        return Ok(());
    }
    let groups = PARTITION_GROUPS
        .iter()
        .map(|(g, _)| *g)
        .chain(["other", "_staging"]);
    for group in groups {
        let Ok(entries) = fs::read_dir(dir.join(group)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_img = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("img"));
            // symlink_metadata: a symlink named *.img is removed as a link,
            // never followed.
            let is_file = fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir());
            if is_img && is_file {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

fn move_into_groups(images: &[PathBuf], base: &Path) -> Result<HashMap<String, Vec<PathBuf>>> {
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for img in images {
        let stem = img.file_stem().unwrap_or_default().to_string_lossy();
        let group = resolve_group(&stem);
        let dest_dir = base.join(group);
        fs::create_dir_all(&dest_dir)?;
        let file_name = img
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Image path has no filename: {}", img.display()))?;
        let dest = dest_dir.join(file_name);
        if img.canonicalize().ok() != dest.canonicalize().ok() {
            // Use safe_move which prevents symlink attacks
            safe_move(img, &dest).with_context(|| {
                format!("Failed to move {} to {}", img.display(), dest.display())
            })?;
        }
        groups.entry(group.to_string()).or_default().push(dest);
    }
    Ok(groups)
}

/// Run payload_dumper (Rust) or payload-dumper-go to extract images.
///
/// `wait_with_output`, but polling so that a cancel kills the child. The
/// pipes are drained on threads meanwhile: a child blocked on a full pipe
/// would never exit.
fn wait_cancellable(
    mut child: std::process::Child,
    cancel: Option<&AtomicBool>,
) -> io::Result<std::process::Output> {
    use std::io::Read;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    Ok(std::process::Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// payload_dumper accepts ZIP files directly (no need to unpack payload.bin first).
/// payload-dumper-go requires a raw payload.bin path.
fn run_payload_dumper(
    input: &Path,
    output: &Path,
    partitions: Option<&[String]>,
    on_log: Option<&dyn Fn(String)>,
    cancel: Option<&AtomicBool>,
) -> bool {
    let (tool, images_flag) = if let Some(dumper) = crate::deps::find_payload_dumper() {
        let flag = if dumper.is_rust() { "-i" } else { "-p" };
        (dumper.into_path(), flag)
    } else {
        // A `payload_dumper` may well be in $PATH and still be the wrong
        // program — say so, or the user reinstalls the one they already have.
        let hint = match crate::deps::foreign_payload_dumper() {
            Some(p) => format!(
                "{} is not payload-dumper-rust — LFFF cannot use it. Run 'lfff deps', \
                 or set {}=/path/to/payload_dumper",
                p.display(),
                crate::deps::PDG_ENV_OVERRIDE
            ),
            None => {
                "No payload dumper found in $PATH. Install: cargo install payload_dumper".into()
            }
        };
        tracing::error!("{}", hint);
        if let Some(cb) = on_log {
            cb(format!("ERROR: {}", hint));
        }
        return false;
    };
    let tool_name = tool
        .file_name()
        .unwrap_or(tool.as_os_str())
        .to_string_lossy()
        .into_owned();

    use std::process::Stdio;

    // payload_dumper (Rust binary) uses indicatif which suppresses all output
    // when stdout/stderr are not a TTY — nothing arrives until the process
    // exits, so stdout/stderr are replayed after completion. Live "Extracted:"
    // progress comes from the GUI worker, which watches the output directory.
    let mut cmd = Command::new(&tool);
    cmd.arg("-o").arg(output);
    if let Some(parts) = partitions
        && !parts.is_empty()
    {
        cmd.arg(images_flag).arg(parts.join(","));
    }
    cmd.arg(input);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    debug!("Running: {:?}", cmd);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Some(cb) = on_log {
                cb(format!("ERROR: failed to start {}: {}", tool.display(), e));
            }
            return false;
        }
    };

    if let Some(cb) = on_log {
        cb(format!("Running {}...", tool_name));
    }
    let output_result = wait_cancellable(child, cancel);

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn is_noise(s: &str) -> bool {
        if s.contains("[00:") {
            return true;
        }
        if s.contains("\x1b") {
            return true;
        }
        if s == "[" || s == "]" {
            return true;
        }

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
                    if !t.is_empty()
                        && !is_noise(&t)
                        && let Some(cb) = on_log
                    {
                        cb(t);
                        shown += 1;
                    }
                }
            }
            if shown == 0 {
                // payload_dumper wrote nothing useful — show a summary anyway
                if let Some(cb) = on_log {
                    cb(format!(
                        "{} finished (stdout: {} B, stderr: {} B, exit: {})",
                        tool_name,
                        stdout_bytes,
                        stderr_bytes,
                        if out.status.success() { "OK" } else { "FAIL" }
                    ));
                }
            }
            out.status.success()
        }
        Err(e) => {
            if let Some(cb) = on_log {
                cb(format!("ERROR: {}", e));
            }
            false
        }
    }
}

/// Check if payload_dumper (Rust version) is available.
/// It supports ZIP input directly, so we can skip payload.bin extraction.
fn has_payload_dumper_rust() -> bool {
    crate::deps::find_payload_dumper_rust().is_some()
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
    extract_firmware_cancellable(zip_path, output_dir, checksum, partitions, on_log, None)
}

/// [`extract_firmware_with_log`] that stops, killing payload_dumper, once
/// `cancel` is set. A cancelled run fails with the error "Cancelled" and is
/// never taken for a finished extraction.
pub fn extract_firmware_cancellable(
    zip_path: &Path,
    output_dir: &Path,
    checksum: Option<&str>,
    partitions: Option<&[String]>,
    on_log: Option<&dyn Fn(String)>,
    cancel: Option<&AtomicBool>,
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
    if let Err(e) = clear_previous_extraction(output_dir) {
        return ExtractionResult::fail(
            output_dir,
            &format!("Cannot clear images from a previous extraction: {}", e),
        );
    }
    // Written before extracting, so even a failed run is cleaned up next time.
    fs::write(output_dir.join(EXTRACTION_MARKER), b"").ok();
    fs::remove_file(output_dir.join(COMPLETE_MARKER)).ok();
    info!(
        "Extracting {} → {}",
        zip_path.file_name().unwrap_or_default().to_string_lossy(),
        output_dir.display()
    );

    let file = match fs::File::open(&zip_path) {
        Ok(f) => f,
        Err(e) => return ExtractionResult::fail(output_dir, &format!("Cannot open: {}", e)),
    };
    // The central directory is read only to decide which extraction route to
    // take; payload.bin itself is parsed out of the archive by payload_dumper,
    // which brings its own ZIP reader. Large OTA archives (>4 GB, ZIP64) do
    // trip this one — "Support for multi-disk files is not implemented" turns
    // up on 8 GB OnePlus/Xiaomi packages — so a failure here must not abort an
    // extraction that would have worked.
    let (has_payload, has_img, props) = match zip::ZipArchive::new(file) {
        Ok(mut archive) => {
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

            (has_payload, has_img, props)
        }
        Err(e) => {
            // Only the payload.bin route can survive this: raw .img extraction
            // needs the very reader that just failed.
            if !has_payload_dumper_rust() {
                return ExtractionResult::fail(
                    output_dir,
                    &format!("Cannot read the ZIP index: {}", e),
                );
            }
            tracing::warn!(
                "Cannot read the ZIP index ({}) — passing the archive to payload_dumper as-is",
                e
            );
            if let Some(cb) = on_log {
                cb(format!(
                    "WARNING: cannot read the ZIP index ({}) — letting payload_dumper read the archive directly",
                    e
                ));
            }
            (true, false, HashMap::new())
        }
    };

    if has_payload {
        info!("Format: payload.bin (modern OTA)");
        let staging = output_dir.join("_staging");
        fs::create_dir_all(&staging).ok();

        let ok = if has_payload_dumper_rust() {
            // payload_dumper (Rust) accepts ZIP directly — no need to extract payload.bin
            info!("Using payload_dumper with ZIP input (no unzipping needed)");
            run_payload_dumper(&zip_path, &staging, partitions, on_log, cancel)
        } else {
            // payload-dumper-go needs raw payload.bin — extract it first.
            // Unique per-run temp dir (auto-removed on drop): a fixed /tmp path
            // collides between concurrent runs and is open to symlink attacks.
            info!("Extracting payload.bin for payload-dumper-go ...");
            let tmp = match tempfile::Builder::new().prefix("lfff-extract-").tempdir() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Cannot create temp dir: {}", e);
                    return ExtractionResult::fail(
                        output_dir,
                        &format!("Cannot create temp dir: {}", e),
                    );
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
                run_payload_dumper(&payload_tmp, &staging, partitions, on_log, cancel)
            }
            // `tmp` (with payload.bin inside) is removed when it drops here.
        };

        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return ExtractionResult::fail(output_dir, "Cancelled");
        }
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
        mark_complete(&zip_path, output_dir);
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
            Err(e) => {
                return ExtractionResult::fail(output_dir, &format!("Cannot open zip: {}", e));
            }
        };
        let mut z = match zip::ZipArchive::new(f) {
            Ok(z) => z,
            Err(e) => return ExtractionResult::fail(output_dir, &format!("Invalid zip: {}", e)),
        };
        let mut extracted = Vec::new();
        for i in 0..z.len() {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return ExtractionResult::fail(output_dir, "Cancelled");
            }
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
        mark_complete(&zip_path, output_dir);
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
///
/// The result is always a single safe directory name: every caller joins it
/// onto an output root, and the first two strategies read it from inside an
/// archive the user downloaded — `product_name=../../x` must not escape.
pub fn get_firmware_name(zip_path: &Path) -> String {
    let stem = zip_path.file_stem().unwrap_or_default().to_string_lossy();
    safe_dir_name(&firmware_name_from_archive(zip_path))
        .or_else(|| safe_dir_name(&stem))
        .unwrap_or_else(|| "firmware".to_string())
}

/// Reduce `raw` to a name that stays one directory deep: only ASCII letters,
/// digits, `.`, `_` and `-` survive, everything else (separators included)
/// becomes `_`, and leading dots are dropped so `..` and hidden names cannot
/// come out. `None` if nothing usable is left.
fn safe_dir_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '_') {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// Device codenames an OTA is built for (`pre-device` in
/// META-INF/com/android/metadata, comma-separated when several). Empty when
/// the archive does not say — custom and repacked builds often don't.
pub fn firmware_devices(zip_path: &Path) -> Vec<String> {
    let Ok(f) = fs::File::open(zip_path) else {
        return Vec::new();
    };
    let Ok(mut z) = zip::ZipArchive::new(f) else {
        return Vec::new();
    };
    let Some(props) = z
        .by_name("META-INF/com/android/metadata")
        .ok()
        .and_then(|mut e| {
            let mut buf = String::new();
            io::Read::read_to_string(&mut e, &mut buf).ok()?;
            Some(parse_payload_properties(&buf))
        })
    else {
        return Vec::new();
    };
    props
        .get("pre-device")
        .map(|v| {
            v.split(',')
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn firmware_name_from_archive(zip_path: &Path) -> String {
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

    #[test]
    fn safe_dir_name_keeps_ordinary_firmware_names() {
        for name in [
            "RMX3709_16.0.2.400",
            "RMX3709TR",
            "OnePlus-11_A.15",
            "fw.v2",
        ] {
            assert_eq!(safe_dir_name(name).as_deref(), Some(name), "{name}");
        }
    }

    #[test]
    fn safe_dir_name_never_leaves_one_directory() {
        for (raw, want) in [
            ("../../x", Some("_.._x")),
            ("/home/victim/x", Some("_home_victim_x")),
            ("a/b\\c", Some("a_b_c")),
            ("..", None),
            (".", None),
            ("...hidden", Some("hidden")),
            ("  ", None),
            ("", None),
            ("///", None),
            ("16.0.2.400(EX01)", Some("16.0.2.400_EX01_")),
            ("имя", None),
        ] {
            let got = safe_dir_name(raw);
            assert_eq!(got.as_deref(), want, "{raw:?}");
            if let Some(g) = got {
                assert!(!g.contains('/') && !g.contains('\\') && !g.starts_with('.'));
            }
        }
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, b"x").expect("write");
    }

    #[test]
    fn an_html_page_is_not_taken_for_an_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let page = tmp.path().join("go");
        fs::write(&page, b"<!DOCTYPE html><html>4PDA</html>").unwrap();
        assert!(!looks_like_zip(&page));
        let zip = tmp.path().join("fw.zip");
        fs::write(&zip, b"PK\x03\x04rest").unwrap();
        assert!(looks_like_zip(&zip));
    }

    #[test]
    fn an_extraction_counts_as_cached_only_for_the_same_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("fw.zip");
        fs::write(&zip, b"zip bytes").unwrap();
        let out = tmp.path().join("out");
        fs::create_dir_all(&out).unwrap();
        assert!(!is_extracted(&zip, &out));
        mark_complete(&zip, &out);
        assert!(is_extracted(&zip, &out));
        assert_eq!(finished_extractions(tmp.path()), Vec::<PathBuf>::new());
        touch(&out.join(EXTRACTION_MARKER));
        assert_eq!(finished_extractions(tmp.path()), vec![out.clone()]);
        // A re-downloaded archive of another size is not the same firmware.
        fs::write(&zip, b"other, longer zip bytes").unwrap();
        assert!(!is_extracted(&zip, &out));
    }

    #[test]
    fn clearing_the_cache_spares_folders_lfff_did_not_make() {
        let tmp = tempfile::tempdir().unwrap();
        let ours = tmp.path().join("RMX3709_16");
        let theirs = tmp.path().join("photos");
        fs::create_dir_all(ours.join("system")).unwrap();
        fs::create_dir_all(&theirs).unwrap();
        touch(&ours.join(EXTRACTION_MARKER));
        fs::write(ours.join("system/system.img"), b"1234").unwrap();
        fs::write(theirs.join("a.jpg"), b"x").unwrap();
        let (count, bytes) = extraction_cache(tmp.path());
        assert_eq!(count, 1);
        assert!(bytes >= 4, "{bytes}");
        clear_extraction_cache(tmp.path()).unwrap();
        assert!(!ours.exists());
        assert!(theirs.join("a.jpg").exists());
    }

    #[test]
    fn a_previous_lfff_extraction_is_cleared_of_its_images_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        touch(&d.join(EXTRACTION_MARKER));
        touch(&d.join("other/persist.img"));
        touch(&d.join("critical/XBL.IMG"));
        touch(&d.join("_staging/boot.img"));
        // Not LFFF's to remove: other file types, nested folders, the root.
        touch(&d.join("other/notes.txt"));
        touch(&d.join("other/keep/deep.img"));
        touch(&d.join("mine.img"));
        touch(&d.join("custom/user.img"));

        clear_previous_extraction(d).expect("clear");

        for gone in ["other/persist.img", "critical/XBL.IMG", "_staging/boot.img"] {
            assert!(!d.join(gone).exists(), "{gone} should be removed");
        }
        for kept in [
            "other/notes.txt",
            "other/keep/deep.img",
            "mine.img",
            "custom/user.img",
        ] {
            assert!(d.join(kept).exists(), "{kept} should be kept");
        }
    }

    #[test]
    fn a_directory_lfff_did_not_create_is_never_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        touch(&d.join("system/system.img"));
        touch(&d.join("other/whatever.img"));

        clear_previous_extraction(d).expect("clear");

        assert!(d.join("system/system.img").exists());
        assert!(d.join("other/whatever.img").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_image_is_removed_as_a_link_not_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let d = dir.path();
        let target = outside.path().join("precious.img");
        touch(&target);
        touch(&d.join(EXTRACTION_MARKER));
        fs::create_dir_all(d.join("other")).expect("mkdir");
        std::os::unix::fs::symlink(&target, d.join("other/link.img")).expect("symlink");

        clear_previous_extraction(d).expect("clear");

        assert!(!d.join("other/link.img").exists());
        assert!(target.exists(), "the link's target must survive");
    }

    fn zip_with(dir: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        use std::io::Write;
        let path = dir.join(name);
        let mut w = zip::ZipWriter::new(fs::File::create(&path).expect("create zip"));
        for (entry, body) in files {
            w.start_file(*entry, zip::write::SimpleFileOptions::default())
                .expect("start entry");
            w.write_all(body.as_bytes()).expect("write entry");
        }
        w.finish().expect("finish zip");
        path
    }

    #[test]
    fn firmware_name_from_hostile_metadata_stays_inside_the_output_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Path::new("/out");
        for (meta, label) in [
            (
                "product_name=../../escape\nversion_name=X_1.0\n",
                "relative",
            ),
            ("product_name=/etc/evil\nversion_name=\n", "absolute"),
            ("product_name=..\nversion_name=\n", "dotdot"),
        ] {
            let zip = zip_with(
                dir.path(),
                &format!("{label}.zip"),
                &[("META-INF/com/android/metadata", meta)],
            );
            let name = get_firmware_name(&zip);
            let joined = root.join(&name);
            assert_eq!(joined.parent(), Some(root), "{label}: {name:?}");
            assert!(
                joined.components().all(|c| matches!(
                    c,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )),
                "{label}: {name:?}"
            );
        }
    }

    #[test]
    fn firmware_devices_reads_pre_device() {
        let tmp = tempfile::tempdir().unwrap();
        let z = zip_with(
            tmp.path(),
            "ota.zip",
            &[(
                "META-INF/com/android/metadata",
                "ota-type=AB\npre-device=RE5C3B, RE5C3BL1\nproduct_name=RMX3709\n",
            )],
        );
        assert_eq!(firmware_devices(&z), vec!["RE5C3B", "RE5C3BL1"]);
        let bare = zip_with(tmp.path(), "bare.zip", &[("payload.bin", "x")]);
        assert!(firmware_devices(&bare).is_empty());
    }

    #[test]
    fn firmware_name_reads_real_metadata_and_payload_properties() {
        let dir = tempfile::tempdir().expect("tempdir");
        let meta = zip_with(
            dir.path(),
            "a.zip",
            &[(
                "META-INF/com/android/metadata",
                "product_name=RMX3709TR\nversion_name=RMX3709_16.0.2.400(EX01)\n",
            )],
        );
        assert_eq!(get_firmware_name(&meta), "RMX3709TR_16.0.2.400");

        let props = zip_with(
            dir.path(),
            "b.zip",
            &[("payload_properties.txt", "ota_target_version=RMX 14/ok\n")],
        );
        assert_eq!(get_firmware_name(&props), "RMX_14_ok");

        // Nothing usable inside: the zip's own stem.
        let plain = zip_with(dir.path(), "my-fw.zip", &[("readme.txt", "hi")]);
        assert_eq!(get_firmware_name(&plain), "my-fw");
    }
}
