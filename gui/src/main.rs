// src/main.rs — LFFF GUI entry point

mod config;
mod device_poll;
mod env_path;
mod log_models;
mod ota;
mod ui_callbacks;
mod worker;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use slint::ComponentHandle;

slint::include_modules!();

#[derive(Debug, Clone)]
enum LogLevel {
    Info,
    Warn,
    Error,
    Success,
}

/// Values of the `confirm-action` property — mirror of the ConfirmAction
/// global in `ui/globals/confirm-action.slint`; keep both in sync.
/// Only the values set from Rust are listed.
mod confirm_action {
    pub const FLASH_ALL_FINAL: i32 = 4;
    pub const SUCCESS: i32 = 7;
    pub const SOURCE_FINAL: i32 = 8;
    pub const CABLE_TEST: i32 = 9;
    pub const FLASH_FAILURE: i32 = 10;
    pub const SESSION_ERROR: i32 = 11;
    pub const FLASH_METHOD: i32 = 12;
    pub const FASTBOOTD_FALLBACK: i32 = 13;
    pub const CLOSE_FLASHING: i32 = 15;
}

/// Flash method — always an explicit user choice, never auto-detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashMethod {
    /// Everything from fastbootd: the phone switches to the other slot and
    /// the modem goes there with the system. No bootloader, which some
    /// phones cannot reach.
    Snapdragon,
    Mtk,
    /// The legacy Snapdragon way: the modem through the bootloader. Not
    /// recommended; kept as an explicit choice.
    SnapdragonBootloader,
}

impl FlashMethod {
    /// From the flash-method dialog's value; 0 (no choice) and anything
    /// unknown give `None`.
    fn from_choice(v: i32) -> Option<Self> {
        match v {
            1 => Some(Self::Snapdragon),
            2 => Some(Self::Mtk),
            3 => Some(Self::SnapdragonBootloader),
            _ => None,
        }
    }

    fn is_snapdragon(self) -> bool {
        self != Self::Mtk
    }
}

#[derive(Debug, Clone)]
enum WMsg {
    Log {
        level: LogLevel,
        message: String,
        tab: u8,
    },
    Progress {
        fraction: f32,
        partition: String,
    },
    DeviceDetected(device_poll::DeviceSnapshot),
    DeviceDisconnected,
    FlashComplete {
        success: bool,
        message: String,
        log_summary: String,
        failed_partitions: Vec<String>,
    },
    DepsResult {
        message: String,
        ok: bool,
        /// Tools still not usable after the check or the install.
        missing: Vec<String>,
    },
    Flashing(bool),
    FwPath(String),
    DlProgress {
        percent: f32,
        speed: String,
        eta: String,
        downloaded: String,
        total: String,
        raw_line: String,
    },
    Downloading(bool),
    TestStep {
        step: i32,
        status: String,
    },
    /// Firmware is extracted/validated; GUI must now run the warning dialogs
    /// (ARB for Snapdragon, preloader for MediaTek) before the final confirm.
    FlashPrepared {
        dir: String,
        is_source: bool,
        /// `None` when no ARB warning applies (MediaTek, or a source build).
        arb: Option<lfff_lib::arb::ArbVerdict>,
        /// ARB installed on the phone, when it could be read (adb, root).
        phone_arb: Option<u32>,
        has_preloader: bool,
    },
    /// The phone's ARB for the install sheet, or why it could not be read.
    PhoneArb(Result<u32, lfff_lib::arb::DeviceArbError>),
    ReadyToFlash,
    CableTestProgress {
        step: u8,
        total: u8,
        status: String,
    },
    FlashFailure {
        partition: String,
        slot: String,
        error: String,
        response: std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction>,
    },
    /// The phone is not confirmed as fastbootd; the UI words the reason in
    /// its own language.
    FastbootdFallback {
        status: lfff_lib::flasher::FastbootdStatus,
        response: std::sync::mpsc::Sender<bool>,
    },
    UpdateAvailable {
        version: String,
        url: String,
        body: String,
    },
    /// Unpacking a firmware archive: images written so far, and the latest.
    Unpacking {
        extracted: usize,
        current: String,
    },
    /// The chosen firmware is unpacked and checked. Sent before any device
    /// is involved, so every warning can be shown before the install starts.
    FirmwareReady {
        dir: String,
        name: String,
        parts: usize,
        /// Read whenever the firmware has an xbl_config; whether to warn
        /// about it depends on the chip, which the UI knows.
        arb: lfff_lib::arb::ArbVerdict,
        has_preloader: bool,
        cached: bool,
        /// Device codenames the OTA is built for; empty when it does not say.
        devices: Vec<String>,
        /// The super layout a rebuild would write, in plain words; empty when
        /// the firmware describes none.
        super_plan: String,
    },
    FirmwareFailed(String),
    /// Install progress. stage: 0 prepare the phone, 1 cable check,
    /// 2 install, 3 finish. state: 1 running, 2 done, 3 failed.
    Stage {
        stage: u8,
        state: u8,
        detail: String,
    },
    /// The cable check failed; the run waits for retry (true) or stop.
    CableFailed {
        detail: String,
        response: std::sync::mpsc::Sender<bool>,
    },
    /// The install stopped before anything was written to the phone.
    InstallStopped(String),
    /// A phone is on adb but has not allowed USB debugging from this computer.
    DeviceUnauthorized,
    /// The OTA repair finished: Ok(partitions removed) or why it stopped.
    RepairDone(Result<usize, String>),
    /// Official firmware search (`ota`); `generation` drops stale answers.
    OtaProgress {
        generation: u64,
        done: usize,
        total: usize,
    },
    OtaFound {
        generation: u64,
        result: Result<Vec<lfff_ota::Found>, String>,
    },
    OtaChanges {
        generation: u64,
        index: i32,
        text: String,
    },
}

