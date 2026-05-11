// src/main.rs — LFFF GUI

use slint::Model;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
#[cfg(unix)] extern crate libc;

slint::include_modules!();

#[derive(Debug, Clone)]
enum LogLevel { Info, Warn, Error, Success }

#[derive(Debug, Clone)]
enum WMsg {
    Log { level: LogLevel, message: String, tab: u8 },
    Progress { fraction: f32, partition: String },
    DeviceDetected { name: String, serial: String, slot: String },
    DeviceDisconnected,
    FlashComplete { success: bool, message: String },
    DepsResult { message: String, ok: bool },
    Flashing(bool),
    FwPath(String),
    DlProgress { percent: f32, speed: String, eta: String, downloaded: String, total: String, raw_line: String },
    Downloading(bool),
    TestStep { step: i32, status: String },
    ArbWarning { version: u32 },
    ReadyToFlash,  // device is in fastbootd, show final confirm dialog
}

#[derive(Debug)]
enum Cmd {
    CheckDevice, Flash { path: String, skip_arb: bool },
    FlashSingle { path: String, partition: Option<String>, reboot_choice: u8 },
    CancelFlash, CheckDeps, InstallDeps, Download { url: String }, Extract { path: String },
    DriverTest, RebootTo(String), CancelDownload,
    PostFlashReboot, PostFlashWipe,
    ConfirmArbAndFlash { path: String },
    RebootForFlash { reboot_choice: u8 },
    FlashFromSource { dir: String },
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

fn fw_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join("firmwares");
    std::fs::create_dir_all(&dir).ok();
    dir
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

fn do_flash(tx: &mpsc::Sender<WMsg>, source: &lfff_lib::flasher::FirmwareSource, serial: &Option<String>) {
    let sref = serial.as_deref();
    let tx_log = tx.clone();
    let tx_prog = tx.clone();
    let session = lfff_lib::flasher::run_flash_session_with_log(
        source,
        sref,
        &|msg| { tx_log.send(WMsg::Log{level:LogLevel::Info,message:msg,tab:2}).ok(); },
        &|p| {
            let fraction = if p.total > 0 { p.done as f32 / p.total as f32 } else { 0.0 };
            tx_prog.send(WMsg::Progress{fraction,partition:format!("{}_{}", p.partition, p.slot)}).ok();
        },
    );
    tx.send(WMsg::Progress{fraction:1.0,partition:String::new()}).ok();
    let failed = session.failed().len();
    let total = session.results.len();
    let crit_failed = session.critical_failed().len();
    for r in session.failed() {
        tx.send(WMsg::Log{level:LogLevel::Error,message:format!("FAILED: {}_{} — {}",r.partition,r.slot,r.error),tab:2}).ok();
    }
    let success = crit_failed == 0 && !session.aborted;
    tx.send(WMsg::FlashComplete{
        success,
        message: if success {
            format!("Done! {}/{} OK", total - failed, total)
        } else if session.aborted {
            format!("Aborted after critical failure ({} errors)", failed)
        } else {
            format!("{} errors out of {}", failed, total)
        }
    }).ok();
    tx.send(WMsg::Flashing(false)).ok();
}

fn worker(rx: mpsc::Receiver<Cmd>, tx: mpsc::Sender<WMsg>) {
    let mut serial: Option<String> = None;
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
                            log(&tx,LogLevel::Info,2,"Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(5));
                            wait_for_fastboot(&tx, 2, 90)
                        }
                        2 => {
                            log(&tx,LogLevel::Info,2,"Rebooting to fastbootd via fastboot...");
                            let _ = std::process::Command::new("fastboot").args(["reboot","fastboot"]).status();
                            log(&tx,LogLevel::Info,2,"Waiting for device to reboot...");
                            std::thread::sleep(Duration::from_secs(4));
                            wait_for_fastboot(&tx, 2, 90)
                        }
                        _ => {
                            log(&tx,LogLevel::Info,2,"Checking device is in fastbootd...");
                            wait_for_fastboot(&tx, 2, 10)
                        }
                    };
                    if ready {
                        tx.send(WMsg::ReadyToFlash).ok();
                    } else {
                        log(&tx,LogLevel::Error,2,"Device not found in fastbootd — aborting");
                        tx.send(WMsg::FlashComplete{success:false,message:"Device not found in fastbootd".into()}).ok();
                    }
                }
                Cmd::Flash{path,skip_arb}=>{
                    tx.send(WMsg::Flashing(true)).ok();
                    // Reboot already done by RebootForFlash — device is in fastbootd
                    log(&tx,LogLevel::Info,2,"Starting flash...");
                    let fw=Path::new(&path);
                    let dir=if path.ends_with(".zip"){
                        let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                        let out=fw_dir().join(&fw_name);
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
                        if !r.success{log(&tx,LogLevel::Error,2,format!("Extract fail: {}",r.error));tx.send(WMsg::FlashComplete{success:false,message:r.error}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                        log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                    }else{fw.to_path_buf()};
                    // ARB check — ARB>0 is always dangerous (raises counter, can't roll back)
                    if !skip_arb {
                        if let Some(xbl)=lfff_lib::arb::find_xbl_config(&dir){
                            let a=lfff_lib::arb::extract_arb_from_xbl(&xbl);
                            let ver = a.version.unwrap_or(0);
                            if ver > 0 {
                                // Pause and require explicit user confirmation
                                tx.send(WMsg::Flashing(false)).ok();
                                tx.send(WMsg::ArbWarning { version: ver }).ok();
                                log(&tx,LogLevel::Warn,2,format!("ARB={} — anti-rollback will be raised, waiting for confirmation...",ver));
                                continue;
                            } else {
                                log(&tx,LogLevel::Success,2,"ARB=0 — safe, no rollback counter change");
                            }
                        }
                    }
                    do_flash(&tx, &lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()), &serial);
                }
                Cmd::ConfirmArbAndFlash{path}=>{
                    // User confirmed ARB warning, proceed with flash
                    tx.send(WMsg::Flashing(true)).ok();
                    let fw=Path::new(&path);
                    let dir=if path.ends_with(".zip"){
                        let fw_name=lfff_lib::extractor::get_firmware_name(fw);
                        let out=fw_dir().join(&fw_name);
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
                        if !r.success{log(&tx,LogLevel::Error,2,format!("Extract fail: {}",r.error));tx.send(WMsg::FlashComplete{success:false,message:r.error}).ok();tx.send(WMsg::Flashing(false)).ok();continue;}
                        log(&tx,LogLevel::Success,2,format!("{} groups extracted",r.groups.len()));r.output_dir
                    }else{fw.to_path_buf()};
                    log(&tx,LogLevel::Info,2,"ARB warning confirmed by user, proceeding...");
                    do_flash(&tx, &lfff_lib::flasher::FirmwareSource::Extracted(dir.clone()), &serial);
                }
                Cmd::FlashFromSource{dir}=>{
                    tx.send(WMsg::Flashing(true)).ok();
                    log(&tx,LogLevel::Info,2,&format!("Flashing from source dir: {}",dir));
                    let d = lfff_lib::flasher::FirmwareSource::SourceBuild(std::path::PathBuf::from(&dir));
                    let images = lfff_lib::flasher::collect_images_from_source(d.path());
                    if images.is_empty() {
                        log(&tx,LogLevel::Error,2,"No flashable .img files found in the selected source directory");
                        tx.send(WMsg::FlashComplete{success:false,message:"No flashable images found".into()}).ok();
                        tx.send(WMsg::Flashing(false)).ok();
                        continue;
                    }
                    log(&tx,LogLevel::Info,2,&format!("Found {} images to flash",images.len()));
                    for (name,path) in &images {
                        let size_mb = std::fs::metadata(path).map(|m|m.len() as f64/1024.0/1024.0).unwrap_or(0.0);
                        log(&tx,LogLevel::Info,2,&format!("  {} ({:.1} MB)",name,size_mb));
                    }
                    do_flash(&tx, &d, &serial);
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
                        tx.send(WMsg::FlashComplete{success:false,message:"Device not found in fastboot".into()}).ok();
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
                    tx.send(WMsg::FlashComplete{success:fail==0,message:if fail==0{format!("{} flashed OK",p)}else{format!("{} errors",fail)}}).ok();
                    tx.send(WMsg::Flashing(false)).ok();
                }
                Cmd::CancelFlash=>log(&tx,LogLevel::Warn,2,"Cancelled"),
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
                    match std::process::Command::new(cmd).args(args).status() {
                        Ok(s) if s.success() => log(&tx,LogLevel::Success,0,format!("Reboot to {} initiated", target)),
                        _ => log(&tx,LogLevel::Error,0,format!("Failed to reboot to {}", target)),
                    }
                }
                Cmd::CheckDeps=>{
                    log(&tx,LogLevel::Info,0,"Checking dependencies...");
                    let r=lfff_lib::deps::install_dependencies(None,true);
                    for d in &r.results{
                        if d.already_installed{log(&tx,LogLevel::Success,0,format!("{}: OK",d.tool));}
                        else if d.skipped{log(&tx,LogLevel::Warn,0,format!("{}: skipped",d.tool));}
                        else if !d.error.is_empty(){log(&tx,LogLevel::Error,0,format!("{}: {}",d.tool,d.error));}
                    }
                    tx.send(WMsg::DepsResult{ok:r.all_ok(),message:if r.all_ok(){"All dependencies OK".into()}else{"Some missing".into()}}).ok();
                }
                Cmd::InstallDeps=>{
                    log(&tx,LogLevel::Info,0,"Installing dependencies...");
                    let r=lfff_lib::deps::install_dependencies(None,false);
                    for d in &r.results{
                        if d.installed{log(&tx,LogLevel::Success,0,format!("{}: installed",d.tool));}
                        else if d.already_installed{log(&tx,LogLevel::Success,0,format!("{}: already OK",d.tool));}
                        else if !d.error.is_empty(){log(&tx,LogLevel::Error,0,format!("{}: {}",d.tool,d.error));}
                    }
                    tx.send(WMsg::DepsResult{ok:r.all_ok(),message:if r.all_ok(){"All OK".into()}else{"Some failed".into()}}).ok();
                }
                Cmd::Download{url}=>{
                    tx.send(WMsg::Downloading(true)).ok();
                    log(&tx,LogLevel::Info,1,"Starting download...");
                    let tx_dl=tx.clone();
                    let token = lfff_lib::downloader::CancelToken::new();
                    *dl_cancel_token.lock().unwrap() = Some(token.clone());
                    std::thread::spawn(move||{
                        let out=fw_dir();
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
                    let fw=Path::new(&path);let out=fw_dir().join(lfff_lib::extractor::get_firmware_name(fw));
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

fn poll(w: &Weak<MainWindow>, rx: &mpsc::Receiver<WMsg>, last_dl_pct: &mut u32) {
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
            WMsg::Log{level,message,tab} => add_log(&ui,&level,tab,&message),
            WMsg::Progress{fraction,partition} => { ui.set_flash_progress(fraction); ui.set_current_partition(partition.clone().into()); if !partition.is_empty() { ui.set_flash_status(partition.into()); } }
            WMsg::DeviceDetected{name,serial,slot} => ui.set_device(DeviceInfo{connected:true,name:name.into(),serial:serial.into(),slot:slot.into()}),
            WMsg::DeviceDisconnected => ui.set_device(DeviceInfo{connected:false,name:"\u{2014}".into(),serial:"\u{2014}".into(),slot:"\u{2014}".into()}),
            WMsg::FlashComplete{success,message} => {
                ui.set_is_flashing(false);
                ui.set_flash_status(message.clone().into());
                if success { ui.set_flash_progress(1.0); }
                add_log(&ui,if success{&LogLevel::Success}else{&LogLevel::Error},2,&message);
                if success {
                    ui.set_confirm_action(7);
                    ui.set_show_confirm(true);
                }
            }
            WMsg::DepsResult{message,ok} => add_log(&ui,if ok{&LogLevel::Success}else{&LogLevel::Error},0,&message),
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
                    add_log(&ui, &LogLevel::Info, 1, msg);
                } else if !raw_line.is_empty() {
                    // Log everything except raw progress-bar lines (those update dl_percent)
                    let is_bar = raw_line.starts_with("[#") || raw_line.starts_with('#');
                    if !is_bar {
                        add_log(&ui, &LogLevel::Info, 1, &raw_line);
                    }
                }
            }
            WMsg::TestStep{step,status} => { ui.set_test_step(step); ui.set_test_status(status.into()); }
            WMsg::ArbWarning{version} => {
                add_log(&ui,&LogLevel::Warn,2,&format!("⚠ ARB={} — flashing will permanently raise the anti-rollback counter. You will NOT be able to downgrade firmware afterwards!",version));
                ui.set_arb_warning_version(version as i32);
                ui.set_show_arb_warning(true);
            }
            WMsg::ReadyToFlash => {
                ui.set_confirm_action(4);
                ui.set_show_confirm(true);
            }
        }
    }
}

