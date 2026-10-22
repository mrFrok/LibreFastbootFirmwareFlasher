use slint::{ComponentHandle, VecModel, Weak};
use std::rc::Rc;
use std::sync::mpsc;

use crate::config::{get_output_dir, load_config, save_config, save_scale, set_output_dir};
use crate::log_models::{LogModels, add_log_m};
use crate::{
    ArbState, Cmd, DeviceInfo, FlashMethod, LogEntry, LogLevel, MainWindow, WMsg, confirm_action,
};
use lfff_lib::arb::ArbVerdict;

thread_local! {
    /// When the current install started, for the time on the result pages.
    static INSTALL_START: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
    /// Codenames the chosen firmware is built for.
    static FW_DEVICES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The connected phone's codename, when Android reported it.
    static PHONE_CODENAME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Warn when the firmware names the devices it is for and the phone's
/// codename is none of them. Silent when either side is unknown: fastboot
/// alone gives only the platform, and custom builds often name no device.
fn update_wrong_device(ui: &MainWindow) {
    let wanted = FW_DEVICES.with(|d| d.borrow().clone());
    let phone = PHONE_CODENAME.with(|p| p.borrow().clone());
    let wrong = match phone {
        Some(p) if !wanted.is_empty() && !wanted.iter().any(|w| w.eq_ignore_ascii_case(&p)) => {
            wanted.join(", ")
        }
        _ => String::new(),
    };
    ui.set_fw_wrong_device(wrong.into());
}

/// A desktop notification with a sound, for an install the user may have
/// walked away from. Best effort, off the UI thread. Off in Settings means
/// neither.
fn notify_done(title: String, body: String, ok: bool) {
    if !load_config().sound {
        return;
    }
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        {
            let script = format!(
                "display notification {:?} with title {:?} sound name {:?}",
                body,
                title,
                if ok { "Glass" } else { "Basso" }
            );
            let _ = std::process::Command::new("osascript")
                .args(["-e", &script])
                .status();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = std::process::Command::new("notify-send")
                .args([
                    "--app-name=Libre Fastboot Firmware Flasher",
                    "--icon=lfff-gui",
                ])
                .arg(&title)
                .arg(&body)
                .status();
            let sound = if ok { "complete" } else { "dialog-error" };
            let played = std::process::Command::new("canberra-gtk-play")
                .args(["-i", sound])
                .status()
                .is_ok_and(|s| s.success());
            if !played {
                let file = format!("/usr/share/sounds/freedesktop/stereo/{sound}.oga");
                for player in ["paplay", "pw-play"] {
                    if std::process::Command::new(player)
                        .arg(&file)
                        .status()
                        .is_ok_and(|s| s.success())
                    {
                        break;
                    }
                }
            }
        }
    });
}

fn tr(lang: &str, en: &str, ru: &str, cn: &str, tw: &str) -> String {
    match lang {
        "ru" => ru,
        "zh-CN" => cn,
        "zh-TW" => tw,
        _ => en,
    }
    .to_string()
}

/// Pinned tool names in config.json, by Tools-page id - 1.
const TOOL_NAMES: [&str; 9] = [
    "partition",
    "build",
    "reboot",
    "slot",
    "connection",
    "programs",
    "download",
    "logs",
    "ota-repair",
];

pub fn apply_pins(ui: &MainWindow, pinned: &[String]) {
    let ids: Vec<i32> = pinned
        .iter()
        .filter_map(|n| TOOL_NAMES.iter().position(|t| t == n))
        .map(|i| i as i32 + 1)
        .collect();
    let flags: Vec<bool> = TOOL_NAMES
        .iter()
        .map(|t| pinned.iter().any(|p| p == t))
        .collect();
    ui.set_pinned_tools(Rc::new(VecModel::from(ids)).into());
    ui.set_pinned_flags(Rc::new(VecModel::from(flags)).into());
}

