use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Cmd, WMsg, LogLevel};

fn log(tx: &mpsc::Sender<WMsg>, l: LogLevel, tab: u8, m: impl Into<String>) {
    tx.send(WMsg::Log { level: l, message: m.into(), tab }).ok();
}

fn get_output_dir() -> PathBuf {
    crate::config::get_output_dir()
}

/// Extract ZIP firmware and watch staging directory for new .img files.
/// Returns the output directory on success, or None on failure.
/// This helper eliminates ~200 lines of duplicated code across 4 Cmd handlers.
fn extract_and_watch(
    zip_path: &str,
    tx: &mpsc::Sender<WMsg>,
    tab: u8,
) -> Option<PathBuf> {
    let fw = Path::new(zip_path);
    let fw_name = lfff_lib::extractor::get_firmware_name(fw);
    let out = get_output_dir().join(&fw_name);

    log(tx, LogLevel::Info, tab, format!("Extracting to {}...", out.display()));

    let tx_ex = tx.clone();
    let staging = out.join("_staging");
    std::fs::create_dir_all(&staging).ok();

    let tx_w = tx.clone();
    let wd = staging.clone();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        let mut known = std::collections::HashSet::<String>::new();
        let mut secs = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if stop_rx.try_recv().is_ok() { break; }
            secs += 1;
            let mut q = vec![wd.clone()];
            while let Some(d) = q.pop() {
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() { q.push(p); continue; }
                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                        if name.ends_with(".img") && known.insert(name.clone()) {
                            tx_w.send(WMsg::Log { level: LogLevel::Info, message: format!("Extracted: {}", name), tab }).ok();
                        }
                    }
                }
            }
            if secs.is_multiple_of(5) && known.is_empty() {
                tx_w.send(WMsg::Log { level: LogLevel::Info, message: format!("Extracting... ({}s)", secs), tab }).ok();
            }
        }
    });

    let r = lfff_lib::extractor::extract_firmware_with_log(
        fw, &out, None, None,
        Some(&|line: String| {
            tx_ex.send(WMsg::Log { level: LogLevel::Info, message: line, tab }).ok();
        }),
    );
    stop_tx.send(()).ok();

    if !r.success {
        log(tx, LogLevel::Error, tab, "Extract failed");
        tx.send(WMsg::FlashComplete {
            success: false, message: r.error,
            log_summary: "Extract failed".into(), failed_partitions: vec![],
        }).ok();
        tx.send(WMsg::Flashing(false)).ok();
        return None;
    }

    log(tx, LogLevel::Success, tab, format!("{} groups extracted", r.groups.len()));
    Some(r.output_dir)
}

fn wait_for_fastboot(tx: &mpsc::Sender<WMsg>, tab: u8, timeout_secs: u64) -> bool {
    log(tx, LogLevel::Info, tab, "Waiting for device in fastboot...");
    for i in 1..=timeout_secs {
        std::thread::sleep(Duration::from_secs(1));
        let found = std::process::Command::new("fastboot")
            .args(["devices"])
            .output()
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                s.lines().any(|l| !l.trim().is_empty() && !l.starts_with("List"))
            })
            .unwrap_or(false);
        if found {
            log(tx, LogLevel::Success, tab, "Device found in fastboot!");
            return true;
        }
        if i % 5 == 0 {
            log(tx, LogLevel::Info, tab, format!("Still waiting... ({}/{}s)", i, timeout_secs));
        }
    }
    log(tx, LogLevel::Error, tab, "Timeout waiting for device in fastboot");
    false
}

