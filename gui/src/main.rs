// src/main.rs — LFFF GUI entry point

mod config;
mod log_models;
mod worker;
mod ui_callbacks;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use slint::ComponentHandle;

slint::include_modules!();

#[derive(Debug, Clone)]
enum LogLevel { Info, Warn, Error, Success }

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
}

/// Flash method — always an explicit user choice, never auto-detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashMethod { Snapdragon, Mtk }

#[derive(Debug, Clone)]
enum WMsg {
    Log { level: LogLevel, message: String, tab: u8 },
    Progress { fraction: f32, partition: String },
    DeviceDetected { name: String, serial: String, slot: String, is_fastboot_mode: bool },
    DeviceDisconnected,
    FlashComplete { success: bool, message: String, log_summary: String, failed_partitions: Vec<String> },
    DepsResult { message: String, ok: bool },
    Flashing(bool),
    FwPath(String),
    DlProgress { percent: f32, speed: String, eta: String, downloaded: String, total: String, raw_line: String },
    Downloading(bool),
    TestStep { step: i32, status: String },
    /// Firmware is extracted/validated; GUI must now run the warning dialogs
    /// (ARB for Snapdragon, preloader for MediaTek) before the final confirm.
    FlashPrepared { dir: String, is_source: bool, arb_version: i32, has_preloader: bool },
    ReadyToFlash,
    CableTestProgress { step: u8, total: u8, status: String },
    FlashFailure { partition: String, slot: String, error: String, response: std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction> },
    UpdateAvailable { version: String, url: String, body: String },
}

#[derive(Debug)]
enum Cmd {
    CheckDevice,
    /// Extract (if needed) and validate firmware; reply with FlashPrepared.
    PrepareFlash { path: String, is_source: bool, method: FlashMethod },
    /// Run the actual flash after all confirmations. `dir` is the prepared
    /// (already extracted) firmware directory.
    StartFlash { dir: String, is_source: bool, method: FlashMethod, skip_xbl_abl: bool, skip_preloader: bool, skip_partitions: String },
    FlashSingle { path: String, partition: Option<String>, reboot_choice: u8 },
    CancelFlash, CheckDeps, InstallDeps, Download { url: String }, Extract { path: String },
    DriverTest, RebootTo(String), CancelDownload,
    PostFlashReboot, PostFlashWipe,
    SetActiveSlot { slot: String },
    RebootForFlash { reboot_choice: u8 },
    CableTest,
    RetryFlash { failed_partitions: Vec<String> },
    CheckForUpdates,
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
        unsafe { std::env::set_var("SLINT_RENDERER", "software"); }
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
                if loader_paths.iter().any(|p| std::path::Path::new(p).exists()) {
                    std::process::Command::new("vulkaninfo")
                        .arg("--summary")
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
                } else { false }
            }
            #[cfg(target_os = "windows")]
            {
                if std::path::Path::new(r"C:\Windows\System32\vulkan-1.dll").exists() {
                    std::process::Command::new("vulkaninfo")
                        .arg("--summary")
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(true)
                } else { false }
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            false
        };

        if vulkan_ok {
            unsafe { std::env::set_var("SLINT_RENDERER", "skia-vulkan"); }
            log::info!("Renderer: skia-vulkan (detected)");
            return;
        }
    }

    log::info!("Renderer: skia-opengl (default)");
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    select_renderer();

    #[cfg(target_os = "linux")]
    unsafe { std::env::set_var("LFFF_SUDO_CMD", "pkexec"); }

    let config = config::load_config();
    unsafe { std::env::set_var("SLINT_SCALE_FACTOR", config.scale.to_string()); }

    let ui = MainWindow::new()?;
    ui.set_ui_scale(config.scale);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_lang(config.lang.as_str().into());
    ui.set_is_dark(config.theme == "dark");

    let (ctx, crx) = mpsc::channel::<Cmd>();
    let (mtx, mrx) = mpsc::channel::<WMsg>();
    let flash_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flash_cancel_w = flash_cancel.clone();
    let fail_resp = std::rc::Rc::new(std::cell::RefCell::new(None));

    thread::spawn(move || worker::worker(crx, mtx, flash_cancel_w));

    let models = log_models::LogModels::new();
    models.attach(&ui);
    ui.set_output_dir(config::get_output_dir().display().to_string().as_str().into());

    models.refresh_history();

    ui_callbacks::register_callbacks(&ui, &ctx, &models, &fail_resp, &flash_cancel);

    ctx.send(Cmd::CheckForUpdates).ok();

    let w = ui.as_weak();
    let timer = slint::Timer::default();
    let mut last_dl_pct: u32 = 0;
    let fr_poll = fail_resp.clone();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        ui_callbacks::poll(&w, &mrx, &mut last_dl_pct, &models, &fr_poll);
    });

    ui.run()
}
