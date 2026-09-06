use slint::{ComponentHandle, VecModel, Weak};
use std::rc::Rc;
use std::sync::mpsc;

use crate::config::{get_output_dir, load_config, save_config, save_scale, set_output_dir};
use crate::log_models::{LogModels, add_log_m};
use crate::{Cmd, DeviceInfo, FlashMethod, LogEntry, LogLevel, MainWindow, WMsg, confirm_action};

fn export_log(model: &VecModel<LogEntry>, tab_name: &str) -> Option<String> {
    use slint::Model;
    let now = chrono::Local::now();
    let fname = format!(
        "lfff-{}-log-{}.txt",
        tab_name,
        now.format("%Y-%m-%d_%H-%M-%S")
    );
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(&fname)
        .add_filter("Text", &["txt"])
        .save_file()
    {
        let mut content = String::new();
        content.push_str(&format!(
            "LFFF {} Log Export — {}\n",
            tab_name,
            now.format("%Y-%m-%d %H:%M:%S")
        ));
        content.push_str(&"=".repeat(60));
        content.push('\n');
        for i in 0..model.row_count() {
            if let Some(e) = model.row_data(i) {
                content.push_str(&format!("[{}] {} {}\n", e.timestamp, e.level, e.message));
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("Failed to export log: {}", e);
            return Some(format!("Error: {}", e));
        }
        return Some(path.display().to_string());
    }
    None
}

pub fn poll(
    w: &Weak<MainWindow>,
    rx: &mpsc::Receiver<WMsg>,
    last_dl_pct: &mut u32,
    models: &LogModels,
    fail_resp: &std::rc::Rc<
        std::cell::RefCell<Option<std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction>>>,
    >,
    fastbootd_resp: &std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<bool>>>>,
) {
    let Some(ui) = w.upgrade() else { return };

    while let Ok(m) = rx.try_recv() {
        let dl_log: Option<String> = if let WMsg::DlProgress {
            ref percent,
            ref speed,
            ref eta,
            ref downloaded,
            ref total,
            ..
        } = m
        {
            let pct = *percent as u32;
            let milestone = (pct / 10) * 10;
            if !downloaded.is_empty()
                && !total.is_empty()
                && (milestone > *last_dl_pct || pct >= 99)
            {
                *last_dl_pct = milestone;
                Some(format!(
                    "{:.0}%  {} / {}  ↓ {}  ETA {}",
                    percent, downloaded, total, speed, eta
                ))
            } else {
                None
            }
        } else {
            None
        };

        if let WMsg::DlProgress { ref downloaded, .. } = m
            && downloaded.is_empty()
        {
            *last_dl_pct = 0;
        }

        match m {
            WMsg::Log {
                level,
                message,
                tab,
            } => add_log(models, &ui, &level, tab, &message),
            WMsg::Progress {
                fraction,
                partition,
            } => {
                ui.set_flash_progress(fraction);
                ui.set_current_partition(partition.clone().into());
                if !partition.is_empty() {
                    ui.set_flash_status(partition.into());
                }
            }
            WMsg::DeviceDetected {
                name,
                serial,
                slot,
                is_fastboot_mode,
            } => {
                ui.set_device(DeviceInfo {
                    connected: true,
                    name: name.into(),
                    serial: serial.into(),
                    slot: slot.into(),
                    is_fastboot_mode,
                });
            }
            WMsg::DeviceDisconnected => {
                ui.set_device(DeviceInfo {
                    connected: false,
                    name: "\u{2014}".into(),
                    serial: "\u{2014}".into(),
                    slot: "\u{2014}".into(),
                    is_fastboot_mode: false,
                });
            }
            WMsg::FlashComplete {
                success,
                message,
                log_summary,
                failed_partitions,
            } => {
                ui.set_is_flashing(false);
                ui.set_pending_source_flash(false);
                ui.set_flash_status(log_summary.clone().into());
                if success {
                    ui.set_flash_progress(1.0);
                }
                let is_cancel =
                    message.contains("aborted by user") || message.contains("cancelled");
                add_log(
                    models,
                    &ui,
                    if success {
                        &LogLevel::Success
                    } else {
                        &LogLevel::Error
                    },
                    2,
                    &log_summary,
                );
                models.refresh_history();

                if success {
                    ui.set_confirm_action(confirm_action::SUCCESS);
                    ui.set_show_confirm(true);
                } else if !is_cancel {
                    ui.set_flash_fail_partition("".into());
                    ui.set_flash_fail_slot("".into());
                    ui.set_flash_fail_error(message.into());
                    ui.set_flash_failed_partitions(failed_partitions.join(",").into());
                    ui.set_confirm_action(confirm_action::SESSION_ERROR);
                    ui.set_show_confirm(true);
                }
            }
            WMsg::FlashFailure {
                partition,
                slot,
                error,
                response,
            } => {
                ui.set_flash_fail_partition(partition.into());
                ui.set_flash_fail_slot(slot.into());
                ui.set_flash_fail_error(error.into());
                ui.set_confirm_action(confirm_action::FLASH_FAILURE);
                ui.set_show_confirm(true);
                *fail_resp.borrow_mut() = Some(response);
            }
            WMsg::FastbootdFallback { detail, response } => {
                ui.set_fastbootd_fallback_detail(detail.into());
                ui.set_confirm_action(confirm_action::FASTBOOTD_FALLBACK);
                ui.set_show_confirm(true);
                *fastbootd_resp.borrow_mut() = Some(response);
            }
            WMsg::DepsResult { message, ok } => add_log(
                models,
                &ui,
                if ok {
                    &LogLevel::Success
                } else {
                    &LogLevel::Error
                },
                0,
                &message,
            ),
            WMsg::Flashing(f) => ui.set_is_flashing(f),
            WMsg::Downloading(f) => ui.set_is_downloading(f),
            WMsg::FwPath(p) => ui.set_firmware_path(p.into()),
            WMsg::DlProgress {
                percent,
                speed,
                eta,
                downloaded,
                total,
                raw_line,
            } => {
                ui.set_dl_percent(percent);
                ui.set_dl_speed(speed.into());
                ui.set_dl_eta(eta.into());
                ui.set_dl_downloaded(downloaded.into());
                ui.set_dl_total(total.into());
                if let Some(ref msg) = dl_log {
                    add_log(models, &ui, &LogLevel::Info, 1, msg);
                } else if !raw_line.is_empty() {
                    let is_bar = raw_line.starts_with("[#") || raw_line.starts_with('#');
                    if !is_bar {
                        add_log(models, &ui, &LogLevel::Info, 1, &raw_line);
                    }
                }
            }
            WMsg::TestStep { step, status } => {
                ui.set_test_step(step);
                ui.set_test_status(status.into());
            }
            WMsg::FlashPrepared {
                dir,
                is_source,
                arb_version,
                has_preloader,
            } => {
                ui.set_prepared_dir(dir.into());
                ui.set_is_flashing(false);
                // Warning order is fixed: ARB (Snapdragon) or preloader
                // (MediaTek) FIRST, the final confirmation dialog AFTER.
                let method = ui.get_flash_method(); // 1 = Snapdragon, 2 = MediaTek
                if method == 1 && !is_source {
                    add_log(
                        models,
                        &ui,
                        &LogLevel::Warn,
                        2,
                        &format!(
                            "ARB={} — flashing may permanently raise the anti-rollback counter. Confirm to continue.",
                            arb_version
                        ),
                    );
                    ui.set_arb_warning_version(arb_version);
                    ui.set_show_arb_warning(true);
                } else if method == 2 && has_preloader {
                    add_log(
                        models,
                        &ui,
                        &LogLevel::Warn,
                        2,
                        "⚠ preloader.img detected — confirm how to handle it",
                    );
                    ui.set_show_preloader_warning(true);
                } else {
                    ui.set_confirm_action(if is_source {
                        confirm_action::SOURCE_FINAL
                    } else {
                        confirm_action::FLASH_ALL_FINAL
                    });
                    ui.set_show_confirm(true);
                }
            }
            WMsg::ReadyToFlash => {
                ui.set_confirm_action(confirm_action::CABLE_TEST);
                ui.set_cable_test_progress(0.0);
                ui.set_cable_test_status(
                    if ui.get_lang() == "ru" {
                        "Подготовка к тесту..."
                    } else {
                        "Preparing test..."
                    }
                    .into(),
                );
                ui.set_cable_test_passed(false);
                ui.set_show_confirm(true);
            }
            WMsg::CableTestProgress {
                step,
                total,
                status,
            } => {
                ui.set_cable_test_progress(step as f32 / total as f32);
                ui.set_cable_test_status(status.into());
                if step >= total {
                    ui.set_cable_test_passed(true);
                }
            }
            WMsg::UpdateAvailable { version, url, body } => {
                ui.set_update_version(version.into());
                ui.set_update_url(url.into());
                ui.set_update_body(body.into());
                ui.set_show_update_dialog(true);
            }
        }
    }
}

fn add_log(models: &LogModels, ui: &MainWindow, l: &LogLevel, tab: u8, m: &str) {
    add_log_m(models.by_tab(tab), ui, l, m);
}

pub fn register_callbacks(
    ui: &MainWindow,
    ctx: &mpsc::Sender<Cmd>,
    models: &LogModels,
    fail_resp: &std::rc::Rc<
        std::cell::RefCell<Option<std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction>>>,
    >,
    fastbootd_resp: &std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<bool>>>>,
    flash_cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let t = ctx.clone();
    ui.on_check_device(move || {
        t.send(Cmd::CheckDevice).ok();
    });

    let dl = Rc::clone(&models.download);
    let w = ui.as_weak();
    ui.on_browse_firmware(move || {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Firmware", &["zip"])
            .add_filter("All", &["*"])
            .set_directory(get_output_dir())
            .pick_file()
            && let Some(ui) = w.upgrade()
        {
            ui.set_firmware_path(p.display().to_string().into());
            add_log_m(
                &dl,
                &ui,
                &LogLevel::Info,
                &format!(
                    "Selected: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }
    });

    let fl = Rc::clone(&models.flash);
    let w = ui.as_weak();
    ui.on_browse_folder(move || {
        if let Some(p) = rfd::FileDialog::new()
            .set_directory(get_output_dir())
            .pick_folder()
            && let Some(ui) = w.upgrade()
        {
            ui.set_firmware_path(p.display().to_string().into());
            add_log_m(
                &fl,
                &ui,
                &LogLevel::Info,
                &format!(
                    "Selected folder: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }
    });

    let fl = Rc::clone(&models.flash);
    let w = ui.as_weak();
    ui.on_browse_source_dir(move || {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Select build output directory (containing .img files)")
            .set_directory(get_output_dir())
            .pick_folder()
            && let Some(ui) = w.upgrade()
        {
            let dir_str = p.display().to_string();
            let images = lfff_lib::flasher::collect_images_from_source(&p);
            ui.set_firmware_path(dir_str.clone().into());
            ui.set_source_dir(dir_str.clone().into());
            ui.set_source_image_count(images.len() as i32);
            ui.set_source_total_ops(images.len() as i32);
            add_log_m(
                &fl,
                &ui,
                &LogLevel::Info,
                &format!(
                    "Source dir selected: {} ({} images)",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    images.len(),
                ),
            );
            let mut sorted: Vec<_> = images.iter().collect();
            sorted.sort_by_key(|(k, _)| *k);
            for (name, path) in &sorted {
                let mb = std::fs::metadata(path)
                    .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                    .unwrap_or(0.0);
                add_log_m(
                    &fl,
                    &ui,
                    &LogLevel::Info,
                    &format!("  {} ({:.1} MB)", name, mb),
                );
            }
        }
    });

    let pt = Rc::clone(&models.partition);
    let w = ui.as_weak();
    ui.on_browse_single_image(move || {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Image", &["img"])
            .add_filter("All", &["*"])
            .set_directory(get_output_dir())
            .pick_file()
            && let Some(ui) = w.upgrade()
        {
            ui.set_single_image_path(p.display().to_string().into());
            add_log_m(
                &pt,
                &ui,
                &LogLevel::Info,
                &format!(
                    "Selected: {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }
    });

    let w = ui.as_weak();
    ui.on_browse_output_dir(move || {
        // Start at the currently configured output dir (config.json or the
        // default data dir) so the user adjusts from there.
        if let Some(p) = rfd::FileDialog::new()
            .set_directory(get_output_dir())
            .pick_folder()
            && let Some(ui) = w.upgrade()
        {
            let s = p.display().to_string();
            set_output_dir(&s);
            ui.set_output_dir(s.as_str().into());
        }
    });

    // Extract/validate the firmware. Called after the cable test; the worker
    // replies with FlashPrepared, which drives the ARB/preloader dialogs.
    let fl = Rc::clone(&models.flash);
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_prepare_flash(move || {
        if let Some(ui) = w.upgrade() {
            let is_source = ui.get_pending_source_flash();
            let path = if is_source {
                ui.get_source_dir()
            } else {
                ui.get_firmware_path()
            }
            .to_string();
            if path.is_empty() {
                add_log_m(&fl, &ui, &LogLevel::Error, "No firmware selected");
                return;
            }
            // The flash-method dialog guarantees a choice, but never start
            // flashing without one even if the UI state is inconsistent.
            let method = match ui.get_flash_method() {
                1 => FlashMethod::Snapdragon,
                2 => FlashMethod::Mtk,
                _ => {
                    add_log_m(
                        &fl,
                        &ui,
                        &LogLevel::Error,
                        "Flash method not selected — choose Snapdragon or MediaTek first",
                    );
                    return;
                }
            };
            ui.set_prepared_dir("".into());
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status("Preparing...".into());
            t.send(Cmd::PrepareFlash {
                path,
                is_source,
                method,
            })
            .ok();
        }
    });

    // Start the actual flash. Only reachable from the final-warning dialogs,
    // after the method choice and the ARB/preloader confirmations.
    let fl = Rc::clone(&models.flash);
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_start_flash(move || {
        if let Some(ui) = w.upgrade() {
            let dir = ui.get_prepared_dir().to_string();
            if dir.is_empty() {
                add_log_m(
                    &fl,
                    &ui,
                    &LogLevel::Error,
                    "Firmware not prepared — please restart the flash flow",
                );
                return;
            }
            let method = match ui.get_flash_method() {
                1 => FlashMethod::Snapdragon,
                2 => FlashMethod::Mtk,
                _ => {
                    add_log_m(
                        &fl,
                        &ui,
                        &LogLevel::Error,
                        "Flash method not selected — flashing refused",
                    );
                    return;
                }
            };
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status("Starting...".into());
            t.send(Cmd::StartFlash {
                dir,
                is_source: ui.get_pending_source_flash(),
                method,
                skip_xbl_abl: ui.get_skip_xbl_abl(),
                skip_preloader: ui.get_skip_preloader(),
                skip_partitions: if ui.get_show_skip_partitions() {
                    ui.get_skip_partitions().to_string()
                } else {
                    String::new()
                },
            })
            .ok();
        }
    });

    let fl = Rc::clone(&models.flash);
    let w = ui.as_weak();
    ui.on_start_flash_from_source(move || {
        if let Some(ui) = w.upgrade() {
            let dir = ui.get_source_dir().to_string();
            if dir.is_empty() {
                add_log_m(&fl, &ui, &LogLevel::Error, "No source directory selected");
                return;
            }
            ui.set_pending_source_flash(true);
            // Method choice is mandatory — reset and ask again for each flow.
            ui.set_flash_method(0);
            ui.set_reboot_choice(0);
            ui.set_confirm_action(confirm_action::FLASH_METHOD);
            ui.set_show_confirm(true);
        }
    });

    let fl = Rc::clone(&models.flash);
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_retry_flash(move || {
        if let Some(ui) = w.upgrade() {
            let failed_str = ui.get_flash_failed_partitions().to_string();
            let failed: Vec<String> = failed_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if failed.is_empty() {
                add_log_m(&fl, &ui, &LogLevel::Error, "No failed partitions to retry");
                return;
            }
            ui.set_show_confirm(false);
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status(format!("Retrying {} partition(s)...", failed.len()).into());
            t.send(Cmd::RetryFlash {
                failed_partitions: failed,
            })
            .ok();
        }
    });

    // Reboot must go through the worker — a blocking `adb reboot` here would
    // freeze the UI thread for as long as adb takes to respond.
    let dv = Rc::clone(&models.device);
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_reboot_device(move || {
        if let Some(ui) = w.upgrade() {
            add_log_m(&dv, &ui, &LogLevel::Info, "Rebooting device...");
        }
        t.send(Cmd::RebootTo("adb-reboot".into())).ok();
    });

    let t = ctx.clone();
    ui.on_reboot_to(move |target| {
        t.send(Cmd::RebootTo(target.to_string())).ok();
    });

    let w = ui.as_weak();
    ui.on_set_scale(move |scale| {
        save_scale(scale);
        if let Some(ui) = w.upgrade() {
            ui.set_pending_scale(scale);
        }
    });

    let w = ui.as_weak();
    ui.on_apply_pending_scale(move || {
        if let Some(ui) = w.upgrade() {
            let s = ui.get_pending_scale();
            if s > 0.0 {
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                        scale_factor: s,
                    });
                ui.set_pending_scale(0.0);
                ui.set_effect_opacity(0.0);
            }
        }
    });

    ui.on_save_lang(|l| {
        let mut config = load_config();
        config.lang = l.to_string();
        save_config(&config);
    });

    ui.on_save_theme(|d| {
        let mut config = load_config();
        config.theme = if d {
            "dark".to_string()
        } else {
            "light".to_string()
        };
        save_config(&config);
    });

    let fl = Rc::clone(&models.flash);
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_reboot_for_flash(move || {
        if let Some(ui) = w.upgrade() {
            let reboot_choice = ui.get_reboot_choice() as u8;
            ui.set_show_confirm(false);
            add_log_m(&fl, &ui, &LogLevel::Info, "Rebooting to fastbootd...");
            t.send(Cmd::RebootForFlash { reboot_choice }).ok();
        }
    });

    let t = ctx.clone();
    ui.on_run_cable_test(move || {
        t.send(Cmd::CableTest).ok();
    });

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_start_single_flash(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_pending_source_flash(false);
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status("Starting...".into());
            let path = ui.get_single_image_path().to_string();
            let part = ui.get_partition_name().to_string();
            let reboot_choice = ui.get_reboot_choice() as u8;
            t.send(Cmd::FlashSingle {
                path,
                partition: if part.is_empty() { None } else { Some(part) },
                reboot_choice,
            })
            .ok();
        }
    });

    let t = ctx.clone();
    ui.on_cancel_download(move || {
        t.send(Cmd::CancelDownload).ok();
    });

    let fc_cancel = flash_cancel.clone();
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_cancel_flash(move || {
        fc_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        t.send(Cmd::CancelFlash).ok();
        if let Some(ui) = w.upgrade() {
            ui.set_pending_source_flash(false);
            ui.set_is_flashing(false);
            ui.set_flash_status("Cancelled".into());
        }
    });

    let fr = fail_resp.clone();
    ui.on_flash_fail_skip(move || {
        if let Some(tx) = fr.borrow_mut().take() {
            tx.send(lfff_lib::flasher::FailureAction::Skip).ok();
        }
    });

    let fr = fail_resp.clone();
    ui.on_flash_fail_abort(move || {
        if let Some(tx) = fr.borrow_mut().take() {
            tx.send(lfff_lib::flasher::FailureAction::Abort).ok();
        }
    });

    let fr = fail_resp.clone();
    ui.on_flash_fail_retry(move || {
        if let Some(tx) = fr.borrow_mut().take() {
            tx.send(lfff_lib::flasher::FailureAction::Retry).ok();
        }
    });

    let fb = fastbootd_resp.clone();
    ui.on_fastbootd_fallback_continue(move || {
        if let Some(tx) = fb.borrow_mut().take() {
            tx.send(true).ok();
        }
    });

    let fb = fastbootd_resp.clone();
    ui.on_fastbootd_fallback_stop(move || {
        if let Some(tx) = fb.borrow_mut().take() {
            tx.send(false).ok();
        }
    });

    let t = ctx.clone();
    ui.on_check_deps(move || {
        t.send(Cmd::CheckDeps).ok();
    });

    let t = ctx.clone();
    ui.on_install_deps(move || {
        t.send(Cmd::InstallDeps).ok();
    });

    let t = ctx.clone();
    ui.on_run_driver_test(move || {
        t.send(Cmd::DriverTest).ok();
    });

    let t = ctx.clone();
    ui.on_get_device_info(move || {
        t.send(Cmd::CheckDevice).ok();
    });

    let t = ctx.clone();
    ui.on_post_flash_reboot(move || {
        t.send(Cmd::PostFlashReboot).ok();
    });

    let t = ctx.clone();
    ui.on_post_flash_wipe(move || {
        t.send(Cmd::PostFlashWipe).ok();
    });

    let t = ctx.clone();
    ui.on_download_firmware(move |u| {
        t.send(Cmd::Download { url: u.to_string() }).ok();
    });

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_extract_firmware(move || {
        if let Some(ui) = w.upgrade() {
            let p = ui.get_firmware_path().to_string();
            if !p.is_empty() {
                t.send(Cmd::Extract { path: p }).ok();
            }
        }
    });

    let w = ui.as_weak();
    ui.on_paste_clipboard(move || {
        if let Some(ui) = w.upgrade() {
            let text = arboard::Clipboard::new()
                .ok()
                .and_then(|mut c| c.get_text().ok())
                .or_else(|| {
                    std::process::Command::new("wl-paste")
                        .arg("-n")
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                })
                .or_else(|| {
                    std::process::Command::new("xclip")
                        .args(["-selection", "clipboard", "-o"])
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                });
            if let Some(t) = text {
                let trimmed = t.trim().to_string();
                if !trimmed.is_empty() {
                    ui.set_download_url(trimmed.into());
                }
            }
        }
    });

    ui.on_open_url(move |url| {
        let u = url.to_string();
        std::thread::spawn(move || {
            #[cfg(target_os = "linux")]
            {
                let _ = std::process::Command::new("xdg-open").arg(&u).spawn();
            }
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("open").arg(&u).spawn();
            }
        });
    });

    let w = ui.as_weak();
    ui.on_request_back(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_page(0);
        }
    });

    let t = ctx.clone();
    ui.on_set_active_slot(move |slot| {
        t.send(Cmd::SetActiveSlot {
            slot: slot.to_string(),
        })
        .ok();
    });

    let t = ctx.clone();
    ui.on_check_for_updates(move || {
        t.send(Cmd::CheckForUpdates).ok();
    });

    let w = ui.as_weak();
    ui.on_open_update_url(move || {
        if let Some(ui) = w.upgrade() {
            let url = ui.get_update_url().to_string();
            ui.set_show_update_dialog(false);
            std::thread::spawn(move || {
                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                }
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open").arg(&url).spawn();
                }
            });
        }
    });

    let fl = Rc::clone(&models.flash);
    let w_flash = ui.as_weak();
    ui.on_export_flash_log(move || {
        if let Some(path) = export_log(&fl, "flash")
            && let Some(ui) = w_flash.upgrade()
        {
            ui.set_flash_export_status(path.into());
        }
    });

    let pl = Rc::clone(&models.partition);
    let w_part = ui.as_weak();
    ui.on_export_partition_log(move || {
        if let Some(path) = export_log(&pl, "partition")
            && let Some(ui) = w_part.upgrade()
        {
            ui.set_partition_export_status(path.into());
        }
    });

    let dl = Rc::clone(&models.device);
    let w_dev = ui.as_weak();
    ui.on_export_device_log(move || {
        if let Some(path) = export_log(&dl, "device")
            && let Some(ui) = w_dev.upgrade()
        {
            ui.set_device_export_status(path.into());
        }
    });

    let dl2 = Rc::clone(&models.download);
    let w_dl = ui.as_weak();
    ui.on_export_download_log(move || {
        if let Some(path) = export_log(&dl2, "download")
            && let Some(ui) = w_dl.upgrade()
        {
            ui.set_download_export_status(path.into());
        }
    });
}
