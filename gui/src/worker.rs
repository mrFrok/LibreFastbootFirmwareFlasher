use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use lfff_lib::arb::ArbVerdict;

use super::{Cmd, FlashMethod, LogLevel, WMsg};
use crate::device_poll::{self, DeviceSnapshot};

/// Clears the worker-busy flag when a command ends, however it ends —
/// `continue` leaves most arms early.
struct Idle<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for Idle<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn log(tx: &mpsc::Sender<WMsg>, l: LogLevel, tab: u8, m: impl Into<String>) {
    tx.send(WMsg::Log {
        level: l,
        message: m.into(),
        tab,
    })
    .ok();
}

fn get_output_dir() -> PathBuf {
    crate::config::get_output_dir()
}

/// Watch `dir` for newly created .img files and log each one, with a
/// heartbeat while nothing new appears. Returns a stop sender — send `()`
/// (or drop it) to end the watcher.
fn spawn_img_watcher(tx: mpsc::Sender<WMsg>, dir: PathBuf, tab: u8) -> mpsc::Sender<()> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut known = std::collections::HashSet::<String>::new();
        let mut secs = 0u32;
        loop {
            thread::sleep(Duration::from_secs(1));
            match stop_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            secs += 1;
            let mut q = vec![dir.clone()];
            while let Some(d) = q.pop() {
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            q.push(p);
                            continue;
                        }
                        let name = p
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if name.ends_with(".img") && known.insert(name.clone()) {
                            tx.send(WMsg::Log {
                                level: LogLevel::Info,
                                message: format!("Extracted: {}", name),
                                tab,
                            })
                            .ok();
                        }
                    }
                }
            }
            if secs.is_multiple_of(5) && known.is_empty() {
                tx.send(WMsg::Log {
                    level: LogLevel::Info,
                    message: format!("Extracting... ({}s)", secs),
                    tab,
                })
                .ok();
            }
        }
    });
    stop_tx
}

/// After a fastboot-mode wait succeeds, adopt the device's fastboot serial
/// when it differs from the stored one (adb and fastboot serials can differ
/// on some devices) — later per-serial fastboot calls would otherwise hang
/// until timeout with a confusing error.
fn adopt_fastboot_serial(tx: &mpsc::Sender<WMsg>, serial: &mut Option<String>, tab: u8) {
    let devs = lfff_lib::device::list_fastboot_devices();
    match serial {
        Some(cur) if !devs.contains(cur) && devs.len() == 1 => {
            log(
                tx,
                LogLevel::Warn,
                tab,
                format!("Device serial changed: {} → {}", cur, devs[0]),
            );
            *serial = Some(devs[0].clone());
        }
        None if devs.len() == 1 => *serial = Some(devs[0].clone()),
        _ => {}
    }
}

/// Parse "1.2.3" (optional leading `v`, pre-release/build suffix ignored)
/// into a tuple for ordering comparisons.
fn version_tuple(s: &str) -> (u64, u64, u64) {
    let core = s.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// Extract ZIP firmware and watch staging directory for new .img files.
/// Returns the output directory on success, or None on failure.
/// This helper eliminates ~200 lines of duplicated code across 4 Cmd handlers.
fn extract_and_watch(zip_path: &str, tx: &mpsc::Sender<WMsg>, tab: u8) -> Option<PathBuf> {
    let fw = Path::new(zip_path);
    let fw_name = lfff_lib::extractor::get_firmware_name(fw);
    let out = get_output_dir().join(&fw_name);

    log(
        tx,
        LogLevel::Info,
        tab,
        format!("Extracting to {}...", out.display()),
    );

    let tx_ex = tx.clone();
    let staging = out.join("_staging");
    std::fs::create_dir_all(&staging).ok();
    let stop_tx = spawn_img_watcher(tx.clone(), staging.clone(), tab);

    let r = lfff_lib::extractor::extract_firmware_with_log(
        fw,
        &out,
        None,
        None,
        Some(&|line: String| {
            tx_ex
                .send(WMsg::Log {
                    level: LogLevel::Info,
                    message: line,
                    tab,
                })
                .ok();
        }),
    );
    stop_tx.send(()).ok();

    if !r.success {
        log(tx, LogLevel::Error, tab, "Extract failed");
        tx.send(WMsg::FlashComplete {
            success: false,
            message: r.error,
            log_summary: "Extract failed".into(),
            failed_partitions: vec![],
        })
        .ok();
        tx.send(WMsg::Flashing(false)).ok();
        return None;
    }

    log(
        tx,
        LogLevel::Success,
        tab,
        format!("{} groups extracted", r.groups.len()),
    );
    Some(r.output_dir)
}

/// Wait for a device in fastboot/fastbootd mode. Checks immediately and then
/// polls at a short interval (500ms, inside the lib helper) — no fixed sleeps.
fn wait_for_fastboot(
    tx: &mpsc::Sender<WMsg>,
    tab: u8,
    timeout_secs: u64,
    serial: Option<&str>,
) -> bool {
    log(tx, LogLevel::Info, tab, "Waiting for device in fastboot...");
    if lfff_lib::flasher::wait_for_any_fastboot(serial, timeout_secs) {
        log(tx, LogLevel::Success, tab, "Device found in fastboot!");
        true
    } else {
        log(
            tx,
            LogLevel::Error,
            tab,
            "Timeout waiting for device in fastboot",
        );
        false
    }
}

fn fastbootd_fallback_detail(status: lfff_lib::flasher::FastbootdStatus) -> String {
    match status {
        lfff_lib::flasher::FastbootdStatus::UserspaceFastboot => {
            "The device is listed as plain 'fastboot', but it reports 'is-userspace: yes'. This is a known OnePlus / OPPO / Realme fastbootd label issue. Continuing will use this userspace fastboot session for dynamic partitions.".into()
        }
        lfff_lib::flasher::FastbootdStatus::BootloaderFastboot => {
            "The device is still listed as bootloader fastboot and did not confirm userspace fastboot. Dynamic partition flashing may fail from this mode. Continue only if this device is known to support it.".into()
        }
        lfff_lib::flasher::FastbootdStatus::UnknownFastboot => {
            "A fastboot device is connected, but LFFF could not confirm fastbootd/userspace mode. Dynamic partition flashing may fail from this mode.".into()
        }
        lfff_lib::flasher::FastbootdStatus::NotFound => {
            "No fastboot device was found.".into()
        }
        lfff_lib::flasher::FastbootdStatus::Fastbootd => String::new(),
    }
}

fn confirm_fastbootd_fallback(
    tx: &mpsc::Sender<WMsg>,
    tab: u8,
    status: lfff_lib::flasher::FastbootdStatus,
) -> bool {
    let detail = fastbootd_fallback_detail(status);
    log(tx, LogLevel::Warn, tab, detail.clone());
    let (resp_tx, resp_rx) = std::sync::mpsc::channel();
    if tx
        .send(WMsg::FastbootdFallback {
            detail,
            response: resp_tx,
        })
        .is_err()
    {
        return false;
    }
    resp_rx.recv().unwrap_or(false)
}

fn handle_fastbootd_status(
    tx: &mpsc::Sender<WMsg>,
    tab: u8,
    status: lfff_lib::flasher::FastbootdStatus,
) -> bool {
    match status {
        lfff_lib::flasher::FastbootdStatus::Fastbootd => {
            log(tx, LogLevel::Success, tab, "Device confirmed in fastbootd");
            true
        }
        lfff_lib::flasher::FastbootdStatus::UserspaceFastboot => {
            if confirm_fastbootd_fallback(tx, tab, status) {
                log(
                    tx,
                    LogLevel::Success,
                    tab,
                    "Continuing with confirmed userspace fastboot",
                );
                true
            } else {
                log(
                    tx,
                    LogLevel::Warn,
                    tab,
                    "Stopped by user at fastbootd warning",
                );
                false
            }
        }
        lfff_lib::flasher::FastbootdStatus::BootloaderFastboot
        | lfff_lib::flasher::FastbootdStatus::UnknownFastboot => {
            if confirm_fastbootd_fallback(tx, tab, status) {
                log(
                    tx,
                    LogLevel::Warn,
                    tab,
                    "Continuing without confirmed fastbootd/userspace mode",
                );
                true
            } else {
                log(
                    tx,
                    LogLevel::Warn,
                    tab,
                    "Stopped by user at fastbootd warning",
                );
                false
            }
        }
        lfff_lib::flasher::FastbootdStatus::NotFound => {
            log(tx, LogLevel::Error, tab, "Device not found in fastbootd");
            false
        }
    }
}

fn wait_for_flash_fastbootd(
    tx: &mpsc::Sender<WMsg>,
    tab: u8,
    serial: Option<&str>,
    timeout_secs: u64,
) -> bool {
    log(
        tx,
        LogLevel::Info,
        tab,
        "Waiting for device to enter fastbootd/userspace fastboot...",
    );
    let status = lfff_lib::flasher::wait_for_fastbootd_status(serial, timeout_secs);
    handle_fastbootd_status(tx, tab, status)
}

/// Result of the cable check: a status line, and whether it passed.
fn cable_check(serial: Option<&str>) -> (bool, String) {
    let mut args: Vec<&str> = vec!["fastboot"];
    if let Some(s) = serial {
        args.extend(["-s", s]);
    }
    args.extend(["getvar", "product"]);
    if lfff_lib::utils::run_cmd(&args, 10).code != 0 {
        return (
            false,
            "✗ Device not responding — check cable/USB port and retry".into(),
        );
    }
    let r = lfff_lib::device::test_cable_speed(serial);
    if r.passed {
        let status = if r.speed_mbs > 0.0 {
            let label = if r.speed_mbs >= 30.0 {
                "excellent"
            } else if r.speed_mbs >= 10.0 {
                "good"
            } else {
                "ok"
            };
            format!("✓ {:.1} MB/s ({})", r.speed_mbs, label)
        } else {
            // fastboot stage unsupported — skipped, counts as pass
            format!("✓ {}", r.error)
        };
        (true, status)
    } else if r.error.is_empty() {
        (
            false,
            format!(
                "✗ Too slow: {:.2} MB/s (need ≥1 MB/s) — try another cable or USB port",
                r.speed_mbs
            ),
        )
    } else {
        (false, format!("✗ {}", r.error))
    }
}

/// Pre-flash safety gate: refuse a locked bootloader or a near-empty
/// battery, and make sure the device still answers. Returns the reason to
/// stop, if any.
fn preflight(tx: &mpsc::Sender<WMsg>, serial: Option<&str>) -> Result<(), String> {
    log(tx, LogLevel::Info, 2, "Running pre-flash safety checks...");
    let Some(info) = lfff_lib::device::get_device_info(serial) else {
        return Err("Cannot communicate with the device (fastboot getvar failed). Check the cable and that the device is in fastbootd/userspace fastboot.".into());
    };
    let ul = info.unlocked.to_lowercase();
    // Strict gate, same rule as the CLI pre-flash check: only an explicit
    // yes/true/1 counts as unlocked — an empty/unknown value must not pass.
    if !matches!(ul.as_str(), "yes" | "true" | "1") {
        let state = if ul.is_empty() {
            "unknown"
        } else {
            ul.as_str()
        };
        return Err(format!(
            "Bootloader lock state is '{}' — flashing requires an unlocked bootloader. Unlock it first: fastboot flashing unlock",
            state
        ));
    }
    if info.battery_level >= 0 && info.battery_level < 30 {
        return Err(format!(
            "Battery too low ({}%). Charge to at least 30% before flashing.",
            info.battery_level
        ));
    }
    let batt = if info.battery_level >= 0 {
        format!("{}%", info.battery_level)
    } else {
        "n/a".into()
    };
    log(
        tx,
        LogLevel::Success,
        2,
        format!("Pre-flash checks OK (unlocked: {}, battery: {})", ul, batt),
    );
    Ok(())
}

/// Set to stop the firmware being unpacked; cleared when the next starts.
static UNPACK_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cancel_unpack() {
    UNPACK_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Keeps the computer awake while it lives: sleeping halfway through
/// writing super leaves the phone half-flashed. Best effort: without
/// an inhibitor tool the flash goes on as before.
struct NoSleep(Option<std::process::Child>);

impl NoSleep {
    fn new() -> Self {
        let why = "Flashing a phone; sleeping now would leave it half-flashed";
        #[cfg(target_os = "macos")]
        let tries: Vec<Vec<String>> = vec![vec!["caffeinate".into(), "-i".into()]];
        #[cfg(not(target_os = "macos"))]
        let tries: Vec<Vec<String>> = ["systemd-inhibit", "elogind-inhibit"]
            .iter()
            .map(|t| {
                vec![
                    t.to_string(),
                    "--what=sleep:idle".into(),
                    "--who=LibreFastbootFirmwareFlasher".into(),
                    format!("--why={why}"),
                    "--mode=block".into(),
                    "sleep".into(),
                    "infinity".into(),
                ]
            })
            .collect();
        let _ = why;
        for t in tries {
            if let Ok(child) = std::process::Command::new(&t[0])
                .args(&t[1..])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                return NoSleep(Some(child));
            }
        }
        NoSleep(None)
    }
}

impl Drop for NoSleep {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Counts the images payload_dumper has written so far, for the unpacking
/// progress. Send `()` or drop the returned sender to stop it.
fn spawn_unpack_counter(tx: mpsc::Sender<WMsg>, dir: PathBuf) -> mpsc::Sender<()> {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut seen = std::collections::HashSet::<String>::new();
        loop {
            match stop_rx.recv_timeout(Duration::from_millis(700)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let mut newest = None;
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.ends_with(".img") && seen.insert(name.clone()) {
                        newest = Some(name);
                    }
                }
            }
            if let Some(n) = newest {
                tx.send(WMsg::Unpacking {
                    extracted: seen.len(),
                    current: n.trim_end_matches(".img").to_string(),
                })
                .ok();
            }
        }
    });
    stop_tx
}

