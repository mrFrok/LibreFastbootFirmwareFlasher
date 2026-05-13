//! Core flash orchestrator for LibreFastbootFirmwareFlasher.
//!
//! Public API:
//!   - `run_flash_session()` — full firmware flash with A/B slot management
//!   - `run_flash_single()` — flash a single partition image
//!   - `flash_partition()` — low-level single flash primitive
//!   - `FlashSession`, `FlashResult`

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use log::{info, warn};

use crate::arb::{
    ArbInfo, arb_confirmation_gate, compare_arb_versions, extract_arb_from_xbl, find_xbl_config,
};
use crate::device::run_pre_flash_checks;
use crate::utils::run_cmd;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Partitions flashed in bootloader mode (everything else goes through fastbootd).
const BOOTLOADER_MODE_PARTITIONS: &[&str] = &["modem"];

/// Dynamic partitions inside super — flash to active slot only.
const SUPER_PARTITIONS: &[&str] = &[
    "system",
    "system_ext",
    "system_dlkm",
    "product",
    "odm",
    "odm_dlkm",
    "vendor",
    "vendor_dlkm",
    "my_bigball",
    "my_carrier",
    "my_engineering",
    "my_heytap",
    "my_manifest",
    "my_product",
    "my_region",
    "my_stock",
];

/// Critical partitions — failure aborts flash immediately.
const CRITICAL_PARTITIONS: &[&str] = &[
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
];

const SLOTS: &[&str] = &["a", "b"];
const REBOOT_SETTLE_SECS: u64 = 2;
const REBOOT_TIMEOUT_SECS: u64 = 90;
const POLL_INTERVAL_SECS: u64 = 3;

fn is_bootloader_partition(name: &str) -> bool {
    BOOTLOADER_MODE_PARTITIONS.contains(&name)
}

fn is_super_partition(name: &str) -> bool {
    SUPER_PARTITIONS.contains(&name)
}

fn is_critical_partition(name: &str) -> bool {
    CRITICAL_PARTITIONS.contains(&name)
}

pub fn is_xbl_abl(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with("xbl") || lower.starts_with("abl")
}

pub fn is_preloader(name: &str) -> bool {
    name.eq_ignore_ascii_case("preloader")
}

pub fn is_mediatek_build(images: &HashMap<String, PathBuf>) -> bool {
    images.keys().any(|k| is_preloader(k))
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Source of firmware images — regular firmware directory or Android source build output.
#[derive(Debug, Clone)]
pub enum FirmwareSource {
    /// Regular firmware directory (e.g. extracted from ZIP/OTA). ARB check applies.
    Extracted(PathBuf),
    /// Android source build output directory (`out/target/product/*/`). Skips ARB.
    SourceBuild(PathBuf),
}

impl FirmwareSource {
    pub fn path(&self) -> &Path {
        match self {
            FirmwareSource::Extracted(p) => p,
            FirmwareSource::SourceBuild(p) => p,
        }
    }

    pub fn is_source(&self) -> bool {
        matches!(self, FirmwareSource::SourceBuild(_))
    }

    pub fn into_path(self) -> PathBuf {
        match self {
            FirmwareSource::Extracted(p) => p,
            FirmwareSource::SourceBuild(p) => p,
        }
    }
}

/// Current device mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMode {
    System,
    Bootloader,
    Fastbootd,
    Unknown,
}

/// Result of flashing a single partition.
#[derive(Debug, Clone)]
pub struct FlashResult {
    pub partition: String,
    pub slot: String,
    pub success: bool,
    pub error: String,
    pub duration_s: f64,
}

/// A flash session containing all results from a flash operation.
#[derive(Debug, Clone)]
pub struct FlashSession {
    pub firmware_dir: PathBuf,
    pub source: FirmwareSource,
    pub results: Vec<FlashResult>,
    pub serial: Option<String>,
    pub aborted: bool,
    pub dry_run: bool,
}

impl FlashSession {
    pub fn new(source: &FirmwareSource, serial: Option<&str>, dry_run: bool) -> Self {
        Self {
            firmware_dir: source.path().to_path_buf(),
            source: source.clone(),
            results: Vec::new(),
            serial: serial.map(|s| s.to_string()),
            aborted: false,
            dry_run,
        }
    }

    pub fn failed(&self) -> Vec<&FlashResult> {
        self.results.iter().filter(|r| !r.success).collect()
    }

    pub fn succeeded(&self) -> Vec<&FlashResult> {
        self.results.iter().filter(|r| r.success).collect()
    }

    pub fn critical_failed(&self) -> Vec<&FlashResult> {
        self.failed()
            .into_iter()
            .filter(|r| is_critical_partition(&r.partition))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Low-level subprocess helpers (local to flasher, not going through utils)
// ---------------------------------------------------------------------------

fn fastboot_cmd(args: &[&str], timeout: u64) -> (i32, String, String) {
    let mut cmd = vec!["fastboot"];
    cmd.extend_from_slice(args);
    let r = run_cmd(&cmd, timeout);
    (r.code, r.stdout, r.stderr)
}

fn adb_cmd(args: &[&str], timeout: u64) -> (i32, String, String) {
    let mut cmd = vec!["adb"];
    cmd.extend_from_slice(args);
    let r = run_cmd(&cmd, timeout);
    (r.code, r.stdout, r.stderr)
}

// ---------------------------------------------------------------------------
// Device mode detection
// ---------------------------------------------------------------------------

/// Detect device mode via fastboot devices, then adb devices.
pub fn detect_mode(serial: Option<&str>) -> DeviceMode {
    let (rc, out, _) = fastboot_cmd(&["devices"], 10);
    if rc == 0 {
        for line in out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            if let Some(s) = serial {
                if parts[0] != s {
                    continue;
                }
            }
            if parts[1] == "fastbootd" {
                return DeviceMode::Fastbootd;
            }
            if parts[1] == "fastboot" {
                return DeviceMode::Bootloader;
            }
        }
    }

    let (rc, out, _) = adb_cmd(&["devices"], 10);
    if rc == 0 {
        for line in out.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "device" {
                if serial.is_none() || Some(parts[0]) == serial {
                    return DeviceMode::System;
                }
            }
        }
    }

    DeviceMode::Unknown
}