fn add_log(ui: &MainWindow, l: &LogLevel, tab: u8, m: &str) {
    let mut e: Vec<LogEntry> = match tab {
        0 => ui.get_device_log(),
        1 => ui.get_download_log(),
        2 => ui.get_flash_log(),
        _ => ui.get_partition_log(),
    }.iter().collect();
    e.push(LogEntry{timestamp:ts().into(),level:SharedString::from(lvl(l)),message:SharedString::from(m)});
    if e.len()>500{e.drain(0..e.len()-500);}
    let model = ModelRc::new(VecModel::from(e));
    match tab {
        0 => ui.set_device_log(model),
        1 => ui.set_download_log(model),
        2 => ui.set_flash_log(model),
        _ => ui.set_partition_log(model),
    }
}




fn add_log_ui(ui: &MainWindow, tab: u8, msg: &str) {
    add_log(ui, &LogLevel::Info, tab, msg);
}

fn scale_path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            p.push(".config");
            p
        })
        .join("lfff/scale")
}

fn load_scale() -> f32 {
    std::fs::read_to_string(scale_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1.0_f32)
        .clamp(0.5, 2.5)
}

fn save_and_relaunch(scale: f32) {
    let p = scale_path();
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(&p, scale.to_string());
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    unsafe { std::env::set_var("LFFF_SUDO_CMD", "pkexec"); }

    let scale = load_scale();
    unsafe { std::env::set_var("SLINT_SCALE_FACTOR", scale.to_string()); }

    let ui=MainWindow::new()?;
    ui.set_ui_scale(scale);

    // Apply dark color scheme by default (user can toggle via the ☀/🌙 button in UI,
    // which writes directly to Palette.color-scheme in Slint — no Rust needed for toggle).
    // We only set the *initial* scheme here.
    // Dark theme is set via Palette.color-scheme in the .slint UI directly.
    // The toggle button handles it without Rust code.

    let(ctx,crx)=mpsc::channel::<Cmd>();
    let(mtx,mrx)=mpsc::channel::<WMsg>();
    thread::spawn(move||worker(crx,mtx));

    // Device detection
    {let t=ctx.clone();ui.on_check_device(move||{t.send(Cmd::CheckDevice).ok();});}

    // Browse firmware ZIP file only
    {let w=ui.as_weak();ui.on_browse_firmware(move||{
        if let Some(p)=rfd::FileDialog::new()
            .add_filter("Firmware",&["zip","ops","ofp"])
            .add_filter("All",&["*"])
            .set_directory(fw_dir())
            .pick_file()
        {
            if let Some(ui)=w.upgrade(){
                ui.set_firmware_path(p.display().to_string().into());
                add_log(&ui,&LogLevel::Info,1,&format!("Selected: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Browse extracted firmware folder
    {let w=ui.as_weak();ui.on_browse_folder(move||{
        if let Some(p)=rfd::FileDialog::new()
            .set_directory(fw_dir())
            .pick_folder()
        {
            if let Some(ui)=w.upgrade(){
                ui.set_firmware_path(p.display().to_string().into());
                add_log(&ui,&LogLevel::Info,2,&format!("Selected folder: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Browse Android source build directory
    {let w=ui.as_weak();ui.on_browse_source_dir(move||{
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
                add_log(&ui,&LogLevel::Info,2,&format!(
                    "Source dir selected: {} ({} images)",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    images.len()
                ));
                let mut sorted: Vec<_> = images.iter().collect();
                sorted.sort_by_key(|(k,_)| k.to_string());
                for (name,path) in &sorted {
                    let mb = std::fs::metadata(path).map(|m|m.len() as f64/1024.0/1024.0).unwrap_or(0.0);
                    add_log(&ui,&LogLevel::Info,2,&format!("  {} ({:.1} MB)",name,mb));
                }
            }
        }
    });}

    // Browse single .img
    {let w=ui.as_weak();ui.on_browse_single_image(move||{
        if let Some(p)=rfd::FileDialog::new().add_filter("Image",&["img"]).add_filter("All",&["*"]).set_directory(fw_dir()).pick_file(){
            if let Some(ui)=w.upgrade(){
                ui.set_single_image_path(p.display().to_string().into());
                add_log(&ui,&LogLevel::Info,3,&format!("Selected: {}",p.file_name().unwrap_or_default().to_string_lossy()));
            }
        }
    });}

    // Flash
    {let t=ctx.clone();let w=ui.as_weak();ui.on_start_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            t.send(Cmd::Flash{path:ui.get_firmware_path().to_string(),skip_arb:ui.get_skip_arb_check()}).ok();
        }
    });}

    // Flash from source dir (no reboot dialog needed — source mode should be simple)
    {let t=ctx.clone();let w=ui.as_weak();ui.on_start_flash_from_source(move||{
        if let Some(ui)=w.upgrade(){
            let dir = ui.get_source_dir().to_string();
            if dir.is_empty() {
                add_log(&ui,&LogLevel::Error,2,"No source directory selected");
                return;
            }
            ui.set_is_flashing(true);
            ui.set_flash_progress(0.0);
            ui.set_flash_status("Starting flash from source...".into());
            t.send(Cmd::FlashFromSource{dir}).ok();
        }
    });}

    // Reboot to target
    {let t=ctx.clone();ui.on_reboot_to(move|target|{
        t.send(Cmd::RebootTo(target.to_string())).ok();
    });}

    // Scale — save and relaunch with new scale factor
    ui.on_set_scale(move|scale|{
        save_and_relaunch(scale);
    });

    // ARB warning confirmed — continue flash
    {let t=ctx.clone();let w=ui.as_weak();ui.on_confirm_arb_and_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            t.send(Cmd::ConfirmArbAndFlash{path:ui.get_firmware_path().to_string()}).ok();
        }
    });}

    // Reboot to fastbootd before flash (step 1 -> reboot -> step 2)
    {let t=ctx.clone();let w=ui.as_weak();ui.on_reboot_for_flash(move||{
        if let Some(ui)=w.upgrade(){
            let reboot_choice = ui.get_reboot_choice() as u8;
            ui.set_show_confirm(false);
            add_log_ui(&ui, 2, "Rebooting to fastbootd...");
            t.send(Cmd::RebootForFlash{reboot_choice}).ok();
        }
    });}

    // Single flash — use partition-name if provided
    {let t=ctx.clone();let w=ui.as_weak();ui.on_start_single_flash(move||{
        if let Some(ui)=w.upgrade(){
            ui.set_is_flashing(true);ui.set_flash_progress(0.0);ui.set_flash_status("Starting...".into());
            let path=ui.get_single_image_path().to_string();
            let part=ui.get_partition_name().to_string();
            let reboot_choice = ui.get_reboot_choice() as u8;
            t.send(Cmd::FlashSingle{path,partition:if part.is_empty(){None}else{Some(part)},reboot_choice}).ok();
        }
    });}

    // Cancel
    {let t=ctx.clone();ui.on_cancel_download(move||{t.send(Cmd::CancelDownload).ok();});}
    {let t=ctx.clone();let w=ui.as_weak();ui.on_cancel_flash(move||{
        t.send(Cmd::CancelFlash).ok();
        if let Some(ui)=w.upgrade(){ui.set_is_flashing(false);ui.set_flash_status("Cancelled".into());}
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
    timer.start(slint::TimerMode::Repeated,Duration::from_millis(50),move||{
        poll(&w,&mrx,&mut last_dl_pct);
    });
    ui.run()
}
