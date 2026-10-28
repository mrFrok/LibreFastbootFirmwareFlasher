//! Core flash orchestrator for LibreFastbootFirmwareFlasher.
//!
//! Public API:
//!   - `run_flash_session_with_log()` — full firmware flash with A/B slot management
//!   - `run_flash_single()` — flash a single partition image
//!   - `flash_partition()` — low-level single flash primitive
//!   - `FlashSession`, `FlashResult`
//!
//! The flash platform (Snapdragon vs MediaTek) is always chosen explicitly by
//! the user via `FlashOptions::as_mediatek` — there is no device auto-detection.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{info, warn};

/// Case-insensitive prefix check without allocating.
fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.as_bytes()
        .get(..prefix.len())
        .is_some_and(|b| b.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Append `-s <serial>` to a fastboot argument list.
fn push_serial_arg<'a>(args: &mut Vec<&'a str>, serial: Option<&'a str>) {
    if let Some(s) = serial {
        args.push("-s");
        args.push(s);
    }
}

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
const REBOOT_TIMEOUT_SECS: u64 = 90;
/// Device-presence polling interval. `fastboot devices` is a cheap local USB
/// enumeration (no device round-trip), so a short interval keeps detection
/// latency low without meaningful overhead.
const POLL_INTERVAL_MS: u64 = 500;
/// How long to wait for a rebooting device to drop off the USB bus before
/// looking for it again (prevents matching the stale pre-reboot enumeration).
const DISAPPEAR_TIMEOUT_SECS: u64 = 5;

fn is_bootloader_partition(name: &str) -> bool {
    BOOTLOADER_MODE_PARTITIONS.contains(&name)
}

pub fn is_super_partition(name: &str) -> bool {
    SUPER_PARTITIONS.contains(&name)
}

pub fn is_critical_partition(name: &str) -> bool {
    CRITICAL_PARTITIONS.contains(&name)
}

pub fn is_xbl_abl(name: &str) -> bool {
    starts_with_ignore_ascii_case(name, "xbl") || starts_with_ignore_ascii_case(name, "abl")
}

pub fn is_preloader(name: &str) -> bool {
    starts_with_ignore_ascii_case(name, "preloader")
}

pub fn is_mediatek_build(images: &HashMap<String, PathBuf>) -> bool {
    let has_preloader = images.keys().any(|k| is_preloader(k));
    if !has_preloader {
        return false;
    }
    // Qualcomm always has xbl or xbl_config; MediaTek doesn't
    let has_xbl = images.keys().any(|k| {
        k.eq_ignore_ascii_case("xbl")
            || k.eq_ignore_ascii_case("xbl.img")
            || k.as_bytes()
                .windows(10)
                .any(|w| w.eq_ignore_ascii_case(b"xbl_config"))
    });
    !has_xbl
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

/// Whether the current fastboot connection is suitable for userspace-fastboot
/// operations. Some OnePlus / OPPO / Realme devices report plain `fastboot`
/// even though `getvar is-userspace` says `yes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastbootdStatus {
    Fastbootd,
    UserspaceFastboot,
    BootloaderFastboot,
    UnknownFastboot,
    NotFound,
}

impl FastbootdStatus {
    pub fn is_usable(self) -> bool {
        matches!(self, Self::Fastbootd | Self::UserspaceFastboot)
    }
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
    pub end_reason: Option<String>,
    /// Dynamic partitions the phone did not have, created by fastbootd in
    /// group `default`. The phone boots, but the next OTA may refuse them
    /// until the OTA repair is run.
    pub outside_group: Vec<String>,
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
            end_reason: None,
            outside_group: Vec::new(),
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

fn fastboot_cmd_serial(serial: Option<&str>, args: &[&str], timeout: u64) -> (i32, String, String) {
    let mut full: Vec<&str> = Vec::new();
    if let Some(s) = serial {
        full.extend(["-s", s]);
    }
    full.extend_from_slice(args);
    fastboot_cmd(&full, timeout)
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
            if let Some(s) = serial
                && parts[0] != s
            {
                continue;
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
            if parts.len() >= 2
                && parts[1] == "device"
                && (serial.is_none() || Some(parts[0]) == serial)
            {
                return DeviceMode::System;
            }
        }
    }

    DeviceMode::Unknown
}

/// Return current active slot ('a' or 'b'), or `None` when it cannot be
/// determined. Callers MUST abort instead of guessing: flashing dynamic
/// (super) partitions to the wrong slot leaves the device unbootable.
pub fn get_active_slot(serial: Option<&str>) -> Option<String> {
    let mut args: Vec<&str> = Vec::new();
    push_serial_arg(&mut args, serial);
    args.extend(&["getvar", "current-slot"]);

    let (_, out, err) = fastboot_cmd(&args, 10);
    let slot = parse_current_slot(&format!("{}\n{}", out, err));
    if slot.is_none() {
        warn!("Could not detect active slot");
    }
    slot
}

/// The other A/B slot.
fn other_slot(slot: &str) -> &'static str {
    if slot == "a" { "b" } else { "a" }
}

/// Make `slot` the active one. In fastbootd this does not reboot: fastbootd
/// keeps running from the slot it booted, which is what lets the modem be
/// written to the newly active one.
pub fn set_active_slot(serial: Option<&str>, slot: &str) -> Result<(), String> {
    let (code, out, err) = fastboot_cmd_serial(serial, &["set_active", slot], 15);
    if code == 0 {
        return Ok(());
    }
    let why = if err.trim().is_empty() { out } else { err };
    warn!("fastboot set_active {} failed: {}", slot, why.trim());
    Err(why.trim().to_string())
}

/// Where the modem goes in a Snapdragon flash.
#[derive(Debug, PartialEq, Eq)]
enum ModemRoute {
    /// Reboot to the bootloader and flash it to both slots — the legacy way,
    /// or no modem at all (then nothing goes to the bootloader).
    Bootloader,
    /// Switch to the other slot first and flash it there from fastbootd,
    /// together with the dynamic partitions.
    Fastbootd,
    /// The fastbootd route was asked for but the flash has a modem and no
    /// system: refused rather than falling back to the bootloader, which
    /// some phones cannot reach — the reason this route exists.
    NeedsSystem,
}

/// The fastbootd route boots the phone from the other slot, so it needs that
/// slot's dynamic partitions in the same session; without them the phone
/// would come up with an old or empty system. Without a modem there is
/// nothing to gain from switching.
fn modem_route(wanted: bool, has_modem: bool, has_super: bool) -> ModemRoute {
    match (wanted && has_modem, has_super) {
        (true, true) => ModemRoute::Fastbootd,
        (true, false) => ModemRoute::NeedsSystem,
        (false, _) => ModemRoute::Bootloader,
    }
}

/// Partitions the firmware has but the user chose to leave as they are on
/// the phone (`select_images`'s "… excluded" reasons). Names it never flashes
/// anyway (device-unique data, odd names) are not among them.
pub fn held_back(skipped: &[(String, String)]) -> Vec<&str> {
    skipped
        .iter()
        .filter(|(_, reason)| reason.ends_with("excluded"))
        .map(|(name, _)| name.as_str())
        .collect()
}

/// Logged when a session stops after the slot switch, so the user knows the
/// phone would now boot a slot whose system is not fully flashed.
fn warn_switched_slot(switched_to: Option<&str>, on_log: &dyn Fn(String)) {
    if let Some(slot) = switched_to {
        on_log(format!(
            "Warning: the active slot is already {} and its system was not fully flashed — finish flashing before rebooting",
            slot.to_uppercase()
        ));
    }
}