/// Return current active slot ('a' or 'b'). Defaults to 'a'.
pub fn get_active_slot(serial: Option<&str>) -> String {
    let mut args: Vec<&str> = Vec::new();
    let serial_str;
    if let Some(s) = serial {
        serial_str = s.to_string();
        args.push("-s");
        args.push(&serial_str);
    }
    args.extend(&["getvar", "current-slot"]);

    let (_, out, err) = fastboot_cmd(&args, 10);
    let combined = format!("{}\n{}", out, err).to_lowercase();
    for line in combined.lines() {
        if line.contains("current-slot:") {
            let slot = line.split("current-slot:").last().unwrap_or("").trim();
            if slot == "a" || slot == "b" {
                return slot.to_string();
            }
        }
    }
    warn!("Could not detect active slot — defaulting to 'a'");
    "a".to_string()
}

// ---------------------------------------------------------------------------
// Reboot helpers
// ---------------------------------------------------------------------------

/// Poll until device reports 'fastbootd' mode.
fn wait_for_fastbootd(serial: Option<&str>, timeout: u64) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    while Instant::now() < deadline {
        let (_, out, _) = fastboot_cmd(&["devices"], 10);
        for line in out.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            if let Some(s) = serial {
                if parts[0] != s {
                    continue;
                }
            }
            if parts[1] == "fastbootd" {
                return true;
            }
        }
        let remaining = (deadline - Instant::now()).as_secs();
        info!("Waiting for fastbootd ... ({}s)", remaining);
        thread::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
    }
    false
}

/// Poll until any fastboot device appears.
fn wait_for_fastboot(serial: Option<&str>, timeout: u64) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    let mut args: Vec<&str> = Vec::new();
    let serial_str;
    if let Some(s) = serial {
        serial_str = s.to_string();
        args.push("-s");
        args.push(&serial_str);
    }
    args.push("devices");

    while Instant::now() < deadline {
        let (_, out, _) = fastboot_cmd(&args, 10);
        if !out.trim().is_empty() {
            return true;
        }
        let remaining = (deadline - Instant::now()).as_secs();
        info!("Waiting for bootloader ... ({}s)", remaining);
        thread::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS));
    }
    false
}

/// Reboot from fastbootd into bootloader.
pub fn enter_bootloader(serial: Option<&str>) -> bool {
    info!("Rebooting to bootloader ...");
    let mut args: Vec<&str> = Vec::new();
    let serial_str;
    if let Some(s) = serial {
        serial_str = s.to_string();
        args.push("-s");
        args.push(&serial_str);
    }
    args.extend(&["reboot", "bootloader"]);

    let (rc, _, err) = fastboot_cmd(&args, 30);
    if rc != 0 {
        log::error!("fastboot reboot bootloader failed: {}", err);
        return false;
    }
    thread::sleep(std::time::Duration::from_secs(REBOOT_SETTLE_SECS));
    wait_for_fastboot(serial, REBOOT_TIMEOUT_SECS)
}

// ---------------------------------------------------------------------------
// Core flash primitive
// ---------------------------------------------------------------------------

/// Flash image_path to partition on slot using `fastboot --slot <s> flash <p>`.
/// Using `--slot` avoids the double-suffix bug (e.g. abl_a_a).
pub fn flash_partition(
    image_path: &Path,
    partition: &str,
    slot: &str,
    serial: Option<&str>,
) -> FlashResult {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = serial {
        args.push("-s".into());
        args.push(s.into());
    }
    args.push("--slot".into());
    args.push(slot.into());
    args.push("flash".into());
    args.push(partition.into());
    args.push(image_path.to_string_lossy().into());

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let start = Instant::now();
    let (rc, out, err) = fastboot_cmd(&refs, 300);
    let duration = start.elapsed().as_secs_f64();

    if rc == 0 {
        FlashResult {
            partition: partition.into(),
            slot: slot.into(),
            success: true,
            error: String::new(),
            duration_s: duration,
        }
    } else {
        FlashResult {
            partition: partition.into(),
            slot: slot.into(),
            success: false,
            error: if err.is_empty() { out } else { err },
            duration_s: duration,
        }
    }
}

// ---------------------------------------------------------------------------
// Progress bar
// ---------------------------------------------------------------------------

fn print_progress(done: usize, total: usize, partition: &str, slot: &str, elapsed: f64) {
    let pct = if total > 0 { done * 100 / total } else { 0 };
    let bar_w = 24;
    let filled = if total > 0 { bar_w * done / total } else { 0 };
    let bar: String = "█".repeat(filled) + &"░".repeat(bar_w - filled);
    let mins = elapsed as u64 / 60;
    let secs = elapsed as u64 % 60;
    let time_s = if mins > 0 {
        format!("{}m{:02}s", mins, secs)
    } else {
        format!("{}s", secs)
    };
    let label = if slot.is_empty() {
        partition.to_string()
    } else {
        format!("{}_{}", partition, slot)
    };
    print!(
        "\r  [{}] {:>3}%  {}/{}  {}  {:<28}",
        bar, pct, done, total, time_s, label
    );
    io::stdout().flush().ok();
}

/// Flash partition in background thread while animating progress bar.
fn flash_with_progress(
    image_path: &Path,
    partition: &str,
    slot: &str,
    serial: Option<&str>,
    done: usize,
    total: usize,
    flash_start: Instant,
) -> FlashResult {
    let img = image_path.to_path_buf();
    let part = partition.to_string();
    let sl = slot.to_string();
    let ser = serial.map(|s| s.to_string());

    let result: Arc<Mutex<Option<FlashResult>>> = Arc::new(Mutex::new(None));
    let result_clone = result.clone();

    let handle = thread::spawn(move || {
        let r = flash_partition(&img, &part, &sl, ser.as_deref());
        *result_clone.lock().unwrap() = Some(r);
    });

    while !handle.is_finished() {
        print_progress(
            done,
            total,
            partition,
            slot,
            flash_start.elapsed().as_secs_f64(),
        );
        thread::sleep(std::time::Duration::from_millis(150));
    }
    handle.join().ok();

    print_progress(
        done + 1,
        total,
        partition,
        slot,
        flash_start.elapsed().as_secs_f64(),
    );

    result.lock().unwrap().take().unwrap()
}

// ---------------------------------------------------------------------------
// Super partition wipe
// ---------------------------------------------------------------------------