#[derive(Debug)]
enum Cmd {
    CheckDevice,
    /// Extract (if needed) and validate firmware; reply with FlashPrepared.
    PrepareFlash {
        path: String,
        is_source: bool,
        method: FlashMethod,
    },
    /// Run the actual flash after all confirmations. `dir` is the prepared
    /// (already extracted) firmware directory.
    StartFlash {
        dir: String,
        is_source: bool,
        method: FlashMethod,
        skip_xbl_abl: bool,
        skip_preloader: bool,
        skip_partitions: String,
    },
    FlashSingle {
        path: String,
        partition: Option<String>,
        reboot_choice: u8,
    },
    CancelFlash,
    CheckDeps,
    InstallDeps,
    Download {
        url: String,
    },
    Extract {
        path: String,
    },
    DriverTest,
    RebootTo(String),
    CancelDownload,
    PostFlashReboot,
    PostFlashWipe,
    SetActiveSlot {
        slot: String,
    },
    RebootForFlash {
        reboot_choice: u8,
    },
    CableTest,
    RetryFlash {
        failed_partitions: Vec<String>,
    },
    CheckForUpdates,
    /// Unpack (or reuse an earlier unpack of) and check a firmware, on a
    /// thread of its own so the phone can be found meanwhile.
    PrepareFirmware {
        path: String,
        /// The app downloaded the archive itself, so it may delete it once
        /// unpacked.
        downloaded: bool,
    },
    /// Read the ARB installed on the phone (adb, root), for the install sheet.
    ReadPhoneArb,
    /// Restart into fastbootd, delete the inactive slot's partitions so the
    /// next OTA installs (lfff_lib::flasher::repair_ota_slots), restart.
    RepairOta,
    /// The whole install after the confirmations: restart the phone into
    /// fastbootd, check the cable, run the safety checks, flash.
    RunInstall {
        dir: String,
        method: FlashMethod,
        skip_xbl_abl: bool,
        skip_preloader: bool,
        /// Comma-separated partitions to leave as they are.
        skip_partitions: String,
    },
}

fn ts() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn lvl(l: &LogLevel) -> &'static str {
    match l {
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
        LogLevel::Success => "success",
    }
}

