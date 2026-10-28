// Background device detection: the install page shows the phone as soon as it
// is plugged in, without a "Find phone" button.
//
// A thread of its own rather than a worker command: the worker runs one
// command at a time and blocks, so a queued check would freeze the status for
// the whole of a download or a flash. While the worker is busy the poller
// stays quiet, and the worker waits out a round already running, so
// `fastboot devices` never interleaves with flashing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use lfff_lib::utils::run_cmd;

use crate::WMsg;

const INTERVAL: Duration = Duration::from_secs(2);

/// Held by the poller for a whole round. A round takes seconds (several
/// getvars), so checking `busy` once at its start is not enough: the worker
/// sets `busy`, then calls [`wait_for_poll`] to let a round already running
/// finish before it talks to the phone itself.
static ROUND: Mutex<()> = Mutex::new(());

/// Block until a poll round in progress is over. Call after setting `busy`.
pub fn wait_for_poll() {
    drop(ROUND.lock().unwrap_or_else(|e| e.into_inner()));
}

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
    /// `product` is the codename Android reported (now, or remembered),
    /// not fastboot's platform name: only then can it be checked against
    /// the device a firmware is built for.
    pub codename_known: bool,
    /// `ro.build.version.ota` and `ro.build.oplus_nv_id` (the region's
    /// carrier), read from Android only; empty otherwise. They tell which
    /// official build the phone runs.
    pub ota_version: String,
    pub nv_id: String,
}

/// One adb round-trip for every property instead of a shell per value.
pub fn read_adb(serial: &str) -> DeviceSnapshot {
    let script = "echo model=$(getprop ro.product.model); \
                  echo device=$(getprop ro.product.device); \
                  echo slot=$(getprop ro.boot.slot_suffix); \
                  echo locked=$(getprop ro.boot.flash.locked); \
                  echo ota=$(getprop ro.build.version.ota); \
                  echo nv=$(getprop ro.build.oplus_nv_id); \
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
    let codename_known = !product.is_empty();
    if codename_known {
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
        // Only "unlocked" is believed from Android: tools that hide the
        // unlock (a GBL exploit, root hiders) make it report locked on a
        // phone fastboot can flash. fastboot's own answer decides, and the
        // pre-flash check still refuses a phone fastboot calls locked.
        unlocked: (get("locked") == "0").then_some(true),
        battery: get("battery").parse().unwrap_or(-1),
        codename_known,
        ota_version: get("ota"),
        nv_id: get("nv"),
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
    let codename_known = known.is_some();
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
        codename_known,
        ota_version: String::new(),
        nv_id: String::new(),
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
    let adb = run_cmd(&["adb", "devices"], 5);
    if adb.code == 0 {
        for line in adb.stdout.lines().skip(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                [s, "device", ..] => out.push((s.to_string(), "adb")),
                [s, "unauthorized", ..] => out.push((s.to_string(), "unauthorized")),
                _ => {}
            }
        }
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
            // `busy` is read under the lock: a worker that set it and then
            // waited in `wait_for_poll` is seen here, so no round starts
            // after it.
            let round = ROUND.lock().unwrap_or_else(|e| e.into_inner());
            if !busy.load(Ordering::Relaxed) {
                let now = listing();
                if now != last {
                    // A usable device wins over one still asking for
                    // permission.
                    let usable = now.iter().find(|(_, m)| *m != "unauthorized");
                    let snap = match usable {
                        Some((s, "adb")) => Some(read_adb(s)),
                        Some((s, _)) => read_fastboot(s),
                        None => None,
                    };
                    let unauthorized = usable.is_none() && !now.is_empty();
                    // The worker may have started while the device was read:
                    // its own messages win, so drop this one.
                    if !busy.load(Ordering::Relaxed) {
                        *current.lock().unwrap() = snap.clone();
                        let msg = match snap {
                            Some(s) => WMsg::DeviceDetected(s),
                            None if unauthorized => WMsg::DeviceUnauthorized,
                            None => WMsg::DeviceDisconnected,
                        };
                        if tx.send(msg).is_err() {
                            return;
                        }
                        last = now;
                    }
                }
            }
            drop(round);
            thread::sleep(INTERVAL);
        }
    });
}