/// For each dynamic partition: delete old entries and COW snapshots,
/// then recreate with size=0.
pub fn wipe_super(serial: Option<&str>, super_names: &[String]) {
    wipe_super_with_log(serial, super_names, &|msg| println!("{}", msg));
}

pub fn wipe_super_with_log(serial: Option<&str>, super_names: &[String], on_log: &dyn Fn(String)) {
    let mut base_args: Vec<String> = vec!["fastboot".into()];
    if let Some(s) = serial {
        base_args.push("-s".into());
        base_args.push(s.into());
    }

    on_log(format!("Preparing {} super partition(s) ...", super_names.len()));
    let base_len = base_args.len();

    for base in super_names {
        let mut cmd = base_args.clone();

        // Delete existing entries + COW snapshots
        for slot in &["a", "b"] {
            let part = format!("{}_{}", base, slot);
            let mut candidates = vec![part.clone()];
            for suffix in &["-cow", "_cow", "-cow-img"] {
                candidates.push(format!("{}{}", part, suffix));
            }
            for cand in &candidates {
                cmd.truncate(base_len);
                cmd.push("delete-logical-partition".into());
                cmd.push(cand.clone());
                let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
                let r = run_cmd(&refs, 15);
                if r.code != 0 {
                    let err_lower = r.stderr.to_lowercase();
                    if !err_lower.contains("does not exist") && !err_lower.contains("no such") {
                        warn!("delete-logical-partition {}: {}", cand, r.stderr);
                    }
                }
            }
        }

        // Recreate with size=0
        for slot in &["a", "b"] {
            let part = format!("{}_{}", base, slot);
            cmd.truncate(base_len);
            cmd.push("create-logical-partition".into());
            cmd.push(part.clone());
            cmd.push("0".into());
            let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let r = run_cmd(&refs, 15);
            if r.code != 0 {
                warn!("create-logical-partition {}: {}", part, r.stderr);
            }
        }
    }

    on_log("Super partitions cleared and recreated with size=0".into());
}

// ---------------------------------------------------------------------------
// Image collection
// ---------------------------------------------------------------------------

/// Scan firmware_dir for .img files.
/// Strips _a/_b suffix so abl_a.img -> key "abl" (prevents abl_a_a bug).
/// Shallower paths win on duplicates.
pub fn collect_images(firmware_dir: &Path) -> HashMap<String, PathBuf> {
    let mut images: HashMap<String, PathBuf> = HashMap::new();
    let mut entries: Vec<PathBuf> = Vec::new();

    // Recursively collect all .img files
    fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    collect_recursive(&p, out);
                } else if p.extension().map(|e| e == "img").unwrap_or(false) {
                    out.push(p);
                }
            }
        }
    }

    collect_recursive(firmware_dir, &mut entries);
    // Sort by path depth (shallower first)
    entries.sort_by_key(|p| p.components().count());

    for img in entries {
        let mut stem = img
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        for suffix in &["_a", "_b"] {
            if stem.ends_with(suffix) {
                stem = stem[..stem.len() - suffix.len()].to_string();
                break;
            }
        }
        images.entry(stem).or_insert(img);
    }

    images
}

/// Scan a source build directory for flashable .img files.
/// Applies Android build output filtering — ignores debug images, test images,
/// temporary files, metadata, and build artifacts. Exception: `vendor_ramdump.img`
/// is NOT ignored.
pub fn collect_images_from_source(dir: &Path) -> HashMap<String, PathBuf> {
    fn is_ignored_file(name: &str) -> bool {
        let lower = name.to_lowercase();
        if lower == "vendor_ramdump.img" { return false; }
        lower.ends_with("-debug.img")
            || lower.ends_with("-test-harness.img")
            || lower.contains("-test")
            || lower.contains("_harness")
            || lower.starts_with("ramdisk")
            || lower == "super_empty.img"
            || lower == "dtb.img"
            || lower == "vendor-bootconfig.img"
            || lower == "vendor_ramdisk.img"
            || lower.starts_with("ota_metadata")
            || lower.ends_with(".pb")
    }

    let mut images: HashMap<String, PathBuf> = HashMap::new();
    let mut entries: Vec<PathBuf> = Vec::new();

    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().map(|e| e == "img").unwrap_or(false) {
                let fname = p.file_name().unwrap_or_default().to_string_lossy();
                if !is_ignored_file(&fname) {
                    entries.push(p);
                }
            }
        }
    }

    entries.sort();

    for img in entries {
        let mut stem = img.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        for suffix in &["_a", "_b"] {
            if stem.ends_with(suffix) {
                stem = stem[..stem.len() - suffix.len()].to_string();
                break;
            }
        }
        images.entry(stem).or_insert(img);
    }

    images
}

/// Check whether `fastboot getvar partition-size:<name>` returns a valid size.
/// Returns `false` for non-existent partitions (so we skip them silently).
fn device_has_partition(serial: Option<&str>, name: &str) -> bool {
    let mut args: Vec<&str> = Vec::new();
    let serial_str;
    if let Some(s) = serial {
        serial_str = s.to_string();
        args.push("-s");
        args.push(&serial_str);
    }
    let var = format!("partition-size:{}", name);
    args.push("getvar");
    args.push(&var);
    let (code, out, err) = fastboot_cmd(&args, 5);
    if code != 0 { return false; }
    let combined = format!("{} {}", out, err).to_lowercase();
    !combined.contains("not found")
}

// ---------------------------------------------------------------------------
// Error reporting and interactive handler
// ---------------------------------------------------------------------------

fn report_failure(result: &FlashResult) {
    let is_crit = is_critical_partition(&result.partition);
    let err_lower = result.error.to_lowercase();

    println!();
    println!("{}", "━".repeat(60));
    println!("  ✗  FAILED  {}_{}", result.partition, result.slot);
    println!("  {}", result.error);
    println!();

    if err_lower.contains("resize") || err_lower.contains("not enough space") {
        println!("  Cause: Dynamic partition resize failed.");
        println!("  Fix  : Make sure the device is in fastbootd and retry.");
    } else if err_lower.contains("does not exist") || err_lower.contains("not found") {
        println!("  Cause: Partition not present on this device.");
        println!("  Fix  : This image may not be compatible with your device variant.");
    } else if err_lower.contains("permission denied") || err_lower.contains("not allowed") {
        println!("  Cause: Bootloader is locked.");
        println!("  Fix  : fastboot flashing unlock");
    } else if err_lower.contains("timeout") {
        println!("  Cause: USB timeout.");
        println!("  Fix  : Try a different cable or USB 3.0 port.");
    } else {
        println!("  Possible causes:");
        println!("    • Faulty USB cable — try a different one");
        println!("    • Bootloader is locked  →  fastboot flashing unlock");
        println!("    • Corrupted image — re-download the firmware");
        println!("    • Low battery during flash");
    }

    if is_crit {
        println!();
        println!("  ⚠  CRITICAL partition — do NOT reboot or unplug until resolved.");
    }
    println!("{}", "━".repeat(60));
    println!();
}