/// Unpack (unless already unpacked) and check a firmware. Runs on its own
/// thread; answers with FirmwareReady or FirmwareFailed.
fn prepare_firmware(path: &str, tx: &mpsc::Sender<WMsg>) {
    let src = Path::new(path);
    // A file is taken as an archive whatever its name; it has to be a ZIP.
    let is_zip = src.is_file();
    if is_zip && !lfff_lib::extractor::looks_like_zip(src) {
        let msg = "This file is not a ZIP firmware archive.";
        log(tx, LogLevel::Error, 2, format!("{msg} ({})", src.display()));
        tx.send(WMsg::FirmwareFailed(msg.into())).ok();
        return;
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());

    let (dir, cached) = if is_zip {
        let out = get_output_dir().join(lfff_lib::extractor::get_firmware_name(src));
        if lfff_lib::extractor::is_extracted(src, &out) {
            log(
                tx,
                LogLevel::Info,
                2,
                format!("Already unpacked: {}", out.display()),
            );
            (out, true)
        } else {
            log(
                tx,
                LogLevel::Info,
                2,
                format!("Unpacking to {}...", out.display()),
            );
            tx.send(WMsg::Unpacking {
                extracted: 0,
                current: String::new(),
            })
            .ok();
            let staging = out.join("_staging");
            std::fs::create_dir_all(&staging).ok();
            let stop = spawn_unpack_counter(tx.clone(), staging);
            let tx_log = tx.clone();
            let r = lfff_lib::extractor::extract_firmware_cancellable(
                src,
                &out,
                None,
                None,
                Some(&|line: String| log(&tx_log, LogLevel::Info, 2, line)),
                Some(&UNPACK_CANCEL),
            );
            stop.send(()).ok();
            if !r.success {
                log(
                    tx,
                    LogLevel::Error,
                    2,
                    format!("Unpacking failed: {}", r.error),
                );
                tx.send(WMsg::FirmwareFailed(r.error)).ok();
                return;
            }
            (r.output_dir, false)
        }
    } else {
        (src.to_path_buf(), false)
    };

    // Which devices the firmware is for, kept beside the unpacked images so
    // a firmware picked again as a folder still knows.
    let devices_file = dir.join(".lfff-devices");
    let devices = if is_zip {
        let d = lfff_lib::extractor::firmware_devices(src);
        std::fs::write(&devices_file, d.join("\n")).ok();
        d
    } else {
        std::fs::read_to_string(&devices_file)
            .map(|s| {
                s.lines()
                    .map(str::to_string)
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    let images = lfff_lib::flasher::collect_images(&dir);
    if images.is_empty() {
        log(tx, LogLevel::Error, 2, "No flashable .img files found");
        tx.send(WMsg::FirmwareFailed("No flashable .img files found".into()))
            .ok();
        return;
    }
    log(
        tx,
        LogLevel::Success,
        2,
        format!("Firmware ready: {} images", images.len()),
    );
    tx.send(WMsg::FirmwareReady {
        dir: dir.display().to_string(),
        name,
        parts: images.len(),
        arb: lfff_lib::arb::firmware_arb(&dir),
        has_preloader: images.keys().any(|k| lfff_lib::flasher::is_preloader(k)),
        cached,
        devices,
    })
    .ok();
}

fn stage(tx: &mpsc::Sender<WMsg>, stage: u8, state: u8, detail: impl Into<String>) {
    tx.send(WMsg::Stage {
        stage,
        state,
        detail: detail.into(),
    })
    .ok();
}

/// Wait for fastbootd in short slices so that a cancel is noticed.
fn wait_fastbootd_cancellable(
    serial: Option<&str>,
    timeout_secs: u64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<lfff_lib::flasher::FastbootdStatus> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last = lfff_lib::flasher::FastbootdStatus::NotFound;
    loop {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let st = lfff_lib::flasher::wait_for_fastbootd_status(serial, 3);
        if st.is_usable() {
            return Some(st);
        }
        if st != lfff_lib::flasher::FastbootdStatus::NotFound {
            last = st;
        }
        if std::time::Instant::now() >= deadline {
            return Some(last);
        }
    }
}

fn do_flash(
    tx: &mpsc::Sender<WMsg>,
    source: &lfff_lib::flasher::FirmwareSource,
    serial: &Option<String>,
    device_product: &str,
    options: lfff_lib::flasher::FlashOptions,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let sref = serial.as_deref();
    let tx_log = tx.clone();
    let tx_prog = tx.clone();
    let tx_fail = tx.clone();

    let session = lfff_lib::flasher::run_flash_session_with_log(
        source,
        sref,
        &options,
        cancel,
        &|msg| {
            tx_log
                .send(WMsg::Log {
                    level: LogLevel::Info,
                    message: msg,
                    tab: 2,
                })
                .ok();
        },
        &|p| {
            let fraction = if p.total > 0 {
                p.done as f32 / p.total as f32
            } else {
                0.0
            };
            tx_prog
                .send(WMsg::Progress {
                    fraction,
                    partition: format!("{}_{}", p.partition, p.slot),
                })
                .ok();
        },
        &|partition, slot, error| {
            let (resp_tx, resp_rx) = std::sync::mpsc::channel();
            tx_fail
                .send(WMsg::FlashFailure {
                    partition: partition.to_string(),
                    slot: slot.to_string(),
                    error: error.to_string(),
                    response: resp_tx,
                })
                .ok();
            resp_rx
                .recv()
                .unwrap_or(lfff_lib::flasher::FailureAction::Abort)
        },
    );

    tx.send(WMsg::Progress {
        fraction: 1.0,
        partition: String::new(),
    })
    .ok();
    let failed = session.failed().len();
    let total = session.results.len();
    let success = !session.aborted && failed == 0;
    for r in session.failed() {
        let level = if session
            .critical_failed()
            .iter()
            .any(|c| c.partition == r.partition && c.slot == r.slot)
        {
            LogLevel::Error
        } else {
            LogLevel::Warn
        };
        log(
            tx,
            level,
            2,
            format!("{}_{}: {}", r.partition, r.slot, r.error),
        );
    }

    // Write flash history BEFORE sending FlashComplete
    let duration_s: f64 = session.results.iter().map(|r| r.duration_s).sum();
    let failed_parts: Vec<lfff_lib::flash_history::FailedPartition> = session
        .failed()
        .iter()
        .map(|r| lfff_lib::flash_history::FailedPartition {
            name: r.partition.clone(),
            slot: r.slot.clone(),
            error: r.error.clone(),
        })
        .collect();
    let fw_name = source
        .path()
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());
    let resolved_device_product = if !device_product.is_empty() {
        device_product.to_string()
    } else if let Some(ser) = serial.as_deref() {
        lfff_lib::device::get_device_info(Some(ser))
            .map(|i| i.product)
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        "unknown".to_string()
    };

    let entry = lfff_lib::flash_history::FlashHistoryEntry {
        timestamp: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        firmware_name: fw_name,
        firmware_path: source.path().display().to_string(),
        device_serial: serial.as_deref().unwrap_or("unknown").to_string(),
        device_product: resolved_device_product,
        total_partitions: session.results.len(),
        succeeded: session.succeeded().len(),
        failed,
        aborted: session.aborted,
        duration_s,
        end_reason: session.end_reason.clone(),
        failed_partitions: failed_parts,
    };
    if let Err(e) = lfff_lib::flash_history::append_entry(&entry) {
        tx.send(WMsg::Log {
            level: LogLevel::Warn,
            message: format!("Failed to write flash history: {}", e),
            tab: 2,
        })
        .ok();
    }

    let crit_failed = session.critical_failed().len();
    let detail_lines: Vec<String> = session
        .failed()
        .iter()
        .map(|r| format!("{}_{}: {}", r.partition, r.slot, r.error))
        .collect();
    // A user-initiated cancel always reports as cancelled, even though the
    // killed fastboot process leaves a failed result behind — otherwise the
    // user gets an error dialog for an action they asked for.
    let was_cancelled = session.end_reason.as_deref() == Some("Cancelled");
    let msg = if was_cancelled {
        "Flash cancelled by user".into()
    } else if failed > 0 {
        let crit = if crit_failed > 0 {
            format!("\n⚠ {} critical partition(s) failed!", crit_failed)
        } else {
            String::new()
        };
        format!(
            "{}/{} failed:{}\n{}",
            failed,
            total,
            crit,
            detail_lines.join("\n")
        )
    } else if session.aborted {
        "Flash aborted by user".into()
    } else {
        format!("Done! {}/{} OK", total, total)
    };
    let failed_partitions: Vec<String> = session
        .failed()
        .iter()
        .map(|r| r.partition.clone())
        .collect();
    let log_msg = if was_cancelled {
        "Flash cancelled".into()
    } else if failed > 0 {
        format!("{}/{} partitions failed", failed, total)
    } else if session.aborted {
        "Flash aborted".into()
    } else {
        format!("{}/{} OK", total, total)
    };
    tx.send(WMsg::FlashComplete {
        success,
        message: msg,
        log_summary: log_msg,
        failed_partitions,
    })
    .ok();
    tx.send(WMsg::Flashing(false)).ok();
}

pub fn worker(
    rx: mpsc::Receiver<Cmd>,
    tx: mpsc::Sender<WMsg>,
    flash_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    current_device: std::sync::Arc<std::sync::Mutex<Option<DeviceSnapshot>>>,
) {
    let mut serial: Option<String> = None;
    let mut current_device_product = String::new();
    let mut current_source: Option<lfff_lib::flasher::FirmwareSource> = None;
    let dl_cancel_token: std::sync::Arc<
        std::sync::Mutex<Option<lfff_lib::downloader::CancelToken>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(None));

    while let Ok(cmd) = rx.recv() {
        // The poller's reading is the freshest device the worker can know of
        // between commands; within one, the worker tracks it itself (a reboot
        // can change the serial it must use).
        if let Some(d) = current_device.lock().unwrap().clone() {
            serial = Some(d.serial);
            current_device_product = d.product;
        }
        busy.store(true, std::sync::atomic::Ordering::Relaxed);
        let _idle = Idle(&busy);
        match cmd {
            Cmd::CheckDevice => {
                log(&tx, LogLevel::Info, 0, "Searching for device...");
                let snap = if let Some(ser) = lfff_lib::device::list_adb_devices().first() {
                    Some(device_poll::read_adb(ser))
                } else if let Some(ser) = lfff_lib::device::list_fastboot_devices().first() {
                    let s = device_poll::read_fastboot(ser);
                    if s.is_none() {
                        log(&tx, LogLevel::Error, 0, "Device found but getvar failed");
                        continue;
                    }
                    s
                } else {
                    None
                };
                *current_device.lock().unwrap() = snap.clone();
                match snap {
                    Some(d) => {
                        serial = Some(d.serial.clone());
                        current_device_product = d.product.clone();
                        let mut info = format!("Device: {} ({}) | {:?}", d.name, d.product, d.mode);
                        if d.battery >= 0 {
                            info.push_str(&format!(" | Battery: {}%", d.battery));
                        }
                        if let Some(u) = d.unlocked {
                            info.push_str(&format!(
                                " | Unlocked: {}",
                                if u { "yes" } else { "no" }
                            ));
                        }
                        log(&tx, LogLevel::Success, 0, info);
                        tx.send(WMsg::DeviceDetected(d)).ok();
                    }
                    None => {
                        log(
                            &tx,
                            LogLevel::Error,
                            0,
                            "No device found via ADB or fastboot",
                        );
                        tx.send(WMsg::DeviceDisconnected).ok();
                        serial = None;
                        current_device_product.clear();
                    }
                }
            }

            Cmd::RebootForFlash { reboot_choice } => {
                let ready = match reboot_choice {
                    1 => {
                        // Active wait, no fixed sleep: the device leaves adb
                        // immediately, and `fastboot devices` won't list it
                        // until fastbootd is actually up — polling right away
                        // is safe and detects the device as soon as it boots.
                        log(&tx, LogLevel::Info, 2, "Rebooting to fastbootd via ADB...");
                        let mut args: Vec<&str> = Vec::new();
                        let ser_s;
                        if let Some(s) = &serial {
                            ser_s = s.clone();
                            args.extend(["-s", ser_s.as_str()]);
                        }
                        args.extend(["reboot", "fastboot"]);
                        let _ = std::process::Command::new("adb").args(&args).status();
                        wait_for_flash_fastbootd(&tx, 2, serial.as_deref(), 90)
                    }
                    2 => {
                        // No fixed sleep: the device is currently in
                        // bootloader ("fastboot"), and we wait specifically
                        // for "fastbootd" — the stale enumeration can't
                        // produce a false positive.
                        log(
                            &tx,
                            LogLevel::Info,
                            2,
                            "Rebooting to fastbootd via fastboot...",
                        );
                        let mut args: Vec<&str> = Vec::new();
                        let ser_s;
                        if let Some(s) = &serial {
                            ser_s = s.clone();
                            args.extend(["-s", ser_s.as_str()]);
                        }
                        args.extend(["reboot", "fastboot"]);
                        let _ = std::process::Command::new("fastboot").args(&args).status();
                        wait_for_flash_fastbootd(&tx, 2, serial.as_deref(), 90)
                    }
                    3 => {
                        log(
                            &tx,
                            LogLevel::Info,
                            2,
                            "Verifying device is in fastbootd/userspace fastboot mode...",
                        );
                        let status = lfff_lib::flasher::fastbootd_status(serial.as_deref());
                        handle_fastbootd_status(&tx, 2, status)
                    }
                    _ => false,
                };
                if ready {
                    adopt_fastboot_serial(&tx, &mut serial, 2);
                    tx.send(WMsg::ReadyToFlash).ok();
                } else {
                    log(&tx, LogLevel::Warn, 2, "Flash flow stopped before flashing");
                    tx.send(WMsg::Flashing(false)).ok();
                }
            }

            Cmd::PrepareFlash {
                path,
                is_source,
                method,
            } => {
                flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                tx.send(WMsg::Flashing(true)).ok();
                let method_name = match method {
                    FlashMethod::Snapdragon => "Snapdragon",
                    FlashMethod::Mtk => "MediaTek",
                };
                log(
                    &tx,
                    LogLevel::Info,
                    2,
                    format!("Flash method: {} (selected by user)", method_name),
                );
                log(&tx, LogLevel::Info, 2, "Preparing firmware...");

                let is_zip = Path::new(&path)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
                let dir = if !is_source && is_zip {
                    match extract_and_watch(&path, &tx, 2) {
                        Some(d) => d,
                        None => continue,
                    }
                } else {
                    Path::new(&path).to_path_buf()
                };

                let images = if is_source {
                    lfff_lib::flasher::collect_images_from_source(&dir)
                } else {
                    lfff_lib::flasher::collect_images(&dir)
                };
                if images.is_empty() {
                    log(&tx, LogLevel::Error, 2, "No flashable .img files found");
                    tx.send(WMsg::FlashComplete {
                        success: false,
                        message: "No flashable images found".into(),
                        log_summary: "No images found".into(),
                        failed_partitions: vec![],
                    })
                    .ok();
                    tx.send(WMsg::Flashing(false)).ok();
                    continue;
                }
                log(
                    &tx,
                    LogLevel::Info,
                    2,
                    format!("Found {} images to flash", images.len()),
                );
                let mut sorted: Vec<_> = images.iter().collect();
                sorted.sort_by_key(|(k, _)| (*k).clone());
                for (name, img_path) in &sorted {
                    let size_mb = std::fs::metadata(img_path)
                        .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                        .unwrap_or(0.0);
                    log(
                        &tx,
                        LogLevel::Info,
                        2,
                        format!("  {} ({:.1} MB)", name, size_mb),
                    );
                }

                // Warn on apparent method/firmware mismatch — the user's
                // explicit choice still wins, but they should know.
                let has_preloader = images.keys().any(|k| lfff_lib::flasher::is_preloader(k));
                let has_xbl = images.keys().any(|k| lfff_lib::flasher::is_xbl_abl(k));
                if method == FlashMethod::Snapdragon && has_preloader {
                    log(
                        &tx,
                        LogLevel::Warn,
                        2,
                        "⚠ preloader.img found — this firmware looks like MediaTek, but the Snapdragon method is selected. Double-check your choice!",
                    );
                }
                if method == FlashMethod::Mtk && has_xbl {
                    log(
                        &tx,
                        LogLevel::Warn,
                        2,
                        "⚠ xbl/abl images found — this firmware looks like Qualcomm, but the MediaTek method is selected. Double-check your choice!",
                    );
                }

                // Snapdragon firmware: read the ARB version so the GUI can
                // show the mandatory ARB warning before the final confirm.
                let arb = if method == FlashMethod::Snapdragon && !is_source {
                    let verdict = lfff_lib::arb::firmware_arb(&dir);
                    let msg = match verdict {
                        ArbVerdict::Known(v) => {
                            format!("Firmware ARB={} — waiting for user confirmation...", v)
                        }
                        ArbVerdict::NotFound => "xbl_config.img not found — firmware ARB version unknown, waiting for user confirmation...".into(),
                        ArbVerdict::Unparsed => "xbl_config.img could not be parsed — firmware ARB version unknown, waiting for user confirmation...".into(),
                    };
                    log(&tx, LogLevel::Warn, 2, msg);
                    Some(verdict)
                } else {
                    None
                };

                tx.send(WMsg::Flashing(false)).ok();
                tx.send(WMsg::FlashPrepared {
                    dir: dir.display().to_string(),
                    is_source,
                    arb,
                    has_preloader,
                })
                .ok();
            }

            Cmd::StartFlash {
                dir,
                is_source,
                method,
                skip_xbl_abl,
                skip_preloader,
                skip_partitions,
            } => {
                let _awake = NoSleep::new();
                flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                tx.send(WMsg::Flashing(true)).ok();

                if let Err(msg) = preflight(&tx, serial.as_deref()) {
                    log(&tx, LogLevel::Error, 2, &msg);
                    tx.send(WMsg::FlashComplete {
                        success: false,
                        message: msg,
                        log_summary: "Pre-flash check failed".into(),
                        failed_partitions: vec![],
                    })
                    .ok();
                    tx.send(WMsg::Flashing(false)).ok();
                    continue;
                }

                let src = if is_source {
                    lfff_lib::flasher::FirmwareSource::SourceBuild(PathBuf::from(&dir))
                } else {
                    lfff_lib::flasher::FirmwareSource::Extracted(PathBuf::from(&dir))
                };
                current_source = Some(src.clone());

                let opts = lfff_lib::flasher::FlashOptions {
                    // On MediaTek devices xbl/abl partitions do not exist.
                    skip_xbl_abl: skip_xbl_abl || method == FlashMethod::Mtk,
                    skip_preloader,
                    as_mediatek: Some(method == FlashMethod::Mtk),
                    skip_partitions,
                    ..Default::default()
                };
                do_flash(
                    &tx,
                    &src,
                    &serial,
                    &current_device_product,
                    opts,
                    flash_cancel.clone(),
                );
            }

            Cmd::FlashSingle {
                path,
                partition,
                reboot_choice,
            } => {
                tx.send(WMsg::Flashing(true)).ok();
                let ready = match reboot_choice {
                    1 => {
                        // Device leaves adb right away and only shows up in
                        // `fastboot devices` once the bootloader is up —
                        // start polling immediately, no fixed sleep needed.
                        log(&tx, LogLevel::Info, 3, "Rebooting to bootloader via ADB...");
                        let mut args: Vec<&str> = Vec::new();
                        let ser_s;
                        if let Some(s) = &serial {
                            ser_s = s.clone();
                            args.extend(["-s", ser_s.as_str()]);
                        }
                        args.extend(["reboot", "bootloader"]);
                        let _ = std::process::Command::new("adb").args(&args).status();
                        wait_for_fastboot(&tx, 3, 90, serial.as_deref())
                    }
                    2 => {
                        // The device is already visible to fastboot, so wait
                        // for it to drop off the bus first — otherwise the
                        // stale pre-reboot enumeration would match instantly.
                        log(
                            &tx,
                            LogLevel::Info,
                            3,
                            "Rebooting to bootloader via fastboot...",
                        );
                        let mut args: Vec<&str> = Vec::new();
                        let ser_s;
                        if let Some(s) = &serial {
                            ser_s = s.clone();
                            args.extend(["-s", ser_s.as_str()]);
                        }
                        args.push("reboot-bootloader");
                        let _ = std::process::Command::new("fastboot").args(&args).status();
                        lfff_lib::flasher::wait_for_device_gone(serial.as_deref(), 5);
                        wait_for_fastboot(&tx, 3, 90, serial.as_deref())
                    }
                    _ => {
                        log(&tx, LogLevel::Info, 3, "Checking device is in fastboot...");
                        wait_for_fastboot(&tx, 3, 10, serial.as_deref())
                    }
                };
                if !ready {
                    tx.send(WMsg::FlashComplete {
                        success: false,
                        message: "Device not found in fastboot".into(),
                        log_summary: "Device not found".into(),
                        failed_partitions: vec![],
                    })
                    .ok();
                    tx.send(WMsg::Flashing(false)).ok();
                    continue;
                }
                adopt_fastboot_serial(&tx, &mut serial, 3);
                let img = Path::new(&path);
                let sref = serial.as_deref();
                if !img.exists() {
                    log(&tx, LogLevel::Error, 3, "File not found");
                    tx.send(WMsg::Flashing(false)).ok();
                    continue;
                }
                let p = if let Some(ref pn) = partition {
                    pn.clone()
                } else {
                    let mut p = img
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase();
                    for s in &["_a", "_b"] {
                        if p.ends_with(s) {
                            p = p[..p.len() - s.len()].to_string();
                            break;
                        }
                    }
                    p
                };
                log(&tx, LogLevel::Info, 3, format!("Flashing partition: {}", p));
                // Dynamic (super) partitions exist only in the active
                // slot — flashing the inactive one fails or corrupts the
                // super layout (same rule as RetryFlash).
                let slots: Vec<String> = if lfff_lib::flasher::is_super_partition(&p.to_lowercase())
                {
                    match lfff_lib::flasher::get_active_slot(sref) {
                        Some(s) => {
                            log(
                                &tx,
                                LogLevel::Info,
                                3,
                                format!("Dynamic partition — flashing active slot '{}' only", s),
                            );
                            vec![s]
                        }
                        None => {
                            log(
                                &tx,
                                LogLevel::Error,
                                3,
                                "Could not detect active slot — cannot safely flash a dynamic partition",
                            );
                            tx.send(WMsg::FlashComplete {
                                success: false,
                                message: "Could not detect active slot".into(),
                                log_summary: "Flash failed".into(),
                                failed_partitions: vec![],
                            })
                            .ok();
                            tx.send(WMsg::Flashing(false)).ok();
                            continue;
                        }
                    }
                } else {
                    vec!["a".into(), "b".into()]
                };
                let total = slots.len();
                let mut fail = 0;
                for (done, slot) in slots.iter().enumerate() {
                    let lbl = format!("{}_{}", p, slot);
                    tx.send(WMsg::Progress {
                        fraction: done as f32 / total as f32,
                        partition: lbl.clone(),
                    })
                    .ok();
                    let r = lfff_lib::flasher::flash_partition(img, &p, slot, sref);
                    if r.success {
                        log(&tx, LogLevel::Success, 3, format!("{} OK", lbl));
                    } else {
                        fail += 1;
                        log(&tx, LogLevel::Error, 3, format!("{} FAILED", lbl));
                    }
                }
                tx.send(WMsg::Progress {
                    fraction: 1.0,
                    partition: String::new(),
                })
                .ok();
                tx.send(WMsg::FlashComplete {
                    success: fail == 0,
                    message: if fail == 0 {
                        format!("{} flashed OK", p)
                    } else {
                        format!("{} errors", fail)
                    },
                    log_summary: if fail == 0 {
                        "Flash OK".into()
                    } else {
                        format!("{} errors", fail)
                    },
                    failed_partitions: vec![],
                })
                .ok();
                tx.send(WMsg::Flashing(false)).ok();
            }

            Cmd::CancelFlash => log(&tx, LogLevel::Warn, 2, "Cancelling flash..."),

            Cmd::PostFlashReboot => {
                log(&tx, LogLevel::Info, 2, "Rebooting to system...");
                let mut args = vec!["fastboot"];
                let ser_s;
                if let Some(s) = &serial {
                    ser_s = s.clone();
                    args.extend(&["-s", &ser_s]);
                }
                args.push("reboot");
                match std::process::Command::new(args[0])
                    .args(&args[1..])
                    .status()
                {
                    Ok(s) if s.success() => log(&tx, LogLevel::Success, 2, "Reboot initiated"),
                    _ => log(&tx, LogLevel::Error, 2, "Failed to reboot"),
                }
            }

            Cmd::PostFlashWipe => {
                log(&tx, LogLevel::Warn, 2, "Wiping data (fastboot -w)...");
                let mut args = vec!["fastboot"];
                let ser_s;
                if let Some(s) = &serial {
                    ser_s = s.clone();
                    args.extend(&["-s", &ser_s]);
                }
                args.push("-w");
                match std::process::Command::new(args[0])
                    .args(&args[1..])
                    .status()
                {
                    Ok(s) if s.success() => {
                        log(&tx, LogLevel::Success, 2, "Wipe done, rebooting...");
                        let mut args2 = vec!["fastboot"];
                        let ser_s2;
                        if let Some(s) = &serial {
                            ser_s2 = s.clone();
                            args2.extend(&["-s", &ser_s2]);
                        }
                        args2.push("reboot");
                        std::process::Command::new(args2[0])
                            .args(&args2[1..])
                            .status()
                            .ok();
                    }
                    _ => log(&tx, LogLevel::Error, 2, "Wipe failed"),
                }
            }

            Cmd::RebootTo(target) => {
                let (cmd, args): (&str, &[&str]) = match target.as_str() {
                    "adb-recovery" | "recovery" => ("adb", &["reboot", "recovery"]),
                    "adb-bootloader" | "bootloader" => ("adb", &["reboot", "bootloader"]),
                    "adb-fastboot" | "fastboot" => ("adb", &["reboot", "fastboot"]),
                    "adb-reboot" | "reboot" => ("adb", &["reboot"]),
                    "fb-recovery" => ("fastboot", &["reboot", "recovery"]),
                    "fb-bootloader" => ("fastboot", &["reboot-bootloader"]),
                    "fb-fastboot" => ("fastboot", &["reboot", "fastboot"]),
                    "fb-reboot" => ("fastboot", &["reboot"]),
                    _ => {
                        log(&tx, LogLevel::Error, 0, "Unknown reboot target");
                        continue;
                    }
                };
                log(
                    &tx,
                    LogLevel::Info,
                    0,
                    format!("Rebooting to {}...", target),
                );
                let is_fastboot_cmd = cmd == "fastboot";
                let mut child = std::process::Command::new(cmd)
                    .args(args)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                let success = match child {
                    Ok(ref mut c) => {
                        let timeout = if is_fastboot_cmd { 5 } else { 15 };
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(timeout);
                        loop {
                            if let Ok(Some(status)) = c.try_wait() {
                                break status.success();
                            }
                            if std::time::Instant::now() >= deadline {
                                let _ = c.kill();
                                break false;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                    Err(_) => false,
                };
                if success {
                    log(
                        &tx,
                        LogLevel::Success,
                        0,
                        format!("Reboot to {} initiated", target),
                    );
                } else if is_fastboot_cmd {
                    log(
                        &tx,
                        LogLevel::Error,
                        0,
                        "Device not in fastboot mode — use ADB reboot buttons instead",
                    );
                } else {
                    log(
                        &tx,
                        LogLevel::Error,
                        0,
                        format!("Failed to reboot to {}", target),
                    );
                }
            }

            Cmd::SetActiveSlot { slot } => {
                if let Some(ref ser) = serial {
                    // Check if device is in fastboot (bootloader) mode, not fastbootd
                    let check = std::process::Command::new("fastboot")
                        .args(["-s", ser, "getvar", "product"])
                        .output();

                    match check {
                        Ok(o) if o.status.success() => {
                            // Device is in fastboot mode, proceed with slot switch
                            log(
                                &tx,
                                LogLevel::Info,
                                0,
                                format!("Setting active slot to {}...", slot.to_uppercase()),
                            );
                            let args = vec!["-s", ser, "set_active", &slot];
                            let out = std::process::Command::new("fastboot").args(&args).output();
                            match out {
                                Ok(o) if o.status.success() => {
                                    log(
                                        &tx,
                                        LogLevel::Success,
                                        0,
                                        format!("Active slot set to {}", slot.to_uppercase()),
                                    );
                                    if let Some(d) = device_poll::read_fastboot(ser) {
                                        *current_device.lock().unwrap() = Some(d.clone());
                                        tx.send(WMsg::DeviceDetected(d)).ok();
                                    }
                                }
                                Ok(o) => {
                                    let err = String::from_utf8_lossy(&o.stderr);
                                    log(
                                        &tx,
                                        LogLevel::Error,
                                        0,
                                        format!("Failed to set slot: {}", err.trim()),
                                    );
                                }
                                Err(e) => log(
                                    &tx,
                                    LogLevel::Error,
                                    0,
                                    format!("Failed to run fastboot: {}", e),
                                ),
                            }
                        }
                        _ => {
                            log(
                                &tx,
                                LogLevel::Error,
                                0,
                                "Device is not in fastboot (bootloader) mode. Slot switching only works in fastboot mode. Please reboot to bootloader first.",
                            );
                        }
                    }
                } else {
                    log(&tx, LogLevel::Error, 0, "No device connected");
                }
            }

            Cmd::CableTest => {
                // Real throughput test: 8 MiB via `fastboot stage` (RAM
                // only, no NAND write). A latency-only ping cannot catch a
                // slow-but-working cable, and flash timeouts are sized
                // assuming the cable sustains at least 1 MB/s.
                let total = 3u8;
                tx.send(WMsg::CableTestProgress {
                    step: 1,
                    total,
                    status: "Measuring transfer speed (8 MiB)...".into(),
                })
                .ok();
                let (passed, status) = cable_check(serial.as_deref());
                log(
                    &tx,
                    if passed {
                        LogLevel::Success
                    } else {
                        LogLevel::Error
                    },
                    2,
                    status.clone(),
                );
                tx.send(WMsg::CableTestProgress {
                    step: if passed { total } else { 1 },
                    total,
                    status,
                })
                .ok();
            }

            Cmd::RetryFlash { failed_partitions } => {
                flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                tx.send(WMsg::Flashing(true)).ok();
                log(
                    &tx,
                    LogLevel::Info,
                    2,
                    format!(
                        "Retrying {} partition(s): {}",
                        failed_partitions.len(),
                        failed_partitions.join(", ")
                    ),
                );

                let source = match &current_source {
                    Some(s) => s.clone(),
                    None => {
                        log(
                            &tx,
                            LogLevel::Error,
                            2,
                            "No firmware source available for retry",
                        );
                        tx.send(WMsg::FlashComplete {
                            success: false,
                            message: "No firmware source available".into(),
                            log_summary: "No source available".into(),
                            failed_partitions: vec![],
                        })
                        .ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
                };

                let sref = serial.as_deref();

                // Collect images once — not on every slot iteration.
                let fw_dir = source.path();
                let images = if source.is_source() {
                    lfff_lib::flasher::collect_images_from_source(fw_dir)
                } else {
                    lfff_lib::flasher::collect_images(fw_dir)
                };

                // Dynamic (super) partitions must only be flashed to the
                // active slot — writing them to the inactive slot can fail
                // or corrupt the super layout.
                let needs_active_slot = failed_partitions
                    .iter()
                    .any(|p| lfff_lib::flasher::is_super_partition(&p.to_lowercase()));
                let active_slot = if needs_active_slot {
                    match lfff_lib::flasher::get_active_slot(sref) {
                        Some(s) => Some(s),
                        None => {
                            log(
                                &tx,
                                LogLevel::Error,
                                2,
                                "Could not detect active slot — cannot safely retry dynamic partitions",
                            );
                            tx.send(WMsg::FlashComplete {
                                success: false,
                                message: "Could not detect active slot".into(),
                                log_summary: "Retry failed".into(),
                                failed_partitions: failed_partitions.clone(),
                            })
                            .ok();
                            tx.send(WMsg::Flashing(false)).ok();
                            continue;
                        }
                    }
                } else {
                    None
                };

                let plan: Vec<(String, Vec<String>)> = failed_partitions
                    .iter()
                    .map(|p| {
                        let slots = if lfff_lib::flasher::is_super_partition(&p.to_lowercase()) {
                            vec![active_slot.clone().unwrap_or_else(|| "a".into())]
                        } else {
                            vec!["a".to_string(), "b".to_string()]
                        };
                        (p.clone(), slots)
                    })
                    .collect();
                let total: usize = plan.iter().map(|(_, s)| s.len()).sum();
                let mut done = 0;
                let mut fail_count = 0;
                let mut failed_list = Vec::new();
                let mut cancelled = false;

                'retry: for (partition, slots) in &plan {
                    for slot in slots {
                        if flash_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            log(&tx, LogLevel::Warn, 2, "Retry cancelled by user");
                            tx.send(WMsg::FlashComplete {
                                success: false,
                                message: "Retry cancelled".into(),
                                log_summary: "Retry cancelled".into(),
                                failed_partitions: failed_list.clone(),
                            })
                            .ok();
                            tx.send(WMsg::Flashing(false)).ok();
                            cancelled = true;
                            break 'retry;
                        }

                        let lbl = format!("{}_{}", partition, slot);
                        tx.send(WMsg::Progress {
                            fraction: done as f32 / total as f32,
                            partition: lbl.clone(),
                        })
                        .ok();

                        let part_lc = partition.to_lowercase();
                        let img_path = images.get(&part_lc).cloned().or_else(|| {
                            images
                                .iter()
                                .find(|(name, _)| name.to_lowercase().starts_with(&part_lc))
                                .map(|(_, path)| path.clone())
                        });

                        if let Some(img) = img_path {
                            log(
                                &tx,
                                LogLevel::Info,
                                2,
                                format!("Retrying {} from {}...", lbl, img.display()),
                            );
                            let result =
                                lfff_lib::flasher::flash_partition(&img, partition, slot, sref);
                            if result.success {
                                log(&tx, LogLevel::Success, 2, format!("{} OK", lbl));
                            } else {
                                fail_count += 1;
                                failed_list.push(partition.clone());
                                log(
                                    &tx,
                                    LogLevel::Error,
                                    2,
                                    format!("{} FAILED: {}", lbl, result.error),
                                );
                            }
                        } else {
                            fail_count += 1;
                            failed_list.push(partition.clone());
                            log(
                                &tx,
                                LogLevel::Error,
                                2,
                                format!("Image not found for {}", partition),
                            );
                        }
                        done += 1;
                    }
                }
                if cancelled {
                    continue;
                }

                let success = fail_count == 0;
                let msg = if success {
                    format!(
                        "Retry complete! {} partition(s) OK",
                        failed_partitions.len()
                    )
                } else {
                    format!("{}/{} failed on retry", fail_count, failed_partitions.len())
                };
                let log_msg = if success {
                    "Retry OK".into()
                } else {
                    format!("{}/{} failed", fail_count, failed_partitions.len())
                };

                tx.send(WMsg::Progress {
                    fraction: 1.0,
                    partition: String::new(),
                })
                .ok();
                tx.send(WMsg::FlashComplete {
                    success,
                    message: msg,
                    log_summary: log_msg,
                    failed_partitions: failed_list,
                })
                .ok();
                tx.send(WMsg::Flashing(false)).ok();
            }

            Cmd::CheckDeps => {
                log(&tx, LogLevel::Info, 0, "Checking dependencies...");
                let tx_log = tx.clone();
                let r = lfff_lib::deps::install_dependencies(None, true, &move |line| {
                    let msg = line.trim().to_string();
                    if !msg.is_empty() {
                        tx_log
                            .send(WMsg::Log {
                                level: LogLevel::Info,
                                message: msg,
                                tab: 0,
                            })
                            .ok();
                    }
                });
                for d in &r.results {
                    if d.already_installed {
                        log(&tx, LogLevel::Success, 0, format!("{}: OK", d.tool));
                    } else if d.skipped {
                        log(&tx, LogLevel::Warn, 0, format!("{}: skipped", d.tool));
                    } else if !d.error.is_empty() {
                        log(&tx, LogLevel::Error, 0, format!("{}: {}", d.tool, d.error));
                    }
                }
                tx.send(WMsg::DepsResult {
                    missing: r
                        .results
                        .iter()
                        .filter(|d| !d.already_installed && !d.installed)
                        .map(|d| d.tool.clone())
                        .collect(),
                    ok: r.all_ok(),
                    message: if r.all_ok() {
                        "All dependencies OK".into()
                    } else {
                        "Some missing".into()
                    },
                })
                .ok();
            }

            Cmd::InstallDeps => {
                log(&tx, LogLevel::Info, 0, "Installing dependencies...");
                let tx_log = tx.clone();
                let r = lfff_lib::deps::install_dependencies(None, false, &move |line| {
                    let msg = line.trim().to_string();
                    if !msg.is_empty() {
                        tx_log
                            .send(WMsg::Log {
                                level: LogLevel::Info,
                                message: msg,
                                tab: 0,
                            })
                            .ok();
                    }
                });
                for d in &r.results {
                    if d.installed {
                        log(&tx, LogLevel::Success, 0, format!("{}: installed", d.tool));
                    } else if d.already_installed {
                        log(&tx, LogLevel::Success, 0, format!("{}: already OK", d.tool));
                    } else if !d.error.is_empty() {
                        log(&tx, LogLevel::Error, 0, format!("{}: {}", d.tool, d.error));
                    }
                }
                tx.send(WMsg::DepsResult {
                    missing: r
                        .results
                        .iter()
                        .filter(|d| !d.already_installed && !d.installed)
                        .map(|d| d.tool.clone())
                        .collect(),
                    ok: r.all_ok(),
                    message: if r.all_ok() {
                        "All OK".into()
                    } else {
                        "Some failed".into()
                    },
                })
                .ok();
            }

            Cmd::Download { url } => {
                tx.send(WMsg::Downloading(true)).ok();
                log(&tx, LogLevel::Info, 1, "Starting download...");
                let tx_dl = tx.clone();
                let token = lfff_lib::downloader::CancelToken::new();
                if let Ok(mut guard) = dl_cancel_token.lock() {
                    *guard = Some(token.clone());
                }
                std::thread::spawn(move || {
                    let out = get_output_dir();
                    log(
                        &tx_dl,
                        LogLevel::Info,
                        1,
                        format!("Output: {}", out.display()),
                    );
                    let tx2 = tx_dl.clone();
                    let last_update = std::sync::Mutex::new(std::time::Instant::now());
                    let r = lfff_lib::downloader::download_firmware_with_progress(
                        &url,
                        Some(&out),
                        16,
                        token,
                        move |p| {
                            let mut last = last_update.lock().unwrap();
                            if last.elapsed().as_millis() >= 100 || p.percent >= 100.0 {
                                tx2.send(WMsg::DlProgress {
                                    percent: p.percent,
                                    speed: p.speed,
                                    eta: p.eta,
                                    downloaded: p.downloaded,
                                    total: p.total_size,
                                    raw_line: p.raw_line,
                                })
                                .ok();
                                *last = std::time::Instant::now();
                            }
                        },
                    );
                    if r.success {
                        let p = r
                            .output_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        match r.output_path {
                            Some(path) if lfff_lib::extractor::looks_like_zip(&path) => {
                                log(&tx_dl, LogLevel::Success, 1, format!("Downloaded: {}", p));
                                tx_dl.send(WMsg::FwPath(path.display().to_string())).ok();
                            }
                            Some(path) => {
                                // Usually a web page saved in the firmware's
                                // place: an expired or not-direct link.
                                let msg = "The link gave a web page, not a firmware archive. Check that the link is a direct one and has not expired.";
                                log(
                                    &tx_dl,
                                    LogLevel::Error,
                                    1,
                                    format!("{msg} ({})", path.display()),
                                );
                                std::fs::remove_file(&path).ok();
                                tx_dl.send(WMsg::FirmwareFailed(msg.into())).ok();
                            }
                            None => {}
                        }
                    } else if r.error == "Cancelled" {
                        log(&tx_dl, LogLevel::Warn, 1, "Download cancelled");
                    } else {
                        log(&tx_dl, LogLevel::Error, 1, format!("Failed: {}", r.error));
                        tx_dl
                            .send(WMsg::FirmwareFailed(format!(
                                "Download failed: {}",
                                r.error
                            )))
                            .ok();
                    }
                    tx_dl
                        .send(WMsg::DlProgress {
                            percent: 0.0,
                            speed: String::new(),
                            eta: String::new(),
                            downloaded: String::new(),
                            total: String::new(),
                            raw_line: String::new(),
                        })
                        .ok();
                    tx_dl.send(WMsg::Downloading(false)).ok();
                });
            }

            Cmd::CancelDownload => {
                if let Some(token) = dl_cancel_token.lock().ok().and_then(|mut g| g.take()) {
                    token.cancel();
                    log(&tx, LogLevel::Warn, 1, "Cancelling download...");
                }
            }

            Cmd::Extract { path } => {
                let fw = Path::new(&path);
                let out = get_output_dir().join(lfff_lib::extractor::get_firmware_name(fw));
                log(
                    &tx,
                    LogLevel::Info,
                    1,
                    format!("Extracting to {}...", out.display()),
                );
                let tx_ex = tx.clone();
                let staging = out.join("_staging");
                std::fs::create_dir_all(&staging).ok();
                let stop_tx = spawn_img_watcher(tx.clone(), staging.clone(), 1);
                let r = lfff_lib::extractor::extract_firmware_with_log(
                    fw,
                    &out,
                    None,
                    None,
                    Some(&|line: String| {
                        tx_ex
                            .send(WMsg::Log {
                                level: LogLevel::Info,
                                message: line,
                                tab: 1,
                            })
                            .ok();
                    }),
                );
                stop_tx.send(()).ok();
                if r.success {
                    let n: usize = r.groups.values().map(|v| v.len()).sum();
                    log(
                        &tx,
                        LogLevel::Success,
                        1,
                        format!("{} images extracted to {}", n, out.display()),
                    );
                } else {
                    log(
                        &tx,
                        LogLevel::Error,
                        1,
                        format!("Extraction failed: {}", r.error),
                    );
                }
            }

            Cmd::DriverTest => {
                use std::process::Command as Cmd2;
                let step = |tx: &mpsc::Sender<WMsg>, s: i32, msg: &str| {
                    tx.send(WMsg::TestStep {
                        step: s,
                        status: msg.into(),
                    })
                    .ok();
                    log(tx, LogLevel::Info, 0, msg);
                };

                step(&tx, 1, "Checking ADB connection...");
                let adb_ok = {
                    let r = lfff_lib::utils::run_cmd(&["adb", "devices"], 5);
                    r.code == 0
                        && r.stdout.lines().skip(1).any(|l| {
                            let p: Vec<&str> = l.split_whitespace().collect();
                            p.len() >= 2 && p[1] == "device"
                        })
                };
                if !adb_ok {
                    let fb = lfff_lib::device::list_fastboot_devices();
                    if fb.is_empty() {
                        step(&tx, -1, "No device found via ADB or fastboot");
                        continue;
                    }
                    step(&tx, 2, "Device found in fastboot mode");
                } else {
                    step(&tx, 2, "ADB OK — device connected");
                    // Single adb round-trip for all three properties.
                    let r = lfff_lib::utils::run_cmd(
                        &[
                            "adb",
                            "shell",
                            "echo model=$(getprop ro.product.model); \
                             echo build=$(getprop ro.build.display.id); \
                             echo abi=$(getprop ro.product.cpu.abi)",
                        ],
                        5,
                    );
                    for (key, label) in [("model", "Model"), ("build", "Build"), ("abi", "ABI")] {
                        let val = r
                            .stdout
                            .lines()
                            .find_map(|l| l.trim().strip_prefix(&format!("{}=", key)))
                            .unwrap_or("")
                            .trim();
                        if !val.is_empty() {
                            log(&tx, LogLevel::Success, 0, format!("{}: {}", label, val));
                        }
                    }
                    step(&tx, 2, "Rebooting to bootloader...");
                    let _ = Cmd2::new("adb").args(["reboot", "bootloader"]).status();
                    // No fixed 8s sleep — the device leaves adb immediately
                    // and the active wait below catches it as soon as the
                    // bootloader enumerates.
                }

                step(&tx, 3, "Checking fastboot...");
                if !lfff_lib::flasher::wait_for_any_fastboot(None, 60) {
                    step(&tx, -1, "Fastboot not detected after 60s");
                    continue;
                }
                let fb = lfff_lib::device::list_fastboot_devices();
                if let Some(first) = fb.first() {
                    log(&tx, LogLevel::Success, 0, format!("Fastboot OK: {}", first));
                    serial = Some(first.clone());
                }

                if let Some(ref ser) = serial
                    && let Some(info) = lfff_lib::device::get_device_info(Some(ser))
                {
                    current_device_product = info.product.clone();
                    if let Some(d) = device_poll::read_fastboot(ser) {
                        *current_device.lock().unwrap() = Some(d.clone());
                        tx.send(WMsg::DeviceDetected(d)).ok();
                    }
                    log(
                        &tx,
                        LogLevel::Success,
                        0,
                        format!(
                            "Product: {} | Slot: {} | Battery: {}%",
                            info.product, info.current_slot, info.battery_level
                        ),
                    );
                }

                step(&tx, 4, "Rebooting to fastbootd...");
                let _ = Cmd2::new("fastboot").args(["reboot", "fastboot"]).status();
                // No fixed 6s sleep: we wait specifically for "fastbootd",
                // and the device currently reports "fastboot" — the stale
                // enumeration cannot false-positive.
                if !lfff_lib::flasher::wait_for_fastbootd(serial.as_deref(), 60) {
                    step(&tx, -1, "Fastbootd not detected after 60s");
                    continue;
                }
                log(&tx, LogLevel::Success, 0, "Fastbootd OK");

                step(&tx, 5, "All drivers OK!");
                log(
                    &tx,
                    LogLevel::Success,
                    0,
                    "Driver test completed successfully",
                );
            }

            Cmd::PrepareFirmware { path } => {
                UNPACK_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
                let tx = tx.clone();
                thread::spawn(move || prepare_firmware(&path, &tx));
            }

            Cmd::RunInstall {
                dir,
                method,
                skip_xbl_abl,
                skip_preloader,
                skip_partitions,
            } => {
                use lfff_lib::flasher::FastbootdStatus;
                use std::sync::atomic::Ordering;
                let _awake = NoSleep::new();
                flash_cancel.store(false, Ordering::Relaxed);
                tx.send(WMsg::Flashing(true)).ok();
                let stop = |reason: String| {
                    log(&tx, LogLevel::Error, 2, &reason);
                    tx.send(WMsg::InstallStopped(reason)).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                };

                // 0. Into fastbootd, the way the phone's current mode needs.
                stage(&tx, 0, 1, "");
                let mode = lfff_lib::flasher::detect_mode(serial.as_deref());
                let ser_args: Vec<String> = serial
                    .iter()
                    .flat_map(|s| ["-s".to_string(), s.clone()])
                    .collect();
                let reboot = |tool: &str| {
                    log(
                        &tx,
                        LogLevel::Info,
                        2,
                        format!("Restarting into fastbootd via {tool}..."),
                    );
                    let _ = std::process::Command::new(tool)
                        .args(&ser_args)
                        .args(["reboot", "fastboot"])
                        .status();
                };
                let status = match mode {
                    lfff_lib::flasher::DeviceMode::System => {
                        reboot("adb");
                        wait_fastbootd_cancellable(serial.as_deref(), 90, &flash_cancel)
                    }
                    lfff_lib::flasher::DeviceMode::Bootloader => {
                        // A phone that lists fastbootd as plain fastboot is
                        // already there; only a real bootloader is restarted.
                        match lfff_lib::flasher::fastbootd_status(serial.as_deref()) {
                            FastbootdStatus::UserspaceFastboot => {
                                Some(FastbootdStatus::UserspaceFastboot)
                            }
                            _ => {
                                reboot("fastboot");
                                wait_fastbootd_cancellable(serial.as_deref(), 90, &flash_cancel)
                            }
                        }
                    }
                    lfff_lib::flasher::DeviceMode::Fastbootd => Some(FastbootdStatus::Fastbootd),
                    lfff_lib::flasher::DeviceMode::Unknown => {
                        stage(&tx, 0, 3, "");
                        stop(
                            "The phone is not connected, or not in a mode the app can use.".into(),
                        );
                        continue;
                    }
                };
                let Some(status) = status else {
                    stage(&tx, 0, 3, "");
                    stop("Cancelled".into());
                    continue;
                };
                if !handle_fastbootd_status(&tx, 2, status) {
                    stage(&tx, 0, 3, "");
                    stop(match status {
                        FastbootdStatus::NotFound => {
                            "The phone did not reach fastbootd within 90 seconds.".into()
                        }
                        _ => "Stopped at the fastbootd warning.".into(),
                    });
                    continue;
                }
                adopt_fastboot_serial(&tx, &mut serial, 2);
                stage(&tx, 0, 2, "");

                // 1. Cable, as many times as the user asks.
                stage(&tx, 1, 1, "");
                let cable_ok = loop {
                    let (passed, status) = cable_check(serial.as_deref());
                    log(
                        &tx,
                        if passed {
                            LogLevel::Success
                        } else {
                            LogLevel::Error
                        },
                        2,
                        status.clone(),
                    );
                    if passed {
                        stage(&tx, 1, 2, status.trim_start_matches("✓ "));
                        break true;
                    }
                    stage(&tx, 1, 3, status.trim_start_matches("✗ "));
                    let (resp_tx, resp_rx) = mpsc::channel();
                    tx.send(WMsg::CableFailed {
                        detail: status,
                        response: resp_tx,
                    })
                    .ok();
                    if !resp_rx.recv().unwrap_or(false) {
                        break false;
                    }
                    stage(&tx, 1, 1, "");
                };
                if !cable_ok {
                    stop("Stopped at the cable check.".into());
                    continue;
                }

                // 2. Safety gate, then flash.
                if let Err(msg) = preflight(&tx, serial.as_deref()) {
                    stage(&tx, 2, 3, "");
                    stop(msg);
                    continue;
                }
                stage(&tx, 2, 1, "");
                let src = lfff_lib::flasher::FirmwareSource::Extracted(PathBuf::from(&dir));
                current_source = Some(src.clone());
                let opts = lfff_lib::flasher::FlashOptions {
                    // On MediaTek devices xbl/abl partitions do not exist.
                    skip_xbl_abl: skip_xbl_abl || method == FlashMethod::Mtk,
                    skip_preloader,
                    as_mediatek: Some(method == FlashMethod::Mtk),
                    skip_partitions,
                    ..Default::default()
                };
                do_flash(
                    &tx,
                    &src,
                    &serial,
                    &current_device_product,
                    opts,
                    flash_cancel.clone(),
                );
            }

            Cmd::CheckForUpdates => {
                log(&tx, LogLevel::Info, 0, "Checking for updates...");
                let current = env!("CARGO_PKG_VERSION");
                let client = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .user_agent("LFFF")
                    .build();
                if let Ok(client) = client
                        && let Ok(resp) = client.get("https://api.github.com/repos/mrFrok/LibreFastbootFirmwareFlasher/releases/latest").send()
                        && let Ok(json) = resp.json::<serde_json::Value>()
                        && let Some(tag) = json.get("tag_name").and_then(|v| v.as_str())
                    {
                        let latest = tag.trim_start_matches('v');
                        // Strictly newer only — a local dev build ahead of the
                        // latest release must not trigger the update dialog.
                        if version_tuple(latest) > version_tuple(current) {
                            let body = json.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let url = json.get("html_url").and_then(|v| v.as_str()).unwrap_or("https://github.com/mrFrok/LibreFastbootFirmwareFlasher/releases/latest").to_string();
                            tx.send(WMsg::UpdateAvailable {
                                version: latest.to_string(),
                                url,
                                body,
                            }).ok();
                        } else {
                            log(&tx, LogLevel::Success, 0, "You are running the latest version");
                        }
                    }
            }
        }
    }
}