pub fn chips_summary(config: &crate::config::Config) -> String {
    config
        .chips
        .iter()
        .map(|(product, chip)| {
            let name = if chip == "mediatek" {
                "MediaTek"
            } else {
                "Snapdragon"
            };
            format!("{product} — {name}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fill the install page's list of firmware unpacked earlier.
pub fn refresh_cached_firmwares(ui: &MainWindow) {
    let names: Vec<slint::SharedString> =
        lfff_lib::extractor::finished_extractions(&get_output_dir())
            .iter()
            .map(|d| {
                d.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
                    .into()
            })
            .collect();
    ui.set_cached_firmwares(Rc::new(VecModel::from(names)).into());
}

pub fn cache_summary() -> String {
    let (count, bytes) = lfff_lib::extractor::extraction_cache(&get_output_dir());
    if count == 0 {
        String::new()
    } else {
        format!("{} · {:.1} GB", count, bytes as f64 / 1e9)
    }
}

/// Chip remembered for this codename: 0 none, 1 Snapdragon, 2 MediaTek.
fn chip_for(product: &str) -> i32 {
    match load_config().chips.get(product).map(String::as_str) {
        Some("snapdragon") => 1,
        Some("mediatek") => 2,
        _ => 0,
    }
}

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

type Reply<T> = std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<T>>>>;

/// Where the worker's blocking questions wait for the user's answer.
pub struct Replies {
    pub fail: Reply<lfff_lib::flasher::FailureAction>,
    pub fastbootd: Reply<bool>,
    pub cable: Reply<bool>,
}

pub fn poll(
    w: &Weak<MainWindow>,
    rx: &mpsc::Receiver<WMsg>,
    last_dl_pct: &mut u32,
    models: &LogModels,
    replies: &Replies,
    ctx: &mpsc::Sender<Cmd>,
) {
    let (fail_resp, fastbootd_resp, cable_resp) =
        (&replies.fail, &replies.fastbootd, &replies.cable);
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
            WMsg::DeviceDetected(d) => {
                ui.set_chip(chip_for(&d.product));
                ui.set_device_unauthorized(false);
                PHONE_CODENAME
                    .with(|p| *p.borrow_mut() = d.codename_known.then(|| d.product.clone()));
                update_wrong_device(&ui);
                ui.set_device(DeviceInfo {
                    connected: true,
                    name: d.name.into(),
                    serial: d.serial.into(),
                    slot: if d.slot.is_empty() {
                        "\u{2014}".into()
                    } else {
                        d.slot.into()
                    },
                    is_fastboot_mode: d.mode != crate::device_poll::Mode::System,
                    mode: d.mode.as_ui(),
                    unlocked: match d.unlocked {
                        Some(true) => 1,
                        Some(false) => 0,
                        None => -1,
                    },
                    product: d.product.into(),
                    battery: d.battery,
                });
            }
            WMsg::DeviceDisconnected | WMsg::DeviceUnauthorized => {
                ui.set_device_unauthorized(matches!(m, WMsg::DeviceUnauthorized));
                PHONE_CODENAME.with(|p| *p.borrow_mut() = None);
                update_wrong_device(&ui);
                ui.set_device(DeviceInfo {
                    connected: false,
                    name: "\u{2014}".into(),
                    serial: "\u{2014}".into(),
                    slot: "\u{2014}".into(),
                    is_fastboot_mode: false,
                    mode: 0,
                    unlocked: -1,
                    product: "".into(),
                    battery: -1,
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

                if !ui.get_classic_ui() && ui.get_install_view() == 1 {
                    // The new interface shows the result as a page, not a dialog.
                    if let Some(start) = INSTALL_START.with(|s| s.get()) {
                        let secs = start.elapsed().as_secs();
                        ui.set_install_minutes((secs / 60) as i32);
                        ui.set_install_seconds((secs % 60) as i32);
                    }
                    let lang = ui.get_lang();
                    if success {
                        ui.set_stage2(2);
                        ui.set_install_view(2);
                        notify_done(
                            tr(
                                &lang,
                                "Firmware installed",
                                "Прошивка установлена",
                                "固件已安装",
                                "韌體已安裝",
                            ),
                            tr(
                                &lang,
                                "Restart the phone in the app.",
                                "Перезагрузите смартфон в программе.",
                                "请在程序中重启手机。",
                                "請在程式中重新啟動手機。",
                            ),
                            true,
                        );
                    } else {
                        notify_done(
                            tr(
                                &lang,
                                "The installation did not finish",
                                "Установка не завершилась",
                                "安装未完成",
                                "安裝未完成",
                            ),
                            tr(
                                &lang,
                                "Don't restart the phone; see the app.",
                                "Не перезагружайте смартфон, посмотрите в программе.",
                                "请勿重启手机，请查看程序。",
                                "請勿重新啟動手機，請查看程式。",
                            ),
                            false,
                        );
                        ui.set_stage2(3);
                        ui.set_untouched(false);
                        ui.set_result_detail(message.into());
                        ui.set_flash_failed_partitions(failed_partitions.join(",").into());
                        ui.set_install_view(3);
                    }
                } else if success {
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
            WMsg::DepsResult {
                message,
                ok,
                missing,
            } => {
                ui.set_deps_state(if ok { 2 } else { 1 });
                ui.set_deps_missing(missing.join(", ").into());
                add_log(
                    models,
                    &ui,
                    if ok {
                        &LogLevel::Success
                    } else {
                        &LogLevel::Error
                    },
                    0,
                    &message,
                )
            }
            WMsg::Flashing(f) => ui.set_is_flashing(f),
            WMsg::Downloading(f) => {
                ui.set_is_downloading(f);
                if !ui.get_classic_ui() {
                    if f {
                        ui.set_fw_state(1);
                    } else if ui.get_fw_state() == 1 {
                        // Cancelled or failed: back to choosing.
                        ui.set_fw_state(0);
                    }
                }
            }
            WMsg::FwPath(p) => {
                ui.set_firmware_path(p.clone().into());
                // In the new interface a downloaded firmware goes straight on
                // to unpacking, like a picked one.
                if !ui.get_classic_ui() {
                    prepare(&ui, ctx, &p);
                }
            }
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
                arb,
                has_preloader,
            } => {
                ui.set_prepared_dir(dir.into());
                ui.set_is_flashing(false);
                // Warning order is fixed: ARB (Snapdragon) or preloader
                // (MediaTek) FIRST, the final confirmation dialog AFTER.
                let method = ui.get_flash_method(); // 1 = Snapdragon, 2 = MediaTek
                if method == 1 && !is_source {
                    // The worker always sends a verdict on this path; if it
                    // somehow did not, warn as for an unknown ARB, never as zero.
                    let (state, version, msg) = match arb.unwrap_or(ArbVerdict::Unparsed) {
                        ArbVerdict::Known(v) => (
                            ArbState::Known,
                            v as i32,
                            format!(
                                "ARB={} — flashing may permanently raise the anti-rollback counter. Confirm to continue.",
                                v
                            ),
                        ),
                        ArbVerdict::NotFound => (
                            ArbState::NotFound,
                            0,
                            "ARB unknown (xbl_config.img not found) — confirm to continue.".into(),
                        ),
                        ArbVerdict::Unparsed => (
                            ArbState::Unparsed,
                            0,
                            "ARB unknown (xbl_config.img could not be parsed) — confirm to continue."
                                .into(),
                        ),
                    };
                    add_log(models, &ui, &LogLevel::Warn, 2, &msg);
                    ui.set_arb_warning_state(state);
                    ui.set_arb_warning_version(version);
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
                    } else if ui.get_lang() == "zh-CN" {
                        "正在准备测试..."
                    } else if ui.get_lang() == "zh-TW" {
                        "正在準備測試..."
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
            WMsg::Unpacking { extracted, current } => {
                ui.set_fw_state(2);
                ui.set_unpack_count(extracted as i32);
                ui.set_unpack_current(current.into());
            }
            WMsg::FirmwareReady {
                dir,
                name,
                parts,
                arb,
                has_preloader,
                cached,
                devices,
            } => {
                FW_DEVICES.with(|d| *d.borrow_mut() = devices);
                update_wrong_device(&ui);
                ui.set_fw_dir(dir.into());
                ui.set_fw_name(name.into());
                ui.set_fw_parts(parts as i32);
                ui.set_fw_arb(match arb {
                    ArbVerdict::Known(v) => v as i32,
                    _ => -1,
                });
                ui.set_fw_has_preloader(has_preloader);
                ui.set_fw_state(if cached { 4 } else { 3 });
                ui.set_cache_summary(cache_summary().into());
                refresh_cached_firmwares(&ui);
            }
            WMsg::FirmwareFailed(e) if e == "Cancelled" => ui.set_fw_state(0),
            WMsg::FirmwareFailed(e) => {
                ui.set_fw_error(firmware_error_text(&ui.get_lang(), &e).into());
                ui.set_fw_state(5);
            }
            WMsg::Stage {
                stage,
                state,
                detail,
            } => match stage {
                0 => ui.set_stage0(state as i32),
                1 => {
                    ui.set_stage1(state as i32);
                    ui.set_stage1_detail(detail.into());
                    if state != 3 {
                        ui.set_cable_failed(false);
                    }
                }
                _ => ui.set_stage2(state as i32),
            },
            WMsg::CableFailed { detail, response } => {
                ui.set_cable_detail(detail.into());
                ui.set_cable_failed(true);
                *cable_resp.borrow_mut() = Some(response);
            }
            WMsg::RepairDone(r) => {
                ui.set_repair_running(false);
                match r {
                    Ok(n) => {
                        ui.set_repair_state(2);
                        ui.set_repair_count(n as i32);
                    }
                    Err(e) => {
                        ui.set_repair_state(3);
                        ui.set_repair_error(e.into());
                    }
                }
            }
            WMsg::InstallStopped(reason) => {
                let lang = ui.get_lang();
                notify_done(
                    tr(
                        &lang,
                        "The installation stopped",
                        "Установка остановлена",
                        "安装已停止",
                        "安裝已停止",
                    ),
                    tr(
                        &lang,
                        "Nothing on the phone was changed.",
                        "На смартфоне ничего не изменилось.",
                        "手机上没有任何改动。",
                        "手機上沒有任何變更。",
                    ),
                    false,
                );
                ui.set_untouched(true);
                ui.set_result_detail(reason.into());
                ui.set_install_view(3);
            }
            WMsg::UpdateAvailable { version, url, body } => {
                ui.set_update_version(version.into());
                ui.set_update_known(true);
                ui.set_update_url(url.into());
                ui.set_update_body(body.into());
                ui.set_show_update_dialog(true);
            }
        }
    }
}

/// The firmware errors a beginner meets, in the interface language; anything
/// else is shown as the tools reported it.
fn firmware_error_text(lang: &str, e: &str) -> String {
    let pick = |en: &str, ru: &str, cn: &str, tw: &str| match lang {
        "ru" => ru.to_string(),
        "zh-CN" => cn.to_string(),
        "zh-TW" => tw.to_string(),
        _ => en.to_string(),
    };
    if e.starts_with("The link gave a web page") {
        pick(
            e,
            "По ссылке пришла веб-страница, а не архив прошивки. Проверьте, что ссылка прямая и не устарела.",
            "链接返回的是网页，而不是固件压缩包。请检查链接是否为直链且未过期。",
            "連結回傳的是網頁，而不是韌體壓縮檔。請檢查連結是否為直連且未過期。",
        )
    } else if e.starts_with("This file is not a ZIP") {
        pick(
            e,
            "Этот файл не архив прошивки ZIP.",
            "此文件不是 ZIP 固件压缩包。",
            "此檔案不是 ZIP 韌體壓縮檔。",
        )
    } else if e.starts_with("No flashable .img") {
        pick(
            e,
            "В прошивке не нашлось образов разделов (.img).",
            "固件中没有找到分区镜像（.img）。",
            "韌體中沒有找到分割區映像（.img）。",
        )
    } else {
        e.to_string()
    }
}

/// Start unpacking/checking a firmware for the install page.
fn prepare(ui: &MainWindow, ctx: &mpsc::Sender<Cmd>, path: &str) {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    ui.set_fw_name(name.into());
    ui.set_fw_dir("".into());
    ui.set_unpack_count(0);
    ui.set_unpack_current("".into());
    ui.set_fw_state(2);
    ctx.send(Cmd::PrepareFirmware {
        path: path.to_string(),
    })
    .ok();
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
    cable_resp: &std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<bool>>>>,
) {
    register_install_callbacks(ui, ctx, cable_resp);

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
            }
        }
    });

    ui.on_save_lang(|l| {
        let mut config = load_config();
        config.lang = l.to_string();
        save_config(&config);
    });

    ui.on_save_scheme(|i| {
        let mut config = load_config();
        config.scheme = i;
        save_config(&config);
    });

    ui.on_save_translucent(|t| {
        let mut config = load_config();
        config.translucent = t;
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
            ui.set_page(if ui.get_classic_ui() { 0 } else { 11 });
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
            // The URL comes from a network response; xdg-open and open will
            // launch whatever handler a scheme maps to, so only the project's
            // own GitHub pages are opened.
            if !url.starts_with("https://github.com/mrFrok/LibreFastbootFirmwareFlasher/") {
                return;
            }
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

/// Callbacks of the new interface: install page, Tools pins, Settings.
fn register_install_callbacks(
    ui: &MainWindow,
    ctx: &mpsc::Sender<Cmd>,
    cable_resp: &std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<bool>>>>,
) {
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_pick_zip(move || {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Firmware", &["zip"])
            .set_directory(get_output_dir())
            .pick_file()
            && let Some(ui) = w.upgrade()
        {
            prepare(&ui, &t, &p.display().to_string());
        }
    });

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_pick_folder(move || {
        if let Some(p) = rfd::FileDialog::new()
            .set_directory(get_output_dir())
            .pick_folder()
            && let Some(ui) = w.upgrade()
        {
            prepare(&ui, &t, &p.display().to_string());
        }
    });

    ui.on_cancel_unpack(crate::worker::cancel_unpack);

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_repair_ota(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_repair_running(true);
            ui.set_repair_state(1);
        }
        t.send(Cmd::RepairOta).ok();
    });

    // Same order as refresh_cached_firmwares built the list in.
    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_pick_cached(move |i| {
        let dirs = lfff_lib::extractor::finished_extractions(&get_output_dir());
        if let (Some(d), Some(ui)) = (
            usize::try_from(i).ok().and_then(|i| dirs.get(i)),
            w.upgrade(),
        ) {
            prepare(&ui, &t, &d.display().to_string());
        }
    });

    let w = ui.as_weak();
    ui.on_change_firmware(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_fw_state(0);
            ui.set_fw_dir("".into());
        }
    });

    // 0 forgets the choice for this model, so the cards come back.
    let w = ui.as_weak();
    ui.on_choose_chip(move |c| {
        if let Some(ui) = w.upgrade() {
            let product = ui.get_device().product.to_string();
            if product.is_empty() {
                return;
            }
            match c {
                1 => crate::config::set_chip(&product, "snapdragon"),
                2 => crate::config::set_chip(&product, "mediatek"),
                _ => crate::config::remove_chip(&product),
            }
            ui.set_chip(c);
            ui.set_chips_summary(chips_summary(&load_config()).into());
        }
    });

    let t = ctx.clone();
    let w = ui.as_weak();
    ui.on_run_install(move || {
        if let Some(ui) = w.upgrade() {
            // The sheet only opens with a chip chosen; never start without one.
            let method = match ui.get_chip() {
                1 => FlashMethod::Snapdragon,
                2 => FlashMethod::Mtk,
                _ => return,
            };
            let dir = ui.get_fw_dir().to_string();
            if dir.is_empty() {
                return;
            }
            ui.set_stage0(1);
            ui.set_stage1(0);
            ui.set_stage2(0);
            ui.set_stage1_detail("".into());
            ui.set_cable_failed(false);
            ui.set_flash_progress(0.0);
            ui.set_current_partition("".into());
            ui.set_install_view(1);
            INSTALL_START.with(|s| s.set(Some(std::time::Instant::now())));
            t.send(Cmd::RunInstall {
                dir,
                method,
                skip_xbl_abl: ui.get_skip_xbl_abl(),
                skip_preloader: ui.get_skip_preloader(),
                skip_partitions: ui.get_install_skips().to_string(),
            })
            .ok();
        }
    });

    let cr = cable_resp.clone();
    let w = ui.as_weak();
    ui.on_cable_retry(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_cable_failed(false);
        }
        if let Some(tx) = cr.borrow_mut().take() {
            tx.send(true).ok();
        }
    });

    let cr = cable_resp.clone();
    ui.on_cable_stop(move || {
        if let Some(tx) = cr.borrow_mut().take() {
            tx.send(false).ok();
        }
    });

    let w = ui.as_weak();
    ui.on_install_finish(move || {
        if let Some(ui) = w.upgrade() {
            ui.set_install_view(0);
        }
    });

    // After a stop before flashing, back to the checklist; after a failure
    // partway, flash again only what failed.
    let w = ui.as_weak();
    ui.on_install_retry(move || {
        if let Some(ui) = w.upgrade() {
            if ui.get_untouched() || ui.get_flash_failed_partitions().is_empty() {
                ui.set_install_view(0);
            } else {
                ui.set_stage2(1);
                ui.set_install_view(1);
                ui.invoke_retry_flash();
            }
        }
    });

    ui.on_set_rebuild_super(|on| {
        let mut config = load_config();
        config.rebuild_super = on;
        save_config(&config);
    });

    ui.on_set_sound(|on| {
        let mut config = load_config();
        config.sound = on;
        save_config(&config);
    });

    ui.on_accept_notice(|| {
        let mut config = load_config();
        config.notice_accepted = true;
        save_config(&config);
    });

    let w = ui.as_weak();
    ui.on_set_classic(move |c| {
        let mut config = load_config();
        config.classic_ui = c;
        save_config(&config);
        if let Some(ui) = w.upgrade() {
            ui.set_classic_ui(c);
        }
    });

    let w = ui.as_weak();
    ui.on_toggle_pin(move |id| {
        let Some(name) = usize::try_from(id - 1).ok().and_then(|i| TOOL_NAMES.get(i)) else {
            return;
        };
        let mut config = load_config();
        if let Some(i) = config.pinned_tools.iter().position(|p| p == name) {
            config.pinned_tools.remove(i);
        } else {
            config.pinned_tools.push(name.to_string());
        }
        save_config(&config);
        if let Some(ui) = w.upgrade() {
            apply_pins(&ui, &config.pinned_tools);
        }
    });

    let w = ui.as_weak();
    ui.on_clear_cache(move || {
        if let Err(e) = lfff_lib::extractor::clear_extraction_cache(&get_output_dir()) {
            eprintln!("Failed to clear unpacked firmware: {}", e);
        }
        if let Some(ui) = w.upgrade() {
            ui.set_cache_summary(cache_summary().into());
            refresh_cached_firmwares(&ui);
            // The firmware on the install page may have been one of them.
            if ui.get_fw_state() == 3 || ui.get_fw_state() == 4 {
                ui.set_fw_state(0);
            }
        }
    });

    let w = ui.as_weak();
    ui.on_forget_chips(move || {
        crate::config::clear_chips();
        if let Some(ui) = w.upgrade() {
            ui.set_chip(0);
            ui.set_chips_summary("".into());
        }
    });
}
