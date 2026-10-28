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
        unlocked: match get(&["unlocked"]) {
            // Some fastbootd builds (OnePlus Nord 5) leave it out of
            // `getvar all` but answer it asked on its own.
            v if v.is_empty() => getvar(serial, "unlocked"),
            v => v,
        },
        battery_level: battery,
        slot_count: slots,
        current_slot: get(&["current-slot"]),
        raw,
    })
}

/// One `fastboot getvar <name>`; empty when the device does not say.
pub fn getvar(serial: Option<&str>, name: &str) -> String {
    let mut cmd: Vec<&str> = vec!["fastboot"];
    if let Some(ser) = serial {
        cmd.extend(["-s", ser]);
    }
    cmd.extend(["getvar", name]);
    let r = run_cmd(&cmd, 10);
    let out = if r.stderr.is_empty() {
        &r.stdout
    } else {
        &r.stderr
    };
    parse_getvar_output(out)
        .get(name)
        .cloned()
        .unwrap_or_default()
}

/// What fastboot says about the bootloader lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    Unlocked,
    Locked,
    /// The device does not say. Flashing goes on: a locked bootloader
    /// refuses every write itself, so nothing is half-written.
    Unknown,
}

impl LockState {
    pub fn from_getvar(value: &str) -> LockState {
        match value.trim().to_lowercase().as_str() {
            "yes" | "true" | "1" => LockState::Unlocked,
            "no" | "false" | "0" => LockState::Locked,
            _ => LockState::Unknown,
        }
    }
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
    match LockState::from_getvar(&di.unlocked) {
        LockState::Unlocked => c.unlocked = true,
        LockState::Locked => c
            .errors
            .push("Bootloader is locked. Unlock: fastboot flashing unlock".into()),
        LockState::Unknown => {
            c.unlocked = true;
            c.warnings.push(
                "The device does not report its bootloader lock state; a locked bootloader will refuse to flash".into(),
            );
        }
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

// ---------------------------------------------------------------------------
// Reading partitions over adb as root
// ---------------------------------------------------------------------------

/// Why a partition could not be read from the phone over adb.
///
/// fastbootd cannot help here: its `fetch` is compiled out of user builds,
/// refused on locked devices and limited to vendor_boot. Partitions can only
/// be read from Android or a recovery over adb, as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdbReadError {
    /// No phone in adb mode (booted Android with USB debugging, or recovery).
    NoDevice,
    /// The adb shell is not root and `su` was refused or is missing.
    NoRoot,
    /// Root works but the partition could not be read whole.
    ReadFailed(String),
}

impl std::fmt::Display for AdbReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no phone found over adb"),
            Self::NoRoot => write!(f, "root is required to read partitions from the phone"),
            Self::ReadFailed(e) => write!(f, "could not read from the phone: {e}"),
        }
    }
}

/// A root shell on a phone over adb.
pub struct AdbRoot {
    serial: Option<String>,
    /// Commands go through `su -c`; false when the adb shell is root already.
    use_su: bool,
    /// The active slot as Android booted it: `_a`, `_b`, or empty.
    pub slot_suffix: String,
}

impl AdbRoot {
    /// Root on the phone: as is when the adb shell is already root
    /// (recovery, `adb root` on a userdebug build), through `su` otherwise.
    /// Magisk asks on the phone's screen the first time, hence the long
    /// timeout; KernelSU and APatch never ask — Shell must already be granted
    /// in their manager, or `su` is refused.
    pub fn open(serial: Option<&str>) -> Result<Self, AdbReadError> {
        let devices = list_adb_devices();
        let present = match serial {
            Some(s) => devices.iter().any(|d| d == s),
            None => !devices.is_empty(),
        };
        if !present {
            return Err(AdbReadError::NoDevice);
        }
        let mut root = Self {
            serial: serial.map(str::to_string),
            use_su: false,
            slot_suffix: String::new(),
        };
        let r = root.run("id -u", 15);
        if !(r.code == 0 && r.stdout.trim() == "0") {
            root.use_su = true;
            let r = root.run("id -u", 60);
            if !(r.code == 0 && r.stdout.trim() == "0") {
                return Err(AdbReadError::NoRoot);
            }
        }
        root.slot_suffix = root
            .run("getprop ro.boot.slot_suffix", 15)
            .stdout
            .trim()
            .to_string();
        Ok(root)
    }