#[derive(Debug, Clone)]
enum ErrorAction { Retry, Skip, Abort }

/// Ask user what to do after a flash failure.
fn on_flash_error(result: &FlashResult, serial: Option<&str>, target_mode: DeviceMode) -> ErrorAction {
    report_failure(result);

    let (mode_label, reboot_args) = match target_mode {
        DeviceMode::Bootloader => ("bootloader", vec!["reboot", "bootloader"]),
        _ => ("fastbootd", vec!["reboot", "fastboot"]),
    };

    println!("  What do you want to do?");
    println!("  [1] Retry this partition now");
    println!("  [2] Reboot to {} first, then retry", mode_label);
    println!("  [3] Skip this partition and continue");
    println!("  [4] Abort flashing");

    loop {
        let choice = crate::utils::prompt("\n  Choice", "");
        match choice.as_str() {
            "3" => return ErrorAction::Skip,
            "4" => return ErrorAction::Abort,
            "1" => return ErrorAction::Retry,
            "2" => {
                println!();
                println!("  Rebooting to {} ...", mode_label);
                let mut args: Vec<&str> = Vec::new();
                let serial_str;
                if let Some(s) = serial {
                    serial_str = s.to_string();
                    args.push("-s");
                    args.push(&serial_str);
                }
                args.extend(reboot_args.iter());
                let (rc, out, err) = fastboot_cmd(&args, 30);
                if rc != 0 {
                    println!(
                        "  ✗ Reboot command failed: {}",
                        if err.is_empty() { &out } else { &err }
                    );
                    println!("  Reboot manually, then press [1] to retry.");
                    continue;
                }
                println!("  Waiting for {} ...", mode_label);
                let ok = match target_mode {
                    DeviceMode::Bootloader => wait_for_fastboot(serial, REBOOT_TIMEOUT_SECS),
                    _ => wait_for_fastbootd(serial, REBOOT_TIMEOUT_SECS),
                };
                if ok {
                    println!("  ✓ Device is in {} — retrying ...", mode_label);
                    return ErrorAction::Retry;
                } else {
                    println!("  ✗ Device did not enter {}.", mode_label);
                    println!("  Try rebooting manually, then press [1] to retry.");
                }
            }
            _ => println!("  Enter 1, 2, 3 or 4"),
        }
    }
}

// ---------------------------------------------------------------------------
// Full flash session
// ---------------------------------------------------------------------------