fn do_flash(
    tx: &mpsc::Sender<WMsg>,
    source: &lfff_lib::flasher::FirmwareSource,
    serial: &Option<String>,
    device_product: &str,
    skip_xbl_abl: bool,
    skip_preloader: bool,
    as_mediatek: Option<bool>,
    skip_partitions: String,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let sref = serial.as_deref();
    let tx_log = tx.clone();
    let tx_prog = tx.clone();
    let tx_fail = tx.clone();

    let session = lfff_lib::flasher::run_flash_session_with_log(
        source, sref, false, skip_xbl_abl, skip_preloader, as_mediatek,
        cancel, skip_partitions,
        &|msg| { tx_log.send(WMsg::Log { level: LogLevel::Info, message: msg, tab: 2 }).ok(); },
        &|p| {
            let fraction = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
            tx_prog.send(WMsg::Progress { fraction, partition: format!("{}_{}", p.partition, p.slot) }).ok();
        },
        &|partition, slot, error| {
            let (resp_tx, resp_rx) = std::sync::mpsc::channel();
            tx_fail.send(WMsg::FlashFailure {
                partition: partition.to_string(), slot: slot.to_string(),
                error: error.to_string(), response: resp_tx,
            }).ok();
            resp_rx.recv().unwrap_or(lfff_lib::flasher::FailureAction::Abort)
        },
    );

    tx.send(WMsg::Progress { fraction: 1.0, partition: String::new() }).ok();
    let failed = session.failed().len();
    let total = session.results.len();
    let success = !session.aborted && failed == 0;
    for r in session.failed() {
        let level = if session.critical_failed().iter().any(|c| c.partition == r.partition && c.slot == r.slot) {
            LogLevel::Error
        } else {
            LogLevel::Warn
        };
        log(tx, level, 2, &format!("{}_{}: {}", r.partition, r.slot, r.error));
    }

    // Write flash history BEFORE sending FlashComplete
    let duration_s: f64 = session.results.iter().map(|r| r.duration_s).sum();
    let failed_parts: Vec<lfff_lib::flash_history::FailedPartition> = session.failed().iter().map(|r| {
        lfff_lib::flash_history::FailedPartition {
            name: r.partition.clone(),
            slot: r.slot.clone(),
            error: r.error.clone(),
        }
    }).collect();
    let fw_name = source.path()
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
        tx.send(WMsg::Log { level: LogLevel::Warn, message: format!("Failed to write flash history: {}", e), tab: 2 }).ok();
    }

    let crit_failed = session.critical_failed().len();
    let detail_lines: Vec<String> = session.failed().iter()
        .map(|r| format!("{}_{}: {}", r.partition, r.slot, r.error))
        .collect();
    let msg = if failed > 0 {
        let crit = if crit_failed > 0 { format!("\n⚠ {} critical partition(s) failed!", crit_failed) } else { String::new() };
        format!("{}/{} failed:{}\n{}", failed, total, crit, detail_lines.join("\n"))
    } else if session.aborted {
        "Flash aborted by user".into()
    } else {
        format!("Done! {}/{} OK", total, total)
    };
    let failed_partitions: Vec<String> = session.failed().iter().map(|r| r.partition.clone()).collect();
    let log_msg = if failed > 0 {
        format!("{}/{} partitions failed", failed, total)
    } else if session.aborted {
        "Flash aborted".into()
    } else {
        format!("{}/{} OK", total, total)
    };
    tx.send(WMsg::FlashComplete { success, message: msg, log_summary: log_msg, failed_partitions }).ok();
    tx.send(WMsg::Flashing(false)).ok();
}