/// Pull the active slot out of `fastboot getvar current-slot` output. Only a
/// bare `a` or `b` is accepted; anything else is `None`, so the caller aborts.
fn parse_current_slot(output: &str) -> Option<String> {
    let combined = output.to_lowercase();
    for line in combined.lines() {
        if line.contains("current-slot:") {
            let slot = line.split("current-slot:").last().unwrap_or("").trim();
            if slot == "a" || slot == "b" {
                return Some(slot.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Reboot helpers
// ---------------------------------------------------------------------------

/// Parse `fastboot devices` into (serial, mode) pairs.
/// Mode is "fastboot" (bootloader) or "fastbootd" (userspace).
fn fastboot_device_list() -> Vec<(String, String)> {
    let (rc, out, _) = fastboot_cmd(&["devices"], 5);
    if rc != 0 {
        return Vec::new();
    }
    out.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some((parts[0].to_string(), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect()
}

fn target_fastboot_device(serial: Option<&str>) -> Option<(String, String)> {
    let devices = fastboot_device_list();
    if devices.is_empty() {
        return None;
    }
    if let Some(s) = serial {
        if let Some(device) = devices.iter().find(|(ser, _)| ser == s) {
            return Some(device.clone());
        }
        if devices.len() == 1 {
            warn!(
                "Expected serial {} but found single device {} — accepting (serial can change between adb and fastboot)",
                s, devices[0].0
            );
            return Some(devices[0].clone());
        }
        return None;
    }
    devices.into_iter().next()
}

fn parse_fastboot_getvar(output: &str, key: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim().trim_start_matches("(bootloader)").trim();
        if let Some((k, v)) = line.split_once(':')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn fastboot_getvar(serial: Option<&str>, key: &str, timeout: u64) -> Option<String> {
    let mut args: Vec<&str> = Vec::new();
    push_serial_arg(&mut args, serial);
    args.extend(&["getvar", key]);

    let (rc, out, err) = fastboot_cmd(&args, timeout);
    if rc != 0 && out.is_empty() && err.is_empty() {
        return None;
    }
    parse_fastboot_getvar(&format!("{}\n{}", out, err), key)
}

pub fn is_userspace_fastboot(serial: Option<&str>) -> Option<bool> {
    let (ser, _) = target_fastboot_device(serial)?;
    let value = fastboot_getvar(Some(&ser), "is-userspace", 10)?;
    match value.to_lowercase().as_str() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

pub fn fastbootd_status(serial: Option<&str>) -> FastbootdStatus {
    let Some((ser, mode)) = target_fastboot_device(serial) else {
        return FastbootdStatus::NotFound;
    };
    if mode == "fastbootd" {
        return FastbootdStatus::Fastbootd;
    }
    if is_userspace_fastboot(Some(&ser)) == Some(true) {
        return FastbootdStatus::UserspaceFastboot;
    }
    if mode == "fastboot" {
        return FastbootdStatus::BootloaderFastboot;
    }
    FastbootdStatus::UnknownFastboot
}

/// True when a device in the target mode is present.
///
/// When a serial is given but not found, a single device in the target mode is
/// still accepted: some devices report different serials in adb vs fastboot,
/// and rejecting them made the UI sit out the full timeout with the device
/// already connected.
fn device_in_mode(serial: Option<&str>, mode_pred: &dyn Fn(&str) -> bool) -> bool {
    let devices = fastboot_device_list();
    let in_mode: Vec<&(String, String)> = devices.iter().filter(|(_, m)| mode_pred(m)).collect();
    match serial {
        None => !in_mode.is_empty(),
        Some(s) => {
            if in_mode.iter().any(|(ser, _)| ser == s) {
                return true;
            }
            if in_mode.len() == 1 {
                warn!(
                    "Expected serial {} but found single device {} — accepting (serial can change between adb and fastboot)",
                    s, in_mode[0].0
                );
                return true;
            }
            false
        }
    }
}

/// Actively poll until a device in the target mode appears, with a short
/// interval so detection reacts as soon as the device is up.
fn wait_for_mode(
    serial: Option<&str>,
    timeout: u64,
    mode_pred: &dyn Fn(&str) -> bool,
    label: &str,
) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    let mut last_log = Instant::now();
    loop {
        if device_in_mode(serial, mode_pred) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if last_log.elapsed().as_secs() >= 5 {
            let remaining = (deadline - Instant::now()).as_secs();
            info!("Waiting for {} ... ({}s left)", label, remaining);
            last_log = Instant::now();
        }
        thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// Poll until device reports 'fastbootd' mode.
pub fn wait_for_fastbootd(serial: Option<&str>, timeout: u64) -> bool {
    wait_for_fastbootd_status(serial, timeout).is_usable()
}

/// Poll until the device is usable for fastbootd operations, accepting devices
/// that report `is-userspace: yes` even when the mode label remains `fastboot`.
pub fn wait_for_fastbootd_status(serial: Option<&str>, timeout: u64) -> FastbootdStatus {
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    let mut last_log = Instant::now();
    let mut last_status = FastbootdStatus::NotFound;
    loop {
        let status = fastbootd_status(serial);
        if status.is_usable() {
            return status;
        }
        if status != FastbootdStatus::NotFound {
            last_status = status;
        }
        if Instant::now() >= deadline {
            return last_status;
        }
        if last_log.elapsed().as_secs() >= 5 {
            let remaining = (deadline - Instant::now()).as_secs();
            info!(
                "Waiting for fastbootd/userspace fastboot ... ({}s left)",
                remaining
            );
            last_log = Instant::now();
        }
        thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// Poll until device reports 'fastboot' (bootloader) mode.
fn wait_for_fastboot(serial: Option<&str>, timeout: u64) -> bool {
    wait_for_mode(serial, timeout, &|m| m == "fastboot", "bootloader")
}

/// Poll until a device appears in either bootloader or fastbootd mode.
pub fn wait_for_any_fastboot(serial: Option<&str>, timeout: u64) -> bool {
    wait_for_mode(
        serial,
        timeout,
        &|m| m == "fastboot" || m == "fastbootd",
        "fastboot/fastbootd",
    )
}

/// Wait until the device disappears from `fastboot devices` (i.e. it actually
/// started rebooting). Returning false is not fatal — the caller proceeds to
/// wait for the device to come back either way.
pub fn wait_for_device_gone(serial: Option<&str>, timeout: u64) -> bool {
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout);
    loop {
        let devices = fastboot_device_list();
        let present = match serial {
            Some(s) => devices.iter().any(|(ser, _)| ser == s),
            None => !devices.is_empty(),
        };
        if !present {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// Reboot from fastbootd into bootloader.
pub fn enter_bootloader(serial: Option<&str>) -> bool {
    info!("Rebooting to bootloader ...");
    let mut args: Vec<&str> = Vec::new();
    push_serial_arg(&mut args, serial);
    args.extend(&["reboot", "bootloader"]);

    let (rc, _, err) = fastboot_cmd(&args, 30);
    if rc != 0 {
        tracing::error!("fastboot reboot bootloader failed: {}", err);
        return false;
    }
    // Active wait instead of a fixed settle sleep: first let the stale
    // pre-reboot enumeration drop off the bus, then wait for the bootloader.
    wait_for_device_gone(serial, DISAPPEAR_TIMEOUT_SECS);
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
    flash_partition_inner(image_path, partition, slot, serial, None)
}

pub fn flash_partition_cancellable(
    image_path: &Path,
    partition: &str,
    slot: &str,
    serial: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
) -> FlashResult {
    flash_partition_inner(image_path, partition, slot, serial, Some(cancel))
}

/// Minimum sustained transfer rate used to size flash timeouts. Matches the
/// cable-test pass threshold: any cable that passed the pre-flash check can
/// sustain at least this rate.
const FLASH_MIN_SPEED_MB_S: u64 = 1;
/// Base flash timeout on top of the size-derived transfer allowance.
const FLASH_BASE_TIMEOUT_SECS: u64 = 300;

fn flash_partition_inner(
    image_path: &Path,
    partition: &str,
    slot: &str,
    serial: Option<&str>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> FlashResult {
    // Never kill fastboot mid-write on a critical partition: an interrupted
    // write to abl/xbl/boot can hard-brick the device. Cancellation for these
    // takes effect between partitions (the batch loop checks the flag).
    let cancel = if is_critical_partition(partition) {
        None
    } else {
        cancel
    };

    // Scale the timeout with the image size so a large image on a slow (but
    // passing) cable is never killed mid-write — killing fastboot mid-flash
    // corrupts the partition being written.
    let size_mb = fs::metadata(image_path)
        .map(|m| m.len() / (1024 * 1024))
        .unwrap_or(0);
    let timeout_secs = FLASH_BASE_TIMEOUT_SECS + size_mb / FLASH_MIN_SPEED_MB_S;

    let mut args: Vec<String> = Vec::new();
    if let Some(s) = serial {
        args.push("-s".into());
        args.push(s.into());
    }
    if !slot.is_empty() {
        args.push("--slot".into());
        args.push(slot.into());
    }
    args.push("flash".into());
    args.push(partition.into());
    args.push(image_path.to_string_lossy().into());

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd: Vec<&str> = vec!["fastboot"];
    cmd.extend(&refs);
    let start = Instant::now();
    let result = match cancel {
        Some(c) => crate::utils::run_cmd_with_cancel(&cmd, timeout_secs, c),
        None => crate::utils::run_cmd(&cmd, timeout_secs),
    };
    let duration = start.elapsed().as_secs_f64();

    let (rc, out, err) = (result.code, result.stdout, result.stderr);

    if rc == -125 {
        return FlashResult {
            partition: partition.into(),
            slot: slot.into(),
            success: false,
            error: "Cancelled by user".into(),
            duration_s: duration,
        };
    }

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
// Super partition preparation
// ---------------------------------------------------------------------------

// TODO: build super's layout instead of patching it. The payload carries the
// exact layout (`payload_dumper --metadata`: groups, their sizes and partition
// names). Write LP metadata from it in Rust — an empty super with the right
// groups and `readonly` partitions — install it with `fastboot wipe-super`, then
// flash the images. That is the full fix: the in-place resize below keeps a
// correct layout correct, but cannot repair a device whose partitions an older
// LFFF already moved into `default`. Firmware folders without a payload keep
// the in-place path.

/// What [`repair_ota_slots`] would delete, read from `fastboot getvar all`
/// output: every logical partition of the inactive slot. Refuses anything
/// but a Virtual A/B device with no update in progress.
pub fn ota_repair_plan(getvar_all: &str) -> Result<Vec<String>, String> {
    let status = parse_fastboot_getvar(getvar_all, "snapshot-update-status").ok_or(
        "The phone does not report snapshot-update-status: not a Virtual A/B device, or not in fastbootd",
    )?;
    if status != "none" {
        return Err(format!(
            "An update is still in progress on the phone (snapshot-update-status: {status}). Boot Android and let it finish first."
        ));
    }
    let slot = parse_fastboot_getvar(getvar_all, "current-slot")
        .map(|s| s.trim_start_matches('_').to_lowercase())
        .ok_or("Could not read the active slot")?;
    let inactive = match slot.as_str() {
        "a" => "b",
        "b" => "a",
        other => return Err(format!("Unexpected active slot: {other}")),
    };
    let suffix = format!("_{inactive}");
    let mut names: Vec<String> = getvar_all
        .lines()
        .filter_map(|l| l.split("is-logical:").nth(1))
        .filter_map(|v| v.trim().strip_suffix(":yes"))
        .filter(|n| {
            n.ends_with(&suffix)
                || [&suffix, "-cow", "_cow", "-cow-img"]
                    .iter()
                    .any(|c| n.contains(&format!("{suffix}{c}")))
        })
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// Make the next OTA installable on a phone an older LFFF flashed: delete
/// the inactive slot's logical partitions (empty on Virtual A/B after an
/// update merged). Older LFFF recreated them in group `default`, and
/// update_engine, which only clears its own `<group>_<slot>`, then failed
/// with "Attempting to create duplication partition". The active slot and
/// the factory partitions are not touched. Fastbootd only. Each OTA moves to
/// the other slot, so a repair before each of the next two OTAs heals both.
pub fn repair_ota_slots(
    serial: Option<&str>,
    on_log: &dyn Fn(String),
) -> Result<Vec<String>, String> {
    let (_, out, err) = fastboot_cmd_serial(serial, &["getvar", "all"], 30);
    let plan = ota_repair_plan(&format!("{out}{err}"))?;
    if plan.is_empty() {
        on_log("Nothing to repair: the inactive slot has no partitions".into());
        return Ok(plan);
    }
    for name in &plan {
        on_log(format!("Removing {name}"));
        let (rc, _, e) = fastboot_cmd_serial(serial, &["delete-logical-partition", name], 15);
        if rc != 0 {
            return Err(format!(
                "delete-logical-partition {name} failed: {}",
                e.trim()
            ));
        }
    }
    Ok(plan)
}

/// Install a fresh super layout with `fastboot wipe-super` (fastbootd only).
/// Every dynamic partition is gone afterwards, empty in its group, until
/// flashed again.
pub fn wipe_super(
    serial: Option<&str>,
    layout: &[u8],
    on_log: &dyn Fn(String),
) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("temp dir: {e}"))?;
    let img = tmp.path().join("super_empty.img");
    fs::write(&img, layout).map_err(|e| format!("writing the layout: {e}"))?;
    on_log("Writing a new super partition layout (fastboot wipe-super)...".into());
    let mut args: Vec<String> = vec!["fastboot".into()];
    if let Some(s) = serial {
        args.extend(["-s".into(), s.into()]);
    }
    args.extend(["wipe-super".into(), img.display().to_string()]);
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let r = run_cmd(&refs, 120);
    if r.code != 0 {
        return Err(format!("fastboot wipe-super failed: {}", r.stderr.trim()));
    }
    on_log("Super layout written".into());
    Ok(())
}

/// What `prepare_super_with_log` does to make room in super before the
/// active slot's dynamic partitions are flashed.
#[derive(Debug, PartialEq, Eq)]
pub struct SuperPrep {
    /// The inactive slot's copies and the COW images an update leaves behind.
    pub delete: Vec<String>,
    /// The active slot's copies, shrunk to 0 so that growing one image never
    /// hits its group's limit while another has not been shrunk yet.
    pub shrink: Vec<String>,
}

/// Nothing is ever deleted and recreated. fastbootd's `create-logical-partition`
/// cannot set a group or attributes: a recreated partition lands in `default`
/// without `readonly`, and the next OTA fails with "Attempting to create
/// duplication partition" when update_engine adds the same name to its own
/// group. Resizing and flashing in place keep both.
pub fn super_prep_plan(super_names: &[String], active_slot: &str) -> SuperPrep {
    let inactive = if active_slot == "a" { "b" } else { "a" };
    let mut delete = Vec::new();
    let mut shrink = Vec::new();
    for base in super_names {
        delete.push(format!("{base}_{inactive}"));
        for slot in [active_slot, inactive] {
            for suffix in ["-cow", "_cow", "-cow-img"] {
                delete.push(format!("{base}_{slot}{suffix}"));
            }
        }
        shrink.push(format!("{base}_{active_slot}"));
    }
    SuperPrep { delete, shrink }
}

/// `fastboot getvar is-logical:<name>` answers on stderr, as every getvar.
/// `None` when there is no answer (timeout, USB error): that is not a "no",
/// and taking it for one would recreate an existing partition in `default`.
fn is_logical(base_args: &[String], name: &str) -> Option<bool> {
    let mut cmd = base_args.to_vec();
    cmd.push("getvar".into());
    cmd.push(format!("is-logical:{name}"));
    let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    let r = run_cmd(&refs, 15);
    parse_is_logical(&format!("{}\n{}", r.stdout, r.stderr), name)
}

/// fastbootd answers a partition it does not have with an error,
/// `getvar:is-logical:<name> FAILED (remote: 'Unknown partition')` — and
/// fastboot still exits 0 for a failed getvar. Any `remote:` answer is the
/// device's "no"; no answer at all (timeout, USB error) is `None`.
fn parse_is_logical(output: &str, name: &str) -> Option<bool> {
    let out = output.to_lowercase();
    let key = format!("is-logical:{}:", name.to_lowercase());
    if let Some(rest) = out.lines().find_map(|l| l.split_once(&key).map(|(_, v)| v)) {
        return match rest.trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
    }
    out.contains("remote:").then_some(false)
}

/// Returns the partitions it had to create outside their group (see
/// [`FlashSession::outside_group`]).
pub fn prepare_super_with_log(
    serial: Option<&str>,
    super_names: &[String],
    active_slot: &str,
    on_log: &dyn Fn(String),
) -> Vec<String> {
    let mut base_args: Vec<String> = vec!["fastboot".into()];
    if let Some(s) = serial {
        base_args.push("-s".into());
        base_args.push(s.into());
    }
    let run = |verb: &str, args: &[&str]| {
        let mut cmd = base_args.clone();
        cmd.push(verb.into());
        cmd.extend(args.iter().map(|a| a.to_string()));
        let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        run_cmd(&refs, 15)
    };

    on_log(format!(
        "Preparing {} super partition(s) ...",
        super_names.len()
    ));
    let plan = super_prep_plan(super_names, active_slot);

    for name in &plan.delete {
        let r = run("delete-logical-partition", &[name]);
        if r.code != 0 {
            let err_lower = r.stderr.to_lowercase();
            if !err_lower.contains("does not exist") && !err_lower.contains("no such") {
                warn!("delete-logical-partition {}: {}", name, r.stderr);
            }
        }
    }

    let mut outside_group = Vec::new();
    for name in &plan.shrink {
        let logical = is_logical(&base_args, name);
        if logical == Some(true) {
            let r = run("resize-logical-partition", &[name, "0"]);
            if r.code != 0 {
                warn!("resize-logical-partition {}: {}", name, r.stderr);
            }
        } else if logical.is_none() {
            // Not shrunk: flashing it may still fit, and if it does not,
            // fastboot's own error says so.
            on_log(format!(
                "WARNING: could not ask the device about {name}; leaving it as it is"
            ));
        } else {
            // Not on the phone: a partition this firmware adds, or the
            // slot is empty (Virtual A/B after an update, or an older LFFF
            // deleted it). fastbootd can only put it in `default`; one
            // summary below rather than a warning per partition.
            let r = run("create-logical-partition", &[name, "0"]);
            if r.code == 0 {
                outside_group.push(name.clone());
            } else {
                warn!("create-logical-partition {}: {}", name, r.stderr);
            }
        }
    }

    if outside_group.is_empty() {
        on_log("Super partitions prepared; groups and attributes kept".into());
    } else {
        on_log(format!(
            "WARNING: {} partition(s) of slot {} were not on the phone and were created outside their partition group: {}. The phone boots normally; run the OTA repair before each of the next two system updates.",
            outside_group.len(),
            active_slot.to_uppercase(),
            outside_group.join(", ")
        ));
    }
    outside_group
}

// ---------------------------------------------------------------------------
// Image collection
// ---------------------------------------------------------------------------

/// Scan firmware_dir for .img files.
/// Strips _a/_b suffix so abl_a.img -> key "abl" (prevents abl_a_a bug).
/// On duplicates the shallower path wins, then the lexically smaller one —
/// a fixed rule rather than directory order, because the ARB check reads
/// xbl_config through this same function and must see the image that is
/// actually flashed.
pub fn collect_images(firmware_dir: &Path) -> HashMap<String, PathBuf> {
    let mut images: HashMap<String, PathBuf> = HashMap::new();
    let mut entries: Vec<PathBuf> = Vec::new();

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
    entries.sort_by(|a, b| (a.components().count(), a).cmp(&(b.components().count(), b)));

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
        if lower == "vendor_ramdump.img" {
            return false;
        }
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

// ---------------------------------------------------------------------------
// Single-partition flash
// ---------------------------------------------------------------------------

/// Flash a single .img file to one or both slots.
/// All human-readable output goes through `on_log` — the caller decides how
/// to present it (terminal, GUI log pane, …).
pub fn run_flash_single(
    image_path: &Path,
    partition: Option<&str>,
    slots: Option<&[String]>,
    serial: Option<&str>,
    dry_run: bool,
    on_log: &dyn Fn(String),
) -> FlashSession {
    let mut session = FlashSession::new(
        &FirmwareSource::Extracted(image_path.parent().unwrap_or(Path::new(".")).to_path_buf()),
        serial,
        dry_run,
    );

    if !image_path.exists() {
        on_log(format!("✗ Image not found: {}", image_path.display()));
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
    let is_super = is_super_partition(&part_name);
    // Dynamic (super) partitions exist only in the active slot's super
    // metadata — flashing the inactive slot fails or corrupts the layout.
    // Unless slots were chosen explicitly, restrict them to the active one.
    let flash_slots: Vec<String> = match slots {
        Some(s) => s.to_vec(),
        None if is_super && !dry_run => match get_active_slot(serial) {
            Some(s) => {
                on_log(format!(
                    "  Dynamic partition — flashing active slot '{}' only",
                    s
                ));
                vec![s]
            }
            None => {
                on_log(format!(
                    "✗ Could not detect the active slot — refusing to flash dynamic partition '{}'.",
                    part_name
                ));
                on_log(
                    "  Check that the device is in fastbootd, or pass --slot explicitly.".into(),
                );
                return session;
            }
        },
        None if is_super => vec!["<active>".to_string()],
        None => SLOTS.iter().map(|s| s.to_string()).collect(),
    };

    let size_mb = fs::metadata(image_path)
        .map(|m| m.len() as f64 / 1024.0 / 1024.0)
        .unwrap_or(0.0);

    on_log(format!(
        "\n── Flash single partition: {} ──────────────────",
        part_name
    ));
    on_log(format!(
        "  Image     : {}  ({:.1} MB)",
        image_path.file_name().unwrap_or_default().to_string_lossy(),
        size_mb
    ));
    on_log(format!("  Partition : {}", part_name));
    let slot_label = if flash_slots == [""] {
        "non-A/B".to_string()
    } else {
        flash_slots.join(", ")
    };
    on_log(format!("  Slots     : {}", slot_label));
    if is_crit {
        on_log("  ⚠  CRITICAL partition — do not unplug during flash".into());
    }

    if dry_run {
        for slot in &flash_slots {
            let label = if slot.is_empty() {
                part_name.clone()
            } else {
                format!("{}_{}", part_name, slot)
            };
            on_log(format!(
                "  [dry-run] would flash: {} <- {}",
                label,
                image_path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        on_log("────────────────────────────────────────────────────────".into());
        return session;
    }

    for slot in &flash_slots {
        let label = if slot.is_empty() {
            part_name.clone()
        } else {
            format!("{}_{}", part_name, slot)
        };
        on_log(format!("  Flashing {} ...", label));

        let result = flash_partition(image_path, &part_name, slot, serial);
        session.results.push(result.clone());

        if result.success {
            on_log(format!("  {} OK  ({:.1}s)", label, result.duration_s));
        } else {
            on_log(format!("  {} FAILED", label));
            return session;
        }
    }

    on_log("────────────────────────────────────────────────────────".into());
    on_log(format!("  ✓ {} flashed successfully", part_name));
    session
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

/// Action to take when a partition flash fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    /// Retry flashing this partition.
    Retry,
    /// Skip this partition and continue flashing.
    Skip,
    /// Abort the entire flash session.
    Abort,
}

/// Flash a single partition with retry and logging.
fn flash_partition_with_log(
    image_path: &Path,
    partition: &str,
    slot: &str,
    serial: Option<&str>,
    on_log: &dyn Fn(String),
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> FlashResult {
    on_log(format!("Flashing {}_{} ...", partition, slot));
    let result = match cancel {
        Some(c) => flash_partition_cancellable(image_path, partition, slot, serial, c),
        None => flash_partition(image_path, partition, slot, serial),
    };
    if result.success {
        on_log(format!(
            "{}_{} OK ({:.1}s)",
            partition, slot, result.duration_s
        ));
    } else {
        on_log(format!("{}_{} FAILED", partition, slot));
    }
    result
}

/// Flags controlling a flash session. Bundled into a struct so call sites use
/// named fields instead of a string of positional `bool` arguments.
#[derive(Debug, Clone, Default)]
pub struct FlashOptions {
    pub dry_run: bool,
    pub skip_xbl_abl: bool,
    pub skip_preloader: bool,
    /// `Some(true/false)` to force the platform, `None` to auto-detect.
    pub as_mediatek: Option<bool>,
    /// Comma-separated partition names the user explicitly wants to skip.
    pub skip_partitions: String,
    /// Snapdragon only, and the default the frontends pass: flash the modem
    /// from fastbootd instead of rebooting to the bootloader, which some
    /// phones cannot reach (a flash stuck halfway, up to a brick). false is
    /// the legacy bootloader route, kept as an explicit, not recommended
    /// choice. The session switches to the other slot first, then
    /// writes the modem and the dynamic partitions there; the old slot keeps
    /// its old modem.
    pub modem_in_fastbootd: bool,
    /// With `modem_in_fastbootd`: images read from the phone's active slot
    /// for the partitions the user chose not to flash, by name. They are
    /// written to the new slot, so "don't flash" still means "keep what the
    /// phone has" (a rooted init_boot, a custom recovery) after the switch.
    pub keep_images: HashMap<String, PathBuf>,
    /// With `modem_in_fastbootd`: when the slot switched to has none of
    /// its dynamic partitions, rebuild super from the firmware's layout
    /// (`fastboot wipe-super`) instead of letting fastbootd recreate them in
    /// group `default`, which breaks the next OTAs. Needs the layout saved
    /// at extraction ([`crate::extractor::PAYLOAD_METADATA_FILE`]); without
    /// it, or whenever the rebuild cannot be done safely, the session says
    /// why and keeps the layout as it is. The frontends pass true unless
    /// the user turned it off.
    pub rebuild_super: bool,
}

/// What a super rebuild installs: the empty layout, and the partitions the
/// firmware lacks that get their stand-in image
/// ([`crate::super_layout::STUB_IMAGES`]).
#[derive(Debug, Clone)]
pub struct SuperRebuild {
    pub image: Vec<u8>,
    pub stubs: Vec<String>,
    /// Factory partitions the rebuild deletes ([`crate::super_layout::DROPPABLE`]).
    pub dropped: Vec<String>,
}

/// Logical partitions `fastboot getvar all` lists as present, with slot
/// suffix.
fn logical_partitions(getvar_all: &str) -> Vec<String> {
    getvar_all
        .lines()
        .filter_map(|l| l.split("is-logical:").nth(1))
        .filter_map(|v| v.trim().strip_suffix(":yes"))
        .map(str::to_string)
        .collect()
}

/// The size of super from `fastboot getvar partition-size:super`, 0 when
/// there is none. The key holds a colon itself, so not parse_fastboot_getvar.
fn parse_super_size(out: &str) -> u64 {
    out.lines()
        .find_map(|l| l.split("partition-size:super:").nth(1))
        .and_then(|v| u64::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

/// Whether and how to rebuild super before flashing `super_names` into
/// `target_slot`, decided from `fastboot getvar all`, the size of super
/// (0 when unknown) and the firmware's saved layout. `Err` is the reason
/// not to — logged, and the session goes on with the layout the phone has,
/// as before.
pub fn super_rebuild_plan(
    getvar_all: &str,
    super_size: u64,
    layout_json: Option<&str>,
    super_names: &[String],
    target_slot: &str,
) -> Result<Option<SuperRebuild>, String> {
    use crate::super_layout::{DROPPABLE, SuperLayout, stub_image};

    let device = logical_partitions(getvar_all);
    if device.is_empty() {
        return Err("could not read the phone's partitions".into());
    }
    // Only an empty target slot needs it: fastbootd would recreate its
    // partitions outside their group. A slot that has them is resized in
    // place, groups kept.
    if super_names
        .iter()
        .all(|n| device.contains(&format!("{n}_{target_slot}")))
    {
        return Ok(None);
    }
    let json = layout_json.ok_or(
        "this firmware has no saved partition layout (unpack the .zip again with this version)",
    )?;
    if super_size == 0 {
        return Err("could not read the size of super".into());
    }
    let size = super_size;

    let plain = SuperLayout::from_payload_metadata(json, size)?;
    let (stubs, others): (Vec<String>, Vec<String>) = plain
        .missing_from(&device)
        .into_iter()
        .partition(|n| stub_image(n).is_some());
    let (dropped, others): (Vec<String>, Vec<String>) = others
        .into_iter()
        .partition(|n| DROPPABLE.contains(&n.as_str()));
    if !others.is_empty() {
        return Err(format!(
            "the phone has {} which this firmware does not contain, and wiping super would delete them",
            others.join(", ")
        ));
    }
    let layout = SuperLayout::from_payload_metadata_with_stubs(json, size, &stubs)?;
    // Everything in the new layout must be flashed after wipe-super: a
    // dynamic partition this session takes for an ordinary one goes out
    // before it and would be wiped.
    let suffix = format!("_{target_slot}");
    let unknown: Vec<&str> = layout
        .partitions
        .iter()
        .filter_map(|p| p.name.strip_suffix(&suffix))
        .filter(|n| !is_super_partition(n) && stub_image(n).is_none())
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "the firmware's layout has partitions LFFF does not flash as dynamic: {}",
            unknown.join(", ")
        ));
    }
    let image = layout.to_checked_image()?;
    Ok(Some(SuperRebuild {
        image,
        stubs,
        dropped,
    }))
}

/// Outcome of flashing a batch of partitions.
enum BatchOutcome {
    Done,
    Cancelled,
    Aborted,
}

/// Flash a list of partitions to one slot, with cancel support and a retry
/// loop: a failed partition re-asks `on_failure` until it succeeds or the
/// user chooses Skip/Abort. Critical partitions are never silently skipped.
#[allow(clippy::too_many_arguments)]
fn flash_batch_with_log(
    items: &[(&str, &PathBuf)],
    slot: &str,
    serial: Option<&str>,
    total_ops: usize,
    done_ops: &mut usize,
    cancel: &std::sync::atomic::AtomicBool,
    session: &mut FlashSession,
    on_log: &dyn Fn(String),
    on_progress: &dyn Fn(FlashProgress),
    on_failure: &dyn Fn(&str, &str, &str) -> FailureAction,
) -> BatchOutcome {
    for (partition, image_path) in items {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            on_log("Flash cancelled by user".into());
            return BatchOutcome::Cancelled;
        }
        on_progress(FlashProgress {
            partition: partition.to_string(),
            slot: slot.to_string(),
            done: *done_ops,
            total: total_ops,
        });
        *done_ops += 1;

        loop {
            let result =
                flash_partition_with_log(image_path, partition, slot, serial, on_log, Some(cancel));
            let cancelled = result.error == "Cancelled by user";
            let success = result.success;
            let error = result.error.clone();
            session.results.push(result);

            if cancelled {
                on_log("Flash cancelled by user".into());
                return BatchOutcome::Cancelled;
            }
            if success {
                break;
            }
            match on_failure(partition, slot, &error) {
                FailureAction::Retry => {
                    on_log(format!("Retrying {}_{} …", partition, slot));
                }
                FailureAction::Skip => {
                    on_log(format!("Skipping {}_{} — continuing", partition, slot));
                    break;
                }
                FailureAction::Abort => {
                    on_log(format!(
                        "Flash aborted due to {}_{} failure",
                        partition, slot
                    ));
                    return BatchOutcome::Aborted;
                }
            }
        }
    }
    BatchOutcome::Done
}

/// Partitions that hold data unique to one device — calibration, IMEI and
/// modem state, factory-reset protection, keys — or the user's own data.
/// No firmware package ships them, and overwriting them cannot be undone, so
/// a downloaded firmware that contains one is suspect and the image is left
/// out. Flashing one on purpose remains possible with `flash-partition`.
const NEVER_FROM_FIRMWARE: &[&str] = &[
    "persist",
    "persistbak",
    "modemst1",
    "modemst2",
    "fsg",
    "fsc",
    "devinfo",
    "frp",
    "userdata",
    "keystore",
];

/// True for names fastboot can only read as a partition name. Partition
/// names come from file names in the firmware; one like `-w` or
/// `--slot=all` would otherwise be read as an option.
fn is_plain_partition_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn skip_list(skip_partitions: &str) -> Vec<String> {
    skip_partitions
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Split collected images into what a session will flash and what it leaves
/// out, with the reason for each. Both the flash session and the CLI's
/// confirmation call this, so the list the user approves is the list flashed.
///
/// `NEVER_FROM_FIRMWARE` applies only to downloaded firmware: a source build
/// is the user's own output, and an AOSP build legitimately contains
/// `userdata.img`.
pub fn select_images(
    images: HashMap<String, PathBuf>,
    options: &FlashOptions,
    is_source: bool,
) -> (HashMap<String, PathBuf>, Vec<(String, String)>) {
    let skips = skip_list(&options.skip_partitions);
    let mut keep = HashMap::new();
    let mut skipped = Vec::new();
    for (name, path) in images {
        let reason = if !is_plain_partition_name(&name) {
            Some("not a plain partition name")
        } else if !is_source && NEVER_FROM_FIRMWARE.contains(&name.as_str()) {
            Some("device-unique data, never part of a firmware")
        } else if options.skip_xbl_abl && is_xbl_abl(&name) {
            Some("xbl/abl excluded")
        } else if options.skip_preloader && is_preloader(&name) {
            Some("preloader excluded")
        } else if skips.contains(&name) {
            Some("user excluded")
        } else {
            None
        };
        match reason {
            Some(r) => skipped.push((name, r.to_string())),
            None => {
                keep.insert(name, path);
            }
        }
    }
    skipped.sort();
    (keep, skipped)
}

pub fn run_flash_session_with_log(
    source: &FirmwareSource,
    serial: Option<&str>,
    options: &FlashOptions,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_log: &dyn Fn(String),
    on_progress: &dyn Fn(FlashProgress),
    on_failure: &dyn Fn(&str, &str, &str) -> FailureAction,
) -> FlashSession {
    // skip_xbl_abl and skip_preloader are applied by select_images.
    let FlashOptions {
        dry_run,
        as_mediatek,
        skip_partitions,
        ..
    } = options.clone();
    let firmware_dir = source.path();
    let mut session = FlashSession::new(source, serial, dry_run);

    let images = if source.is_source() {
        collect_images_from_source(firmware_dir)
    } else {
        collect_images(firmware_dir)
    };

    if images.is_empty() {
        if source.is_source() {
            on_log(format!(
                "No flashable .img files found in source build directory: {}",
                firmware_dir.display()
            ));
        } else {
            on_log(format!("No .img files found in {}", firmware_dir.display()));
        }
        session.end_reason = Some("NoImagesFound".into());
        return session;
    }

    on_log(format!("{} images found", images.len()));

    if !skip_partitions.trim().is_empty() {
        on_log(format!(
            "User-specified partitions to skip: {}",
            skip_list(&skip_partitions).join(", ")
        ));
    }

    let (filtered, skipped) = select_images(images, options, source.is_source());
    for (name, reason) in &skipped {
        on_log(format!("Skipping {} ({})", name, reason));
    }

    if filtered.is_empty() {
        on_log("No images to flash after filtering — aborting".into());
        session.end_reason = Some("NoImagesFound".into());
        return session;
    }

    // Platform is an explicit user choice; the firmware heuristic is only a
    // legacy fallback for callers that did not ask the user.
    let is_mediatek = match as_mediatek {
        Some(v) => v,
        None => {
            let guess = is_mediatek_build(&filtered);
            on_log(format!(
                "Warning: no flash method specified — falling back to firmware heuristic ({})",
                if guess { "MediaTek" } else { "Snapdragon" }
            ));
            guess
        }
    };

    let route = if is_mediatek {
        ModemRoute::Bootloader
    } else {
        modem_route(
            options.modem_in_fastbootd,
            filtered.keys().any(|k| is_bootloader_partition(k)),
            filtered.keys().any(|k| is_super_partition(k)),
        )
    };
    let modem_fastbootd = route == ModemRoute::Fastbootd;

    if route == ModemRoute::NeedsSystem {
        on_log("Refusing to flash: the modem is flashed from fastbootd into the other slot, which needs the system in the same flash — and this one has none. Flash the system too, or choose the bootloader method on purpose.".into());
        session.aborted = true;
        session.end_reason = Some("ModemNeedsSystem".into());
        return session;
    }
    if is_mediatek {
        on_log("MediaTek method — all partitions will be flashed in fastbootd mode".into());
    } else if modem_fastbootd {
        on_log("Snapdragon method — modem flashed in fastbootd, to the other slot together with the system".into());
    } else if options.modem_in_fastbootd {
        on_log("Snapdragon method — no modem in this flash, nothing goes to the bootloader".into());
    } else {
        on_log("Snapdragon method, bootloader route (not recommended) — modem goes through bootloader mode".into());
    }

    // After the switch the phone boots the other slot, where a partition
    // that is not flashed holds whatever old copy was there. So everything
    // the user held back must come from the phone, or nothing is flashed.
    let held = held_back(&skipped);
    let kept: Vec<(&str, &PathBuf)> = if modem_fastbootd {
        let dynamic: Vec<&str> = held
            .iter()
            .copied()
            .filter(|n| is_super_partition(n))
            .collect();
        if !dynamic.is_empty() {
            on_log(format!(
                "Refusing to flash: with the modem in fastbootd the phone switches to the other slot, which would have no {}. Flash them, or use the bootloader method.",
                dynamic.join(", ")
            ));
            session.aborted = true;
            session.end_reason = Some("SkippedDynamic".into());
            return session;
        }
        let unsaved: Vec<&str> = held
            .iter()
            .copied()
            .filter(|n| !options.keep_images.contains_key(*n))
            .collect();
        if !unsaved.is_empty() {
            on_log(format!(
                "Refusing to flash: with the modem in fastbootd the phone switches to the other slot, and {} were not copied from the phone — the new slot would keep its old copies. Start the install from Android with root for Shell, so they can be copied.",
                unsaved.join(", ")
            ));
            session.aborted = true;
            session.end_reason = Some("SkippedNotSaved".into());
            return session;
        }
        let mut kept: Vec<(&str, &PathBuf)> = held
            .iter()
            .filter_map(|n| options.keep_images.get_key_value(*n))
            .map(|(k, v)| (k.as_str(), v))
            .collect();
        kept.sort_by_key(|(k, _)| *k);
        kept
    } else {
        Vec::new()
    };

    let fastbootd_images: HashMap<&str, &PathBuf> = filtered
        .iter()
        .filter(|(k, _)| is_mediatek || modem_fastbootd || !is_bootloader_partition(k))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let bootloader_images: HashMap<&str, &PathBuf> = if is_mediatek || modem_fastbootd {
        HashMap::new()
    } else {
        filtered
            .iter()
            .filter(|(k, _)| is_bootloader_partition(k))
            .map(|(k, v)| (k.as_str(), v))
            .collect()
    };

    if dry_run {
        let mut sorted: Vec<&&str> = fastbootd_images.keys().collect();
        sorted.sort();
        for name in sorted {
            let mode =
                if modem_fastbootd && (is_super_partition(name) || is_bootloader_partition(name)) {
                    "fastbootd, other slot, after set_active"
                } else if is_super_partition(name) {
                    "fastbootd, active slot"
                } else {
                    "fastbootd, both slots"
                };
            on_log(format!("[dry-run] would flash: {} ({})", name, mode));
        }
        let mut sorted_bl: Vec<&&str> = bootloader_images.keys().collect();
        sorted_bl.sort();
        for name in sorted_bl {
            on_log(format!(
                "[dry-run] would flash: {} (bootloader, both slots)",
                name
            ));
        }
        for (name, _) in &kept {
            on_log(format!(
                "[dry-run] would flash: {} (copied from the phone, other slot, after set_active)",
                name
            ));
        }
        on_log("[dry-run] No partitions were flashed".into());
        session.end_reason = Some("DryRun".into());
        return session;
    }

    // -- Stage 1: fastbootd (all partitions on MediaTek, non-bootloader on Snapdragon) --
    if !fastbootd_images.is_empty() {
        match fastbootd_status(serial) {
            FastbootdStatus::Fastbootd => {}
            FastbootdStatus::UserspaceFastboot => {
                on_log("Warning: device reports userspace fastboot but is listed as 'fastboot'. This is a known OnePlus/OPPO/Realme fastbootd label issue; continuing after user confirmation.".into());
            }
            FastbootdStatus::BootloaderFastboot | FastbootdStatus::UnknownFastboot => {
                on_log("Warning: device is not confirmed as fastbootd/userspace fastboot. Dynamic partition flashing may fail.".into());
            }
            FastbootdStatus::NotFound => {
                on_log("Warning: no fastboot device found while starting fastbootd stage.".into());
            }
        }
        let active_slot = match get_active_slot(serial) {
            Some(s) => s,
            None => {
                on_log(
                    "Could not detect the active slot (fastboot getvar current-slot failed)."
                        .into(),
                );
                on_log("Refusing to flash: dynamic partitions written to the wrong slot make the device unbootable. Check that the device is in fastbootd and try again.".into());
                session.aborted = true;
                session.end_reason = Some("SlotDetectFailed".into());
                return session;
            }
        };
        on_log(format!("Active slot: {}", active_slot.to_uppercase()));

        // Rebuild super? Decided before the switch, so that a layout which
        // cannot be built stops nothing: the flash just keeps the phone's.
        let rebuild: Option<SuperRebuild> = if modem_fastbootd && options.rebuild_super {
            let super_names: Vec<String> = fastbootd_images
                .keys()
                .filter(|k| is_super_partition(k))
                .map(|k| k.to_string())
                .collect();
            let target = other_slot(&active_slot);
            let (_, out, err) = fastboot_cmd_serial(serial, &["getvar", "all"], 30);
            // Asked on its own: OnePlus 15 leaves it out of `getvar all`, and
            // for a second or two after coming from Android its fastbootd
            // answers "Could not open partition" — so asked again a while.
            let mut answer = String::new();
            let mut super_size = 0;
            for attempt in 0..10 {
                if attempt > 0 {
                    std::thread::sleep(Duration::from_secs(1));
                }
                let (_, so, se) =
                    fastboot_cmd_serial(serial, &["getvar", "partition-size:super"], 10);
                answer = format!("{so}\n{se}");
                super_size = parse_super_size(&answer);
                if super_size != 0 {
                    break;
                }
            }
            if super_size == 0 {
                on_log(format!(
                    "fastboot getvar partition-size:super answered: {}",
                    answer.split_whitespace().collect::<Vec<_>>().join(" ")
                ));
            }
            let json =
                fs::read_to_string(firmware_dir.join(crate::extractor::PAYLOAD_METADATA_FILE)).ok();
            match super_rebuild_plan(
                &format!("{out}\n{err}"),
                super_size,
                json.as_deref(),
                &super_names,
                target,
            ) {
                Ok(Some(r)) => {
                    on_log(format!(
                        "Slot {} has no partitions: super will be rebuilt from the firmware's layout, every partition read-only in its group, as from the factory",
                        target.to_uppercase()
                    ));
                    if !r.stubs.is_empty() {
                        on_log(format!(
                            "Not in the firmware, written as empty factory images: {}",
                            r.stubs.join(", ")
                        ));
                    }
                    if !r.dropped.is_empty() {
                        on_log(format!(
                            "Factory partitions nothing uses any more, not in the firmware, deleted: {}",
                            r.dropped.join(", ")
                        ));
                    }
                    Some(r)
                }
                Ok(None) => {
                    on_log(format!(
                        "Slot {} has its partitions: super is kept, resized in place",
                        target.to_uppercase()
                    ));
                    None
                }
                Err(e) => {
                    on_log(format!("Super is not rebuilt: {e}"));
                    None
                }
            }
        } else {
            None
        };
        // The stand-ins, as files fastboot can flash; kept until the end.
        let stub_dir = tempfile::tempdir().ok();
        let mut stub_files: Vec<(String, PathBuf)> = Vec::new();
        if let (Some(r), Some(dir)) = (&rebuild, &stub_dir) {
            for name in &r.stubs {
                let path = dir.path().join(format!("{name}.img"));
                let img = crate::super_layout::stub_image(name).unwrap_or_default();
                if fs::write(&path, img).is_ok() {
                    stub_files.push((name.clone(), path));
                }
            }
        }
        if rebuild
            .as_ref()
            .is_some_and(|r| r.stubs.len() != stub_files.len())
        {
            on_log("Could not write the stand-in images — nothing was flashed".into());
            session.aborted = true;
            session.end_reason = Some("StubWriteFailed".into());
            return session;
        }

        // Modem in fastbootd: fastbootd can only write the modem of the slot
        // it is not running from, so switch first; the modem and the system
        // then go to the newly active slot. Nothing is flashed yet if the
        // switch fails.
        let switched: Option<String> = if modem_fastbootd {
            let target = other_slot(&active_slot);
            on_log(format!(
                "Switching the active slot to {}...",
                target.to_uppercase()
            ));
            if let Err(e) = set_active_slot(serial, target) {
                on_log(format!(
                    "Could not switch to slot {} — nothing was flashed: {e}",
                    target.to_uppercase()
                ));
                session.aborted = true;
                session.end_reason = Some("SetActiveFailed".into());
                return session;
            }
            Some(target.to_string())
        } else {
            None
        };
        let system_slot = switched.clone().unwrap_or_else(|| active_slot.clone());

        let super_imgs: HashMap<&str, &PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| is_super_partition(k))
            .map(|(k, v)| (*k, *v))
            .collect();
        let mut modem_imgs: Vec<(&str, &PathBuf)> = fastbootd_images
            .iter()
            .filter(|(k, _)| modem_fastbootd && is_bootloader_partition(k))
            .map(|(k, v)| (*k, *v))
            .collect();
        modem_imgs.sort_by_key(|(k, _)| *k);
        let non_super_imgs: HashMap<&str, &PathBuf> = fastbootd_images
            .iter()
            .filter(|(k, _)| {
                !is_super_partition(k) && !(modem_fastbootd && is_bootloader_partition(k))
            })
            .map(|(k, v)| (*k, *v))
            .collect();

        let total_ops = non_super_imgs.len() * 2
            + kept.len()
            + modem_imgs.len()
            + stub_files.len()
            + super_imgs.len();
        let flash_start = std::time::Instant::now();
        let mut done_ops = 0usize;

        let stages = if bootloader_images.is_empty() { 1 } else { 2 };
        on_log(format!(
            "Stage 1/{}: fastbootd — {} non-super × 2 slots, {} super × 1 slot",
            stages,
            non_super_imgs.len(),
            super_imgs.len()
        ));

        // Non-super → both slots
        let mut sorted_non_super: Vec<(&str, &PathBuf)> = non_super_imgs.into_iter().collect();
        sorted_non_super.sort_by_key(|(k, _)| *k);

        for slot in SLOTS {
            match flash_batch_with_log(
                &sorted_non_super,
                slot,
                serial,
                total_ops,
                &mut done_ops,
                &cancel,
                &mut session,
                on_log,
                on_progress,
                on_failure,
            ) {
                BatchOutcome::Done => {}
                BatchOutcome::Cancelled => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("Cancelled".into());
                    return session;
                }
                BatchOutcome::Aborted => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("UserAborted".into());
                    return session;
                }
            }
        }

        // What the phone had, for the partitions held back → the new slot
        if !kept.is_empty() {
            on_log(format!(
                "Copying what the phone had to slot {}: {}",
                system_slot.to_uppercase(),
                kept.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
            ));
            match flash_batch_with_log(
                &kept,
                &system_slot,
                serial,
                total_ops,
                &mut done_ops,
                &cancel,
                &mut session,
                on_log,
                on_progress,
                on_failure,
            ) {
                BatchOutcome::Done => {}
                BatchOutcome::Cancelled => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("Cancelled".into());
                    return session;
                }
                BatchOutcome::Aborted => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("UserAborted".into());
                    return session;
                }
            }
        }

        // Modem in fastbootd → the newly active slot only
        if !modem_imgs.is_empty() {
            match flash_batch_with_log(
                &modem_imgs,
                &system_slot,
                serial,
                total_ops,
                &mut done_ops,
                &cancel,
                &mut session,
                on_log,
                on_progress,
                on_failure,
            ) {
                BatchOutcome::Done => {}
                BatchOutcome::Cancelled => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("Cancelled".into());
                    return session;
                }
                BatchOutcome::Aborted => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("UserAborted".into());
                    return session;
                }
            }
        }

        // Super → one slot: the active one, or the one just switched to
        if !super_imgs.is_empty() {
            if let Some(r) = &rebuild {
                if let Err(e) = wipe_super(serial, &r.image, on_log) {
                    on_log(format!("Refusing to flash dynamic partitions: {e}"));
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("WipeSuperFailed".into());
                    return session;
                }
                // Into the fresh layout, before the firmware's images.
                let stubs: Vec<(&str, &PathBuf)> =
                    stub_files.iter().map(|(n, p)| (n.as_str(), p)).collect();
                if !stubs.is_empty() {
                    match flash_batch_with_log(
                        &stubs,
                        &system_slot,
                        serial,
                        total_ops,
                        &mut done_ops,
                        &cancel,
                        &mut session,
                        on_log,
                        on_progress,
                        on_failure,
                    ) {
                        BatchOutcome::Done => {}
                        BatchOutcome::Cancelled => {
                            warn_switched_slot(switched.as_deref(), on_log);
                            session.aborted = true;
                            session.end_reason = Some("Cancelled".into());
                            return session;
                        }
                        BatchOutcome::Aborted => {
                            warn_switched_slot(switched.as_deref(), on_log);
                            session.aborted = true;
                            session.end_reason = Some("UserAborted".into());
                            return session;
                        }
                    }
                }
            } else {
                on_log("Preparing super partition...".into());
                let super_names: Vec<String> = super_imgs.keys().map(|k| k.to_string()).collect();
                session.outside_group =
                    prepare_super_with_log(serial, &super_names, &system_slot, &|msg| on_log(msg));
            }

            let mut sorted_super: Vec<(&str, &PathBuf)> = super_imgs.into_iter().collect();
            sorted_super.sort_by_key(|(k, _)| *k);

            match flash_batch_with_log(
                &sorted_super,
                &system_slot,
                serial,
                total_ops,
                &mut done_ops,
                &cancel,
                &mut session,
                on_log,
                on_progress,
                on_failure,
            ) {
                BatchOutcome::Done => {}
                BatchOutcome::Cancelled => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("Cancelled".into());
                    return session;
                }
                BatchOutcome::Aborted => {
                    warn_switched_slot(switched.as_deref(), on_log);
                    session.aborted = true;
                    session.end_reason = Some("UserAborted".into());
                    return session;
                }
            }
        }

        if let Some(slot) = &switched {
            on_log(format!(
                "Active slot is now {}; slot {} keeps its old modem",
                slot.to_uppercase(),
                active_slot.to_uppercase()
            ));
        }

        let elapsed = flash_start.elapsed().as_secs();
        on_log(format!(
            "fastbootd stage complete in {}m{:02}s",
            elapsed / 60,
            elapsed % 60
        ));
    }

    // -- Stage 2: bootloader (Snapdragon only) --
    if !bootloader_images.is_empty() {
        on_log("Rebooting to bootloader for modem/bootloader flash...".into());
        if !enter_bootloader(serial) {
            on_log("Could not reach bootloader — modem was not flashed".into());
            session.end_reason = Some("BootloaderModeFailed".into());
            for &partition in bootloader_images.keys() {
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
        on_log(format!(
            "Stage 2/2: bootloader — {} partitions × 2 slots",
            bootloader_images.len()
        ));

        let mut sorted_bl: Vec<(&str, &PathBuf)> = bootloader_images.into_iter().collect();
        sorted_bl.sort_by_key(|(k, _)| *k);
        for slot in SLOTS {
            match flash_batch_with_log(
                &sorted_bl,
                slot,
                serial,
                total_ops2,
                &mut done_ops2,
                &cancel,
                &mut session,
                on_log,
                on_progress,
                on_failure,
            ) {
                BatchOutcome::Done => {}
                BatchOutcome::Cancelled => {
                    session.aborted = true;
                    session.end_reason = Some("Cancelled".into());
                    return session;
                }
                BatchOutcome::Aborted => {
                    session.aborted = true;
                    session.end_reason = Some("UserAborted".into());
                    return session;
                }
            }
        }

        let elapsed2 = flash_start2.elapsed().as_secs();
        on_log(format!(
            "Stage 2 complete in {}m{:02}s",
            elapsed2 / 60,
            elapsed2 % 60
        ));
    }

    session.end_reason = Some("Completed".into());
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // Recreating a partition through fastbootd puts it in group `default`,
    // which later breaks OTAs; the plan must never contain the active copy in
    // its delete list.
    const GETVAR_A: &str = "(bootloader) current-slot:a\n(bootloader) snapshot-update-status:none\n(bootloader) is-logical:system_a:yes\n(bootloader) is-logical:system_b:yes\n(bootloader) is-logical:odm_b:yes\n(bootloader) is-logical:my_company_a:yes\n(bootloader) is-logical:system_b-cow:yes\n(bootloader) is-logical:boot_b:no\n";

    #[test]
    fn ota_repair_takes_only_the_inactive_slot() {
        assert_eq!(
            ota_repair_plan(GETVAR_A).unwrap(),
            ["odm_b", "system_b", "system_b-cow"]
        );
    }

    #[test]
    fn ota_repair_refuses_mid_update_and_non_vab() {
        let merging = GETVAR_A.replace("status:none", "status:merging");
        assert!(ota_repair_plan(&merging).is_err());
        let no_vab = GETVAR_A.replace("(bootloader) snapshot-update-status:none\n", "");
        assert!(ota_repair_plan(&no_vab).is_err());
    }

    const PAYLOAD: &str = include_str!("../testdata/rmx3709-payload-metadata.json");
    const FIRMWARE_DYNAMIC: [&str; 15] = [
        "system",
        "system_ext",
        "vendor",
        "product",
        "my_product",
        "odm",
        "my_engineering",
        "vendor_dlkm",
        "system_dlkm",
        "my_stock",
        "my_heytap",
        "my_carrier",
        "my_region",
        "my_bigball",
        "my_manifest",
    ];

    /// OnePlus 15 sized.
    const SUPER: u64 = 0x3F9800000;

    /// `fastboot getvar all` of a phone whose logical partitions are
    /// `names`. No size of super in it, as on OnePlus 15.
    fn getvar_all(names: &[String]) -> String {
        let mut out = String::from("(bootloader) current-slot:a\n");
        for n in names {
            out.push_str(&format!("(bootloader) is-logical:{n}:yes\n"));
        }
        out
    }

    fn slot(names: &[&str], suffix: &str) -> Vec<String> {
        names.iter().map(|n| format!("{n}{suffix}")).collect()
    }

    fn firmware() -> Vec<String> {
        FIRMWARE_DYNAMIC.iter().map(|n| n.to_string()).collect()
    }

    // The phones seen so far: only `_a` partitions, plus the two factory
    // ones the regional firmware leaves out. Rebuilt, with their stand-ins.
    #[test]
    fn an_empty_target_slot_is_rebuilt_with_stubs() {
        let mut device = slot(&FIRMWARE_DYNAMIC, "_a");
        device.extend(slot(&["my_company", "my_preload"], "_a"));
        let r = super_rebuild_plan(&getvar_all(&device), SUPER, Some(PAYLOAD), &firmware(), "b")
            .unwrap()
            .unwrap();
        assert_eq!(r.stubs, ["my_company", "my_preload"]);
        let parsed = crate::super_layout::parse_image(&r.image).unwrap();
        assert!(parsed.partitions.iter().any(|p| p.name == "my_company_b"));
    }

    // OnePlus 15 from the factory with OxygenOS 16: the old OKI kernel's
    // modules, which the firmware no longer has. Dropped, not a blocker.
    #[test]
    fn the_old_oki_modules_partition_is_dropped() {
        let mut device = slot(&FIRMWARE_DYNAMIC, "_a");
        device.extend(slot(&["my_company", "my_preload", "system_dlkm_oki"], "_a"));
        let r = super_rebuild_plan(&getvar_all(&device), SUPER, Some(PAYLOAD), &firmware(), "b")
            .unwrap()
            .unwrap();
        assert_eq!(r.dropped, ["system_dlkm_oki"]);
        let parsed = crate::super_layout::parse_image(&r.image).unwrap();
        assert!(!parsed.partitions.iter().any(|p| p.name.contains("oki")));
    }

    #[test]
    fn a_target_slot_with_its_partitions_is_left_alone() {
        let mut device = slot(&FIRMWARE_DYNAMIC, "_a");
        device.extend(slot(&FIRMWARE_DYNAMIC, "_b"));
        let r = super_rebuild_plan(&getvar_all(&device), SUPER, Some(PAYLOAD), &firmware(), "b");
        assert!(r.unwrap().is_none());
    }

    #[test]
    fn a_partition_without_a_stub_stops_the_rebuild() {
        let mut device = slot(&FIRMWARE_DYNAMIC, "_a");
        device.push("odm_dlkm_a".into());
        let err = super_rebuild_plan(&getvar_all(&device), SUPER, Some(PAYLOAD), &firmware(), "b")
            .unwrap_err();
        assert!(err.contains("odm_dlkm"), "{err}");
    }

    #[test]
    fn no_saved_layout_or_no_answer_stops_the_rebuild() {
        let device = slot(&FIRMWARE_DYNAMIC, "_a");
        assert!(super_rebuild_plan(&getvar_all(&device), SUPER, None, &firmware(), "b").is_err());
        assert!(super_rebuild_plan("", SUPER, Some(PAYLOAD), &firmware(), "b").is_err());
        let err = super_rebuild_plan(&getvar_all(&device), 0, Some(PAYLOAD), &firmware(), "b")
            .unwrap_err();
        assert!(err.contains("size of super"), "{err}");
    }

    #[test]
    fn super_size_is_read_from_its_own_getvar() {
        assert_eq!(
            parse_super_size("partition-size:super: 0x3F9800000\nFinished. Total time: 0.001s"),
            SUPER
        );
        assert_eq!(
            parse_super_size("(bootloader) partition-size:super:0x3F9800000"),
            SUPER
        );
        assert_eq!(
            parse_super_size("getvar:partition-size:super FAILED (remote: 'unknown')"),
            0
        );
    }

    // A getvar that got no answer must not read as "not logical": that
    // would recreate an existing partition in `default`.
    #[test]
    fn is_logical_tells_no_from_no_answer() {
        let yes = "is-logical:system_a: yes\nFinished. Total time: 0.001s\n";
        assert_eq!(parse_is_logical(yes, "system_a"), Some(true));
        let no = "is-logical:abl_a: no\n";
        assert_eq!(parse_is_logical(no, "abl_a"), Some(false));
        // A partition the firmware adds: fastboot exits 0 even so.
        let unknown = "getvar:is-logical:my_new_a                         FAILED (remote: 'Unknown partition')\nFinished. Total time: 0.002s\n";
        assert_eq!(parse_is_logical(unknown, "my_new_a"), Some(false));
        let usb = "getvar:is-logical:system_a FAILED (Write to device failed (No such device))\n";
        assert_eq!(parse_is_logical(usb, "system_a"), None);
        assert_eq!(parse_is_logical("", "system_a"), None);
    }

    #[test]
    fn super_prep_keeps_the_active_slot_partitions() {
        let plan = super_prep_plan(&owned(&["odm", "system"]), "a");
        assert!(!plan.delete.contains(&"odm_a".to_string()));
        assert!(!plan.delete.contains(&"system_a".to_string()));
        assert_eq!(plan.shrink, owned(&["odm_a", "system_a"]));
    }

    #[test]
    fn super_prep_frees_the_inactive_slot_and_cow_images() {
        let plan = super_prep_plan(&owned(&["odm"]), "b");
        assert!(plan.delete.contains(&"odm_a".to_string()));
        assert!(plan.delete.contains(&"odm_b-cow".to_string()));
        assert!(plan.delete.contains(&"odm_a_cow".to_string()));
        assert!(plan.delete.contains(&"odm_b-cow-img".to_string()));
        assert_eq!(plan.shrink, owned(&["odm_b"]));
    }

    fn images(names: &[&str]) -> HashMap<String, PathBuf> {
        names
            .iter()
            .map(|n| (n.to_string(), PathBuf::from(format!("{n}.img"))))
            .collect()
    }

    // The routing predicates match exactly and case-sensitively: callers pass
    // names that collect_images has already lowercased and stripped of their
    // slot suffix. These pin that contract, so a caller that skips the
    // normalisation shows up here instead of on a device.

    #[test]
    fn super_partitions_are_matched_exactly() {
        for name in [
            "system",
            "system_ext",
            "vendor",
            "product",
            "odm",
            "my_product",
        ] {
            assert!(is_super_partition(name), "{name} should be dynamic");
        }
        for name in ["System", "system_a", "super", "boot", "modem", ""] {
            assert!(!is_super_partition(name), "{name} should not be dynamic");
        }
    }

    #[test]
    fn critical_partitions_are_matched_exactly() {
        for name in [
            "abl",
            "xbl",
            "xbl_config",
            "boot",
            "init_boot",
            "modem",
            "tz",
        ] {
            assert!(is_critical_partition(name), "{name} should be critical");
        }
        for name in ["system", "XBL", "xbl_a", "recovery", ""] {
            assert!(
                !is_critical_partition(name),
                "{name} should not be critical"
            );
        }
    }

    #[test]
    fn modem_goes_through_fastbootd_only_with_the_system() {
        assert_eq!(modem_route(true, true, true), ModemRoute::Fastbootd);
        // Switching slots without the system would boot an old or empty one,
        // and the bootloader may be unreachable: refused, not rerouted.
        assert_eq!(modem_route(true, true, false), ModemRoute::NeedsSystem);
        // Without a modem there is no reason to switch.
        assert_eq!(modem_route(true, false, true), ModemRoute::Bootloader);
        assert_eq!(modem_route(false, true, true), ModemRoute::Bootloader);
    }

    // Only what the user held back must be carried to the new slot; names
    // that are never flashed are not the user's choice.
    #[test]
    fn held_back_is_only_the_users_choice() {
        let skipped = vec![
            ("init_boot".to_string(), "user excluded".to_string()),
            (
                "persist".to_string(),
                "device-unique data, never part of a firmware".to_string(),
            ),
            ("xbl".to_string(), "xbl/abl excluded".to_string()),
            (
                "odd.name".to_string(),
                "not a plain partition name".to_string(),
            ),
        ];
        assert_eq!(held_back(&skipped), ["init_boot", "xbl"]);
    }

    #[test]
    fn other_slot_flips() {
        assert_eq!(other_slot("a"), "b");
        assert_eq!(other_slot("b"), "a");
    }

    #[test]
    fn only_modem_goes_through_the_bootloader() {
        assert!(is_bootloader_partition("modem"));
        for name in ["modem_a", "Modem", "boot", "xbl", ""] {
            assert!(!is_bootloader_partition(name), "{name}");
        }
    }

    #[test]
    fn xbl_abl_is_a_case_insensitive_prefix_match() {
        for name in [
            "xbl",
            "xbl_config",
            "XBL_CONFIG",
            "abl",
            "ABL_a",
            "xbl_ramdump",
        ] {
            assert!(is_xbl_abl(name), "{name}");
        }
        // Shorter than the prefix, or merely containing it later, is not a match.
        for name in ["", "xb", "ab", "system", "my_xbl"] {
            assert!(!is_xbl_abl(name), "{name}");
        }
    }

    #[test]
    fn preloader_is_a_case_insensitive_prefix_match() {
        for name in ["preloader", "PRELOADER", "preloader_raw", "preloader_a"] {
            assert!(is_preloader(name), "{name}");
        }
        for name in ["", "pre", "loader", "my_preloader"] {
            assert!(!is_preloader(name), "{name}");
        }
    }

    #[test]
    fn prefix_check_never_panics_on_short_or_non_ascii_input() {
        assert!(starts_with_ignore_ascii_case("anything", ""));
        assert!(!starts_with_ignore_ascii_case("x", "xbl"));
        // The comparison is on bytes, so a prefix that ends mid-codepoint is
        // simply a mismatch rather than a slicing panic.
        assert!(!starts_with_ignore_ascii_case("жbl", "xbl"));
        assert!(!starts_with_ignore_ascii_case("ж", "xb"));
    }

    #[test]
    fn mediatek_means_preloader_and_no_xbl() {
        assert!(is_mediatek_build(&images(&["preloader", "boot", "super"])));
        assert!(is_mediatek_build(&images(&["preloader_raw", "lk"])));

        // Qualcomm images always carry xbl or xbl_config.
        assert!(!is_mediatek_build(&images(&["preloader", "xbl"])));
        assert!(!is_mediatek_build(&images(&["preloader", "XBL.img"])));
        assert!(!is_mediatek_build(&images(&["preloader", "xbl_config"])));
        assert!(!is_mediatek_build(&images(&["preloader", "xbl_config_a"])));

        // No preloader at all is never MediaTek, xbl or not.
        assert!(!is_mediatek_build(&images(&["boot", "system"])));
        assert!(!is_mediatek_build(&images(&[])));
    }

    // get_active_slot's docstring: flashing dynamic partitions to the wrong
    // slot leaves the device unbootable, so anything but a clean `a` or `b`
    // must come back as None and make the caller abort.

    #[test]
    fn current_slot_is_read_from_real_fastboot_output() {
        let stderr = "current-slot: a\nFinished. Total time: 0.001s\n";
        assert_eq!(parse_current_slot(stderr).as_deref(), Some("a"));
        assert_eq!(
            parse_current_slot("(bootloader) current-slot: b").as_deref(),
            Some("b")
        );
        assert_eq!(parse_current_slot("Current-Slot: A").as_deref(), Some("a"));
    }

    #[test]
    fn current_slot_refuses_to_guess() {
        for output in [
            "",
            "current-slot:",
            "current-slot: c",
            // Some bootloaders answer with the suffix form. Accepting it is a
            // deliberate decision for another day; today it must not slip
            // through as something else.
            "current-slot: _a",
            "getvar:current-slot FAILED (remote: 'unknown variable')",
            "Finished. Total time: 0.001s",
        ] {
            assert_eq!(parse_current_slot(output), None, "{output:?}");
        }
    }

    #[test]
    fn getvar_matches_the_key_exactly() {
        assert_eq!(
            parse_fastboot_getvar("is-userspace: yes\nFinished.", "is-userspace").as_deref(),
            Some("yes")
        );
        assert_eq!(
            parse_fastboot_getvar("(bootloader) is-userspace: no", "is-userspace").as_deref(),
            Some("no")
        );
        // A key that is only a suffix of the real one must not match it.
        assert_eq!(
            parse_fastboot_getvar("is-userspace: yes", "userspace"),
            None
        );
        // Only the first colon separates key from value.
        assert_eq!(
            parse_fastboot_getvar("version-bootloader: 1.2:3", "version-bootloader").as_deref(),
            Some("1.2:3")
        );
    }

    #[test]
    fn getvar_failures_are_none() {
        for output in [
            "",
            "getvar:is-userspace FAILED (remote: 'unknown variable')",
            "Finished. Total time: 0.001s",
        ] {
            assert_eq!(
                parse_fastboot_getvar(output, "is-userspace"),
                None,
                "{output:?}"
            );
        }
    }

    fn names(map: &HashMap<String, PathBuf>) -> Vec<&str> {
        let mut v: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
        v.sort();
        v
    }

    #[test]
    fn firmware_cannot_overwrite_device_unique_partitions() {
        let (keep, skipped) = select_images(
            images(&[
                "boot", "system", "persist", "modemst1", "fsg", "userdata", "frp",
            ]),
            &FlashOptions::default(),
            false,
        );
        assert_eq!(names(&keep), ["boot", "system"]);
        let held: Vec<&str> = skipped.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(held, ["frp", "fsg", "modemst1", "persist", "userdata"]);
    }

    #[test]
    fn a_source_build_still_flashes_its_own_userdata() {
        let (keep, _) = select_images(
            images(&["boot", "system", "userdata"]),
            &FlashOptions::default(),
            true,
        );
        assert_eq!(names(&keep), ["boot", "system", "userdata"]);
    }

    #[test]
    fn names_fastboot_could_read_as_options_are_refused() {
        let (keep, skipped) = select_images(
            images(&["boot", "-w", "--slot=all", "--set-active=b", "a b", "ok_2"]),
            &FlashOptions::default(),
            true, // refused even for the user's own build
        );
        assert_eq!(names(&keep), ["boot", "ok_2"]);
        assert_eq!(skipped.len(), 4);
        assert!(
            skipped
                .iter()
                .all(|(_, r)| r == "not a plain partition name")
        );
    }

    #[test]
    fn existing_skip_options_keep_their_wording() {
        let opts = FlashOptions {
            skip_xbl_abl: true,
            skip_preloader: true,
            skip_partitions: " Vendor_Boot , ".into(),
            ..FlashOptions::default()
        };
        let (keep, skipped) = select_images(
            images(&["boot", "xbl", "abl", "preloader", "vendor_boot"]),
            &opts,
            false,
        );
        assert_eq!(names(&keep), ["boot"]);
        assert_eq!(
            skipped,
            [
                ("abl".to_string(), "xbl/abl excluded".to_string()),
                ("preloader".to_string(), "preloader excluded".to_string()),
                ("vendor_boot".to_string(), "user excluded".to_string()),
                ("xbl".to_string(), "xbl/abl excluded".to_string()),
            ]
        );
    }

    #[test]
    fn duplicate_images_resolve_the_same_way_every_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        for f in [
            "xbl_config_b.img",
            "XBL_CONFIG.img",
            "xbl_config_a.img",
            "deep/xbl_config.img",
        ] {
            let p = dir.path().join(f);
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(&p, b"x").expect("write");
        }
        let pick = collect_images(dir.path())
            .remove("xbl_config")
            .expect("xbl_config");
        // Shallowest first, then the lexically smallest: "XBL_CONFIG.img"
        // sorts before the lowercase names.
        assert_eq!(
            pick.file_name().and_then(|n| n.to_str()),
            Some("XBL_CONFIG.img")
        );
    }
}
