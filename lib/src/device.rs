//! Device discovery and pre-flash diagnostics.
//!
//! Provides device detection (fastboot/adb), info retrieval via `fastboot getvar`,
//! cable speed testing via `fastboot stage`, and a comprehensive pre-flash check.

use std::collections::HashMap;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use tracing::{error, info, warn};

use crate::utils::run_cmd;

const CABLE_SPEED_THRESHOLD_MB: f64 = 1.0;
const CABLE_TEST_PAYLOAD_MB: usize = 8;
/// Device polling interval in milliseconds. `fastboot devices` is a cheap
/// local USB enumeration, so polling twice a second keeps detection snappy.
const POLL_INTERVAL_MS: u64 = 500;
const BATTERY_MIN_LEVEL: i32 = 30;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// Key device variables retrieved via fastboot getvar.
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub serial: String,
    pub product: String,
    pub variant: String,
    pub bootloader_version: String,
    pub baseband_version: String,
    pub secure: String,
    pub unlocked: String,
    pub battery_level: i32, // -1 = not reported
    pub slot_count: i32,    // A/B devices report 2
    pub current_slot: String,
    pub raw: HashMap<String, String>,
}

impl DeviceInfo {
    pub fn new() -> Self {
        Self {
            battery_level: -1,
            slot_count: 1,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CableTestResult {
    pub passed: bool,
    pub speed_mbs: f64,
    pub error: String,
}

/// Comprehensive pre-flash check result.
#[derive(Debug, Clone)]
pub struct PreFlashCheck {
    pub device_found: bool,
    pub communication_ok: bool,
    pub cable_ok: bool,
    pub battery_ok: bool,
    pub unlocked: bool,
    pub device_info: DeviceInfo,
    pub cable_result: CableTestResult,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl Default for PreFlashCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl PreFlashCheck {
    pub fn new() -> Self {
        Self {
            device_found: false,
            communication_ok: false,
            cable_ok: false,
            battery_ok: false,
            unlocked: false,
            device_info: DeviceInfo::new(),
            cable_result: CableTestResult::default(),
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }
    /// True only when all hard requirements pass.
    pub fn ready(&self) -> bool {
        self.device_found
            && self.communication_ok
            && self.cable_ok
            && self.battery_ok
            && self.unlocked
    }
}

// ---------------------------------------------------------------------------
// Device discovery
// ---------------------------------------------------------------------------

/// Return serial numbers of devices in fastboot/fastbootd mode.
pub fn list_fastboot_devices() -> Vec<String> {
    let r = run_cmd(&["fastboot", "devices"], 5);
    if r.code != 0 || r.stdout.is_empty() {
        return Vec::new();
    }
    r.stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && (parts[1] == "fastboot" || parts[1] == "fastbootd") {
                Some(parts[0].to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Return serial numbers of devices in fastbootd mode only.
pub fn list_fastbootd_devices() -> Vec<String> {
    let r = run_cmd(&["fastboot", "devices"], 5);
    if r.code != 0 || r.stdout.is_empty() {
        return Vec::new();
    }
    r.stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "fastbootd" {
                Some(parts[0].to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Return serial numbers of devices reachable via adb.
pub fn list_adb_devices() -> Vec<String> {
    let r = run_cmd(&["adb", "devices"], 5);
    if r.code != 0 {
        return Vec::new();
    }
    r.stdout
        .lines()
        .skip(1)
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == "device" {
                Some(parts[0].to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Reboot device from ADB into fastbootd.
pub fn reboot_to_fastbootd(serial: Option<&str>) -> bool {
    let mut cmd: Vec<&str> = vec!["adb"];
    let s;
    if let Some(ser) = serial {
        s = ser.to_string();
        cmd.push("-s");
        cmd.push(&s);
    }
    cmd.extend(&["reboot", "fastboot"]);
    let r = run_cmd(&cmd, 15);
    if r.code != 0 {
        error!("adb reboot fastboot failed: {}", r.stderr);
        return false;
    }
    info!("Reboot to fastbootd sent — waiting for device ...");
    true
}

/// Poll fastboot devices until one appears or timeout expires.
pub fn wait_for_device(timeout: u64) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut last_log = Instant::now();
    while Instant::now() < deadline {
        let serials = list_fastboot_devices();
        if !serials.is_empty() {
            return serials.into_iter().next();
        }
        if last_log.elapsed().as_secs() >= 5 {
            let remaining = (deadline - Instant::now()).as_secs();
            info!("No device found — retrying … ({}s remaining)", remaining);
            last_log = Instant::now();
        }
        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
    None
}

// ---------------------------------------------------------------------------
// Device info
// ---------------------------------------------------------------------------

/// Parse `fastboot getvar all` output into key/value pairs.
fn parse_getvar_output(output: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for line in output.lines() {
        let line = line.trim().trim_start_matches("(bootloader)").trim();
        if let Some((key, value)) = line.split_once(':') {
            result.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    result
}

/// Retrieve device info via `fastboot getvar all`. Returns None on failure.
pub fn get_device_info(serial: Option<&str>) -> Option<DeviceInfo> {
    let mut cmd: Vec<&str> = vec!["fastboot"];
    let s;
    if let Some(ser) = serial {
        s = ser.to_string();
        cmd.push("-s");
        cmd.push(&s);
    }
    cmd.extend(&["getvar", "all"]);

    let r = run_cmd(&cmd, 15);
    let raw_output = if r.stderr.is_empty() {
        &r.stdout
    } else {
        &r.stderr
    };
    if raw_output.is_empty() {
        error!("fastboot getvar all returned no output");
        return None;
    }

    let raw = parse_getvar_output(raw_output);
    let get = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(v) = raw.get(*k) {
                return v.clone();
            }
        }
        String::new()
    };

    let battery = get(&["battery-level", "battery_level"])
        .trim_end_matches('%')
        .parse::<i32>()
        .unwrap_or(-1);
    let slots = get(&["slot-count", "slot_count"])
        .parse::<i32>()
        .unwrap_or(1);

    Some(DeviceInfo {
        serial: serial
            .map(|s| s.to_string())
            .unwrap_or_else(|| get(&["serialno"])),
        product: get(&["product"]),
        variant: get(&["variant"]),
        bootloader_version: get(&["version-bootloader", "bootloader-version"]),
        baseband_version: get(&["version-baseband", "baseband-version"]),
        secure: get(&["secure"]),
        unlocked: get(&["unlocked"]),
        battery_level: battery,
        slot_count: slots,
        current_slot: get(&["current-slot"]),
        raw,
    })
}

// ---------------------------------------------------------------------------
// Cable speed test
// ---------------------------------------------------------------------------

/// Estimate USB transfer speed via `fastboot stage` (RAM only, no NAND write).
pub fn test_cable_speed(serial: Option<&str>) -> CableTestResult {
    let bytes = CABLE_TEST_PAYLOAD_MB * 1024 * 1024;
    // Unpredictable per-run temp file: a fixed /tmp name is open to symlink
    // attacks and collides between concurrent runs. Deleted on drop.
    let tmp = match tempfile::Builder::new()
        .prefix("lfff-cable-")
        .suffix(".img")
        .tempfile()
    {
        Ok(f) => f,
        Err(e) => {
            return CableTestResult {
                passed: false,
                speed_mbs: 0.0,
                error: format!("Cannot create test payload: {}", e),
            };
        }
    };
    if let Err(e) = fs::write(tmp.path(), vec![0u8; bytes]) {
        return CableTestResult {
            passed: false,
            speed_mbs: 0.0,
            error: format!("Cannot create test payload: {}", e),
        };
    }

    let ts = tmp.path().to_string_lossy().to_string();
    let mut cmd: Vec<&str> = vec!["fastboot"];
    let s;
    if let Some(ser) = serial {
        s = ser.to_string();
        cmd.push("-s");
        cmd.push(&s);
    }
    cmd.extend(&["stage", &ts]);

    let start = Instant::now();
    let r = run_cmd(&cmd, 60);
    let elapsed = start.elapsed().as_secs_f64();
    drop(tmp);

    if r.code != 0 {
        let stderr_lower = r.stderr.to_lowercase();
        let is_unsupported = stderr_lower.contains("unknown command")
            || stderr_lower.contains("not supported")
            || stderr_lower.contains("not implemented")
            || stderr_lower.contains("no such command");
        if is_unsupported {
            return CableTestResult {
                passed: true,
                speed_mbs: 0.0,
                error: "fastboot stage not supported on this device (speed test skipped)".into(),
            };
        }
        return CableTestResult {
            passed: false,
            speed_mbs: 0.0,
            error: format!("Cable speed test failed: {}", r.stderr),
        };
    }
    if elapsed <= 0.0 {
        return CableTestResult {
            passed: false,
            speed_mbs: 0.0,
            error: "Elapsed time was zero".into(),
        };
    }

    let speed = bytes as f64 / elapsed / (1024.0 * 1024.0);
    let passed = speed >= CABLE_SPEED_THRESHOLD_MB;
    if !passed {
        warn!("Cable speed {:.2} MB/s is below threshold", speed);
    } else {
        info!("Cable speed OK: {:.2} MB/s", speed);
    }
    CableTestResult {
        passed,
        speed_mbs: speed,
        error: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Pre-flash check orchestrator
// ---------------------------------------------------------------------------

/// Run all pre-flash diagnostics and return a summary.
pub fn run_pre_flash_checks(serial: Option<&str>) -> PreFlashCheck {
    let mut c = PreFlashCheck::new();

    // Step 1: device discovery
    info!("==> [1/4] Detecting device …");
    let serials = list_fastboot_devices();
    if serials.is_empty() {
        let adb_s = list_adb_devices();
        if !adb_s.is_empty() {
            c.device_found = true;
            c.errors.push(
                "Device found via ADB, but flashing requires fastboot/fastbootd mode. \
                 Please reboot to bootloader (Vol Down + Power)."
                    .into(),
            );
            return c;
        }
        c.errors.push(
            "No device found via fastboot or adb. Boot into fastboot: hold Vol Down + Power".into(),
        );
        return c;
    }

    let ser = serial
        .map(|s| s.to_string())
        .unwrap_or_else(|| serials[0].clone());
    c.device_found = true;
    info!("Device found: {}", ser);

    // Step 2: communication
    info!("==> [2/4] Testing communication …");
    let di = match get_device_info(Some(&ser)) {
        Some(i) => i,
        None => {
            c.errors.push("fastboot getvar all failed".into());
            return c;
        }
    };
    c.communication_ok = true;
    c.device_info = di.clone();
    info!(
        "  Product: {} | Unlocked: {} | Battery: {}%",
        di.product, di.unlocked, di.battery_level
    );

    // Step 3: cable speed
    info!("==> [3/4] Testing cable speed …");
    let cable = test_cable_speed(Some(&ser));
    c.cable_ok = cable.passed;
    if !cable.passed {
        // ready() treats a failed cable test as a hard error, so report it in
        // errors — otherwise the user sees "checks failed" with no reason listed.
        if cable.error.is_empty() {
            c.errors.push(format!(
                "Cable too slow ({:.2} MB/s, need ≥{:.1} MB/s). Use a different cable or a USB 3.0 port.",
                cable.speed_mbs, CABLE_SPEED_THRESHOLD_MB
            ));
        } else {
            c.errors.push(cable.error.clone());
        }
    }
    c.cable_result = cable;

    // Step 4: safety
    info!("==> [4/4] Safety checks …");
    let ul = di.unlocked.to_lowercase();
    if ul == "yes" || ul == "true" || ul == "1" {
        c.unlocked = true;
    } else {
        c.errors
            .push("Bootloader is locked. Unlock: fastboot flashing unlock".into());
    }

    if di.battery_level == -1 {
        c.warnings.push("Battery level not reported".into());
        c.battery_ok = true;
    } else if di.battery_level < BATTERY_MIN_LEVEL {
        c.battery_ok = false;
        c.errors.push(format!(
            "Battery too low ({}%). Need ≥{}%.",
            di.battery_level, BATTERY_MIN_LEVEL
        ));
    } else {
        c.battery_ok = true;
    }

    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_getvar() {
        let out = "(bootloader) product: lemonade\n(bootloader) unlocked: yes\n(bootloader) battery-level: 85%";
        let v = parse_getvar_output(out);
        assert_eq!(v.get("product"), Some(&"lemonade".to_string()));
        assert_eq!(v.get("unlocked"), Some(&"yes".to_string()));
    }

    #[test]
    fn test_preflash_ready() {
        let mut c = PreFlashCheck::new();
        c.device_found = true;
        c.communication_ok = true;
        c.cable_ok = true;
        c.battery_ok = true;
        c.unlocked = true;
        assert!(c.ready());
        c.battery_ok = false;
        assert!(!c.ready());
        c.battery_ok = true;
        c.unlocked = false;
        assert!(!c.ready());
    }
}
