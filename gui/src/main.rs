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

/// Values of the `confirm-action` property — must match the dialog switch in
/// `ui/main.slint` (see the comment next to the property declaration there).
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
    #[cfg(unix)]
    {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&epoch, &mut tm); }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
    #[cfg(not(unix))]
    {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        format!("{:02}:{:02}:{:02}", (n / 3600) % 24, (n / 60) % 60, n % 60)
    }
}

fn lvl(l: &LogLevel) -> &'static str {
    match l {
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
        LogLevel::Success => "success",
    }
}

/// Run a closure while capturing its stdout lines and forwarding them to the GUI log.
fn with_captured_stdout<F: FnOnce()>(tx: &mpsc::Sender<WMsg>, tab: u8, f: F) {
    unsafe {
        let mut fds = [-1i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 { f(); return; }
        let rd_fd = fds[0];
        let wr_fd = fds[1];
        let saved = libc::dup(libc::STDOUT_FILENO);
        if saved < 0 {
            libc::close(rd_fd);
            libc::close(wr_fd);
            f();
            return;
        }
        if libc::dup2(wr_fd, libc::STDOUT_FILENO) < 0 {
            libc::close(rd_fd);
            libc::close(wr_fd);
            libc::close(saved);
            f();
            return;
        }
        libc::close(wr_fd);

        struct StdoutGuard { saved: libc::c_int }
        impl Drop for StdoutGuard {
            fn drop(&mut self) {
                unsafe {
                    libc::dup2(self.saved, libc::STDOUT_FILENO);
                    libc::close(self.saved);
                }
            }
        }
        let _guard = StdoutGuard { saved };

        let tx2 = tx.clone();
        let reader = thread::spawn(move || {
            use std::io::Read;
            use std::os::unix::io::FromRawFd;
            let mut rd = std::fs::File::from_raw_fd(rd_fd);
            let mut buf = [0u8; 4096];
            let mut partial = String::new();
            while let Ok(n) = rd.read(&mut buf) {
                if n == 0 { break; }
                partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(pos) = partial.find('\n') {
                    let line = partial[..pos].trim().to_string();
                    if !line.is_empty() {
                        tx2.send(WMsg::Log { level: LogLevel::Info, message: line, tab }).ok();
                    }
                    partial = partial[pos + 1..].to_string();
                }
            }
            let last = partial.trim().to_string();
            if !last.is_empty() {
                tx2.send(WMsg::Log { level: LogLevel::Info, message: last, tab }).ok();
            }
        });

        f();
        drop(_guard);
        reader.join().ok();
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
