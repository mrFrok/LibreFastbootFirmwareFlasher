//! Root: patch `init_boot` (or `boot`) with KernelSU, KernelSU Next or
//! SukiSU Ultra, using the fork's own `ksud boot-patch` from its latest
//! GitHub release.
//!
//! `ksud` is run on the PC when the release has a build for it (KernelSU and
//! KernelSU Next ship Linux musl and macOS builds). Otherwise its Android
//! build runs where Android is: locally in Termux, or on the phone over adb
//! (SukiSU ships Android builds only). The KMI is always found here, from the
//! firmware's `boot.img`: `ksud` cannot on a PC, since `init_boot` carries no
//! kernel and `uname` there names the PC's.
//!
//! The fork is the user's choice; nothing here guesses it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::utils::run_cmd;

/// The root solution, chosen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootFork {
    KernelSu,
    KernelSuNext,
    SukiSu,
}

impl RootFork {
    pub const ALL: [RootFork; 3] = [RootFork::KernelSu, RootFork::KernelSuNext, RootFork::SukiSu];

    /// GitHub `owner/repo` the releases come from.
    pub fn repo(self) -> &'static str {
        match self {
            RootFork::KernelSu => "tiann/KernelSU",
            RootFork::KernelSuNext => "KernelSU-Next/KernelSU-Next",
            RootFork::SukiSu => "SukiSU-Ultra/SukiSU-Ultra",
        }
    }

    /// Name shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            RootFork::KernelSu => "KernelSU",
            RootFork::KernelSuNext => "KernelSU Next",
            RootFork::SukiSu => "SukiSU Ultra",
        }
    }

    /// Short id used on the command line and in config.
    pub fn id(self) -> &'static str {
        match self {
            RootFork::KernelSu => "ksu",
            RootFork::KernelSuNext => "ksun",
            RootFork::SukiSu => "sukisu",
        }
    }

    pub fn from_id(id: &str) -> Option<RootFork> {
        RootFork::ALL
            .into_iter()
            .find(|f| f.id().eq_ignore_ascii_case(id.trim()))
    }
}

// ---------------------------------------------------------------------------
// Releases
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The manager app: the release's APK, not KernelSU Next's "spoofed" one.
    pub fn manager_apk(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| {
            let n = a.name.to_ascii_lowercase();
            n.ends_with(".apk") && !n.contains("spoofed")
        })
    }
}

pub fn parse_release(json: &str) -> Result<Release, String> {
    serde_json::from_str(json).map_err(|e| format!("could not read the release: {e}"))
}

/// The fork's latest release, from the GitHub API.
pub fn latest_release(fork: RootFork) -> Result<Release, String> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        fork.repo()
    );
    let r = run_cmd(
        &[
            "curl",
            "-sfL",
            "--max-time",
            "60",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
            "LibreFastbootFirmwareFlasher",
            &url,
        ],
        90,
    );
    if !r.success() {
        return Err(format!(
            "could not reach the {} releases on GitHub (curl exit {}){}",
            fork.name(),
            r.code,
            if r.stderr.is_empty() {
                String::new()
            } else {
                format!(": {}", r.stderr)
            }
        ));
    }
    parse_release(&r.stdout)
}

// ---------------------------------------------------------------------------
// Where ksud runs
// ---------------------------------------------------------------------------

/// Where `ksud boot-patch` runs, with the release asset it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchSite {
    /// The fork's build for this computer.
    Host { asset: String },
    /// Its Android build, run right here in Termux.
    Termux { asset: String },
    /// Its Android build, pushed to the phone and run there over adb.
    Device { asset: String },
}

impl PatchSite {
    pub fn asset(&self) -> &str {
        match self {
            PatchSite::Host { asset }
            | PatchSite::Termux { asset }
            | PatchSite::Device { asset } => asset,
        }
    }
}

/// The release asset name of `ksud` for a desktop OS and CPU, if the forks
/// build one there.
pub fn host_ksud_asset(os: &str, arch: &str) -> Option<String> {
    if !matches!(arch, "x86_64" | "aarch64") {
        return None;
    }
    match os {
        "linux" => Some(format!("ksud-{arch}-unknown-linux-musl")),
        "macos" => Some(format!("ksud-{arch}-apple-darwin")),
        _ => None,
    }
}

