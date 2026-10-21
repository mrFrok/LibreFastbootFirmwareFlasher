// Background device detection: the install page shows the phone as soon as it
// is plugged in, without a "Find phone" button.
//
// A thread of its own rather than a worker command: the worker runs one
// command at a time and blocks, so a queued check would freeze the status for
// the whole of a download or a flash. While the worker is busy the poller
// stays quiet, so `fastboot devices` never interleaves with flashing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use lfff_lib::utils::run_cmd;

use crate::WMsg;

const INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Booted Android, reached through adb.
    System,
    Bootloader,
    /// Userspace fastboot, where dynamic partitions are flashed.
    Fastbootd,
}

impl Mode {
    /// Value of `DeviceInfo.mode` in structs.slint.
    pub fn as_ui(self) -> i32 {
        match self {
            Mode::System => 1,
            Mode::Bootloader => 2,
            Mode::Fastbootd => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub serial: String,
    /// What to show: the marketing model when adb has it, else the product.
    pub name: String,
    /// The codename (`ro.product.device` / `getvar product`): the key the
    /// chip choice is remembered under.
    pub product: String,
    pub slot: String,
    pub mode: Mode,
    /// `None` when the device does not say.
    pub unlocked: Option<bool>,
    /// -1 when unknown.
    pub battery: i32,
}

/// One adb round-trip for every property instead of a shell per value.
pub fn read_adb(serial: &str) -> DeviceSnapshot {
    let script = "echo model=$(getprop ro.product.model); \
                  echo device=$(getprop ro.product.device); \
                  echo slot=$(getprop ro.boot.slot_suffix); \
                  echo locked=$(getprop ro.boot.flash.locked); \
                  echo battery=$(cat /sys/class/power_supply/battery/capacity 2>/dev/null)";
    let r = run_cmd(&["adb", "-s", serial, "shell", script], 5);
    let get = |key: &str| -> String {
        r.stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let model = get("model");
    let product = get("device");
    if !product.is_empty() {
        crate::config::remember_device(
            serial,
            crate::config::KnownDevice {
                name: if model.is_empty() {
                    product.clone()
                } else {
                    model.clone()
                },
                product: product.clone(),
            },
        );
    }
    DeviceSnapshot {
        serial: serial.to_string(),
        name: if model.is_empty() {
            product.clone()
        } else {
            model
        },
        product,
        slot: get("slot").trim_start_matches('_').to_string(),
        mode: Mode::System,
        unlocked: match get("locked").as_str() {
            "0" => Some(true),
            "1" => Some(false),
            _ => None,
        },
        battery: get("battery").parse().unwrap_or(-1),
    }
}

pub fn read_fastboot(serial: &str) -> Option<DeviceSnapshot> {
    let info = lfff_lib::device::get_device_info(Some(serial))?;
    // Some OnePlus / OPPO / Realme devices list fastbootd as plain fastboot;
    // `is-userspace` tells them apart.
    let mode = match lfff_lib::flasher::fastbootd_status(Some(serial)) {
        lfff_lib::flasher::FastbootdStatus::Fastbootd
        | lfff_lib::flasher::FastbootdStatus::UserspaceFastboot => Mode::Fastbootd,
        _ => Mode::Bootloader,
    };
    let unlocked = match info.unlocked.to_lowercase().as_str() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    };
    // Named as Android named it, when the phone was ever seen booted; the
    // chip choice then follows the phone between modes.
    let known = crate::config::load_config()
        .known_devices
        .get(serial)
        .cloned();
    let (name, product) = match known {
        Some(k) => (k.name, k.product),
        None if info.product.is_empty() => (serial.to_string(), String::new()),
        None => (info.product.clone(), info.product),
    };
    Some(DeviceSnapshot {
        serial: serial.to_string(),
        name,
        product,
        slot: info.current_slot,
        mode,
        unlocked,
        battery: info.battery_level,
    })
}

/// Serials and modes as `adb devices` / `fastboot devices` list them, cheap
/// enough to run every cycle; the getvar reads only follow a change.
fn listing() -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    let fb = run_cmd(&["fastboot", "devices"], 5);
    if fb.code == 0 {
        for line in fb.stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                [s, "fastbootd", ..] => out.push((s.to_string(), "fastbootd")),
                [s, "fastboot", ..] => out.push((s.to_string(), "fastboot")),
                _ => {}
            }
        }
    }
    for s in lfff_lib::device::list_adb_devices() {
        out.push((s, "adb"));
    }
    out
}

pub fn spawn(
    tx: mpsc::Sender<WMsg>,
    busy: Arc<AtomicBool>,
    current: Arc<Mutex<Option<DeviceSnapshot>>>,
) {
    thread::spawn(move || {
        let mut last: Vec<(String, &'static str)> = Vec::new();
        loop {
            if !busy.load(Ordering::Relaxed) {
                let now = listing();
                if now != last {
                    let snap = match now.first() {
                        Some((s, "adb")) => Some(read_adb(s)),
                        Some((s, _)) => read_fastboot(s),
                        None => None,
                    };
                    // The worker may have started while the device was read:
                    // its own messages win, so drop this one.
                    if !busy.load(Ordering::Relaxed) {
                        *current.lock().unwrap() = snap.clone();
                        let msg = match snap {
                            Some(s) => WMsg::DeviceDetected(s),
                            None => WMsg::DeviceDisconnected,
                        };
                        if tx.send(msg).is_err() {
                            return;
                        }
                        last = now;
                    }
                }
            }
            thread::sleep(INTERVAL);
        }
    });
}