/// Full firmware flash orchestrator.
///
/// Stages:
///   1. Pre-flash checks
///   2. Collect images
///   3. ARB check
///   4. Ask user how to reach fastbootd
///   5. Stage 1: fastbootd — non-super (both slots) + super (active slot)
///   6. Stage 2: bootloader — modem (both slots)
///   7. Summary, wipe offer, reboot
pub fn run_flash_session(source: &FirmwareSource, serial: Option<&str>, dry_run: bool) -> FlashSession {
    let firmware_dir = source.path();
    let mut session = FlashSession::new(source, serial, dry_run);

    // -- Pre-flash checks --
    info!("==> Running pre-flash checks ...");
    let check = run_pre_flash_checks(serial);
    if !check.ready() {
        println!("\n✗ Pre-flash checks failed. Aborting.\n");
        for err in &check.errors {
            println!("  ✗ {}", err);
        }
        std::process::exit(1);
    }

    let serial = serial.map(|s| s.to_string()).or_else(|| {
        if !check.device_info.serial.is_empty() {
            Some(check.device_info.serial.clone())
        } else {
            None
        }
    });
    session.serial = serial.clone();

    // -- Collect images --
    let images = if source.is_source() {
        collect_images_from_source(firmware_dir)
    } else {
        collect_images(firmware_dir)
    };
    if images.is_empty() {
        if source.is_source() {
            println!("✗ No flashable .img files found in source build directory: {}", firmware_dir.display());
        } else {
            println!("✗ No .img files found in {}", firmware_dir.display());
        }
        std::process::exit(1);
    }

    let fastbootd_images: HashMap<&str, &PathBuf> = images
        .iter()
        .filter(|(k, _)| !is_bootloader_partition(k))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let bootloader_images: HashMap<&str, &PathBuf> = images
        .iter()
        .filter(|(k, _)| is_bootloader_partition(k))
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    println!("\nFound {} image(s) to flash:", images.len());
    let mut sorted: Vec<_> = images.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    for (name, path) in &sorted {
        let mode_tag = if is_bootloader_partition(name) {
            "bootloader"
        } else {
            "fastbootd"
        };
        let crit_tag = if is_critical_partition(name) {
            " [CRITICAL]"
        } else {
            ""
        };
        let size_mb = fs::metadata(path)
            .map(|m| m.len() as f64 / 1024.0 / 1024.0)
            .unwrap_or(0.0);
        println!(
            "  {:<30} {:>7.1} MB  ({}){}",
            name, size_mb, mode_tag, crit_tag
        );
    }

    // -- ARB check (firmware only, skip for source builds) --
    if !source.is_source() {
        if let Some(xbl_path) = find_xbl_config(firmware_dir) {
            let firmware_arb = extract_arb_from_xbl(&xbl_path);
            let device_arb = ArbInfo {
                version: None,
                source: "not checked".into(),
                oem_major: None,
                oem_minor: None,
            };
            let arb_result = compare_arb_versions(&firmware_arb, &device_arb);
            if !arb_confirmation_gate(&arb_result, "none") {
                println!("Aborted by user (ARB check).");
                std::process::exit(0);
            }
        } else {
            warn!("xbl_config.img not found in firmware — ARB check skipped");
        }
    }

    if dry_run {
        println!("\n[dry-run] No partitions were flashed.");
        return session;
    }

    // -- Ask user how to reach fastbootd --
    let serial_ref = serial.as_deref();
    if !fastbootd_images.is_empty() {
        println!();
        println!("── Reboot to fastbootd ──────────────────────────────────");
        println!("  Where is the device right now?");
        println!();
        println!("  [1] In system (Android running)  → adb reboot fastboot");
        println!("  [2] In bootloader                → fastboot reboot fastboot");
        println!("  [3] Already in fastbootd          → skip reboot");
        println!("  [q] Abort");

        let choice = loop {
            let c = crate::utils::prompt("\n  Choice", "");
            if c == "q" {
                println!("Aborted.");
                std::process::exit(0);
            }
            if ["1", "2", "3"].contains(&c.as_str()) {
                break c;
            }
            println!("  Enter 1, 2, 3 or q");
        };

        match choice.as_str() {
            "1" => {
                println!();
                let mut args: Vec<&str> = vec!["adb"];
                let serial_str2;
                if let Some(s) = serial_ref {
                    serial_str2 = s.to_string();
                    args.push("-s");
                    args.push(&serial_str2);
                }
                args.extend(&["reboot", "fastboot"]);
                let r = run_cmd(&args, 15);
                if r.code != 0 {
                    println!("✗ adb reboot fastboot failed: {}", r.stderr);
                    std::process::exit(1);
                }
                println!("  Waiting for fastbootd ...");
                if !wait_for_fastbootd(serial_ref, REBOOT_TIMEOUT_SECS) {
                    println!("✗ Device did not enter fastbootd. Aborting.");
                    std::process::exit(1);
                }
                println!("  ✓ Device is in fastbootd");
            }
            "2" => {
                println!();
                let mut args: Vec<&str> = Vec::new();
                let serial_str2;
                if let Some(s) = serial_ref {
                    serial_str2 = s.to_string();
                    args.push("-s");
                    args.push(&serial_str2);
                }
                args.extend(&["reboot", "fastboot"]);
                let (rc, _, err) = fastboot_cmd(&args, 30);
                if rc != 0 {
                    println!("✗ fastboot reboot fastboot failed: {}", err);
                    std::process::exit(1);
                }
                println!("  Waiting for fastbootd ...");
                if !wait_for_fastbootd(serial_ref, REBOOT_TIMEOUT_SECS) {
                    println!("✗ Device did not enter fastbootd. Aborting.");
                    std::process::exit(1);
                }
                println!("  ✓ Device is in fastbootd");
            }
            _ => {} // "3" — already in fastbootd
        }

        println!("────────────────────────────────────────────────────────");
    }

    println!();
    let _ = crate::utils::prompt(
        "Device is in fastbootd. Press Enter to begin flashing, or Ctrl+C to abort",
        "",
    );

    // -- Stage 1: fastbootd --
    if !fastbootd_images.is_empty() {
        let active_slot = get_active_slot(serial_ref);
        let super_imgs: HashMap<&&str, &&PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| is_super_partition(k))
            .collect();
        let non_super_imgs: HashMap<&&str, &&PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| !is_super_partition(k))
            .collect();

        let total_ops = non_super_imgs.len() * 2 + super_imgs.len();
        let flash_start = Instant::now();
        let mut done_ops = 0;

        println!("\n── Stage 1/2: fastbootd ──────────────────────────────");
        println!("  Active slot    : {}", active_slot.to_uppercase());
        println!(
            "  Non-super      : {} partitions × 2 slots",
            non_super_imgs.len()
        );
        println!(
            "  Super (dynamic): {} partitions × 1 slot (active only)",
            super_imgs.len()
        );
        println!("  Total          : {} flash operations", total_ops);
        println!();

        // Non-super → both slots
        let mut sorted_non_super: Vec<_> = non_super_imgs.iter().collect();
        sorted_non_super.sort_by_key(|(k, _)| *k);

        for slot in SLOTS {
            for (partition, image_path) in &sorted_non_super {
                let result = flash_with_progress(
                    image_path,
                    partition,
                    slot,
                    serial_ref,
                    done_ops,
                    total_ops,
                    flash_start,
                );
                session.results.push(result.clone());
                done_ops += 1;
                if !result.success {
                    println!();
                    match on_flash_error(&result, serial_ref, DeviceMode::Fastbootd) {
                        ErrorAction::Skip => { continue; }
                        ErrorAction::Abort => { session.aborted = true; return session; }
                        ErrorAction::Retry => {}
                    }
                    let retry = flash_with_progress(
                        image_path,
                        partition,
                        slot,
                        serial_ref,
                        done_ops - 1,
                        total_ops,
                        flash_start,
                    );
                    *session.results.last_mut().unwrap() = retry.clone();
                    if !retry.success {
                        println!("\n✗ Retry failed. Aborting.");
                        return session;
                    }
                }
            }
        }

        // Super → active slot only
        if !super_imgs.is_empty() {
            println!("\n\n── Clearing super partition ─────────────────────────────");
            let super_names: Vec<String> = super_imgs.keys().map(|k| k.to_string()).collect();
            wipe_super(serial_ref, &super_names);
            println!();

            let mut sorted_super: Vec<_> = super_imgs.iter().collect();
            sorted_super.sort_by_key(|(k, _)| *k);

            for (partition, image_path) in &sorted_super {
                let result = flash_with_progress(
                    image_path,
                    partition,
                    &active_slot,
                    serial_ref,
                    done_ops,
                    total_ops,
                    flash_start,
                );
                session.results.push(result.clone());
                done_ops += 1;
                if !result.success {
                    println!();
                    match on_flash_error(&result, serial_ref, DeviceMode::Fastbootd) {
                        ErrorAction::Skip => { continue; }
                        ErrorAction::Abort => { session.aborted = true; return session; }
                        ErrorAction::Retry => {}
                    }
                    let retry = flash_with_progress(
                        image_path,
                        partition,
                        &active_slot,
                        serial_ref,
                        done_ops - 1,
                        total_ops,
                        flash_start,
                    );
                    *session.results.last_mut().unwrap() = retry.clone();
                    if !retry.success {
                        println!("\n✗ Retry failed. Aborting.");
                        return session;
                    }
                }
            }
        }

        let elapsed = flash_start.elapsed().as_secs();
        println!(
            "\n  ✓ Stage 1 complete in {}m{:02}s",
            elapsed / 60,
            elapsed % 60
        );
    }

    // -- Stage 2: bootloader (modem) --
    if !bootloader_images.is_empty() {
        info!("==> Rebooting to bootloader for modem flash ...");
        if !enter_bootloader(serial_ref) {
            println!("✗ Could not reach bootloader. Modem was not flashed.");
            for (&partition, _) in &bootloader_images {
                for &slot in SLOTS {
                    session.results.push(FlashResult {
                        partition: partition.to_string(),
                        slot: slot.to_string(),
                        success: false,
                        error: "Could not enter bootloader mode".into(),
                        duration_s: 0.0,
                    });
                }
            }
            return session;
        }

        let total_ops2 = bootloader_images.len() * 2;
        let mut done_ops2 = 0;
        let flash_start2 = Instant::now();
        println!(
            "\n── Stage 2/2: bootloader ({} partitions × 2 slots) ──",
            bootloader_images.len()
        );
        println!();

        let mut sorted_bl: Vec<_> = bootloader_images.iter().collect();
        sorted_bl.sort_by_key(|(k, _)| *k);

        for &slot in SLOTS {
            for (partition, image_path) in &sorted_bl {
                let result = flash_with_progress(
                    image_path,
                    partition,
                    slot,
                    serial_ref,
                    done_ops2,
                    total_ops2,
                    flash_start2,
                );
                session.results.push(result.clone());
                done_ops2 += 1;
                if !result.success {
                    println!();
                    match on_flash_error(&result, serial_ref, DeviceMode::Bootloader) {
                        ErrorAction::Skip => { continue; }
                        ErrorAction::Abort => { session.aborted = true; return session; }
                        ErrorAction::Retry => {}
                    }
                    let retry = flash_with_progress(
                        image_path,
                        partition,
                        slot,
                        serial_ref,
                        done_ops2 - 1,
                        total_ops2,
                        flash_start2,
                    );
                    *session.results.last_mut().unwrap() = retry.clone();
                    if !retry.success {
                        println!("\n✗ Retry failed. Aborting.");
                        return session;
                    }
                }
            }
        }

        let elapsed2 = flash_start2.elapsed().as_secs();
        println!(
            "\n  ✓ Stage 2 complete in {}m{:02}s",
            elapsed2 / 60,
            elapsed2 % 60
        );
    }

    session
}