fn select_renderer() {
    if let Ok(renderer) = std::env::var("SLINT_RENDERER") {
        log::info!("Renderer: {} (from env)", renderer);
        return;
    }

    #[cfg(target_os = "linux")]
    if std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err() {
        unsafe {
            std::env::set_var("SLINT_RENDERER", "software");
        }
        log::info!("Renderer: software (no display server)");
        return;
    }

    #[cfg(feature = "vulkan")]
    {
        let vulkan_ok = {
            #[cfg(target_os = "linux")]
            {
                let loader_paths = [
                    "/usr/lib/libvulkan.so.1",
                    "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
                    "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
                    "/lib/libvulkan.so.1",
                ];
                if loader_paths
                    .iter()
                    .any(|p| std::path::Path::new(p).exists())
                {
                    std::process::Command::new("vulkaninfo")
                        .arg("--summary")
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            #[cfg(target_os = "windows")]
            {
                if std::path::Path::new(r"C:\Windows\System32\vulkan-1.dll").exists() {
                    std::process::Command::new("vulkaninfo")
                        .arg("--summary")
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(true)
                } else {
                    false
                }
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            false
        };

        if vulkan_ok {
            unsafe {
                std::env::set_var("SLINT_RENDERER", "skia-vulkan");
            }
            log::info!("Renderer: skia-vulkan (detected)");
            return;
        }
    }

    log::info!("Renderer: skia-opengl (default)");
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    // Before anything looks for fastboot or adb, and while still single-threaded.
    #[cfg(target_os = "macos")]
    env_path::inherit_shell_path();
    select_renderer();

    #[cfg(target_os = "linux")]
    unsafe {
        std::env::set_var("LFFF_SUDO_CMD", "pkexec");
    }

    let config = config::load_config();
    unsafe {
        std::env::set_var("SLINT_SCALE_FACTOR", config.scale.to_string());
    }

    let ui = MainWindow::new()?;
    // The name of the desktop file: without an app id the window is
    // anonymous on Wayland, and docks and window pickers cannot tie it to its
    // icon. Flatpak names that file after its app id and passes it in.
    #[cfg(target_os = "linux")]
    // After the component: before it, no Slint platform exists to take it.
    slint::set_xdg_app_id(std::env::var("FLATPAK_ID").unwrap_or_else(|_| "lfff-gui".into()))?;
    ui.set_ui_scale(config.scale);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_lang(config.lang.as_str().into());
    ui.set_is_dark(config.theme == "dark");
    ui.set_scheme(config.scheme);
    ui.set_translucent(config.translucent);

    let (ctx, crx) = mpsc::channel::<Cmd>();
    let (mtx, mrx) = mpsc::channel::<WMsg>();
    let flash_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flash_cancel_w = flash_cancel.clone();
    let fail_resp = std::rc::Rc::new(std::cell::RefCell::new(None));
    let fastbootd_resp = std::rc::Rc::new(std::cell::RefCell::new(None));
    let cable_resp = std::rc::Rc::new(std::cell::RefCell::new(None));

    // Set by the worker for the length of each command; the device poller
    // stays quiet meanwhile. `current_device` is the poller's last reading.
    let worker_busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let current_device = std::sync::Arc::new(std::sync::Mutex::new(None));
    device_poll::spawn(mtx.clone(), worker_busy.clone(), current_device.clone());
    let ota_tx = mtx.clone();
    thread::spawn(move || worker::worker(crx, mtx, flash_cancel_w, worker_busy, current_device));

    let models = log_models::LogModels::new();
    models.attach(&ui);
    ui.set_output_dir(
        config::get_output_dir()
            .display()
            .to_string()
            .as_str()
            .into(),
    );

    models.refresh_history();

    ota::register(&ui, &ota_tx, &ctx);
    ui_callbacks::register_callbacks(
        &ui,
        &ctx,
        &models,
        &fail_resp,
        &fastbootd_resp,
        &flash_cancel,
        &cable_resp,
    );

    ui.set_classic_ui(config.classic_ui);
    ui.set_show_notice(!config.notice_accepted);
    ui.set_sound(config.sound);
    ui.set_rebuild_super(config.rebuild_super);
    ui.set_modem_bootloader(config.modem_bootloader);
    ui.set_delete_picked_zip(config.delete_picked_zip);
    ui_callbacks::apply_pins(&ui, &config.pinned_tools);
    ui.set_chips_summary(ui_callbacks::chips_summary(&config).into());
    ui.set_cache_summary(ui_callbacks::cache_summary().into());
    ui_callbacks::refresh_cached_firmwares(&ui);

    // Closing mid-flash leaves the phone half-written: ask first.
    let w_close = ui.as_weak();
    ui.window().on_close_requested(move || {
        let Some(ui) = w_close.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        if ui.get_is_flashing() {
            ui.set_confirm_action(confirm_action::CLOSE_FLASHING);
            ui.set_show_confirm(true);
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });
    ui.on_quit_app(|| {
        slint::quit_event_loop().ok();
    });

    ctx.send(Cmd::CheckForUpdates).ok();
    // The install page's first step; also refreshes the classic deps log.
    ctx.send(Cmd::CheckDeps).ok();

    let w = ui.as_weak();
    let timer = slint::Timer::default();
    let mut last_dl_pct: u32 = 0;
    let replies = ui_callbacks::Replies {
        fail: fail_resp.clone(),
        fastbootd: fastbootd_resp.clone(),
        cable: cable_resp.clone(),
    };
    let ctx_poll = ctx.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            ui_callbacks::poll(&w, &mrx, &mut last_dl_pct, &models, &replies, &ctx_poll);
        },
    );

    ui.run()
}
