// src/main.rs — LFFF GUI

use slint::Model;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

slint::include_modules!();

#[derive(Debug, Clone)]
enum LogLevel { Info, Warn, Error, Success }

#[derive(Debug, Clone)]
enum WMsg {
    Log { level: LogLevel, message: String, tab: u8 },
    Progress { fraction: f32, partition: String },
    DeviceDetected { name: String, serial: String, slot: String },
    DeviceDisconnected,
    FlashComplete { success: bool, message: String, log_summary: String, failed_partitions: Vec<String> },
    DepsResult { message: String, ok: bool },
    Flashing(bool),
    FwPath(String),
    DlProgress { percent: f32, speed: String, eta: String, downloaded: String, total: String, raw_line: String },
    Downloading(bool),
    TestStep { step: i32, status: String },
    ArbWarning { version: u32, as_mediatek: Option<bool> },
    ArbDeviceWarning { path: String, is_source: bool, device_arb: u32 },
    PreloaderWarning { path: String, is_source: bool },
    ReadyToFlash,  // device is in fastbootd, show cable test dialog
    CableTestProgress { step: u8, total: u8, status: String },
    FlashFailure { partition: String, slot: String, error: String, response: std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction> },
}

#[derive(Debug)]
enum Cmd {
    CheckDevice, Flash { path: String, skip_arb: bool, skip_partitions: String },
    FlashSingle { path: String, partition: Option<String>, reboot_choice: u8 },
    CancelFlash, CheckDeps, InstallDeps, Download { url: String }, Extract { path: String },
    DriverTest, RebootTo(String), CancelDownload,
    PostFlashReboot, PostFlashWipe,
    ConfirmArbAndFlash { path: String, skip_xbl_abl: bool, skip_partitions: String },
    ConfirmArbDeviceFlash { path: String, is_source: bool, skip_xbl_abl: bool, skip_partitions: String },
    ConfirmPreloaderFlash { path: String, is_source: bool, skip_preloader: bool, skip_partitions: String },
    RebootForFlash { reboot_choice: u8 },
    FlashFromSource { dir: String, skip_partitions: String },
    CableTest,
    RetryFlash { failed_partitions: Vec<String> },
}

fn ts() -> String {
    // Use local time via libc localtime_r to respect the user's timezone
    #[cfg(unix)]
    {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&epoch, &mut tm); }
        return format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
    }
    #[cfg(not(unix))]
    {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        format!("{:02}:{:02}:{:02}", (n/3600)%24, (n/60)%60, n%60)
    }
}
fn lvl(l: &LogLevel) -> &'static str { match l { LogLevel::Info=>"info", LogLevel::Warn=>"warn", LogLevel::Error=>"error", LogLevel::Success=>"success" } }
fn log(tx: &mpsc::Sender<WMsg>, l: LogLevel, tab: u8, m: impl Into<String>) { tx.send(WMsg::Log{level:l,message:m.into(),tab}).ok(); }

fn get_output_dir() -> PathBuf {
    let dir = load_config().output_dir
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("lfff")
                .join("firmwares")
        });
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn set_output_dir(path: &str) {
    let mut config = load_config();
    config.output_dir = Some(path.to_string());
    save_config(&config);
}

/// Poll for a fastboot device to appear, with progress logs. Returns true if found.
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
            log(tx, LogLevel::Info, tab, &format!("Still waiting... ({}/{}s)", i, timeout_secs));
        }
    }
    log(tx, LogLevel::Error, tab, "Timeout waiting for device in fastboot");
    false
}

fn do_flash(tx: &mpsc::Sender<WMsg>, source: &lfff_lib::flasher::FirmwareSource, serial: &Option<String>,
    skip_xbl_abl: bool, skip_preloader: bool, as_mediatek: Option<bool>, skip_partitions: String,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let sref = serial.as_deref();
    let tx_log = tx.clone();
    let tx_prog = tx.clone();
    let tx_fail = tx.clone();
    let session = lfff_lib::flasher::run_flash_session_with_log(
        source,
        sref,
        false,
        skip_xbl_abl,
        skip_preloader,
        as_mediatek,
        cancel,
        skip_partitions,
        &|msg| { tx_log.send(WMsg::Log{level:LogLevel::Info,message:msg,tab:2}).ok(); },
        &|p| {
            let fraction = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
            tx_prog.send(WMsg::Progress{fraction,partition:format!("{}_{}", p.partition, p.slot)}).ok();
        },
        &|partition, slot, error| {
            let (resp_tx, resp_rx) = std::sync::mpsc::channel();
            tx_fail.send(WMsg::FlashFailure{
                partition: partition.to_string(),
                slot: slot.to_string(),
                error: error.to_string(),
                response: resp_tx,
            }).ok();
            resp_rx.recv().unwrap_or(lfff_lib::flasher::FailureAction::Abort)
        },
    );
    tx.send(WMsg::Progress{fraction:1.0,partition:String::new()}).ok();
    let failed = session.failed().len();
    let total = session.results.len();
    for r in session.failed() {
        let level = if session.critical_failed().iter().any(|c| c.partition == r.partition && c.slot == r.slot) {
            LogLevel::Error
        } else {
            LogLevel::Warn
        };
        tx.send(WMsg::Log{level,message:format!("FAILED: {}_{}",r.partition,r.slot),tab:2}).ok();
    }
    let success = !session.aborted && failed == 0;
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
    let failed_partitions: Vec<String> = session.failed().iter()
        .map(|r| r.partition.clone())
        .collect();
    let log_msg = if failed > 0 {
        format!("{}/{} partitions failed", failed, total)
    } else if session.aborted {
        "Flash aborted".into()
    } else {
        format!("{}/{} OK", total, total)
    };
    tx.send(WMsg::FlashComplete{ success, message: msg, log_summary: log_msg, failed_partitions }).ok();
    tx.send(WMsg::Flashing(false)).ok();
}