/// The Android `ksud` asset for a CPU (`aarch64`, `x86_64`).
pub fn android_ksud_asset(arch: &str) -> String {
    format!("ksud-{arch}-linux-android")
}

/// The CPU of an Android ABI as the release names it (`arm64-v8a` → `aarch64`).
pub fn arch_of_abi(abi: &str) -> Option<&'static str> {
    match abi.trim() {
        "arm64-v8a" => Some("aarch64"),
        "x86_64" => Some("x86_64"),
        _ => None,
    }
}

pub fn is_termux() -> bool {
    std::env::var_os("TERMUX_VERSION").is_some()
        || std::env::var("PREFIX").is_ok_and(|p| p.contains("com.termux"))
}

/// Pick where to patch. `device_arch` is the phone's CPU when it is in
/// Android with adb, `None` otherwise.
pub fn choose_site(
    fork: RootFork,
    release: &Release,
    os: &str,
    arch: &str,
    termux: bool,
    device_arch: Option<&str>,
) -> Result<PatchSite, String> {
    if termux {
        let asset = android_ksud_asset(arch);
        if release.asset(&asset).is_some() {
            return Ok(PatchSite::Termux { asset });
        }
    } else if let Some(asset) = host_ksud_asset(os, arch)
        && release.asset(&asset).is_some()
    {
        return Ok(PatchSite::Host { asset });
    }
    match device_arch {
        Some(a) => {
            let asset = android_ksud_asset(a);
            if release.asset(&asset).is_some() {
                Ok(PatchSite::Device { asset })
            } else {
                Err(format!(
                    "{} {} has no ksud for this phone's CPU ({a})",
                    fork.name(),
                    release.tag_name
                ))
            }
        }
        None => Err(format!(
            "{} {} has no ksud for this computer ({os}/{arch}), so the image is patched on the phone — start with the phone in Android, USB debugging on",
            fork.name(),
            release.tag_name
        )),
    }
}

// ---------------------------------------------------------------------------
// KMI
// ---------------------------------------------------------------------------

const BOOT_MAGIC: &[u8; 8] = b"ANDROID!";

/// The kernel blob of a boot image (header v0–v4).
fn boot_kernel(data: &[u8]) -> Result<&[u8], String> {
    if data.len() < 44 || &data[..8] != BOOT_MAGIC {
        return Err("not an Android boot image".into());
    }
    let u32_at = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let kernel_size = u32_at(8) as usize;
    let header_version = u32_at(40);
    // v3+ have a fixed 4096-byte page; v0–v2 store it in the header.
    let page = if header_version >= 3 {
        4096
    } else {
        u32_at(36) as usize
    };
    if kernel_size == 0 {
        return Err("the boot image has no kernel".into());
    }
    if page == 0 || page > 1 << 20 {
        return Err("the boot image header is damaged".into());
    }
    data.get(page..page + kernel_size)
        .ok_or_else(|| "the boot image is cut short".into())
}

/// The kernel, decompressed if it is gzip or lz4 (legacy or frame).
fn decompress_kernel(k: &[u8]) -> Result<Vec<u8>, String> {
    if k.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(k)
            .read_to_end(&mut out)
            .map_err(|e| format!("could not unpack the gzip kernel: {e}"))?;
        return Ok(out);
    }
    if k.starts_with(&[0x02, 0x21, 0x4c, 0x18]) {
        return lz4_legacy(k);
    }
    if k.starts_with(&[0x04, 0x22, 0x4d, 0x18]) {
        let mut out = Vec::new();
        lz4_flex::frame::FrameDecoder::new(k)
            .read_to_end(&mut out)
            .map_err(|e| format!("could not unpack the lz4 kernel: {e}"))?;
        return Ok(out);
    }
    Ok(k.to_vec())
}