// ---------------------------------------------------------------------------
// Single-partition flash
// ---------------------------------------------------------------------------

/// Flash a single .img file to one or both slots.
pub fn run_flash_single(
    image_path: &Path,
    partition: Option<&str>,
    slots: Option<&[String]>,
    serial: Option<&str>,
    dry_run: bool,
) -> FlashSession {
    let mut session = FlashSession::new(
        &FirmwareSource::Extracted(image_path.parent().unwrap_or(Path::new(".")).to_path_buf()),
        serial,
        dry_run,
    );

    if !image_path.exists() {
        println!("✗ Image not found: {}", image_path.display());
        return session;
    }

    // Determine partition name
    let mut part_name = partition.map(|s| s.to_string()).unwrap_or_else(|| {
        image_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    });
    for suffix in &["_a", "_b"] {
        if part_name.ends_with(suffix) {
            part_name = part_name[..part_name.len() - suffix.len()].to_string();
            break;
        }
    }

    let is_crit = is_critical_partition(&part_name);
    let flash_slots: Vec<String> = slots
        .map(|s| s.to_vec())
        .unwrap_or_else(|| SLOTS.iter().map(|s| s.to_string()).collect());

    let size_mb = fs::metadata(image_path)
        .map(|m| m.len() as f64 / 1024.0 / 1024.0)
        .unwrap_or(0.0);

    println!(
        "\n── Flash single partition: {} ──────────────────",
        part_name
    );
    println!(
        "  Image     : {}  ({:.1} MB)",
        image_path.file_name().unwrap_or_default().to_string_lossy(),
        size_mb
    );
    println!("  Partition : {}", part_name);
    let slot_label = if flash_slots == [""] {
        "non-A/B".to_string()
    } else {
        flash_slots.join(", ")
    };
    println!("  Slots     : {}", slot_label);
    if is_crit {
        println!("  ⚠  CRITICAL partition — do not unplug during flash");
    }
    println!();

    if dry_run {
        for slot in &flash_slots {
            let label = if slot.is_empty() {
                part_name.clone()
            } else {
                format!("{}_{}", part_name, slot)
            };
            println!(
                "  [dry-run] would flash: {} <- {}",
                label,
                image_path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        println!("────────────────────────────────────────────────────────");
        return session;
    }

    for slot in &flash_slots {
        let label = if slot.is_empty() {
            part_name.clone()
        } else {
            format!("{}_{}", part_name, slot)
        };
        print!("  Flashing {} ...", label);
        io::stdout().flush().ok();

        let result = flash_partition(image_path, &part_name, slot, serial);
        session.results.push(result.clone());

        if result.success {
            println!(" OK  ({:.1}s)", result.duration_s);
        } else {
            println!(" FAILED");
            report_failure(&result);
            println!("────────────────────────────────────────────────────────");
            return session;
        }
    }

    println!("────────────────────────────────────────────────────────");
    println!("  ✓ {} flashed successfully", part_name);
    session
}

// ---------------------------------------------------------------------------
// Summary + wipe + reboot
// ---------------------------------------------------------------------------

/// Print session summary, offer wipe, and reboot.
pub fn print_summary(session: &FlashSession) {
    let total = session.results.len();
    let ok = session.succeeded().len();
    let failed_count = session.failed().len();

    println!("\n── Flash session summary ───────────────────────────────");
    println!("  Total      :  {}", total);
    println!("  ✓ OK       :  {}", ok);
    if failed_count > 0 {
        println!("  ✗ Failed   :  {}", failed_count);
    }

    if !session.failed().is_empty() {
        println!("\n  Failed partitions:");
        for r in session.failed() {
            let crit = if is_critical_partition(&r.partition) {
                "  [CRITICAL]"
            } else {
                ""
            };
            println!("    ✗  {}_{}{}", r.partition, r.slot, crit);
            println!("       {}", r.error);
        }
    }

    println!();

    if session.failed().is_empty() && !session.aborted {
        let elapsed_total: f64 = session.succeeded().iter().map(|r| r.duration_s).sum();
        let mins = elapsed_total as u64 / 60;
        let secs = elapsed_total as u64 % 60;
        let time_str = if mins > 0 {
            format!("{}m {:02}s", mins, secs)
        } else {
            format!("{}s", secs)
        };

        println!("{}", "━".repeat(60));
        println!("  ✓  Flash complete!");
        println!("{}", "━".repeat(60));
        println!("  Partitions flashed :  {}", ok);
        println!("  Total flash time   :  {}", time_str);
        println!("{}", "━".repeat(60));
        println!();

        // Offer wipe
        offer_wipe(session);

        // Reboot
        println!("\n  Rebooting to system ...");
        let mut args: Vec<&str> = Vec::new();
        let serial_str;
        if let Some(ref s) = session.serial {
            serial_str = s.clone();
            args.push("-s");
            args.push(&serial_str);
        }
        args.push("reboot");
        fastboot_cmd(&args, 30);
    } else if !session.critical_failed().is_empty() {
        println!("{}", "━".repeat(60));
        println!("  ✗  Critical failure");
        println!("  One or more CRITICAL partitions failed to flash.");
        println!("  The device may not boot.");
        println!("  Do NOT reboot or unplug until resolved.");
        println!("{}", "━".repeat(60));
    } else if !session.failed().is_empty() {
        println!("{}", "━".repeat(60));
        println!("  ⚠  Flash completed with errors");
        println!("  Non-critical partitions failed — device should still boot.");
        println!("  Re-flash the failed partitions to complete the update.");
        println!("{}", "━".repeat(60));
    } else if session.aborted {
        println!("{}", "━".repeat(60));
        println!("  ⚠  Flash was aborted");
        println!("{}", "━".repeat(60));
    }
}

fn offer_wipe(session: &FlashSession) {
    println!("── Format userdata ──────────────────────────────────────");
    println!("  'fastboot -w' wipes ALL user data (contacts, apps, files).");
    println!("  Recommended after a major version change or cross-region flash.");
    println!();
    println!("  ⚠  ALL DATA WILL BE PERMANENTLY ERASED.");
    println!();

    let answer = crate::utils::prompt("  Wipe userdata now? (yes / no)", "no");
    if answer != "yes" {
        println!("  Skipped. Wipe manually later: fastboot -w");
        println!("────────────────────────────────────────────────────────\n");
        return;
    }

    println!("  Wiping userdata ...");
    let mut args: Vec<&str> = Vec::new();
    let serial_str;
    if let Some(ref s) = session.serial {
        serial_str = s.clone();
        args.push("-s");
        args.push(&serial_str);
    }
    args.push("-w");
    let (rc, out, err) = fastboot_cmd(&args, 120);
    if rc == 0 {
        println!("  ✓ Userdata wiped successfully.");
    } else {
        println!(
            "  ✗ Wipe failed: {}",
            if err.is_empty() { &out } else { &err }
        );
    }
    println!("────────────────────────────────────────────────────────\n");
}

// ---------------------------------------------------------------------------
// GUI-friendly flash session — same logic as run_flash_session but uses
// callbacks instead of println!/prompt/exit so it works from a GUI thread.
//
// Assumptions for GUI mode:
//   - Device is already in fastbootd when this is called
//   - No interactive prompts (ARB check is handled by GUI before calling this)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FlashProgress {
    pub partition: String,
    pub slot: String,
    pub done: usize,
    pub total: usize,
}

pub fn run_flash_session_with_log(
    source: &FirmwareSource,
    serial: Option<&str>,
    dry_run: bool,
    skip_xbl_abl: bool,
    skip_preloader: bool,
    as_mediatek: bool,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_log: &dyn Fn(String),
    on_progress: &dyn Fn(FlashProgress),
) -> FlashSession {
    let firmware_dir = source.path();
    let mut session = FlashSession::new(source, serial, dry_run);

    let images = if source.is_source() {
        collect_images_from_source(firmware_dir)
    } else {
        collect_images(firmware_dir)
    };

    if images.is_empty() {
        if source.is_source() {
            on_log(format!("No flashable .img files found in source build directory: {}", firmware_dir.display()));
        } else {
            on_log(format!("No .img files found in {}", firmware_dir.display()));
        }
        return session;
    }

    on_log(format!("{} images found", images.len()));

    // Filter skipped partitions
    let filtered: HashMap<String, PathBuf> = images.into_iter().filter(|(name, _)| {
        if skip_xbl_abl && is_xbl_abl(name) { on_log(format!("Skipping {} (xbl/abl excluded)", name)); false }
        else if skip_preloader && is_preloader(name) { on_log(format!("Skipping {} (preloader excluded)", name)); false }
        else { true }
    }).collect();

    if filtered.is_empty() {
        on_log("No images to flash after filtering — aborting".into());
        return session;
    }

    let is_mediatek = as_mediatek || is_mediatek_build(&filtered);

    if is_mediatek {
        on_log("Mediatek device — all partitions will be flashed in fastbootd mode".into());
    } else {
        on_log("Qualcomm device — bootloader partitions go through bootloader mode".into());
    }

    let fastbootd_images: HashMap<&str, &PathBuf> = filtered
        .iter()
        .filter(|(k, _)| is_mediatek || !is_bootloader_partition(k))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let bootloader_images: HashMap<&str, &PathBuf> = if is_mediatek {
        HashMap::new()
    } else {
        filtered
            .iter()
            .filter(|(k, _)| is_bootloader_partition(k))
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    };

    // -- Stage 1: fastbootd (all partitions on Mediatek, non-bootloader on Qualcomm) --
    if !fastbootd_images.is_empty() {
        let active_slot = get_active_slot(serial);
        on_log(format!("Active slot: {}", active_slot.to_uppercase()));

        let super_imgs: HashMap<&&str, &&PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| is_super_partition(k))
            .collect();
        let non_super_imgs: HashMap<&&str, &&PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| !is_super_partition(k))
            .collect();

        let total_ops = non_super_imgs.len() * 2 + super_imgs.len();
        let flash_start = std::time::Instant::now();
        let mut done_ops = 0usize;

        on_log(format!(
            "Stage 1/2: fastbootd — {} non-super × 2 slots, {} super × 1 slot",
            non_super_imgs.len(), super_imgs.len()
        ));

        // Non-super → both slots
        let mut sorted_non_super: Vec<_> = non_super_imgs.iter().collect();
        sorted_non_super.sort_by_key(|(k, _)| *k);

        for slot in SLOTS {
            for (partition, image_path) in &sorted_non_super {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    on_log("Flash cancelled by user".into());
                    session.aborted = true;
                    return session;
                }
                on_progress(FlashProgress {
                    partition: partition.to_string(),
                    slot: slot.to_string(),
                    done: done_ops,
                    total: total_ops,
                });
                done_ops += 1;
                if !device_has_partition(serial, partition) {
                    on_log(format!("Skipping {}_{} — not a device partition", partition, slot));
                    continue;
                }
                on_log(format!("Flashing {}_{} ...", partition, slot));
                let result = flash_partition(image_path, partition, slot, serial);
                if result.success {
                    on_log(format!("{}_{} OK ({:.1}s)", partition, slot, result.duration_s));
                    session.results.push(result);
                } else {
                    on_log(format!("{}_{} FAILED: {}", partition, slot, result.error));
                    session.results.push(result.clone());
                    on_log(format!("Retrying {}_{} ...", partition, slot));
                    let retry = flash_partition(image_path, partition, slot, serial);
                    if retry.success {
                        on_log(format!("{}_{} OK on retry ({:.1}s)", partition, slot, retry.duration_s));
                        *session.results.last_mut().unwrap() = retry;
                    } else {
                        on_log(format!("{}_{} FAILED on retry — skipping", partition, slot));
                        *session.results.last_mut().unwrap() = retry;
                    }
                }
            }
        }

        // Super → active slot only
        if !super_imgs.is_empty() {
            on_log("Clearing super partition...".into());
            let super_names: Vec<String> = super_imgs.keys()
                .filter(|k| device_has_partition(serial, k))
                .map(|k| k.to_string())
                .collect();
            if !super_names.is_empty() {
                wipe_super_with_log(serial, &super_names, &|msg| on_log(msg));
            }

            let mut sorted_super: Vec<_> = super_imgs.iter().collect();
            sorted_super.sort_by_key(|(k, _)| *k);

            for (partition, image_path) in &sorted_super {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    on_log("Flash cancelled by user".into());
                    session.aborted = true;
                    return session;
                }
                on_progress(FlashProgress {
                    partition: partition.to_string(),
                    slot: active_slot.clone(),
                    done: done_ops,
                    total: total_ops,
                });
                done_ops += 1;
                if !device_has_partition(serial, partition) {
                    on_log(format!("Skipping {} ({}) — not a device partition", partition, active_slot));
                    continue;
                }
                on_log(format!("Flashing {}_{} (super) ...", partition, active_slot));
                let result = flash_partition(image_path, partition, &active_slot, serial);
                if result.success {
                    on_log(format!("{}_{} OK ({:.1}s)", partition, active_slot, result.duration_s));
                    session.results.push(result);
                } else {
                    on_log(format!("{}_{} FAILED: {}", partition, active_slot, result.error));
                    session.results.push(result.clone());
                    on_log(format!("Retrying {}_{} ...", partition, active_slot));
                    let retry = flash_partition(image_path, partition, &active_slot, serial);
                    if retry.success {
                        on_log(format!("{}_{} OK on retry ({:.1}s)", partition, active_slot, retry.duration_s));
                        *session.results.last_mut().unwrap() = retry;
                    } else {
                        on_log(format!("{}_{} FAILED on retry — skipping", partition, active_slot));
                        *session.results.last_mut().unwrap() = retry;
                    }
                }
            }
        }

        let elapsed = flash_start.elapsed().as_secs();
        on_log(format!("fastbootd stage complete in {}m{:02}s", elapsed / 60, elapsed % 60));
    }

    // -- Stage 2: bootloader (Qualcomm only) --
    if !bootloader_images.is_empty() {
        on_log("Rebooting to bootloader for modem/bootloader flash...".into());
        if !enter_bootloader(serial) {
            on_log("Could not reach bootloader — modem was not flashed".into());
            for (&partition, _) in &bootloader_images {
                for &slot in SLOTS {
                    session.results.push(FlashResult {
                        partition: partition.to_string(),
                        slot: slot.to_string(),
                        success: false,
                        error: "Could not enter bootloader mode".into(),
                        duration_s: 0.0,
                    });
                }
            }
            return session;
        }

        let total_ops2 = bootloader_images.len() * 2;
        let mut done_ops2 = 0usize;
        let flash_start2 = std::time::Instant::now();
        on_log(format!("Stage 2/2: bootloader — {} partitions × 2 slots", bootloader_images.len()));

        let mut sorted_bl: Vec<_> = bootloader_images.iter().collect();
        sorted_bl.sort_by_key(|(k, _)| *k);

        for &slot in SLOTS {
            for (partition, image_path) in &sorted_bl {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    on_log("Flash cancelled by user".into());
                    session.aborted = true;
                    return session;
                }
                on_progress(FlashProgress {
                    partition: partition.to_string(),
                    slot: slot.to_string(),
                    done: done_ops2,
                    total: total_ops2,
                });
                done_ops2 += 1;
                if !device_has_partition(serial, partition) {
                    on_log(format!("Skipping {}_{} — not a device partition", partition, slot));
                    continue;
                }
                on_log(format!("Flashing {}_{} ...", partition, slot));
                let result = flash_partition(image_path, partition, slot, serial);
                if result.success {
                    on_log(format!("{}_{} OK ({:.1}s)", partition, slot, result.duration_s));
                    session.results.push(result);
                } else {
                    on_log(format!("{}_{} FAILED: {}", partition, slot, result.error));
                    session.results.push(result.clone());
                    on_log(format!("Retrying {}_{} ...", partition, slot));
                    let retry = flash_partition(image_path, partition, slot, serial);
                    if retry.success {
                        on_log(format!("{}_{} OK on retry ({:.1}s)", partition, slot, retry.duration_s));
                        *session.results.last_mut().unwrap() = retry;
                    } else {
                        on_log(format!("{}_{} FAILED on retry — skipping", partition, slot));
                        *session.results.last_mut().unwrap() = retry;
                    }
                }
            }
        }

        let elapsed2 = flash_start2.elapsed().as_secs();
        on_log(format!("Stage 2 complete in {}m{:02}s", elapsed2 / 60, elapsed2 % 60));
    }

    session
}