fn worker(rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<WMsg>,
    flash_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let mut serial: Option<String> = None;
    let mut skip_xbl_abl: bool = false;
    let mut skip_preloader: bool = false;
    let mut as_mediatek: Option<bool> = None;
    let mut current_source: Option<lfff_lib::flasher::FirmwareSource> = None;
    let dl_cancel_token: std::sync::Arc<std::sync::Mutex<Option<lfff_lib::downloader::CancelToken>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    loop {
        match rx.recv() {
            Ok(cmd) => match cmd {
                Cmd::CheckDevice => {
                    log(&tx,LogLevel::Info,0,"Searching for device...");
                    
                    // Try ADB first
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
                        log(&tx,LogLevel::Success,0,format!("ADB device: {}",ser));
                        serial = Some(ser.clone());
                        
                        // Get info via ADB getprop
                        let getprop = |prop: &str| -> String {
                            std::process::Command::new("adb").args(["shell","getprop",prop]).output()
                                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                                .unwrap_or_default()
                        };
                        let model = getprop("ro.product.model");
                        let product = getprop("ro.product.device");
                        let build = getprop("ro.build.display.id");
                        let android = getprop("ro.build.version.release");
                        let slot = getprop("ro.boot.slot_suffix");
                        let battery = std::process::Command::new("adb").args(["shell","cat","/sys/class/power_supply/battery/capacity"]).output()
                            .map(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i32>().unwrap_or(-1))
                            .unwrap_or(-1);

                        let name = if !model.is_empty() { model.clone() } else { product.clone() };
                        let slot_clean = slot.trim_start_matches('_').to_string();
                        
                        tx.send(WMsg::DeviceDetected{
                            name: name.clone(),
                            serial: ser.clone(),
                            slot: if slot_clean.is_empty() { "N/A".into() } else { slot_clean },
                            
                        }).ok();

                        let mut info = format!("Device: {} ({})",name,product);
                        if !build.is_empty() { info.push_str(&format!(" | Build: {}",build)); }
                        if !android.is_empty() { info.push_str(&format!(" | Android {}",android)); }
                        if battery >= 0 { info.push_str(&format!(" | Battery: {}%",battery)); }
                        log(&tx,LogLevel::Success,0,info);
                        continue;
                    }

                    // Fallback to fastboot
                    let s = lfff_lib::device::list_fastboot_devices();
                    if s.is_empty(){log(&tx,LogLevel::Error,0,"No device found via ADB or fastboot");tx.send(WMsg::DeviceDisconnected).ok();serial=None;continue;}
                    let ser=&s[0];serial=Some(ser.clone());
                    match lfff_lib::device::get_device_info(Some(ser)){
                        Some(i)=>{
                            let name=if i.product.is_empty(){ser.clone()}else{i.product.clone()};
                            let slot=if i.current_slot.is_empty(){"\u{2014}".into()}else{i.current_slot.clone()};
                            tx.send(WMsg::DeviceDetected{name:name.clone(),serial:ser.clone(),slot}).ok();
                            let mut d=format!("Fastboot device: {}",name);
                            if i.battery_level>=0{d.push_str(&format!(" | Battery: {}%",i.battery_level));}
                            if !i.unlocked.is_empty(){d.push_str(&format!(" | Unlocked: {}",i.unlocked));}
                            log(&tx,LogLevel::Success,0,d);
                        }
                        None=>log(&tx,LogLevel::Error,0,"Device found but getvar failed"),
                    }
                }
                Cmd::RebootForFlash{reboot_choice}=>{
                    // Reboot to fastbootd first, then signal UI to show final confirm
                    let ready = match reboot_choice {
                        1 => {
                            log(&tx,LogLevel::Info,2,"Rebooting to fastbootd via ADB...");
                            let _ = std::process::Command::new("adb").args(["reboot","fastboot"]).status();
                            log(&tx,LogLevel::Info,2,"Waiting for device to enter fastbootd...");
                            std::thread::sleep(Duration::from_secs(5));
                            lfff_lib::flasher::wait_for_fastbootd(serial.as_deref(), 90)
                        }
                        2 => {
                            log(&tx,LogLevel::Info,2,"Rebooting to fastbootd via fastboot...");
                            let _ = std::process::Command::new("fastboot").args(["reboot","fastboot"]).status();
                            log(&tx,LogLevel::Info,2,"Waiting for device to enter fastbootd...");
                            std::thread::sleep(Duration::from_secs(4));
                            lfff_lib::flasher::wait_for_fastbootd(serial.as_deref(), 90)
                        }
                        3 => {
                            // Already in fastbootd — verify device is specifically in fastbootd mode
                            log(&tx,LogLevel::Info,2,"Verifying device is in fastbootd mode...");
                            let fb = lfff_lib::device::list_fastbootd_devices();
                            if !fb.is_empty() {
                                log(&tx,LogLevel::Success,2,"Device confirmed in fastbootd");
                                true
                            } else {
                                log(&tx,LogLevel::Error,2,"Device is NOT in fastbootd mode — it may be in bootloader (fastboot) instead. Please reboot to fastbootd and try again.");
                                false
                            }
                        }
                        _ => false,
                    };
                    if ready {
                        tx.send(WMsg::ReadyToFlash).ok();
                    } else {
                        log(&tx,LogLevel::Error,2,"Device not found in fastbootd — aborting");
                        tx.send(WMsg::FlashComplete{success:false,message:"Device not found in fastbootd".into(),log_summary:"Device not found".into(),failed_partitions:vec![]}).ok();
                    }
                }
                Cmd::Flash{path,skip_arb,skip_partitions}=>{
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx,LogLevel::Info,2,"Starting flash...");
                    let fw=Path::new(&path);
                    let dir=if path.ends_with(".zip"){
                        let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                        let out=get_output_dir().join(&fw_name);
                        log(&tx,LogLevel::Info,2,format!("Extracting to {}...",out.display()));
                        let tx_ex=tx.clone();
                        let staging2=out.join("_staging");
                        std::fs::create_dir_all(&staging2).ok();
                        let tx_w2=tx.clone();
                        let wd2=staging2.clone();
                        let (stx2,srx2)=std::sync::mpsc::channel::<()>();
                        std::thread::spawn(move||{
                            let mut known=std::collections::HashSet::<String>::new();
                            let mut secs=0u32;
                            loop{
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                if srx2.try_recv().is_ok(){break;}
                                secs+=1;
                                let mut q=vec![wd2.clone()];
                                while let Some(d)=q.pop(){
                                    if let Ok(rd)=std::fs::read_dir(&d){
                                        for e in rd.flatten(){let p=e.path();if p.is_dir(){q.push(p);continue;}
                                            let name=p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                            if name.ends_with(".img")&&known.insert(name.clone()){
                                                tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracted: {}",name),tab:2}).ok();
                                            }
                                        }
                                    }
                                }
                                if secs%5==0&&known.is_empty(){tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracting... ({}s)",secs),tab:2}).ok();}
                            }
                        });
                        let r=lfff_lib::extractor::extract_firmware_with_log(fw,&out,None,None,Some(&|line:String|{tx_ex.send(WMsg::Log{level:LogLevel::Info,message:line,tab:2}).ok();}));
                        stx2.send(()).ok();
                        if !r.success{log(&tx,LogLevel::Error,2,"Extract failed");tx.send(WMsg::FlashComplete{success:false,message:r.error,log_summary:"Extract failed".into(),failed_partitions:vec![]}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                        log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                    }else{fw.to_path_buf()};
                    // Reset state, detect Mediatek using combined method
                    skip_xbl_abl = false;
                    skip_preloader = false;
                     let images = lfff_lib::flasher::collect_images(&dir);
                     as_mediatek = lfff_lib::flasher::detect_device_type(serial.as_deref(), &images);
                      if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                          log(&tx,LogLevel::Info,2,"Mediatek platform detected (preloader found)");
                          skip_xbl_abl = true;
                      } else if as_mediatek == Some(true) {
                          log(&tx,LogLevel::Info,2,"Mediatek platform detected (no preloader in firmware)");
                          skip_xbl_abl = true;
                      } else if as_mediatek == Some(false) {
                         log(&tx,LogLevel::Info,2,"Qualcomm platform detected");
                     } else {
                         log(&tx,LogLevel::Info,2,"Platform detection inconclusive — proceeding with default logic");
                     }
                    // ARB check — skip for Mediatek (no ARB on Mediatek)
                    if !skip_arb && as_mediatek != Some(true) {
                        if let Some(xbl)=lfff_lib::arb::find_xbl_config(&dir){
                            let a=lfff_lib::arb::extract_arb_from_xbl(&xbl);
                            let ver = a.version.unwrap_or(0);
                            if ver > 0 {
                                tx.send(WMsg::Flashing(false)).ok();
                                tx.send(WMsg::ArbWarning { version: ver, as_mediatek }).ok();
                                log(&tx,LogLevel::Warn,2,format!("ARB={} — anti-rollback will be raised, waiting for confirmation...",ver));
                                continue;
                            }
                            // ARB=0 — warn: device may have higher ARB
                            tx.send(WMsg::Flashing(false)).ok();
                            tx.send(WMsg::ArbDeviceWarning{path:path.clone(),is_source:false,device_arb:0}).ok();
                            log(&tx,LogLevel::Warn,2,"Firmware ARB=0 — device ARB unknown, may be unsafe to flash");
                            continue;
                        }
                    }
                     // Mediatek: show preloader warning only if preloader files exist in firmware
                     if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                         tx.send(WMsg::Flashing(false)).ok();
                         tx.send(WMsg::PreloaderWarning{path: path.clone(), is_source: false}).ok();
                         log(&tx,LogLevel::Warn,2,"preloader detected — Mediatek firmware, waiting for confirmation...");
                         continue;
                     }
                    do_flash(&tx, &lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()), &serial, skip_xbl_abl, skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                    current_source = Some(lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()));
                }
                Cmd::ConfirmArbAndFlash{path, skip_xbl_abl: cmd_skip_xbl_abl, skip_partitions}=>{
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    let fw=Path::new(&path);
                    let dir=if path.ends_with(".zip"){
                        let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                        let out=get_output_dir().join(&fw_name);
                        log(&tx,LogLevel::Info,2,format!("Extracting to {}...",out.display()));
                        let tx_ex=tx.clone();
                        let staging2=out.join("_staging");
                        std::fs::create_dir_all(&staging2).ok();
                        let tx_w2=tx.clone();
                        let wd2=staging2.clone();
                        let (stx2,srx2)=std::sync::mpsc::channel::<()>();
                        std::thread::spawn(move||{
                            let mut known=std::collections::HashSet::<String>::new();
                            let mut secs=0u32;
                            loop{
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                if srx2.try_recv().is_ok(){break;}
                                secs+=1;
                                let mut q=vec![wd2.clone()];
                                while let Some(d)=q.pop(){
                                    if let Ok(rd)=std::fs::read_dir(&d){
                                        for e in rd.flatten(){let p=e.path();if p.is_dir(){q.push(p);continue;}
                                            let name=p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                            if name.ends_with(".img")&&known.insert(name.clone()){
                                                tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracted: {}",name),tab:2}).ok();
                                            }
                                        }
                                    }
                                }
                                if secs%5==0&&known.is_empty(){tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracting... ({}s)",secs),tab:2}).ok();}
                            }
                        });
                        let r=lfff_lib::extractor::extract_firmware_with_log(fw,&out,None,None,Some(&|line:String|{tx_ex.send(WMsg::Log{level:LogLevel::Info,message:line,tab:2}).ok();}));
                        stx2.send(()).ok();
                        if !r.success{log(&tx,LogLevel::Error,2,"Extract failed");tx.send(WMsg::FlashComplete{success:false,message:r.error,log_summary:"Extract failed".into(),failed_partitions:vec![]}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                        log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                    }else{fw.to_path_buf()};
                    log(&tx,LogLevel::Info,2,"ARB warning confirmed by user, proceeding...");
                    let src = lfff_lib::flasher::FirmwareSource::Extracted(dir.clone());
                    do_flash(&tx, &src, &serial, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                    current_source = Some(src);
                }
                Cmd::ConfirmArbDeviceFlash{path,is_source,skip_xbl_abl: cmd_skip_xbl_abl, skip_partitions}=>{
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    if is_source {
                        log(&tx,LogLevel::Info,2,"Device ARB warning confirmed by user, flashing from source...");
                        let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&path));
                        do_flash(&tx, &d, &serial, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(d);
                    } else {
                        log(&tx,LogLevel::Info,2,"Device ARB warning confirmed by user, proceeding...");
                        let dir = if path.ends_with(".zip"){
                            let fw=Path::new(&path);
                            let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                            let out=get_output_dir().join(&fw_name);
                            log(&tx,LogLevel::Info,2,format!("Extracting to {}...",out.display()));
                            let tx_ex=tx.clone();
                            let staging2=out.join("_staging");
                            std::fs::create_dir_all(&staging2).ok();
                            let tx_w2=tx.clone();
                            let wd2=staging2.clone();
                            let (stx2,srx2)=std::sync::mpsc::channel::<()>();
                            std::thread::spawn(move||{
                                let mut known=std::collections::HashSet::<String>::new();
                                let mut secs=0u32;
                                loop{
                                    std::thread::sleep(std::time::Duration::from_secs(1));
                                    if srx2.try_recv().is_ok(){break;}
                                    secs+=1;
                                    let mut q=vec![wd2.clone()];
                                    while let Some(d)=q.pop(){
                                        if let Ok(rd)=std::fs::read_dir(&d){
                                            for e in rd.flatten(){let p=e.path();if p.is_dir(){q.push(p);continue;}
                                                let name=p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                                if name.ends_with(".img")&&known.insert(name.clone()){
                                                    tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracted: {}",name),tab:2}).ok();
                                                }
                                            }
                                        }
                                    }
                                    if secs%5==0&&known.is_empty(){tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracting... ({}s)",secs),tab:2}).ok();}
                                }
                            });
                            let r=lfff_lib::extractor::extract_firmware_with_log(Path::new(&path),&out,None,None,Some(&|line:String|{tx_ex.send(WMsg::Log{level:LogLevel::Info,message:line,tab:2}).ok();}));
                            stx2.send(()).ok();
                            if !r.success{log(&tx,LogLevel::Error,2,"Extract failed");tx.send(WMsg::FlashComplete{success:false,message:r.error,log_summary:"Extract failed".into(),failed_partitions:vec![]}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                            log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                        }else{Path::new(&path).to_path_buf()};
                         let src = lfff_lib::flasher::FirmwareSource::Extracted(dir);
                         do_flash(&tx, &src, &serial, cmd_skip_xbl_abl, false, as_mediatek, skip_partitions, flash_cancel.clone());
                         current_source = Some(src);
                     }
                 }
                 Cmd::ConfirmPreloaderFlash{path,is_source,skip_preloader: cmd_skip_preloader, skip_partitions}=>{
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx,LogLevel::Info,2,"Preloader warning confirmed by user, proceeding...");
                    if is_source {
                        let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&path));
                        do_flash(&tx, &d, &serial, true, cmd_skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                        current_source = Some(d);
                    } else {
                        let dir = if path.ends_with(".zip"){
                            let fw=Path::new(&path);
                            let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                            let out=get_output_dir().join(&fw_name);
                            log(&tx,LogLevel::Info,2,format!("Extracting to {}...",out.display()));
                            let tx_ex=tx.clone();
                            let staging2=out.join("_staging");
                            std::fs::create_dir_all(&staging2).ok();
                            let tx_w2=tx.clone();
                            let wd2=staging2.clone();
                            let (stx2,srx2)=std::sync::mpsc::channel::<()>();
                            std::thread::spawn(move||{
                                let mut known=std::collections::HashSet::<String>::new();
                                let mut secs=0u32;
                                loop{
                                    std::thread::sleep(std::time::Duration::from_secs(1));
                                    if srx2.try_recv().is_ok(){break;}
                                    secs+=1;
                                    let mut q=vec![wd2.clone()];
                                    while let Some(d)=q.pop(){
                                        if let Ok(rd)=std::fs::read_dir(&d){
                                            for e in rd.flatten(){let p=e.path();if p.is_dir(){q.push(p);continue;}
                                                let name=p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                                if name.ends_with(".img")&&known.insert(name.clone()){
                                                    tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracted: {}",name),tab:2}).ok();
                                                }
                                            }
                                        }
                                    }
                                    if secs%5==0&&known.is_empty(){tx_w2.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracting... ({}s)",secs),tab:2}).ok();}
                                }
                            });
                            let r=lfff_lib::extractor::extract_firmware_with_log(Path::new(&path),&out,None,None,Some(&|line:String|{tx_ex.send(WMsg::Log{level:LogLevel::Info,message:line,tab:2}).ok();}));
                            stx2.send(()).ok();
                            if !r.success{log(&tx,LogLevel::Error,2,"Extract failed");tx.send(WMsg::FlashComplete{success:false,message:r.error,log_summary:"Extract failed".into(),failed_partitions:vec![]}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                            log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                        }else{Path::new(&path).to_path_buf()};
                         let src = lfff_lib::flasher::FirmwareSource::Extracted(dir);
                         do_flash(&tx, &src, &serial, true, cmd_skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                         current_source = Some(src);
                     }
                 }
                 Cmd::FlashFromSource{dir, skip_partitions}=>{
                     flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                     tx.send(WMsg::Flashing(true)).ok();
                     log(&tx,LogLevel::Info,2,&format!("Flashing from source dir: {}",dir));
                     let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&dir));
                     current_source = Some(d.clone());
                    let images = lfff_lib::flasher::collect_images_from_source(d.path());
                    if images.is_empty() {
                        log(&tx,LogLevel::Error,2,"No flashable .img files found in the selected source directory");
                        tx.send(WMsg::FlashComplete{success:false,message:"No flashable images found".into(),log_summary:"No images found".into(),failed_partitions:vec![]}).ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
                    log(&tx,LogLevel::Info,2,&format!("Found {} images to flash",images.len()));
                    for (name,path) in &images {
                        let size_mb = std::fs::metadata(path).map(|m|m.len() as f64/1024.0/1024.0).unwrap_or(0.0);
                        log(&tx,LogLevel::Info,2,&format!("  {} ({:.1} MB)",name,size_mb));
                    }
                    // Combined MTK detection: preloader.img + fastboot getvar occt/ocdt
                     let is_mtk = lfff_lib::flasher::detect_device_type(serial.as_deref(), &images);
                     as_mediatek = is_mtk;
                     // Mediatek: show preloader warning only if preloader files exist in firmware
                     if as_mediatek == Some(true) && lfff_lib::flasher::is_mediatek_build(&images) {
                         log(&tx,LogLevel::Info,2,"Mediatek platform detected");
                         tx.send(WMsg::Flashing(false)).ok();
                         tx.send(WMsg::PreloaderWarning{path: dir.clone(), is_source: true}).ok();
                         log(&tx,LogLevel::Warn,2,"preloader detected — Mediatek firmware, waiting for confirmation...");
                         continue;
                     } else if as_mediatek == Some(true) {
                         log(&tx,LogLevel::Info,2,"Mediatek platform detected (no preloader in firmware)");
                     } else if as_mediatek == Some(false) {
                         log(&tx,LogLevel::Info,2,"Qualcomm platform detected");
                     } else {
                         log(&tx,LogLevel::Info,2,"Platform detection inconclusive — proceeding with default logic");
                     }
                    do_flash(&tx, &d, &serial, skip_xbl_abl, skip_preloader, as_mediatek, skip_partitions, flash_cancel.clone());
                }
                Cmd::FlashSingle{path,partition,reboot_choice}=>{
                    tx.send(WMsg::Flashing(true)).ok();
                    // Reboot to bootloader if requested, then poll
                    let ready = match reboot_choice {
                        1 => {
                            log(&tx,LogLevel::Info,3,"Rebooting to bootloader via ADB...");
                            let _ = std::process::Command::new("adb").args(["reboot","bootloader"]).status();
                            log(&tx,LogLevel::Info,3,"Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(5));
                            wait_for_fastboot(&tx, 3, 90)
                        }
                        2 => {
                            log(&tx,LogLevel::Info,3,"Rebooting to bootloader via fastboot...");
                            let _ = std::process::Command::new("fastboot").args(["reboot-bootloader"]).status();
                            log(&tx,LogLevel::Info,3,"Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(4));
                            wait_for_fastboot(&tx, 3, 90)
                        }
                        _ => {
                            log(&tx,LogLevel::Info,3,"Checking device is in fastboot...");
                            wait_for_fastboot(&tx, 3, 10)
                        }
                    };
                    if !ready {
                        tx.send(WMsg::FlashComplete{success:false,message:"Device not found in fastboot".into(),log_summary:"Device not found".into(),failed_partitions:vec![]}).ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
                    let img=Path::new(&path);let sref=serial.as_deref();
                    if !img.exists(){log(&tx,LogLevel::Error,3,"File not found");tx.send(WMsg::Flashing(false)).ok();continue;}
                    // Use provided partition name or detect from filename
                    let p = if let Some(ref pn)=partition { pn.clone() } else {
                        let mut p=img.file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
                        for s in &["_a","_b"]{if p.ends_with(s){p=p[..p.len()-s.len()].to_string();break;}}
                        p
                    };
                    log(&tx,LogLevel::Info,3,format!("Flashing partition: {}",p));
                    let slots=["a","b"];let total=2;let mut done=0;let mut fail=0;
                    for slot in &slots{
                        let lbl=format!("{}_{}",p,slot);
                        tx.send(WMsg::Progress{fraction:done as f32/total as f32,partition:lbl.clone()}).ok();
                        let r=lfff_lib::flasher::flash_partition(img,&p,slot,sref);done+=1;
                        if r.success{log(&tx,LogLevel::Success,3,format!("{} OK",lbl));}else{fail+=1;log(&tx,LogLevel::Error,3,format!("{} FAILED",lbl));}
                    }
                    tx.send(WMsg::Progress{fraction:1.0,partition:String::new()}).ok();
                    tx.send(WMsg::FlashComplete{success:fail==0,message:if fail==0{format!("{} flashed OK",p)}else{format!("{} errors",fail)},log_summary:if fail==0{"Flash OK".into()}else{format!("{} errors",fail)},failed_partitions:vec![]}).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                }
                Cmd::CancelFlash=>log(&tx,LogLevel::Warn,2,"Cancelling flash..."),
                Cmd::PostFlashReboot => {
                    log(&tx,LogLevel::Info,2,"Rebooting to system...");
                    let mut args = vec!["fastboot"];
                    let ser_s;
                    if let Some(s) = &serial { ser_s = s.clone(); args.extend(&["-s", &ser_s]); }
                    args.push("reboot");
                    match std::process::Command::new(args[0]).args(&args[1..]).status() {
                        Ok(s) if s.success() => log(&tx,LogLevel::Success,2,"Reboot initiated"),
                        _ => log(&tx,LogLevel::Error,2,"Failed to reboot"),
                    }
                }
                Cmd::PostFlashWipe => {
                    log(&tx,LogLevel::Warn,2,"Wiping data (fastboot -w)...");
                    let mut args = vec!["fastboot"];
                    let ser_s;
                    if let Some(s) = &serial { ser_s = s.clone(); args.extend(&["-s", &ser_s]); }
                    args.push("-w");
                    match std::process::Command::new(args[0]).args(&args[1..]).status() {
                        Ok(s) if s.success() => {
                            log(&tx,LogLevel::Success,2,"Wipe done, rebooting...");
                            let mut args2 = vec!["fastboot"];
                            let ser_s2;
                            if let Some(s) = &serial { ser_s2 = s.clone(); args2.extend(&["-s", &ser_s2]); }
                            args2.push("reboot");
                            std::process::Command::new(args2[0]).args(&args2[1..]).status().ok();
                        }
                        _ => log(&tx,LogLevel::Error,2,"Wipe failed"),
                    }
                }
                Cmd::RebootTo(target)=>{
                    let (cmd, args): (&str, &[&str]) = match target.as_str() {
                        // ADB variants
                        "adb-recovery"   | "recovery"   => ("adb", &["reboot", "recovery"]),
                        "adb-bootloader" | "bootloader" => ("adb", &["reboot", "bootloader"]),
                        "adb-fastboot"   | "fastboot"   => ("adb", &["reboot", "fastboot"]),
                        "adb-reboot"     | "reboot"     => ("adb", &["reboot"]),
                        // Fastboot variants
                        "fb-recovery"    => ("fastboot", &["reboot", "recovery"]),
                        "fb-bootloader"  => ("fastboot", &["reboot-bootloader"]),
                        "fb-fastboot"    => ("fastboot", &["reboot", "fastboot"]),
                        "fb-reboot"      => ("fastboot", &["reboot"]),
                        _ => { log(&tx,LogLevel::Error,0,"Unknown reboot target"); continue; }
                    };
                    log(&tx,LogLevel::Info,0,format!("Rebooting to {}...", target));
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
                        log(&tx,LogLevel::Success,0,format!("Reboot to {} initiated", target));
                    } else {
                        if is_fastboot_cmd {
                            log(&tx,LogLevel::Error,0,"Device not in fastboot mode — use ADB reboot buttons instead");
                        } else {
                            log(&tx,LogLevel::Error,0,format!("Failed to reboot to {}", target));
                        }
                    }
                }
                Cmd::CableTest => {
                    let total_steps = 10u8;
                    let mut success_count = 0u8;
                    let mut total_latency_ms = 0u64;
                    let mut args: Vec<&str> = vec!["fastboot"];
                    if let Some(ref s) = serial {
                        args.extend(&["-s", s]);
                    }
                    args.extend(&["getvar", "product"]);
                    
                    for step in 0..total_steps {
                        let start = std::time::Instant::now();
                        tx.send(WMsg::CableTestProgress {
                            step,
                            total: total_steps,
                            status: format!("Test {}/{}...", step + 1, total_steps),
                        }).ok();
                        
                        let output = std::process::Command::new(args[0])
                            .args(&args[1..])
                            .output();
                        
                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        
                        match output {
                            Ok(o) if o.status.success() => {
                                success_count += 1;
                                total_latency_ms += elapsed_ms;
                            }
                            _ => {
                                tx.send(WMsg::CableTestProgress {
                                    step: total_steps,
                                    total: total_steps,
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
                        step: total_steps,
                        total: total_steps,
                        status: format!("✓ OK — avg {}ms ({})", avg_ms, speed_label),
                    }).ok();
                }
                Cmd::RetryFlash{failed_partitions} => {
                    // Retry only the failed partitions using stored firmware source
                    flash_cancel.store(false, std::sync::atomic::Ordering::Relaxed);
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx,LogLevel::Info,2,format!("Retrying {} partition(s): {}", failed_partitions.len(), failed_partitions.join(", ")));
                    
                    let source = match &current_source {
                        Some(s) => s.clone(),
                        None => {
                            log(&tx,LogLevel::Error,2,"No firmware source available for retry");
                            tx.send(WMsg::FlashComplete{success:false,message:"No firmware source available".into(),log_summary:"No source available".into(),failed_partitions:vec![]}).ok();
                            tx.send(WMsg::Flashing(false)).ok();
                            continue;
                        }
                    };
                    
                    let total = failed_partitions.len() * 2; // both slots
                    let mut done = 0;
                    let mut fail_count = 0;
                    let mut failed_list = Vec::new();
                    let sref = serial.as_deref();
                    
                    for partition in &failed_partitions {
                        for slot in &["a", "b"] {
                            if flash_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                log(&tx,LogLevel::Warn,2,"Retry cancelled by user");
                                tx.send(WMsg::FlashComplete{success:false,message:"Retry cancelled".into(),log_summary:"Retry cancelled".into(),failed_partitions:failed_list.clone()}).ok();
                                tx.send(WMsg::Flashing(false)).ok();
                                return;
                            }
                            
                            let lbl = format!("{}_{}", partition, slot);
                            tx.send(WMsg::Progress{fraction:done as f32 / total as f32,partition:lbl.clone()}).ok();
                            
                            // Find the image file for this partition in the firmware source
                            let fw_dir = source.path();
                            let mut img_path = None;
                            
                            // Search for the image file
                            for entry in std::fs::read_dir(fw_dir).into_iter().flatten().flatten() {
                                let p = entry.path();
                                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                                    let lower = name.to_lowercase();
                                    if lower.starts_with(&partition.to_lowercase()) && lower.ends_with(".img") {
                                        img_path = Some(p);
                                        break;
                                    }
                                }
                            }
                            
                            if let Some(img) = img_path {
                                log(&tx,LogLevel::Info,2,format!("Retrying {} from {}...", lbl, img.display()));
                                let result = lfff_lib::flasher::flash_partition(&img, partition, slot, sref);
                                if result.success {
                                    log(&tx,LogLevel::Success,2,format!("{} OK", lbl));
                                } else {
                                    fail_count += 1;
                                    failed_list.push(partition.clone());
                                    log(&tx,LogLevel::Error,2,format!("{} FAILED", lbl));
                                }
                            } else {
                                fail_count += 1;
                                failed_list.push(partition.clone());
                                log(&tx,LogLevel::Error,2,format!("Image not found for {}", partition));
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
                    let log_msg = if success {
                        "Retry OK".into()
                    } else {
                        format!("{}/{} failed", fail_count, failed_partitions.len())
                    };
                    
                    tx.send(WMsg::Progress{fraction:1.0,partition:String::new()}).ok();
                    tx.send(WMsg::FlashComplete{success,message:msg,log_summary:log_msg,failed_partitions:failed_list}).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                }
                Cmd::CheckDeps=>{
                    log(&tx,LogLevel::Info,0,"Checking dependencies...");
                    with_captured_stdout(&tx, 0, || {
                        let r=lfff_lib::deps::install_dependencies(None,true);
                        for d in &r.results{
                            if d.already_installed{log(&tx,LogLevel::Success,0,format!("{}: OK",d.tool));}
                            else if d.skipped{log(&tx,LogLevel::Warn,0,format!("{}: skipped",d.tool));}
                            else if !d.error.is_empty(){log(&tx,LogLevel::Error,0,format!("{}: {}",d.tool,d.error));}
                        }
                        tx.send(WMsg::DepsResult{ok:r.all_ok(),message:if r.all_ok(){"All dependencies OK".into()}else{"Some missing".into()}}).ok();
                    });
                }
                Cmd::InstallDeps=>{
                    log(&tx,LogLevel::Info,0,"Installing dependencies...");
                    with_captured_stdout(&tx, 0, || {
                        let r=lfff_lib::deps::install_dependencies(None,false);
                        for d in &r.results{
                            if d.installed{log(&tx,LogLevel::Success,0,format!("{}: installed",d.tool));}
                            else if d.already_installed{log(&tx,LogLevel::Success,0,format!("{}: already OK",d.tool));}
                            else if !d.error.is_empty(){log(&tx,LogLevel::Error,0,format!("{}: {}",d.tool,d.error));}
                        }
                        tx.send(WMsg::DepsResult{ok:r.all_ok(),message:if r.all_ok(){"All OK".into()}else{"Some failed".into()}}).ok();
                    });
                }
                Cmd::Download{url}=>{
                    tx.send(WMsg::Downloading(true)).ok();
                    log(&tx,LogLevel::Info,1,"Starting download...");
                    let tx_dl=tx.clone();
                    let token = lfff_lib::downloader::CancelToken::new();
                    *dl_cancel_token.lock().unwrap() = Some(token.clone());
                    std::thread::spawn(move||{
                        let out=get_output_dir();
                        log(&tx_dl,LogLevel::Info,1,format!("Output: {}",out.display()));
                        let tx2=tx_dl.clone();
                        let last_update = std::sync::Mutex::new(std::time::Instant::now());
                        let r=lfff_lib::downloader::download_firmware_with_progress(&url,Some(&out),16,token,move|p|{
                            let mut last = last_update.lock().unwrap();
                            if last.elapsed().as_millis() >= 100 || p.percent >= 100.0 {
                                tx2.send(WMsg::DlProgress{percent:p.percent,speed:p.speed,eta:p.eta,downloaded:p.downloaded,total:p.total_size,raw_line:p.raw_line}).ok();
                                *last = std::time::Instant::now();
                            }
                        });
                        if r.success{
                            let p=r.output_path.as_ref().map(|p|p.display().to_string()).unwrap_or_default();
                            log(&tx_dl,LogLevel::Success,1,format!("Downloaded: {}",p));
                            if let Some(path)=r.output_path{tx_dl.send(WMsg::FwPath(path.display().to_string())).ok();}
                        } else if r.error == "Cancelled" {
                            log(&tx_dl,LogLevel::Warn,1,"Download cancelled");
                        } else {
                            log(&tx_dl,LogLevel::Error,1,format!("Failed: {}",r.error));
                        }
                        tx_dl.send(WMsg::DlProgress{percent:0.0,speed:String::new(),eta:String::new(),downloaded:String::new(),total:String::new(),raw_line:String::new()}).ok();
                        tx_dl.send(WMsg::Downloading(false)).ok();
                    });
                }
                Cmd::CancelDownload=>{
                    if let Some(token) = dl_cancel_token.lock().unwrap().take() {
                        token.cancel();
                        log(&tx,LogLevel::Warn,1,"Cancelling download...");
                    }
                }
                Cmd::Extract{path}=>{
                    let fw=Path::new(&path);let out=get_output_dir().join(lfff_lib::extractor::get_firmware_name(fw));
                    log(&tx,LogLevel::Info,1,format!("Extracting to {}...",out.display()));
                    let tx_ex=tx.clone();
                    // Watch _staging dir for new .img files and forward to GUI in real time.
                    // payload_dumper writes to /dev/tty directly so we can't capture its output.
                    let staging=out.join("_staging");
                    std::fs::create_dir_all(&staging).ok();
                    let tx_watch=tx.clone();
                    let watch_dir=staging.clone();
                    let (stop_tx,stop_rx)=std::sync::mpsc::channel::<()>();
                    std::thread::spawn(move||{
                        let mut known=std::collections::HashSet::<String>::new();
                        let mut secs=0u32;
                        loop{
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            if stop_rx.try_recv().is_ok(){break;}
                            secs+=1;
                            // Scan recursively for new .img files
                            let mut queue=vec![watch_dir.clone()];
                            while let Some(dir)=queue.pop(){
                                if let Ok(rd)=std::fs::read_dir(&dir){
                                    for e in rd.flatten(){
                                        let p=e.path();
                                        if p.is_dir(){queue.push(p);continue;}
                                        let name=p.file_name().unwrap_or_default().to_string_lossy().to_string();
                                        if name.ends_with(".img")&&known.insert(name.clone()){
                                            tx_watch.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracted: {}",name),tab:1}).ok();
                                        }
                                    }
                                }
                            }
                            if secs%5==0&&known.is_empty(){
                                tx_watch.send(WMsg::Log{level:LogLevel::Info,message:format!("Extracting... ({}s)",secs),tab:1}).ok();
                            }
                        }
                    });
                    let r=lfff_lib::extractor::extract_firmware_with_log(fw,&out,None,None,Some(&|line:String|{tx_ex.send(WMsg::Log{level:LogLevel::Info,message:line,tab:1}).ok();}));
                    stop_tx.send(()).ok();
                    if r.success{let n:usize=r.groups.values().map(|v|v.len()).sum();log(&tx,LogLevel::Success,1,format!("{} images extracted to {}",n,out.display()));}
                    else{log(&tx,LogLevel::Error,1,format!("Extraction failed: {}",r.error));}
                }
                Cmd::DriverTest=>{
                    use std::process::Command as Cmd2;
                    let step = |tx: &mpsc::Sender<WMsg>, s: i32, msg: &str| {
                        tx.send(WMsg::TestStep{step:s,status:msg.into()}).ok();
                        log(tx, LogLevel::Info, 0, msg);
                    };

                    // Step 1: Check ADB
                    step(&tx, 1, "Checking ADB connection...");
                    let adb_ok = Cmd2::new("adb").args(["devices"]).output()
                        .map(|o| String::from_utf8_lossy(&o.stdout).contains("\tdevice"))
                        .unwrap_or(false);
                    if !adb_ok {
                        // Try fastboot instead
                        let fb = lfff_lib::device::list_fastboot_devices();
                        if fb.is_empty() {
                            step(&tx, -1, "No device found via ADB or fastboot");
                            continue;
                        }
                        step(&tx, 2, "Device found in fastboot mode");
                    } else {
                        step(&tx, 2, "ADB OK — device connected");

                        // Get device info via ADB
                        if let Ok(o) = Cmd2::new("adb").args(["shell","getprop","ro.product.model"]).output() {
                            let model = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !model.is_empty() { log(&tx, LogLevel::Success, 0, format!("Model: {}", model)); }
                        }
                        if let Ok(o) = Cmd2::new("adb").args(["shell","getprop","ro.build.display.id"]).output() {
                            let build = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !build.is_empty() { log(&tx, LogLevel::Success, 0, format!("Build: {}", build)); }
                        }
                        if let Ok(o) = Cmd2::new("adb").args(["shell","getprop","ro.product.cpu.abi"]).output() {
                            let abi = String::from_utf8_lossy(&o.stdout).trim().to_string();
                            if !abi.is_empty() { log(&tx, LogLevel::Success, 0, format!("ABI: {}", abi)); }
                        }

                        // Step 2: Reboot to bootloader
                        step(&tx, 2, "Rebooting to bootloader...");
                        let _ = Cmd2::new("adb").args(["reboot","bootloader"]).status();
                        std::thread::sleep(std::time::Duration::from_secs(8));
                    }

                    // Step 3: Check fastboot
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

                    // Get fastboot device info
                    if let Some(ref ser) = serial {
                        if let Some(info) = lfff_lib::device::get_device_info(Some(ser)) {
                            tx.send(WMsg::DeviceDetected{
                                name: info.product.clone(),
                                serial: ser.clone(),
                                slot: info.current_slot.clone(),
                                
                            }).ok();
                            log(&tx, LogLevel::Success, 0, format!("Product: {} | Slot: {} | Battery: {}%", info.product, info.current_slot, info.battery_level));
                        }
                    }

                    // Step 4: Reboot to fastbootd
                    step(&tx, 4, "Rebooting to fastbootd...");
                    let _ = Cmd2::new("fastboot").args(["reboot","fastboot"]).status();
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

                    // Step 5: Done
                    step(&tx, 5, "All drivers OK!");
                    log(&tx, LogLevel::Success, 0, "Driver test completed successfully");
                }
            },
            Err(_)=>break,
        }
    }
}

fn poll(w: &Weak<MainWindow>, rx: &mpsc::Receiver<WMsg>, last_dl_pct: &mut u32, models: &LogModels,
    fail_resp: &std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction>>>>) {
    // poll() is always called from a Slint Timer — already on the event loop thread.
    // invoke_from_event_loop() here just queued work for the *next* tick and silently
    // dropped messages. Access the UI directly instead.
    let Some(ui) = w.upgrade() else { return };

    while let Ok(m) = rx.try_recv() {
        // Extract dl_log before match consumes m
        let dl_log: Option<String> = if let WMsg::DlProgress{ref percent,ref speed,ref eta,ref downloaded,ref total, ..} = m {
            let pct = *percent as u32;
            let milestone = (pct / 10) * 10;
            if !downloaded.is_empty() && !total.is_empty() && (milestone > *last_dl_pct || pct >= 99) {
                *last_dl_pct = milestone;
                Some(format!("{:.0}%  {} / {}  ↓ {}  ETA {}", percent, downloaded, total, speed, eta))
            } else { None }
        } else { None };

        if let WMsg::DlProgress{ref downloaded, ..} = m {
            if downloaded.is_empty() { *last_dl_pct = 0; }
        }

        match m {
            WMsg::Log{level,message,tab} => add_log(models,&ui,&level,tab,&message),
            WMsg::Progress{fraction,partition} => { ui.set_flash_progress(fraction); ui.set_current_partition(partition.clone().into()); if !partition.is_empty() { ui.set_flash_status(partition.into()); } }
            WMsg::DeviceDetected{name,serial,slot} => ui.set_device(DeviceInfo{connected:true,name:name.into(),serial:serial.into(),slot:slot.into()}),
            WMsg::DeviceDisconnected => ui.set_device(DeviceInfo{connected:false,name:"\u{2014}".into(),serial:"\u{2014}".into(),slot:"\u{2014}".into()}),
            WMsg::FlashComplete{success,message,log_summary,failed_partitions} => {
                ui.set_is_flashing(false);
                ui.set_pending_source_flash(false);
                ui.set_flash_status(log_summary.clone().into());
                if success { ui.set_flash_progress(1.0); }
                let is_cancel = message.contains("aborted by user") || message.contains("cancelled");
                add_log(models,&ui,if success{&LogLevel::Success}else{&LogLevel::Error},2,&log_summary);
                if success {
                    ui.set_confirm_action(7);
                    ui.set_show_confirm(true);
                } else if !is_cancel {
                    ui.set_flash_error_message(message.into());
                    ui.set_flash_failed_partitions(failed_partitions.join(",").into());
                    ui.set_show_flash_error(true);
                }
            }
            WMsg::FlashFailure{partition,slot,error,response} => {
                ui.set_flash_fail_partition(partition.into());
                ui.set_flash_fail_slot(slot.into());
                ui.set_flash_fail_error(error.into());
                ui.set_confirm_action(10);
                ui.set_show_confirm(true);
                *fail_resp.borrow_mut() = Some(response);
            }
            WMsg::DepsResult{message,ok} => add_log(models,&ui,if ok{&LogLevel::Success}else{&LogLevel::Error},0,&message),
            WMsg::Flashing(f) => ui.set_is_flashing(f),
            WMsg::Downloading(f) => ui.set_is_downloading(f),
            WMsg::FwPath(p) => ui.set_firmware_path(p.into()),
            WMsg::DlProgress{percent,speed,eta,downloaded,total,raw_line} => {
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
            WMsg::TestStep{step,status} => { ui.set_test_step(step); ui.set_test_status(status.into()); }
            WMsg::ArbWarning{version,as_mediatek} => {
                add_log(models,&ui,&LogLevel::Warn,2,&format!(" ARB={} — flashing will permanently raise the anti-rollback counter. You will NOT be able to downgrade firmware afterwards!",version));
                ui.set_arb_warning_version(version as i32);
                ui.set_show_arb_warning(true);
                ui.set_as_mediatek(as_mediatek.unwrap_or(false));
            }
            WMsg::ArbDeviceWarning{path,is_source,device_arb} => {
                add_log(models,&ui,&LogLevel::Warn,2,&format!("⚠ Device ARB unknown — firmware ARB may be lower, may brick device!"));
                ui.set_arb_device_version(device_arb as i32);
                ui.set_arb_device_is_source(is_source);
                ui.set_firmware_path(path.into());
                ui.set_show_arb_device_warning(true);
            }
            WMsg::PreloaderWarning{path,is_source} => {
                add_log(models,&ui,&LogLevel::Warn,2,&format!("⚠ preloader.img detected — Mediatek platform"));
                ui.set_preloader_is_source(is_source);
                ui.set_firmware_path(path.into());
                ui.set_show_preloader_warning(true);
            }
            WMsg::ReadyToFlash => {
                // Show cable test dialog before final confirm
                ui.set_confirm_action(9);
                ui.set_cable_test_progress(0.0);
                ui.set_cable_test_status(if ui.get_lang() == "ru" { "Подготовка к тесту..." } else { "Preparing test..." }.into());
                ui.set_cable_test_passed(false);
                ui.set_show_confirm(true);
            }
            WMsg::CableTestProgress { step, total, status } => {
                ui.set_cable_test_progress(step as f32 / total.max(1) as f32);
                ui.set_cable_test_status(status.clone().into());
                if step >= total {
                    ui.set_cable_test_passed(true);
                }
            }
        }
    }
}

struct LogModels {
    device: Rc<VecModel<LogEntry>>,
    download: Rc<VecModel<LogEntry>>,
    flash: Rc<VecModel<LogEntry>>,
    partition: Rc<VecModel<LogEntry>>,
}

fn add_log_m(model: &VecModel<LogEntry>, _ui: &MainWindow, l: &LogLevel, m: &str) {
    model.push(LogEntry{timestamp:ts().into(),level:SharedString::from(lvl(l)),message:SharedString::from(m)});
    while model.row_count() > 500 { model.remove(0); }
}

fn add_log_ui_m(model: &VecModel<LogEntry>, ui: &MainWindow, msg: &str) {
    add_log_m(model, ui, &LogLevel::Info, msg);
}

fn log_model<'a>(models: &'a LogModels, tab: u8) -> &'a VecModel<LogEntry> {
    match tab {
        0 => &models.device,
        1 => &models.download,
        2 => &models.flash,
        _ => &models.partition,
    }
}

fn add_log(models: &LogModels, ui: &MainWindow, l: &LogLevel, tab: u8, m: &str) {
    add_log_m(log_model(models, tab), ui, l, m);
}




#[derive(serde::Serialize, serde::Deserialize)]
struct Config {
    #[serde(default)]
    scale: f32,
    #[serde(default = "default_lang")]
    lang: String,
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default)]
    output_dir: Option<String>,
}

fn default_lang() -> String { "en".to_string() }
fn default_theme() -> String { "dark".to_string() }

impl Default for Config {
    fn default() -> Self {
        Self { scale: 1.0, lang: default_lang(), theme: default_theme(), output_dir: None }
    }
}

fn config_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("lfff")
}

fn config_path() -> std::path::PathBuf {
    config_dir().join("config.json")
}

fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) {
    if let Some(d) = config_path().parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(s) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(config_path(), s);
    }
}

fn save_scale(scale: f32) {
    let mut config = load_config();
    config.scale = scale;
    save_config(&config);
}

fn select_renderer() {
    // If user already set a renderer, respect it
    if std::env::var("SLINT_RENDERER").is_ok() {
        log::info!("Renderer: {} (from env)", std::env::var("SLINT_RENDERER").unwrap());
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
            unsafe { std::env::set_var("SLINT_RENDERER", "skia-vulkan"); }
            log::info!("Renderer: skia-vulkan (detected)");
            return;
        }
    }

    log::info!("Renderer: skia-opengl (default)");
}

/// Run a closure while capturing its stdout lines and forwarding them to the GUI log.
fn with_captured_stdout<F: FnOnce()>(tx: &mpsc::Sender<WMsg>, tab: u8, f: F) {
    unsafe {
        let mut fds = [-1i32; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 { f(); return; }
        let rd_fd = fds[0];
        let wr_fd = fds[1];
        let saved = libc::dup(libc::STDOUT_FILENO);
        libc::dup2(wr_fd, libc::STDOUT_FILENO);
        libc::close(wr_fd);

        let tx2 = tx.clone();
        let reader = thread::spawn(move || {
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

        libc::dup2(saved, libc::STDOUT_FILENO);
        libc::close(saved);
        reader.join().ok();
    }
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    select_renderer();
    #[cfg(target_os = "linux")]
    unsafe { std::env::set_var("LFFF_SUDO_CMD", "pkexec"); }

    let config = load_config();
    unsafe { std::env::set_var("SLINT_SCALE_FACTOR", config.scale.to_string()); }

    let ui=MainWindow::new()?;
    ui.set_ui_scale(config.scale);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_lang(config.lang.as_str().into());
    ui.set_is_dark(config.theme == "dark");

    // Apply dark color scheme by default (user can toggle via the ☀/🌙 button in UI,
    // which writes directly to Palette.color-scheme in Slint — no Rust needed for toggle).
    // We only set the *initial* scheme here.
    // Dark theme is set via Palette.color-scheme in the .slint UI directly.
    // The toggle button handles it without Rust code.

    let(ctx,crx)=mpsc::channel::<Cmd>();
    let(mtx,mrx)=mpsc::channel::<WMsg>();
    let flash_cancel: std::sync::Arc<std::sync::atomic::AtomicBool> = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flash_cancel_w = flash_cancel.clone();
    let fail_resp: std::rc::Rc<std::cell::RefCell<Option<std::sync::mpsc::Sender<lfff_lib::flasher::FailureAction>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    thread::spawn(move||worker(crx,mtx,flash_cancel_w));

    // Persistent log models (push to these instead of replacing)
    let models = LogModels {
        device: Rc::new(VecModel::<LogEntry>::default()),
        download: Rc::new(VecModel::<LogEntry>::default()),
        flash: Rc::new(VecModel::<LogEntry>::default()),
        partition: Rc::new(VecModel::<LogEntry>::default()),
    };
    ui.set_device_log(ModelRc::from(Rc::clone(&models.device)));
    ui.set_download_log(ModelRc::from(Rc::clone(&models.download)));
    ui.set_flash_log(ModelRc::from(Rc::clone(&models.flash)));
    ui.set_partition_log(ModelRc::from(Rc::clone(&models.partition)));
    ui.set_output_dir(get_output_dir().display().to_string().as_str().into());

    // Device detection
    {let t=ctx.clone();ui.on_check_device(move||{t.send(Cmd::CheckDevice).ok();});}

    // Browse firmware ZIP file only
    {let dl=Rc::clone(&models.download);let w=ui.as_weak();ui.on_browse_firmware(move||{
        if let Some(p)=rfd::FileDialog::new()
            .add_filter("Firmware",&["zip","ops","ofp"])
            .add_filter("All",&["*"])
            .set_directory(get_output_dir())
            .pick_file()
        {
            if let Some(ui)=w.upgrade(){
                ui.set_firmware_path(p.display().to_string().into());
                add_log_m(&dl,&ui,&LogLevel::Info,&format!("Selected: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Browse extracted firmware folder
    {let fl=Rc::clone(&models.flash);let w=ui.as_weak();ui.on_browse_folder(move||{
        if let Some(p)=rfd::FileDialog::new()
            .set_directory(get_output_dir())
            .pick_folder()
        {
            if let Some(ui)=w.upgrade(){
                ui.set_firmware_path(p.display().to_string().into());
                add_log_m(&fl,&ui,&LogLevel::Info,&format!("Selected folder: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Browse Android source build directory
    {let fl=Rc::clone(&models.flash);let w=ui.as_weak();ui.on_browse_source_dir(move||{
        if let Some(p)=rfd::FileDialog::new()
            .set_title("Select build output directory (containing .img files)")
            .pick_folder()
        {
            if let Some(ui)=w.upgrade(){
                let dir_str = p.display().to_string();
                let images = lfff_lib::flasher::collect_images_from_source(&p);
                ui.set_firmware_path(dir_str.clone().into());
                ui.set_source_dir(dir_str.clone().into());
                ui.set_source_image_count(images.len() as i32);
                add_log_m(&fl,&ui,&LogLevel::Info,&format!(
                    "Source dir selected: {} ({} images)",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    images.len()
                ));
                let mut sorted: Vec<_> = images.iter().collect();
                sorted.sort_by_key(|(k,_)| *k);
                for (name,path) in &sorted {
                    let mb = std::fs::metadata(path).map(|m|m.len() as f64/1024.0/1024.0).unwrap_or(0.0);
                    add_log_m(&fl,&ui,&LogLevel::Info,&format!("  {} ({:.1} MB)",name,mb));
                }
            }
        }
    });}

    // Browse single .img
    {let pt=Rc::clone(&models.partition);let w=ui.as_weak();ui.on_browse_single_image(move||{
        if let Some(p)=rfd::FileDialog::new().add_filter("Image",&["img"]).add_filter("All",&["*"]).set_directory(get_output_dir()).pick_file(){
            if let Some(ui)=w.upgrade(){
                ui.set_single_image_path(p.display().to_string().into());
                add_log_m(&pt,&ui,&LogLevel::Info,&format!("Selected: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Browse output directory — persist immediately
    {let w=ui.as_weak();ui.on_browse_output_dir(move||{
        if let Some(p)=rfd::FileDialog::new().pick_folder(){
            if let Some(ui)=w.upgrade(){
                let s = p.display().to_string();
                set_output_dir(&s);
                ui.set_output_dir(s.as_str().into());
            }
        }
    });}

    // Flash
    {let t=ctx.clone();let w=ui.as_weak();ui.on_start_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_show_flash_error(false);
            ui.set_flash_error_message("".into());
            ui.set_pending_source_flash(false);
            ui.set_flash_error_is_source(false);
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            t.send(Cmd::Flash{path:ui.get_firmware_path().to_string(),skip_arb:ui.get_skip_arb_check(),skip_partitions:if ui.get_show_skip_partitions(){ui.get_skip_partitions().to_string()}else{String::new()}}).ok();
        }
    });}

    // Flash from source dir — show reboot dialog first, then flash in fastbootd
    {let fl=Rc::clone(&models.flash);let _t=ctx.clone();let w=ui.as_weak();ui.on_start_flash_from_source(move||{
        if let Some(ui)=w.upgrade(){
            let dir = ui.get_source_dir().to_string();
            if dir.is_empty() {
                add_log_m(&fl,&ui,&LogLevel::Error,"No source directory selected");
                return;
            }
            ui.set_firmware_path(dir.into());
            ui.set_pending_source_flash(true);
            ui.set_reboot_choice(0);
            ui.set_confirm_action(3);
            ui.set_show_confirm(true);
        }
    });}

    // Source flash confirmed from final dialog — do the actual flash
    {let fl=Rc::clone(&models.flash);let t=ctx.clone();let w=ui.as_weak();ui.on_confirm_source_flash(move||{
        if let Some(ui)=w.upgrade(){
            let dir = ui.get_firmware_path().to_string();
            if dir.is_empty() {
                add_log_m(&fl,&ui,&LogLevel::Error,"Source directory lost — please try again");
                return;
            }
            ui.set_pending_source_flash(false);
            ui.set_flash_error_is_source(true);
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status("Starting flash from source...".into());
            t.send(Cmd::FlashFromSource{dir, skip_partitions: ui.get_skip_partitions().to_string()}).ok();
        }
    });}

    // Retry flash after error — retry only failed partitions
    {let fl=Rc::clone(&models.flash);let t=ctx.clone();let w=ui.as_weak();ui.on_retry_flash(move||{
        if let Some(ui)=w.upgrade(){
            let failed_str = ui.get_flash_failed_partitions().to_string();
            let failed: Vec<String> = failed_str.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if failed.is_empty() {
                add_log_m(&fl,&ui,&LogLevel::Error,"No failed partitions to retry");
                return;
            }
            ui.set_show_flash_error(false);
            ui.set_flash_error_message("".into());
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status(format!("Retrying {} partition(s)...", failed.len()).into());
            t.send(Cmd::RetryFlash{failed_partitions: failed}).ok();
        }
    });}

    // Reboot device (from error dialog)
    {let dv=Rc::clone(&models.device);let w=ui.as_weak();ui.on_reboot_device(move||{
        if let Some(ui)=w.upgrade(){
            add_log_m(&dv,&ui,&LogLevel::Info,"Rebooting device...");
        }
        let _ = std::process::Command::new("adb").args(["reboot"]).status();
    });}

    // Reboot to target
    {let t=ctx.clone();ui.on_reboot_to(move|target|{
        t.send(Cmd::RebootTo(target.to_string())).ok();
    });}

    // Scale — save and apply with fade animation
    {let w=ui.as_weak();ui.on_set_scale(move|scale|{
        save_scale(scale);
        if let Some(ui)=w.upgrade(){
            ui.set_pending_scale(scale);
        }
    });}
    {let w=ui.as_weak();ui.on_apply_pending_scale(move||{
        if let Some(ui)=w.upgrade(){
            let s = ui.get_pending_scale();
            if s > 0.0 {
                ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: s });
                ui.set_pending_scale(0.0);
                ui.set_effect_opacity(0.0);
            }
        }
    });}

    // Persist language and theme preference
    ui.on_save_lang(|l| {
        let mut config = load_config();
        config.lang = l.to_string();
        save_config(&config);
    });
    ui.on_save_theme(|d| {
        let mut config = load_config();
        config.theme = if d { "dark".to_string() } else { "light".to_string() };
        save_config(&config);
    });

    // ARB warning confirmed — continue flash with user's skip flags
    {let t=ctx.clone();let w=ui.as_weak();ui.on_confirm_arb_and_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            t.send(Cmd::ConfirmArbAndFlash{
                path: ui.get_firmware_path().to_string(),
                skip_xbl_abl: ui.get_skip_xbl_abl(),
                skip_partitions: if ui.get_show_skip_partitions(){ui.get_skip_partitions().to_string()}else{String::new()},
            }).ok();
        }
    });}

    // Device ARB warning confirmed — continue flash
    {let t=ctx.clone();let w=ui.as_weak();ui.on_confirm_arb_device_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            let is_source = ui.get_arb_device_is_source();
            t.send(Cmd::ConfirmArbDeviceFlash{
                path: ui.get_firmware_path().to_string(),
                is_source,
                skip_xbl_abl: ui.get_skip_xbl_abl(),
                skip_partitions: if ui.get_show_skip_partitions(){ui.get_skip_partitions().to_string()}else{String::new()},
            }).ok();
        }
    });}

    // Preloader warning confirmed — continue flash
    {let t=ctx.clone();let w=ui.as_weak();ui.on_confirm_preloader_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            let is_source = ui.get_preloader_is_source();
            t.send(Cmd::ConfirmPreloaderFlash{
                path: ui.get_firmware_path().to_string(),
                is_source,
                skip_preloader: ui.get_skip_preloader(),
                skip_partitions: if ui.get_show_skip_partitions(){ui.get_skip_partitions().to_string()}else{String::new()},
            }).ok();
        }
    });}

    // Reboot to fastbootd before flash (step 1 -> reboot -> step 2)
    {let fl=Rc::clone(&models.flash);let t=ctx.clone();let w=ui.as_weak();ui.on_reboot_for_flash(move||{
        if let Some(ui)=w.upgrade(){
            let reboot_choice = ui.get_reboot_choice() as u8;
            ui.set_show_confirm(false);
            add_log_ui_m(&fl, &ui, "Rebooting to fastbootd...");
            t.send(Cmd::RebootForFlash{reboot_choice}).ok();
        }
    });}

    // Cable test — triggered from cable test dialog
    {let t=ctx.clone();ui.on_run_cable_test(move||{
        t.send(Cmd::CableTest).ok();
    });}

    // Single flash — use partition-name if provided
    {let t=ctx.clone();let w=ui.as_weak();ui.on_start_single_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_pending_source_flash(false);
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            let path=ui.get_single_image_path().to_string();
            let part=ui.get_partition_name().to_string();
            let reboot_choice = ui.get_reboot_choice() as u8;
            t.send(Cmd::FlashSingle{path,partition:if part.is_empty(){None}else{Some(part)},reboot_choice}).ok();
        }
    });}

    // Cancel
    {let t=ctx.clone();ui.on_cancel_download(move||{t.send(Cmd::CancelDownload).ok();});}
    let fc_cancel = flash_cancel.clone();
    {let t=ctx.clone();let w=ui.as_weak();ui.on_cancel_flash(move||{
        fc_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        t.send(Cmd::CancelFlash).ok();
        if let Some(ui)=w.upgrade(){ui.set_pending_source_flash(false);ui.set_is_flashing(false);ui.set_flash_status("Cancelled".into());}
    });}

    // Flash failure dialog callbacks
    {let fr=fail_resp.clone();ui.on_flash_fail_skip(move||{
        if let Some(tx)=fr.borrow_mut().take(){tx.send(lfff_lib::flasher::FailureAction::Skip).ok();}
    });}
    {let fr=fail_resp.clone();ui.on_flash_fail_abort(move||{
        if let Some(tx)=fr.borrow_mut().take(){tx.send(lfff_lib::flasher::FailureAction::Abort).ok();}
    });}

    // Deps
    {let t=ctx.clone();ui.on_check_deps(move||{t.send(Cmd::CheckDeps).ok();});}
    {let t=ctx.clone();ui.on_install_deps(move||{t.send(Cmd::InstallDeps).ok();});}
    {let t=ctx.clone();ui.on_run_driver_test(move||{t.send(Cmd::DriverTest).ok();});}
    {let t=ctx.clone();ui.on_get_device_info(move||{t.send(Cmd::CheckDevice).ok();});}
    {let t=ctx.clone();ui.on_post_flash_reboot(move||{t.send(Cmd::PostFlashReboot).ok();});}
    {let t=ctx.clone();ui.on_post_flash_wipe(move||{t.send(Cmd::PostFlashWipe).ok();});}

    // Download
    {let t=ctx.clone();ui.on_download_firmware(move|u|{t.send(Cmd::Download{url:u.to_string()}).ok();});}

    // Extract
    {let t=ctx.clone();let w=ui.as_weak();ui.on_extract_firmware(move||{
        if let Some(ui)=w.upgrade(){
            let p=ui.get_firmware_path().to_string();
            if !p.is_empty(){t.send(Cmd::Extract{path:p}).ok();}
        }
    });}

    // Paste from clipboard — try arboard first, fallback to wl-paste/xclip
    {let w=ui.as_weak();ui.on_paste_clipboard(move||{
        if let Some(ui)=w.upgrade(){
            // Try arboard
            let text = arboard::Clipboard::new()
                .ok()
                .and_then(|mut c| c.get_text().ok())
                // Fallback: wl-paste (Wayland)
                .or_else(|| std::process::Command::new("wl-paste").arg("-n").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()))
                // Fallback: xclip (X11)
                .or_else(|| std::process::Command::new("xclip").args(["-selection","clipboard","-o"]).output().ok().and_then(|o| String::from_utf8(o.stdout).ok()));
            if let Some(t) = text {
                let trimmed = t.trim().to_string();
                if !trimmed.is_empty() {
                    ui.set_download_url(trimmed.into());
                }
            }
        }
    });}

    // Open URL
    ui.on_open_url(move|url|{
        let u=url.to_string();
        std::thread::spawn(move||{
            #[cfg(target_os="linux")]{let _=std::process::Command::new("xdg-open").arg(&u).spawn();}
            #[cfg(target_os="macos")]{let _=std::process::Command::new("open").arg(&u).spawn();}
        });
    });

    // Back (for compatibility — sidebar handles navigation now)
    {let w=ui.as_weak();ui.on_request_back(move||{
        if let Some(ui)=w.upgrade(){ ui.set_page(0); }
    });}

    let w=ui.as_weak();
    let timer=slint::Timer::default();
    let mut last_dl_pct: u32 = 0;
    let fr_poll = fail_resp.clone();
    timer.start(slint::TimerMode::Repeated,Duration::from_millis(50),move||{
        poll(&w,&mrx,&mut last_dl_pct, &models, &fr_poll);
    });
    ui.run()
}