/// lz4 legacy format (`lz4 -l`): the magic, then blocks of up to 8 MiB, each
/// after its compressed size. Another magic may start a new stream.
fn lz4_legacy(data: &[u8]) -> Result<Vec<u8>, String> {
    const MAGIC: u32 = 0x184C_2102;
    const BLOCK: usize = 8 << 20;
    let mut out = Vec::new();
    let mut pos = 4;
    while pos + 4 <= data.len() {
        let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        pos += 4;
        if size == MAGIC {
            continue;
        }
        let size = size as usize;
        let Some(block) = data.get(pos..pos + size) else {
            // Trailing padding after the last block.
            break;
        };
        pos += size;
        let mut buf = vec![0u8; BLOCK];
        let n = lz4_flex::block::decompress_into(block, &mut buf)
            .map_err(|e| format!("could not unpack the lz4 kernel: {e}"))?;
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// The KMI of a kernel release string: `6.12.23-android16-5-o-g…` →
/// `android16-6.12`. `None` for a kernel that is not GKI.
pub fn parse_kmi(release: &str) -> Option<String> {
    let mut nums = release.split(|c: char| !c.is_ascii_digit());
    let major = nums.next().filter(|s| !s.is_empty())?;
    let minor = nums.next().filter(|s| !s.is_empty())?;
    if !release.starts_with(&format!("{major}.{minor}")) {
        return None;
    }
    let at = release.find("android")?;
    let digits: String = release[at + 7..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(format!("android{digits}-{major}.{minor}"))
}

/// The KMI from the `Linux version …` banner inside a kernel image.
pub fn kmi_from_kernel(kernel: &[u8]) -> Option<String> {
    const BANNER: &[u8] = b"Linux version ";
    let mut from = 0;
    while let Some(i) = find(&kernel[from..], BANNER) {
        let start = from + i + BANNER.len();
        let end = kernel[start..]
            .iter()
            .position(|&b| b == b' ' || b == 0 || b == b'\n')
            .map_or(kernel.len(), |e| start + e);
        if let Ok(rel) = std::str::from_utf8(&kernel[start..end])
            && let Some(kmi) = parse_kmi(rel)
        {
            return Some(kmi);
        }
        from = start;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The KMI of the kernel in a boot image.
pub fn kmi_from_boot_image(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let kernel = decompress_kernel(boot_kernel(&data)?)?;
    kmi_from_kernel(&kernel).ok_or_else(|| {
        "the kernel in boot.img is not a GKI kernel (no androidNN in its version), so KernelSU's LKM cannot be used".into()
    })
}

/// `ksud boot-info supported-kmis` output → KMIs (`aarch64/android16-6.12`
/// and `android16-6.12` both read as `android16-6.12`).
pub fn parse_supported_kmis(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out
        .lines()
        .map(|l| l.trim().rsplit('/').next().unwrap_or("").to_string())
        .filter(|l| l.starts_with("android"))
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

fn root_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lfff")
        .join("root")
}

fn safe_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Download a release asset into the cache, once per tag.
pub fn ensure_asset(
    fork: RootFork,
    release: &Release,
    name: &str,
    on_log: &dyn Fn(String),
) -> Result<PathBuf, String> {
    let asset = release
        .asset(name)
        .ok_or_else(|| format!("{} {} has no {name}", fork.name(), release.tag_name))?;
    let dir = root_cache_dir()
        .join(fork.id())
        .join(safe_component(&release.tag_name));
    fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let dest = dir.join(safe_component(name));
    if dest.metadata().is_ok_and(|m| m.len() > 0) {
        return Ok(dest);
    }
    on_log(format!(
        "Downloading {} {}: {name}",
        fork.name(),
        release.tag_name
    ));
    let part = dir.join(format!("{}.part", safe_component(name)));
    let part_s = part.to_string_lossy().to_string();
    let r = run_cmd(
        &[
            "curl",
            "-sfL",
            "--max-time",
            "600",
            "-o",
            &part_s,
            &asset.browser_download_url,
        ],
        660,
    );
    if !r.success() || !part.metadata().is_ok_and(|m| m.len() > 0) {
        let _ = fs::remove_file(&part);
        return Err(format!("could not download {name} (curl exit {})", r.code));
    }
    fs::rename(&part, &dest).map_err(|e| format!("could not save {name}: {e}"))?;
    Ok(dest)
}

#[cfg(unix)]
fn set_exec(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("could not make {} executable: {e}", p.display()))
}

#[cfg(not(unix))]
fn set_exec(_p: &Path) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// adb
// ---------------------------------------------------------------------------

fn adb_args<'a>(serial: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["adb"];
    if let Some(s) = serial {
        v.extend(["-s", s]);
    }
    v.extend_from_slice(args);
    v
}

/// The phone's CPU as the releases name it, when it is in Android with adb.
pub fn device_arch(serial: Option<&str>) -> Option<&'static str> {
    let r = run_cmd(
        &adb_args(serial, &["shell", "getprop", "ro.product.cpu.abi"]),
        15,
    );
    if r.success() {
        arch_of_abi(&r.stdout)
    } else {
        None
    }
}

/// The KMI of the kernel the phone runs now (`uname -r` over adb).
pub fn device_kmi(serial: Option<&str>) -> Option<String> {
    let r = run_cmd(&adb_args(serial, &["shell", "uname", "-r"]), 15);
    if r.success() {
        parse_kmi(r.stdout.trim())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Patching
// ---------------------------------------------------------------------------

/// A patched image ready to flash.
#[derive(Debug, Clone)]
pub struct Patched {
    /// `init_boot` or `boot`.
    pub partition: String,
    pub image: PathBuf,
    pub kmi: String,
    pub fork: RootFork,
    pub release: Release,
}

const DEVICE_DIR: &str = "/data/local/tmp/lfff-root";

/// ksud's output, without its ASCII-art banner (lines with no letter or digit).
fn log_output(text: &str, on_log: &dyn Fn(String)) {
    for line in text
        .lines()
        .map(str::trim_end)
        .filter(|l| l.chars().any(|c| c.is_ascii_alphanumeric()))
    {
        on_log(format!("  ksud: {line}"));
    }
}

/// Which image to patch: `init_boot` when the firmware has one (GKI on
/// Android 13+), `boot` otherwise.
pub fn root_target(names: &[&str]) -> Option<&'static str> {
    if names.contains(&"init_boot") {
        Some("init_boot")
    } else if names.contains(&"boot") {
        Some("boot")
    } else {
        None
    }
}

/// Patch the firmware's `init_boot` (or `boot`) for `fork`. `images` is the
/// firmware's partition → image map ([`crate::flasher::collect_images`]);
/// `serial` the phone, used only when it is in Android with adb (for SukiSU,
/// and for the KMI when the firmware has no `boot.img`). Nothing is flashed.
pub fn patch_firmware(
    fork: RootFork,
    images: &std::collections::HashMap<String, PathBuf>,
    serial: Option<&str>,
    on_log: &dyn Fn(String),
) -> Result<Patched, String> {
    let names: Vec<&str> = images.keys().map(String::as_str).collect();
    let partition = root_target(&names)
        .ok_or("the firmware has neither init_boot.img nor boot.img to patch")?;
    let source = &images[partition];

    let kmi = match images.get("boot") {
        Some(boot) => kmi_from_boot_image(boot)?,
        None => device_kmi(serial).ok_or(
            "the firmware has no boot.img to read the kernel version from, and the phone is not in Android with adb to ask it",
        )?,
    };
    on_log(format!("Kernel: {kmi}"));

    let release = latest_release(fork)?;
    on_log(format!("{} {}", fork.name(), release.tag_name));
    let termux = is_termux();
    let dev_arch = if termux { None } else { device_arch(serial) };
    let site = choose_site(
        fork,
        &release,
        std::env::consts::OS,
        std::env::consts::ARCH,
        termux,
        dev_arch,
    )?;
    let ksud = ensure_asset(fork, &release, site.asset(), on_log)?;

    let out_dir = root_cache_dir().join("patched");
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
    let out_name = format!(
        "{partition}-{}-{}-{kmi}.img",
        fork.id(),
        safe_component(&release.tag_name)
    );
    let out = out_dir.join(&out_name);
    let _ = fs::remove_file(&out);

    match &site {
        PatchSite::Host { .. } | PatchSite::Termux { .. } => {
            set_exec(&ksud)?;
            let ksud_s = ksud.to_string_lossy().to_string();
            check_kmi_supported(
                fork,
                &release,
                &kmi,
                &run_cmd(&[&ksud_s, "boot-info", "supported-kmis"], 30).stdout,
            )?;
            on_log(format!(
                "Patching {partition} with {} on this computer...",
                fork.name()
            ));
            let src = source.to_string_lossy().to_string();
            let dir = out_dir.to_string_lossy().to_string();
            let mut args = vec![
                ksud_s.as_str(),
                "boot-patch",
                "-b",
                &src,
                "--kmi",
                &kmi,
                "-o",
                &dir,
                "--out-name",
                &out_name,
            ];
            // Only the desktop builds take --arch (the phone's, not the PC's).
            if matches!(site, PatchSite::Host { .. }) {
                args.extend(["--arch", "aarch64"]);
            }
            let r = run_cmd(&args, 300);
            log_output(&r.stdout, on_log);
            log_output(&r.stderr, on_log);
            if !r.success() {
                return Err(format!("ksud boot-patch failed (exit {})", r.code));
            }
        }
        PatchSite::Device { .. } => {
            on_log(format!(
                "Patching {partition} with {} on the phone...",
                fork.name()
            ));
            patch_on_device(fork, &release, serial, &ksud, source, &kmi, &out, on_log)?;
        }
    }

    if !out.metadata().is_ok_and(|m| m.len() > 0) {
        return Err("ksud finished but wrote no patched image".into());
    }
    on_log(format!("Patched {partition}: {}", out.display()));
    Ok(Patched {
        partition: partition.to_string(),
        image: out,
        kmi,
        fork,
        release,
    })
}

fn check_kmi_supported(
    fork: RootFork,
    release: &Release,
    kmi: &str,
    supported_out: &str,
) -> Result<(), String> {
    let supported = parse_supported_kmis(supported_out);
    // An empty list means this ksud could not say; let boot-patch decide.
    if !supported.is_empty() && !supported.iter().any(|k| k == kmi) {
        return Err(format!(
            "{} {} has no module for {kmi} (it supports {})",
            fork.name(),
            release.tag_name,
            supported.join(", ")
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn patch_on_device(
    fork: RootFork,
    release: &Release,
    serial: Option<&str>,
    ksud: &Path,
    source: &Path,
    kmi: &str,
    out: &Path,
    on_log: &dyn Fn(String),
) -> Result<(), String> {
    let step = |args: &[&str], timeout: u64, what: &str| -> Result<String, String> {
        let r = run_cmd(&adb_args(serial, args), timeout);
        if r.success() {
            Ok(r.stdout)
        } else {
            Err(format!("{what} failed: {}", r.stderr.trim()))
        }
    };
    let ksud_s = ksud.to_string_lossy().to_string();
    let src_s = source.to_string_lossy().to_string();
    let out_s = out.to_string_lossy().to_string();
    let dev_ksud = format!("{DEVICE_DIR}/ksud");
    let dev_in = format!("{DEVICE_DIR}/in.img");
    let dev_out = format!("{DEVICE_DIR}/out.img");

    step(
        &["shell", "rm", "-rf", DEVICE_DIR],
        15,
        "clearing the phone's work folder",
    )?;
    step(
        &["shell", "mkdir", "-p", DEVICE_DIR],
        15,
        "creating the phone's work folder",
    )?;
    let result = (|| {
        step(
            &["push", &ksud_s, &dev_ksud],
            120,
            "copying ksud to the phone",
        )?;
        step(
            &["push", &src_s, &dev_in],
            300,
            "copying the image to the phone",
        )?;
        step(&["shell", "chmod", "755", &dev_ksud], 15, "chmod ksud")?;
        let supported = run_cmd(
            &adb_args(serial, &["shell", &dev_ksud, "boot-info", "supported-kmis"]),
            30,
        );
        check_kmi_supported(fork, release, kmi, &supported.stdout)?;
        let r = run_cmd(
            &adb_args(
                serial,
                &[
                    "shell",
                    &dev_ksud,
                    "boot-patch",
                    "-b",
                    &dev_in,
                    "--kmi",
                    kmi,
                    "-o",
                    DEVICE_DIR,
                    "--out-name",
                    "out.img",
                ],
            ),
            300,
        );
        log_output(&r.stdout, on_log);
        log_output(&r.stderr, on_log);
        // adb shell passes the exit code through since Android 7.
        if !r.success() {
            return Err(format!(
                "ksud boot-patch on the phone failed (exit {})",
                r.code
            ));
        }
        step(
            &["pull", &dev_out, &out_s],
            300,
            "copying the patched image back",
        )?;
        Ok(())
    })();
    let _ = run_cmd(&adb_args(serial, &["shell", "rm", "-rf", DEVICE_DIR]), 15);
    result
}

/// Install the fork's manager app from the same release over adb.
pub fn install_manager(
    fork: RootFork,
    release: &Release,
    serial: Option<&str>,
    on_log: &dyn Fn(String),
) -> Result<(), String> {
    let apk = release
        .manager_apk()
        .ok_or_else(|| format!("{} {} has no manager APK", fork.name(), release.tag_name))?
        .name
        .clone();
    let path = ensure_asset(fork, release, &apk, on_log)?;
    on_log(format!("Installing the {} manager...", fork.name()));
    let p = path.to_string_lossy().to_string();
    let r = run_cmd(&adb_args(serial, &["install", "-r", &p]), 300);
    if r.success() {
        Ok(())
    } else {
        Err(format!(
            "adb install failed: {}",
            if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            }
            .trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const RELEASE_JSON: &str = r#"{"tag_name":"v3.4.2","assets":[
        {"name":"KernelSU_Next_v3.4.2-spoofed_33355-release.apk","browser_download_url":"https://x/s.apk"},
        {"name":"KernelSU_Next_v3.4.2_33355-release.apk","browser_download_url":"https://x/a.apk"},
        {"name":"ksud-aarch64-linux-android","browser_download_url":"https://x/da"},
        {"name":"ksud-x86_64-unknown-linux-musl","browser_download_url":"https://x/lx"},
        {"name":"ksud-aarch64-apple-darwin","browser_download_url":"https://x/ma"}]}"#;

    fn release() -> Release {
        parse_release(RELEASE_JSON).unwrap()
    }

    fn sukisu_release() -> Release {
        parse_release(
            r#"{"tag_name":"v4.3.0","assets":[{"name":"ksud-aarch64-linux-android","browser_download_url":"u"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn kmi_is_read_from_the_kernel_release() {
        assert_eq!(
            parse_kmi("6.12.23-android16-5-o-g1234abcd").as_deref(),
            Some("android16-6.12")
        );
        assert_eq!(
            parse_kmi("5.10.198-android12-9-00085-g226a9632f13d").as_deref(),
            Some("android12-5.10")
        );
        assert_eq!(parse_kmi("7.2.15-zen1-1-zen"), None);
        assert_eq!(parse_kmi("4.19.157-perf+"), None);
    }

    fn kernel_with_banner() -> Vec<u8> {
        let mut k = vec![0u8; 3000];
        k.extend_from_slice(b"Linux version %s (%s)\0");
        k.extend_from_slice(
            b"Linux version 6.12.69-android16-5-g8a1b (build@host) #1 SMP PREEMPT\n\0",
        );
        k.extend(vec![0u8; 5000]);
        k
    }

    fn boot_v4(kernel: &[u8]) -> Vec<u8> {
        let mut img = vec![0u8; 4096];
        img[..8].copy_from_slice(BOOT_MAGIC);
        img[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
        img[40..44].copy_from_slice(&4u32.to_le_bytes());
        img.extend_from_slice(kernel);
        img.extend(vec![0u8; 4096]);
        img
    }

    fn kmi_of(img: &[u8]) -> Result<String, String> {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(img).unwrap();
        kmi_from_boot_image(f.path())
    }

    #[test]
    fn kmi_from_a_raw_kernel_in_boot_v4() {
        assert_eq!(
            kmi_of(&boot_v4(&kernel_with_banner())).unwrap(),
            "android16-6.12"
        );
    }

    #[test]
    fn kmi_from_a_gzip_kernel() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&kernel_with_banner()).unwrap();
        assert_eq!(
            kmi_of(&boot_v4(&gz.finish().unwrap())).unwrap(),
            "android16-6.12"
        );
    }

    #[test]
    fn kmi_from_an_lz4_legacy_kernel() {
        let block = lz4_flex::block::compress(&kernel_with_banner());
        let mut lz = vec![0x02, 0x21, 0x4c, 0x18];
        lz.extend_from_slice(&(block.len() as u32).to_le_bytes());
        lz.extend_from_slice(&block);
        assert_eq!(kmi_of(&boot_v4(&lz)).unwrap(), "android16-6.12");
    }

    #[test]
    fn kmi_from_an_old_header_uses_its_page_size() {
        let k = kernel_with_banner();
        let mut img = vec![0u8; 2048];
        img[..8].copy_from_slice(BOOT_MAGIC);
        img[8..12].copy_from_slice(&(k.len() as u32).to_le_bytes());
        img[36..40].copy_from_slice(&2048u32.to_le_bytes());
        img.extend_from_slice(&k);
        assert_eq!(kmi_of(&img).unwrap(), "android16-6.12");
    }

    #[test]
    fn a_non_gki_kernel_or_a_non_boot_image_is_refused() {
        let mut k = vec![0u8; 100];
        k.extend_from_slice(b"Linux version 4.19.157-perf+ (x) #1\0");
        assert!(kmi_of(&boot_v4(&k)).is_err());
        assert!(kmi_of(b"not a boot image at all, really not").is_err());
    }

    #[test]
    fn supported_kmis_are_read_with_or_without_arch() {
        assert_eq!(
            parse_supported_kmis(
                "aarch64/android16-6.12\nandroid15-6.6\n\nx86_64/android16-6.12\n"
            ),
            vec!["android15-6.6", "android16-6.12"]
        );
    }

    #[test]
    fn the_manager_is_the_plain_apk() {
        assert_eq!(
            release().manager_apk().unwrap().name,
            "KernelSU_Next_v3.4.2_33355-release.apk"
        );
    }

    #[test]
    fn ksud_runs_on_the_pc_when_the_fork_builds_for_it() {
        let r = release();
        assert_eq!(
            choose_site(RootFork::KernelSuNext, &r, "linux", "x86_64", false, None),
            Ok(PatchSite::Host {
                asset: "ksud-x86_64-unknown-linux-musl".into()
            })
        );
        assert_eq!(
            choose_site(RootFork::KernelSuNext, &r, "macos", "aarch64", false, None),
            Ok(PatchSite::Host {
                asset: "ksud-aarch64-apple-darwin".into()
            })
        );
    }

    #[test]
    fn sukisu_is_patched_on_the_phone_or_in_termux() {
        let r = sukisu_release();
        assert_eq!(
            choose_site(
                RootFork::SukiSu,
                &r,
                "linux",
                "x86_64",
                false,
                Some("aarch64")
            ),
            Ok(PatchSite::Device {
                asset: "ksud-aarch64-linux-android".into()
            })
        );
        assert_eq!(
            choose_site(RootFork::SukiSu, &r, "linux", "aarch64", true, None),
            Ok(PatchSite::Termux {
                asset: "ksud-aarch64-linux-android".into()
            })
        );
        assert!(choose_site(RootFork::SukiSu, &r, "linux", "x86_64", false, None).is_err());
    }

    #[test]
    fn the_image_to_patch_is_init_boot_first() {
        assert_eq!(
            root_target(&["boot", "init_boot", "vendor_boot"]),
            Some("init_boot")
        );
        assert_eq!(root_target(&["boot", "dtbo"]), Some("boot"));
        assert_eq!(root_target(&["system"]), None);
    }

    #[test]
    fn forks_are_named_by_id() {
        for f in RootFork::ALL {
            assert_eq!(RootFork::from_id(f.id()), Some(f));
        }
        assert_eq!(RootFork::from_id("SukiSU"), Some(RootFork::SukiSu));
        assert_eq!(RootFork::from_id("magisk"), None);
    }

    #[test]
    fn an_unsupported_kmi_is_refused_before_patching() {
        let r = release();
        assert!(
            check_kmi_supported(RootFork::KernelSu, &r, "android16-6.12", "android16-6.12\n")
                .is_ok()
        );
        assert!(
            check_kmi_supported(RootFork::KernelSu, &r, "android11-5.4", "android16-6.12\n")
                .is_err()
        );
        assert!(check_kmi_supported(RootFork::KernelSu, &r, "android11-5.4", "").is_ok());
    }
}