    fn args<'a>(&'a self, rest: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["adb"];
        if let Some(s) = &self.serial {
            args.extend(["-s", s.as_str()]);
        }
        args.extend_from_slice(rest);
        args
    }

    fn as_root(&self, cmd: &str) -> String {
        if self.use_su {
            format!("su -c '{cmd}'")
        } else {
            cmd.to_string()
        }
    }

    fn run(&self, cmd: &str, timeout: u64) -> crate::utils::CmdResult {
        let cmd = self.as_root(cmd);
        run_cmd(&self.args(&["shell", &cmd]), timeout)
    }

    /// The block device of `part` (a full name, slot suffix included) and
    /// its size in bytes.
    fn locate(&self, part: &str) -> Result<(String, u64), AdbReadError> {
        for dir in ["/dev/block/by-name", "/dev/block/bootdevice/by-name"] {
            let dev = format!("{dir}/{part}");
            let r = self.run(&format!("blockdev --getsize64 {dev}"), 15);
            if r.code == 0
                && let Ok(size) = r.stdout.trim().parse::<u64>()
            {
                return Ok((dev, size));
            }
        }
        Err(AdbReadError::ReadFailed(format!(
            "{part}: no such partition"
        )))
    }

    /// Read a small partition whole into memory.
    pub fn read(&self, part: &str) -> Result<Vec<u8>, AdbReadError> {
        let (dev, size) = self.locate(part)?;
        let cmd = self.as_root(&format!("cat {dev}"));
        // exec-out, not shell: the image is binary and must arrive unmangled.
        let args = self.args(&["exec-out", &cmd]);
        let out = std::process::Command::new(args[0])
            .args(&args[1..])
            .output()
            .map_err(|e| AdbReadError::ReadFailed(e.to_string()))?;
        check_whole(part, out.stdout.len() as u64, size, &out.stdout)?;
        Ok(out.stdout)
    }

    /// Stream a partition into `path`. The size is checked against the
    /// partition's: exec-out mixes errors into the data, so a short file is
    /// an error message, not an image.
    pub fn dump_to(&self, part: &str, path: &std::path::Path) -> Result<(), AdbReadError> {
        let (dev, size) = self.locate(part)?;
        let file = fs::File::create(path).map_err(|e| AdbReadError::ReadFailed(e.to_string()))?;
        let cmd = self.as_root(&format!("cat {dev}"));
        let args = self.args(&["exec-out", &cmd]);
        let status = std::process::Command::new(args[0])
            .args(&args[1..])
            .stdout(file)
            .status()
            .map_err(|e| AdbReadError::ReadFailed(e.to_string()))?;
        let got = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let head = if got < 512 {
            fs::read(path).unwrap_or_default()
        } else {
            Vec::new()
        };
        if !status.success() {
            fs::remove_file(path).ok();
            return Err(AdbReadError::ReadFailed(format!(
                "{part}: adb exited with {status}"
            )));
        }
        if let Err(e) = check_whole(part, got, size, &head) {
            fs::remove_file(path).ok();
            return Err(e);
        }
        Ok(())
    }
}

/// A read is good only when every byte of the partition arrived.
fn check_whole(part: &str, got: u64, want: u64, data: &[u8]) -> Result<(), AdbReadError> {
    if got == want && want > 0 {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&data[..data.len().min(200)])
        .trim()
        .to_string();
    Err(AdbReadError::ReadFailed(if text.is_empty() {
        format!("{part}: got {got} of {want} bytes")
    } else {
        format!("{part}: got {got} of {want} bytes ({text})")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_state_needs_an_explicit_answer_either_way() {
        assert_eq!(LockState::from_getvar("yes"), LockState::Unlocked);
        assert_eq!(LockState::from_getvar(" True"), LockState::Unlocked);
        assert_eq!(LockState::from_getvar("no"), LockState::Locked);
        assert_eq!(LockState::from_getvar("0"), LockState::Locked);
        assert_eq!(LockState::from_getvar(""), LockState::Unknown);
    }

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
    // exec-out mixes an error message into the data: anything short of the
    // whole partition must be refused, never taken for an image.
    #[test]
    fn a_short_read_is_not_an_image() {
        let msg = b"/system/bin/sh: su: inaccessible or not found";
        assert!(check_whole("xbl_config_b", msg.len() as u64, 1 << 20, msg).is_err());
        assert!(check_whole("boot_a", 0, 0, &[]).is_err());
        assert!(check_whole("boot_a", 4096, 4096, &[]).is_ok());
    }
}