pub fn worker(
    rx: mpsc::Receiver<Cmd>,
    tx: mpsc::Sender<WMsg>,
    flash_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut serial: Option<String> = None;
    #[allow(unused_assignments)]
    let mut skip_xbl_abl: bool = false;
    #[allow(unused_assignments)]
    let mut skip_preloader: bool = false;
    let mut as_mediatek: Option<bool> = None;
    let mut current_device_product = String::new();
    let mut current_source: Option<lfff_lib::flasher::FirmwareSource> = None;
    let dl_cancel_token: std::sync::Arc<std::sync::Mutex<Option<lfff_lib::downloader::CancelToken>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    while let Ok(cmd) = rx.recv() {
        match cmd {
        Cmd::CheckDevice => {
                    log(&tx, LogLevel::Info, 0, "Searching for device...");

                    let adb_out = std::process::Command::new("adb").args(["devices"]).output();
                    let mut adb_device = None;
                    if let Ok(o) = &adb_out {
                        let out = String::from_utf8_lossy(&o.stdout);
                        for line in out.lines().skip(1) {
                            if line.contains("\tdevice") {
                                adb_device = Some(line.split('\t').next().unwrap_or("").to_string());
                                break;
                            }
                        }
                    }

                    if let Some(ref ser) = adb_device {
                        log(&tx, LogLevel::Success, 0, format!("ADB device: {}", ser));
                        serial = Some(ser.clone());

                        let getprop = |prop: &str| -> String {
                            std::process::Command::new("adb").args(["shell", "getprop", prop]).output()
                                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                                .unwrap_or_default()
                        };
                        let model = getprop("ro.product.model");
                        let product = getprop("ro.product.device");
                        let build = getprop("ro.build.display.id");
                        let android = getprop("ro.build.version.release");
                        let slot = getprop("ro.boot.slot_suffix");
                        let battery = std::process::Command::new("adb")
                            .args(["shell", "cat", "/sys/class/power_supply/battery/capacity"]).output()
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i32>().unwrap_or(-1))
                            .unwrap_or(-1);

                        let name = if !model.is_empty() { model.clone() } else { product.clone() };
                        current_device_product = if !product.is_empty() { product.clone() } else { name.clone() };
                        let slot_clean = slot.trim_start_matches('_').to_string();

                        tx.send(WMsg::DeviceDetected {
                            name: name.clone(), serial: ser.clone(),
                            slot: if slot_clean.is_empty() { "N/A".into() } else { slot_clean },
                            is_fastboot_mode: false,
                        }).ok();

                        let mut info = format!("Device: {} ({})", name, product);
                        if !build.is_empty() { info.push_str(&format!(" | Build: {}", build)); }
                        if !android.is_empty() { info.push_str(&format!(" | Android {}", android)); }
                        if battery >= 0 { info.push_str(&format!(" | Battery: {}%", battery)); }
                        log(&tx, LogLevel::Success, 0, info);
                        continue;
                    }

                    let s = lfff_lib::device::list_fastboot_devices();
                    if s.is_empty() {
                        log(&tx, LogLevel::Error, 0, "No device found via ADB or fastboot");
                        tx.send(WMsg::DeviceDisconnected).ok();
                        serial = None;
                        current_device_product.clear();
                        continue;
                    }
                    let ser = &s[0];
                    serial = Some(ser.clone());
                    match lfff_lib::device::get_device_info(Some(ser)) {
                        Some(i) => {
                            let name = if i.product.is_empty() { ser.clone() } else { i.product.clone() };
                            current_device_product = name.clone();
                            let slot = if i.current_slot.is_empty() { "\u{2014}".into() } else { i.current_slot.clone() };
                            tx.send(WMsg::DeviceDetected { name: name.clone(), serial: ser.clone(), slot, is_fastboot_mode: true }).ok();
                            let mut d = format!("Fastboot device: {}", name);
                            if i.battery_level >= 0 { d.push_str(&format!(" | Battery: {}%", i.battery_level)); }
                            if !i.unlocked.is_empty() { d.push_str(&format!(" | Unlocked: {}", i.unlocked)); }
                            log(&tx, LogLevel::Success, 0, d);
                        }
                        None => log(&tx, LogLevel::Error, 0, "Device found but getvar failed"),
                    }
                }

        Cmd::RebootForFlash { reboot_choice } => {
                    let ready = match reboot_choice {
                        1 => {
                            log(&tx, LogLevel::Info, 2, "Rebooting to fastbootd via ADB...");
                            let _ = std::process::Command::new("adb").args(["reboot", "fastboot"]).status();
                            log(&tx, LogLevel::Info, 2, "Waiting for device to enter fastbootd...");
                            std::thread::sleep(Duration::from_secs(5));
                            lfff_lib::flasher::wait_for_fastbootd(serial.as_deref(), 90)
                        }
                        2 => {
                            log(&tx, LogLevel::Info, 2, "Rebooting to fastbootd via fastboot...");
                            let _ = std::process::Command::new("fastboot").args(["reboot", "fastboot"]).status();
                            log(&tx, LogLevel::Info, 2, "Waiting for device to enter fastbootd...");
                            std::thread::sleep(Duration::from_secs(4));
                            lfff_lib::flasher::wait_for_fastbootd(serial.as_deref(), 90)
                        }
                        3 => {
                            log(&tx, LogLevel::Info, 2, "Verifying device is in fastbootd mode...");
                            let fb = lfff_lib::device::list_fastbootd_devices();
                            if !fb.is_empty() {
                                log(&tx, LogLevel::Success, 2, "Device confirmed in fastbootd");
                                true
                            } else {
                                log(&tx, LogLevel::Error, 2, "Device is NOT in fastbootd mode — it may be in bootloader (fastboot) instead. Please reboot to fastbootd and try again.");
                                false
                            }
                        }
                        _ => false,
                    };
                    if ready {
                        tx.send(WMsg::ReadyToFlash).ok();
                    } else {
                        log(&tx, LogLevel::Error, 2, "Device not found in fastbootd — aborting");
                        tx.send(WMsg::FlashComplete {
                            success: false, message: "Device not found in fastbootd".into(),
                            log_summary: "Device not found".into(), failed_partitions: vec![],
                        }).ok();
                    }
                }

        Cmd::Flash { path, skip_arb, skip_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx, LogLevel::Info, 2, "Starting flash...");

                    let dir = if path.ends_with(".zip") {
                        match extract_and_watch(&path, &tx, 2) {
                            Some(d) => d,
                            None => continue,
                        }
                    } else {
                        Path::new(&path).to_path_buf()
                    };

                    skip_xbl_abl = false;
                    skip_preloader = false;
                    let images = lfff_lib::flasher::collect_images(&dir);
                    as_mediatek = lfff_lib::flasher::detect_device_type(serial.as_deref(), &images);

                    if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                        log(&tx, LogLevel::Info, 2, "Mediatek platform detected (preloader found)");
                        skip_xbl_abl = true;
                    } else if as_mediatek == Some(true) {
                        log(&tx, LogLevel::Info, 2, "Mediatek platform detected (no preloader in firmware)");
                        skip_xbl_abl = true;
                    } else if as_mediatek == Some(false) {
                        log(&tx, LogLevel::Info, 2, "Qualcomm platform detected");
                    } else {
                        log(&tx, LogLevel::Info, 2, "Platform detection inconclusive — proceeding with default logic");
                    }

                    if !skip_arb && as_mediatek != Some(true)
                        && let Some(xbl) = lfff_lib::arb::find_xbl_config(&dir) {
                            let a = lfff_lib::arb::extract_arb_from_xbl(&xbl);
                            let ver = a.version.unwrap_or(0);
                            if ver > 0 {
                                tx.send(WMsg::Flashing(false)).ok();
                                tx.send(WMsg::ArbWarning { version: ver, as_mediatek }).ok();
                                log(&tx, LogLevel::Warn, 2, format!("ARB={} — anti-rollback will be raised, waiting for confirmation...", ver));
                                continue;
                            }
                            tx.send(WMsg::Flashing(false)).ok();
                            tx.send(WMsg::ArbDeviceWarning { path: path.clone(), is_source: false, device_arb: 0 }).ok();
                            log(&tx, LogLevel::Warn, 2, "Firmware ARB=0 — device ARB unknown, may be unsafe to flash");
                            continue;
                        }

                    if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                        tx.send(WMsg::Flashing(false)).ok();
                        tx.send(WMsg::PreloaderWarning { path: path.clone(), is_source: false }).ok();
                        log(&tx, LogLevel::Warn, 2, "preloader detected — Mediatek firmware, waiting for confirmation...");
                        continue;
                    }

                    do_flash(&tx, &lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()),
                        &serial, &current_device_product, skip_xbl_abl, skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                    current_source = Some(lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()));
                }

        Cmd::ConfirmArbAndFlash { path, skip_xbl_abl: cmd_skip_xbl_abl, skip_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();

                    let dir = if path.ends_with(".zip") {
                        match extract_and_watch(&path, &tx, 2) {
                            Some(d) => d,
                            None => continue,
                        }
                    } else {
                        Path::new(&path).to_path_buf()
                    };

                    log(&tx, LogLevel::Info, 2, "ARB warning confirmed by user, proceeding...");
                    let src = lfff_lib::flasher::FirmwareSource::Extracted(dir.clone());
                    do_flash(&tx, &src, &serial, &current_device_product, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                    current_source = Some(src);
                }

        Cmd::ConfirmArbDeviceFlash { path, is_source, skip_xbl_abl: cmd_skip_xbl_abl, skip_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();

                    if is_source {
                        log(&tx, LogLevel::Info, 2, "Device ARB warning confirmed by user, flashing from source...");
                        let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&path));
                        do_flash(&tx, &d, &serial, &current_device_product, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(d);
                    } else {
                        log(&tx, LogLevel::Info, 2, "Device ARB warning confirmed by user, proceeding...");
                        let dir = if path.ends_with(".zip") {
                            match extract_and_watch(&path, &tx, 2) {
                                Some(d) => d,
                                None => continue,
                            }
                        } else {
                            Path::new(&path).to_path_buf()
                        };
                        let src = lfff_lib::flasher::FirmwareSource::Extracted(dir);
                        do_flash(&tx, &src, &serial, &current_device_product, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(src);
                    }
                }

        Cmd::ConfirmPreloaderFlash { path, is_source, skip_preloader: cmd_skip_preloader, skip_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx, LogLevel::Info, 2, "Preloader warning confirmed by user, proceeding...");

                    if is_source {
                        let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&path));
                        do_flash(&tx, &d, &serial, &current_device_product, true, cmd_skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(d);
                    } else {
                        let dir = if path.ends_with(".zip") {
                            match extract_and_watch(&path, &tx, 2) {
                                Some(d) => d,
                                None => continue,
                            }
                        } else {
                            Path::new(&path).to_path_buf()
                        };
                        let src = lfff_lib::flasher::FirmwareSource::Extracted(dir);
                        do_flash(&tx, &src, &serial, &current_device_product, true, cmd_skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(src);
                    }
                }

        Cmd::FlashFromSource { dir, skip_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    skip_xbl_abl = false;
                    skip_preloader = false;
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx, LogLevel::Info, 2, format!("Flashing from source dir: {}", dir));
                    let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&dir));
                    current_source = Some(d.clone());

                    let images = lfff_lib::flasher::collect_images_from_source(d.path());
                    if images.is_empty() {
                        log(&tx, LogLevel::Error, 2, "No flashable .img files found in the selected source directory");
                        tx.send(WMsg::FlashComplete {
                            success: false, message: "No flashable images found".into(),
                            log_summary: "No images found".into(), failed_partitions: vec![],
                        }).ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
                    log(&tx, LogLevel::Info, 2, format!("Found {} images to flash", images.len()));
                    for (name, path) in &images {
                        let size_mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1024.0 / 1024.0).unwrap_or(0.0);
                        log(&tx, LogLevel::Info, 2, format!("  {} ({:.1} MB)", name, size_mb));
                    }

                    let is_mtk = lfff_lib::flasher::detect_device_type(serial.as_deref(), &images);
                    as_mediatek = is_mtk;

                    if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                        log(&tx, LogLevel::Info, 2, "Mediatek platform detected");
                        tx.send(WMsg::Flashing(false)).ok();
                        tx.send(WMsg::PreloaderWarning { path: dir.clone(), is_source: true }).ok();
                        log(&tx, LogLevel::Warn, 2, "preloader detected — Mediatek firmware, waiting for confirmation...");
                        continue;
                    } else if as_mediatek == Some(true) {
                        log(&tx, LogLevel::Info, 2, "Mediatek platform detected (no preloader in firmware)");
                    } else if as_mediatek == Some(false) {
                        log(&tx, LogLevel::Info, 2, "Qualcomm platform detected");
                    } else {
                        log(&tx, LogLevel::Info, 2, "Platform detection inconclusive — proceeding with default logic");
                    }

                    do_flash(&tx, &d, &serial, &current_device_product, skip_xbl_abl, skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                }

        Cmd::FlashSingle { path, partition, reboot_choice } => {
                    tx.send(WMsg::Flashing(true)).ok();
                    let ready = match reboot_choice {
                        1 => {
                            log(&tx, LogLevel::Info, 3, "Rebooting to bootloader via ADB...");
                            let _ = std::process::Command::new("adb").args(["reboot", "bootloader"]).status();
                            log(&tx, LogLevel::Info, 3, "Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(5));
                            wait_for_fastboot(&tx, 3, 90)
                        }
                        2 => {
                            log(&tx, LogLevel::Info, 3, "Rebooting to bootloader via fastboot...");
                            let _ = std::process::Command::new("fastboot").args(["reboot-bootloader"]).status();
                            log(&tx, LogLevel::Info, 3, "Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(4));
                            wait_for_fastboot(&tx, 3, 90)
                        }
                        _ => {
                            log(&tx, LogLevel::Info, 3, "Checking device is in fastboot...");
                            wait_for_fastboot(&tx, 3, 10)
                        }
                    };
                    if !ready {
                        tx.send(WMsg::FlashComplete {
                            success: false, message: "Device not found in fastboot".into(),
                            log_summary: "Device not found".into(), failed_partitions: vec![],
                        }).ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
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
                        let mut p = img.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
                        for s in &["_a", "_b"] {
                            if p.ends_with(s) { p = p[..p.len() - s.len()].to_string(); break; }
                        }
                        p
                    };
                    log(&tx, LogLevel::Info, 3, format!("Flashing partition: {}", p));
                    let slots = ["a", "b"];
                    let total = 2;
                    let mut fail = 0;
                    for (done, slot) in slots.iter().enumerate() {
                        let lbl = format!("{}_{}", p, slot);
                        tx.send(WMsg::Progress { fraction: done as f32 / total as f32, partition: lbl.clone() }).ok();
                        let r = lfff_lib::flasher::flash_partition(img, &p, slot, sref);
                        if r.success {
                            log(&tx, LogLevel::Success, 3, format!("{} OK", lbl));
                        } else {
                            fail += 1;
                            log(&tx, LogLevel::Error, 3, format!("{} FAILED", lbl));
                        }
                    }
                    tx.send(WMsg::Progress { fraction: 1.0, partition: String::new() }).ok();
                    tx.send(WMsg::FlashComplete {
                        success: fail == 0,
                        message: if fail == 0 { format!("{} flashed OK", p) } else { format!("{} errors", fail) },
                        log_summary: if fail == 0 { "Flash OK".into() } else { format!("{} errors", fail) },
                        failed_partitions: vec![],
                    }).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                }

        Cmd::CancelFlash => log(&tx, LogLevel::Warn, 2, "Cancelling flash..."),

        Cmd::PostFlashReboot => {
                    log(&tx, LogLevel::Info, 2, "Rebooting to system...");
                    let mut args = vec!["fastboot"];
                    let ser_s;
                    if let Some(s) = &serial { ser_s = s.clone(); args.extend(&["-s", &ser_s]); }
                    args.push("reboot");
                    match std::process::Command::new(args[0]).args(&args[1..]).status() {
                        Ok(s) if s.success() => log(&tx, LogLevel::Success, 2, "Reboot initiated"),
                        _ => log(&tx, LogLevel::Error, 2, "Failed to reboot"),
                    }
                }

        Cmd::PostFlashWipe => {
                    log(&tx, LogLevel::Warn, 2, "Wiping data (fastboot -w)...");
                    let mut args = vec!["fastboot"];
                    let ser_s;
                    if let Some(s) = &serial { ser_s = s.clone(); args.extend(&["-s", &ser_s]); }
                    args.push("-w");
                    match std::process::Command::new(args[0]).args(&args[1..]).status() {
                        Ok(s) if s.success() => {
                            log(&tx, LogLevel::Success, 2, "Wipe done, rebooting...");
                            let mut args2 = vec!["fastboot"];
                            let ser_s2;
                            if let Some(s) = &serial { ser_s2 = s.clone(); args2.extend(&["-s", &ser_s2]); }
                            args2.push("reboot");
                            std::process::Command::new(args2[0]).args(&args2[1..]).status().ok();
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
                        _ => { log(&tx, LogLevel::Error, 0, "Unknown reboot target"); continue; }
                    };
                    log(&tx, LogLevel::Info, 0, format!("Rebooting to {}...", target));
                    let is_fastboot_cmd = cmd == "fastboot";
                    let mut child = std::process::Command::new(cmd).args(args)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    let success = match child {
                        Ok(ref mut c) => {
                            let timeout = if is_fastboot_cmd { 5 } else { 15 };
                            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
                            loop {
                                if let Ok(Some(status)) = c.try_wait() { break status.success(); }
                                if std::time::Instant::now() >= deadline { let _ = c.kill(); break false; }
                                std::thread::sleep(std::time::Duration::from_millis(200));
                            }
                        }
                        Err(_) => false,
                    };
                    if success {
                        log(&tx, LogLevel::Success, 0, format!("Reboot to {} initiated", target));
                    } else if is_fastboot_cmd {
                        log(&tx, LogLevel::Error, 0, "Device not in fastboot mode — use ADB reboot buttons instead");
                    } else {
                        log(&tx, LogLevel::Error, 0, format!("Failed to reboot to {}", target));
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
                                log(&tx, LogLevel::Info, 0, format!("Setting active slot to {}...", slot.to_uppercase()));
                                let args = vec!["-s", ser, "set_active", &slot];
                                let out = std::process::Command::new("fastboot").args(&args)
                                    .output();
                                match out {
                                    Ok(o) if o.status.success() => {
                                        log(&tx, LogLevel::Success, 0, format!("Active slot set to {}", slot.to_uppercase()));
                                        tx.send(WMsg::DeviceDetected {
                                            name: "".into(),
                                            serial: ser.clone(),
                                            slot: slot.clone(),
                                            is_fastboot_mode: true,
                                        }).ok();
                                    }
                                    Ok(o) => {
                                        let err = String::from_utf8_lossy(&o.stderr);
                                        log(&tx, LogLevel::Error, 0, format!("Failed to set slot: {}", err.trim()));
                                    }
                                    Err(e) => log(&tx, LogLevel::Error, 0, format!("Failed to run fastboot: {}", e)),
                                }
                            }
                            _ => {
                                log(&tx, LogLevel::Error, 0, "Device is not in fastboot (bootloader) mode. Slot switching only works in fastboot mode. Please reboot to bootloader first.");
                            }
                        }
                    } else {
                        log(&tx, LogLevel::Error, 0, "No device connected");
                    }
                }

        Cmd::CableTest => {
                    let total_steps = 10u8;
                    let mut success_count = 0u8;
                    let mut total_latency_ms = 0u64;
                    let mut args: Vec<&str> = vec!["fastboot"];
                    if let Some(ref s) = serial { args.extend(&["-s", s]); }
                    args.extend(&["getvar", "product"]);

                    for step in 0..total_steps {
                        let start = std::time::Instant::now();
                        tx.send(WMsg::CableTestProgress {
                            step, total: total_steps,
                            status: format!("Test {}/{}...", step + 1, total_steps),
                        }).ok();
                        let output = std::process::Command::new(args[0]).args(&args[1..]).output();
                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        match output {
                            Ok(o) if o.status.success() => {
                                success_count += 1;
                                total_latency_ms += elapsed_ms;
                            }
                            _ => {
                                tx.send(WMsg::CableTestProgress {
                                    step: total_steps, total: total_steps,
                                    status: format!("✗ {} failed — check cable/USB port", total_steps - success_count),
                                }).ok();
                                tx.send(WMsg::Flashing(false)).ok();
                                return;
                            }
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }

                    let avg_ms = total_latency_ms / total_steps as u64;
                    let speed_label = if avg_ms < 50 { "excellent" } else if avg_ms < 150 { "good" } else if avg_ms < 500 { "fair" } else { "poor" };
                    tx.send(WMsg::CableTestProgress {
                        step: total_steps, total: total_steps,
                        status: format!("✓ OK — avg {}ms ({})", avg_ms, speed_label),
                    }).ok();
                }

        Cmd::RetryFlash { failed_partitions } => {
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx, LogLevel::Info, 2, format!("Retrying {} partition(s): {}", failed_partitions.len(), failed_partitions.join(", ")));

                    let source = match &current_source {
                        Some(s) => s.clone(),
                        None => {
                            log(&tx, LogLevel::Error, 2, "No firmware source available for retry");
                            tx.send(WMsg::FlashComplete {
                                success: false, message: "No firmware source available".into(),
                                log_summary: "No source available".into(), failed_partitions: vec![],
                            }).ok();
                            tx.send(WMsg::Flashing(false)).ok();
                            continue;
                        }
                    };

                    let total = failed_partitions.len() * 2;
                    let mut done = 0;
                    let mut fail_count = 0;
                    let mut failed_list = Vec::new();
                    let sref = serial.as_deref();

                    for partition in &failed_partitions {
                        for slot in &["a", "b"] {
                            if flash_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                log(&tx, LogLevel::Warn, 2, "Retry cancelled by user");
                                tx.send(WMsg::FlashComplete {
                                    success: false, message: "Retry cancelled".into(),
                                    log_summary: "Retry cancelled".into(), failed_partitions: failed_list.clone(),
                                }).ok();
                                tx.send(WMsg::Flashing(false)).ok();
                                return;
                            }

                            let lbl = format!("{}_{}", partition, slot);
                            tx.send(WMsg::Progress { fraction: done as f32 / total as f32, partition: lbl.clone() }).ok();

                            let fw_dir = source.path();
                            let images = if source.is_source() {
                                lfff_lib::flasher::collect_images_from_source(fw_dir)
                            } else {
                                lfff_lib::flasher::collect_images(fw_dir)
                            };

                            let part_lc = partition.to_lowercase();
                            let img_path = images
                                .get(&part_lc)
                                .cloned()
                                .or_else(|| {
                                    images
                                        .iter()
                                        .find(|(name, _)| name.to_lowercase().starts_with(&part_lc))
                                        .map(|(_, path)| path.clone())
                                });

                            if let Some(img) = img_path {
                                log(&tx, LogLevel::Info, 2, format!("Retrying {} from {}...", lbl, img.display()));
                                let result = lfff_lib::flasher::flash_partition(&img, partition, slot, sref);
                                if result.success {
                                    log(&tx, LogLevel::Success, 2, format!("{} OK", lbl));
                                } else {
                                    fail_count += 1;
                                    failed_list.push(partition.clone());
                                    log(&tx, LogLevel::Error, 2, format!("{} FAILED", lbl));
                                }
                            } else {
                                fail_count += 1;
                                failed_list.push(partition.clone());
                                log(&tx, LogLevel::Error, 2, format!("Image not found for {}", partition));
                            }
                            done += 1;
                        }
                    }

                    let success = fail_count == 0;
                    let msg = if success {
                        format!("Retry complete! {} partition(s) OK", failed_partitions.len())
                    } else {
                        format!("{}/{} failed on retry", fail_count, failed_partitions.len())
                    };
                    let log_msg = if success { "Retry OK".into() } else { format!("{}/{} failed", fail_count, failed_partitions.len()) };

                    tx.send(WMsg::Progress { fraction: 1.0, partition: String::new() }).ok();
                    tx.send(WMsg::FlashComplete { success, message: msg, log_summary: log_msg, failed_partitions: failed_list }).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                }

        Cmd::CheckDeps => {
                    log(&tx, LogLevel::Info, 0, "Checking dependencies...");
                    crate::with_captured_stdout(&tx, 0, || {
                        let r = lfff_lib::deps::install_dependencies(None, true);
                        for d in &r.results {
                            if d.already_installed { log(&tx, LogLevel::Success, 0, format!("{}: OK", d.tool)); }
                            else if d.skipped { log(&tx, LogLevel::Warn, 0, format!("{}: skipped", d.tool)); }
                            else if !d.error.is_empty() { log(&tx, LogLevel::Error, 0, format!("{}: {}", d.tool, d.error)); }
                        }
                        tx.send(WMsg::DepsResult { ok: r.all_ok(), message: if r.all_ok() { "All dependencies OK".into() } else { "Some missing".into() } }).ok();
                    });
                }

        Cmd::InstallDeps => {
                    log(&tx, LogLevel::Info, 0, "Installing dependencies...");
                    crate::with_captured_stdout(&tx, 0, || {
                        let r = lfff_lib::deps::install_dependencies(None, false);
                        for d in &r.results {
                            if d.installed { log(&tx, LogLevel::Success, 0, format!("{}: installed", d.tool)); }
                            else if d.already_installed { log(&tx, LogLevel::Success, 0, format!("{}: already OK", d.tool)); }
                            else if !d.error.is_empty() { log(&tx, LogLevel::Error, 0, format!("{}: {}", d.tool, d.error)); }
                        }
                        tx.send(WMsg::DepsResult { ok: r.all_ok(), message: if r.all_ok() { "All OK".into() } else { "Some failed".into() } }).ok();
                    });
                }

        Cmd::Download { url } => {
                    tx.send(WMsg::Downloading(true)).ok();
                    log(&tx, LogLevel::Info, 1, "Starting download...");
                    let tx_dl = tx.clone();
                    let token = lfff_lib::downloader::CancelToken::new();
                    *dl_cancel_token.lock().unwrap() = Some(token.clone());
                    std::thread::spawn(move || {
                        let out = get_output_dir();
                        log(&tx_dl, LogLevel::Info, 1, format!("Output: {}", out.display()));
                        let tx2 = tx_dl.clone();
                        let last_update = std::sync::Mutex::new(std::time::Instant::now());
                        let r = lfff_lib::downloader::download_firmware_with_progress(&url, Some(&out), 16, token, move |p| {
                            let mut last = last_update.lock().unwrap();
                            if last.elapsed().as_millis() >= 100 || p.percent >= 100.0 {
                                tx2.send(WMsg::DlProgress {
                                    percent: p.percent, speed: p.speed, eta: p.eta,
                                    downloaded: p.downloaded, total: p.total_size, raw_line: p.raw_line,
                                }).ok();
                                *last = std::time::Instant::now();
                            }
                        });
                        if r.success {
                            let p = r.output_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                            log(&tx_dl, LogLevel::Success, 1, format!("Downloaded: {}", p));
                            if let Some(path) = r.output_path { tx_dl.send(WMsg::FwPath(path.display().to_string())).ok(); }
                        } else if r.error == "Cancelled" {
                            log(&tx_dl, LogLevel::Warn, 1, "Download cancelled");
                        } else {
                            log(&tx_dl, LogLevel::Error, 1, format!("Failed: {}", r.error));
                        }
                        tx_dl.send(WMsg::DlProgress { percent: 0.0, speed: String::new(), eta: String::new(), downloaded: String::new(), total: String::new(), raw_line: String::new() }).ok();
                        tx_dl.send(WMsg::Downloading(false)).ok();
                    });
                }

        Cmd::CancelDownload => {
                    if let Some(token) = dl_cancel_token.lock().unwrap().take() {
                        token.cancel();
                        log(&tx, LogLevel::Warn, 1, "Cancelling download...");
                    }
                }

        Cmd::Extract { path } => {
                    let fw = Path::new(&path);
                    let out = get_output_dir().join(lfff_lib::extractor::get_firmware_name(fw));
                    log(&tx, LogLevel::Info, 1, format!("Extracting to {}...", out.display()));
                    let tx_ex = tx.clone();
                    let staging = out.join("_staging");
                    std::fs::create_dir_all(&staging).ok();
                    let tx_watch = tx.clone();
                    let watch_dir = staging.clone();
                    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
                    std::thread::spawn(move || {
                        let mut known = std::collections::HashSet::<String>::new();
                        let mut secs = 0u32;
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            if stop_rx.try_recv().is_ok() { break; }
                            secs += 1;
                            let mut queue = vec![watch_dir.clone()];
                            while let Some(dir) = queue.pop() {
                                if let Ok(rd) = std::fs::read_dir(&dir) {
                                    for e in rd.flatten() {
                                        let p = e.path();
                                        if p.is_dir() { queue.push(p); continue; }
                                        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                        if name.ends_with(".img") && known.insert(name.clone()) {
                                            tx_watch.send(WMsg::Log { level: LogLevel::Info, message: format!("Extracted: {}", name), tab: 1 }).ok();
                                        }
                                    }
                                }
                            }
                            if secs.is_multiple_of(5) && known.is_empty() {
                                tx_watch.send(WMsg::Log { level: LogLevel::Info, message: format!("Extracting... ({}s)", secs), tab: 1 }).ok();
                            }
                        }
                    });
                    let r = lfff_lib::extractor::extract_firmware_with_log(fw, &out, None, None,
                        Some(&|line: String| { tx_ex.send(WMsg::Log { level: LogLevel::Info, message: line, tab: 1 }).ok(); }));
                    stop_tx.send(()).ok();
                    if r.success {
                        let n: usize = r.groups.values().map(|v| v.len()).sum();
                        log(&tx, LogLevel::Success, 1, format!("{} images extracted to {}", n, out.display()));
                    } else {
                        log(&tx, LogLevel::Error, 1, format!("Extraction failed: {}", r.error));
                    }
                }

        Cmd::DriverTest => {
                    use std::process::Command as Cmd2;
                    let step = |tx: &mpsc::Sender<WMsg>, s: i32, msg: &str| {
                        tx.send(WMsg::TestStep { step: s, status: msg.into() }).ok();
                        log(tx, LogLevel::Info, 0, msg);
                    };

                    step(&tx, 1, "Checking ADB connection...");
                    let adb_ok = Cmd2::new("adb").args(["devices"]).output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).contains("\tdevice"))
                        .unwrap_or(false);
                    if !adb_ok {
                        let fb = lfff_lib::device::list_fastboot_devices();
                        if fb.is_empty() {
                            step(&tx, -1, "No device found via ADB or fastboot");
                            continue;
                        }
                        step(&tx, 2, "Device found in fastboot mode");
                    } else {
                        step(&tx, 2, "ADB OK — device connected");
                        if let Ok(o) = Cmd2::new("adb").args(["shell", "getprop", "ro.product.model"]).output() {
                            let model = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !model.is_empty() { log(&tx, LogLevel::Success, 0, format!("Model: {}", model)); }
                        }
                        if let Ok(o) = Cmd2::new("adb").args(["shell", "getprop", "ro.build.display.id"]).output() {
                            let build = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !build.is_empty() { log(&tx, LogLevel::Success, 0, format!("Build: {}", build)); }
                        }
                        if let Ok(o) = Cmd2::new("adb").args(["shell", "getprop", "ro.product.cpu.abi"]).output() {
                            let abi = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !abi.is_empty() { log(&tx, LogLevel::Success, 0, format!("ABI: {}", abi)); }
                        }
                        step(&tx, 2, "Rebooting to bootloader...");
                        let _ = Cmd2::new("adb").args(["reboot", "bootloader"]).status();
                        std::thread::sleep(std::time::Duration::from_secs(8));
                    }

                    step(&tx, 3, "Checking fastboot...");
                    let mut retries = 0;
                    loop {
                        let fb = lfff_lib::device::list_fastboot_devices();
                        if !fb.is_empty() {
                            log(&tx, LogLevel::Success, 0, format!("Fastboot OK: {}", fb[0]));
                            serial = Some(fb[0].clone());
                            break;
                        }
                        retries += 1;
                        if retries > 15 { step(&tx, -1, "Fastboot not detected after 15s"); break; }
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                    if retries > 15 { continue; }

                    if let Some(ref ser) = serial
                        && let Some(info) = lfff_lib::device::get_device_info(Some(ser)) {
                            current_device_product = info.product.clone();
                            tx.send(WMsg::DeviceDetected {
                                name: info.product.clone(), serial: ser.clone(),
                                slot: info.current_slot.clone(),
                                is_fastboot_mode: true,
                            }).ok();
                            log(&tx, LogLevel::Success, 0, format!("Product: {} | Slot: {} | Battery: {}%", info.product, info.current_slot, info.battery_level));
                        }

                    step(&tx, 4, "Rebooting to fastbootd...");
                    let _ = Cmd2::new("fastboot").args(["reboot", "fastboot"]).status();
                    std::thread::sleep(std::time::Duration::from_secs(6));

                    let mut retries2 = 0;
                    loop {
                        let fb = lfff_lib::device::list_fastboot_devices();
                        if !fb.is_empty() {
                            log(&tx, LogLevel::Success, 0, "Fastbootd OK");
                            break;
                        }
                        retries2 += 1;
                        if retries2 > 15 { step(&tx, -1, "Fastbootd not detected after 15s"); break; }
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                    if retries2 > 15 { continue; }

                    step(&tx, 5, "All drivers OK!");
                    log(&tx, LogLevel::Success, 0, "Driver test completed successfully");
                }

        Cmd::CheckForUpdates => {
                    log(&tx, LogLevel::Info, 0, "Checking for updates...");
                    let current = env!("CARGO_PKG_VERSION");
                    let client = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(10))
                        .user_agent("LFFF")
                        .build();
                    if let Ok(client) = client {
                        if let Ok(resp) = client.get("https://api.github.com/repos/mrFrok/LibreFastbootFirmwareFlasher/releases/latest").send() {
                            if let Ok(json) = resp.json::<serde_json::Value>() {
                                if let Some(tag) = json.get("tag_name").and_then(|v| v.as_str()) {
                                    let latest = tag.trim_start_matches('v');
                                    if latest != current {
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
        }
    }
}
