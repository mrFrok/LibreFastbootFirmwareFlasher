//! Automatic dependency installer for LFFF.
//!
//! Handles:
//!   - android-tools (fastboot + adb) via system package manager
//!   - aria2c via system package manager
//!   - payload_dumper via `cargo install payload_dumper`
//!
//! Supported package managers: pacman, apt, dnf, zypper, emerge, brew,
//!                        xbps (Void), apk (Alpine), rpm-ostree (atomic Fedora),
//!                        nix profile / nix-env (NixOS)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Return the package name for a given tool on a given package manager.
/// Locate brew binary on macOS (may be outside PATH when launched from .app).
fn find_brew() -> Option<String> {
    if which::which("brew").is_ok() {
        return Some("brew".into());
    }
    for p in &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"] {
        if Path::new(p).is_file() {
            return Some((*p).into());
        }
    }
    None
}

fn pkg_for_tool(pm: &str, tool: &str) -> Option<&'static str> {
    match (pm, tool) {
        ("pacman", "fastboot") | ("pacman", "adb") => Some("android-tools"),
        ("pacman", "aria2c") => Some("aria2"),
        ("apt", "fastboot") => Some("android-tools-fastboot"),
        ("apt", "adb") => Some("android-tools-adb"),
        ("apt", "aria2c") => Some("aria2"),
        ("dnf", "fastboot") | ("dnf", "adb") => Some("android-tools"),
        ("dnf", "aria2c") => Some("aria2"),
        ("zypper", "fastboot") | ("zypper", "adb") => Some("android-tools"),
        ("zypper", "aria2c") => Some("aria2"),
        ("emerge", "fastboot") | ("emerge", "adb") => Some("dev-util/android-tools"),
        ("emerge", "aria2c") => Some("net-misc/aria2"),
        ("brew", "fastboot") | ("brew", "adb") => Some("android-platform-tools"),
        ("brew", "aria2c") => Some("aria2"),
        ("xbps", "fastboot") | ("xbps", "adb") => Some("android-tools"),
        ("xbps", "aria2c") => Some("aria2"),
        ("apk", "fastboot") | ("apk", "adb") => Some("android-tools"),
        ("apk", "aria2c") => Some("aria2"),
        ("rpm-ostree", "fastboot") | ("rpm-ostree", "adb") => Some("android-tools"),
        ("rpm-ostree", "aria2c") => Some("aria2"),
        ("nix-profile", "fastboot") | ("nix-profile", "adb") => Some("android-tools"),
        ("nix-profile", "aria2c") => Some("aria2"),
        ("nix-env", "fastboot") | ("nix-env", "adb") => Some("android-tools"),
        ("nix-env", "aria2c") => Some("aria2"),
        _ => None,
    }
}

/// Return the install command prefix for a package manager.
fn install_cmd(pm: &str) -> Vec<&'static str> {
    match pm {
        "pacman" => vec!["pacman", "-S", "--noconfirm"],
        "apt" => vec!["apt-get", "install", "-y"],
        "dnf" => vec!["dnf", "install", "-y"],
        "zypper" => vec!["zypper", "install", "-y"],
        "emerge" => vec!["emerge", "--noreplace", "--quiet"],
        "brew" => vec!["brew", "install"],
        "xbps" => vec!["xbps-install", "-y"],
        "apk" => vec!["apk", "add"],
        "rpm-ostree" => vec!["rpm-ostree", "install", "-y"],
        "nix-profile" => vec!["nix", "profile", "install"],
        "nix-env" => vec!["nix-env", "-iA"],
        _ => vec![],
    }
}

const PDG_BINARY: &str = "payload_dumper";

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Result for a single tool installation attempt.
#[derive(Debug, Clone)]
pub struct DepResult {
    pub tool: String,
    pub already_installed: bool,
    pub installed: bool,
    pub skipped: bool,
    pub error: String,
}

impl DepResult {
    pub fn ok(&self) -> bool {
        self.already_installed || self.installed
    }
}

/// Aggregate report for all dependency checks.
#[derive(Debug, Clone)]
pub struct DepsReport {
    pub results: Vec<DepResult>,
}

impl DepsReport {
    pub fn all_ok(&self) -> bool {
        self.results.iter().all(|r| r.ok())
    }

    pub fn failed(&self) -> Vec<&DepResult> {
        self.results
            .iter()
            .filter(|r| !r.ok() && !r.skipped)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Package manager detection
// ---------------------------------------------------------------------------

/// Return true if running on an atomic/immutable distro.
fn is_atomic_distro() -> bool {
    if std::env::consts::OS != "linux" {
        return false;
    }

    let atomic_ids = [
        "silverblue",
        "kinoite",
        "sericea",
        "onyx",
        "bazzite",
        "aurora",
        "bluefin",
        "nixos",
        "guix",
        "vanillaos",
        "carbonos",
        "steamos",
    ];

    if let Ok(content) = fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if let Some(id) = line.strip_prefix("ID=") {
                let id = id.trim().trim_matches('"').to_lowercase();
                if atomic_ids.contains(&id.as_str()) {
                    return true;
                }
            }
            if let Some(id_like) = line.strip_prefix("ID_LIKE=") {
                let id_like = id_like.trim().trim_matches('"').to_lowercase();
                for part in id_like.split_whitespace() {
                    if atomic_ids.contains(&part) {
                        return true;
                    }
                }
            }
        }
    }

    Path::new("/ostree").is_dir()
        || Path::new("/etc/NIXOS").exists()
        || which::which("rpm-ostree").is_ok()
}

/// Detect the best available package manager.
fn detect_pkg_manager() -> Option<String> {
    // macOS has exactly one candidate, and it must be decided before the
    // Linux list below is consulted: macOS ships its own /usr/bin/apt, a
    // leftover Java stub that has nothing to do with Debian, so probing for
    // `apt` there "found" it and ran `sudo apt-get install` on a Mac.
    // find_brew also checks Homebrew's default prefixes, which a GUI started
    // from Finder may not have on its PATH.
    if cfg!(target_os = "macos") {
        return find_brew().map(|_| "brew".into());
    }

    if is_atomic_distro() {
        // Atomic distros: try native layering, then modern Nix, then legacy Nix, then per-user installers
        for pm in &["rpm-ostree", "nix", "nix-env", "brew"] {
            if which::which(pm).is_ok() {
                // "nix" binary means nix profile is available (modern Nix)
                if *pm == "nix" {
                    return Some("nix-profile".into());
                }
                return Some((*pm).into());
            }
        }
        return None;
    }

    for pm in &[
        "pacman",
        "apt",
        "apt-get",
        "dnf",
        "zypper",
        "emerge",
        "xbps-install",
        "apk",
        "brew",
    ] {
        if which::which(pm).is_ok() {
            return Some(
                match *pm {
                    "apt-get" => "apt",
                    "xbps-install" => "xbps",
                    other => other,
                }
                .to_string(),
            );
        }
    }

    None
}

fn sudo_cmd() -> String {
    std::env::var("LFFF_SUDO_CMD").unwrap_or_else(|_| "sudo".to_string())
}

/// Run a command with stdout/stderr streamed line-by-line to `on_log`, so
/// package-manager output is visible live in both the CLI and the GUI log.
/// sudo/pkexec password prompts still work — they use /dev/tty or a
/// graphical agent, not the piped descriptors.
fn run_streaming(mut cmd: Command, on_log: &dyn Fn(String)) -> bool {
    use std::io::BufRead;
    use std::process::Stdio;
    use std::sync::mpsc;

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            on_log(format!("Failed to run command: {}", e));
            return false;
        }
    };

    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                tx.send(line).ok();
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                tx.send(line).ok();
            }
        }));
    }
    drop(tx);
    // `on_log` is not Send — forward lines on this thread; the loop ends when
    // both reader threads finish (child closed its pipes).
    for line in rx {
        let t = line.trim();
        if !t.is_empty() {
            on_log(t.to_string());
        }
    }
    for r in readers {
        r.join().ok();
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn needs_sudo(pm: &str) -> bool {
    // Per-user package managers don't need sudo
    if matches!(pm, "brew" | "nix-profile" | "nix-env") {
        return false;
    }
    // Check if running as root via env
    std::env::var("USER").map(|u| u != "root").unwrap_or(true)
}

// ---------------------------------------------------------------------------
// payload_dumper installer (GitHub releases binary)
// ---------------------------------------------------------------------------

const PDG_REPO: &str = "rhythmcache/payload-dumper-rust";

/// The payload-dumper-rust release LFFF installs, pinned rather than "latest":
/// the binary is run straight away and copied into /usr/local/bin with sudo,
/// so a compromised upstream release must not become code running on every
/// machine that clicks "install dependencies".
///
/// Kept identical to `payloadDumperVersion` and its hashes in flake.nix — the
/// same zip files — and a test fails if the two drift apart.
const PDG_VERSION: &str = "0.8.4";

/// `(os, arch, asset, sha256)` for each platform with a prebuilt binary.
const PDG_ASSETS: &[(&str, &str, &str, &str)] = &[
    (
        "linux",
        "x86_64",
        "payload_dumper-linux-x86_64.zip",
        "d2a7cf4d8ef4daa73ebd34e0c6f77eb6c116a1745a1b31ad897e9ce2b2f54151",
    ),
    (
        "linux",
        "aarch64",
        "payload_dumper-linux-aarch64.zip",
        "fb95a825910601f0d07c72f1b614bf736774e64e61e6fea7da98e4dec5bba238",
    ),
    (
        "macos",
        "x86_64",
        "payload_dumper-macos-x86_64.zip",
        "08115533d79420e2a82ff6bbc5e3f02680358cd7cc7722c1f45eeeac48d81adb",
    ),
    (
        "macos",
        "aarch64",
        "payload_dumper-macos-aarch64.zip",
        "b0cf521edd8ab43b0d32e4b2ab70b2635abc559e4055045b35db7632611a1222",
    ),
];

/// Escape hatch for distros that ship a foreign `payload_dumper`: point this at
/// the payload-dumper-rust binary and every lookup below honours it.
pub const PDG_ENV_OVERRIDE: &str = "LFFF_PAYLOAD_DUMPER";

/// Is this candidate payload-dumper-rust, or something else wearing the name?
///
/// `payload_dumper` is not a unique binary name. nixpkgs ships an unrelated
/// 2022-era Python script under it (vm03/payload_dumper): different CLI
/// (`--out`/`--images`, raw payload.bin only, no ZIP input), and generated
/// protobuf stubs that abort on protobuf >= 4 with "Descriptors cannot be
/// created directly". Running it by mistake fails deep inside extraction with
/// an error that reads like ours.
///
/// payload-dumper-rust is a clap program: `--version` exits 0 and prints a
/// banner naming the project. The Python script has no such flag — argparse
/// exits 2 with an empty stdout.
fn is_payload_dumper_rust(bin: &Path) -> bool {
    match Command::new(bin).arg("--version").output() {
        Ok(out) => version_banner_is_rust(out.status.success(), &out.stdout),
        Err(_) => false,
    }
}

/// The decision [`is_payload_dumper_rust`] makes about a `--version` result,
/// split out so it can be tested without writing a script and executing it:
/// doing that from a test thread races with process spawns on other threads
/// and intermittently fails with ETXTBSY.
fn version_banner_is_rust(success: bool, stdout: &[u8]) -> bool {
    if !success {
        return false;
    }
    let banner = String::from_utf8_lossy(stdout);
    banner.contains("payload-dumper-rust") || banner.starts_with("payload_dumper ")
}

/// Locate the payload-dumper-rust binary.
///
/// The copy LFFF installed itself is searched before $PATH: where a foreign
/// `payload_dumper` occupies the name, $PATH resolves to the wrong program and
/// `lfff deps` would otherwise install a binary that is never picked up.
pub fn find_payload_dumper_rust() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os(PDG_ENV_OVERRIDE) {
        candidates.push(PathBuf::from(p));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(Path::new(&home).join(".local/bin").join(PDG_BINARY));
    }
    candidates.push(Path::new("/usr/local/bin").join(PDG_BINARY));
    if let Ok(p) = which::which(PDG_BINARY) {
        candidates.push(p);
    }

    let mut probed: Vec<PathBuf> = Vec::new();
    for cand in candidates {
        if !cand.is_file() {
            continue;
        }
        // The same binary can be reached through several of the paths above.
        let real = cand.canonicalize().unwrap_or_else(|_| cand.clone());
        if probed.contains(&real) {
            continue;
        }
        probed.push(real);
        if is_payload_dumper_rust(&cand) {
            return Some(cand);
        }
        tracing::debug!("{} is not payload-dumper-rust — skipping", cand.display());
    }
    None
}

/// payload-dumper-go, the other dumper LFFF can drive.
///
/// It works, but only through the slower route: it takes a raw payload.bin, so
/// the OTA zip has to be unpacked first, and archives whose ZIP index our
/// reader rejects cannot be recovered at all.
pub fn find_payload_dumper_go() -> Option<PathBuf> {
    which::which("payload-dumper-go").ok()
}

/// A payload dumper LFFF can use, preferring the one it drives best.
#[derive(Debug, Clone)]
pub enum PayloadDumper {
    /// payload-dumper-rust — reads the OTA zip directly.
    Rust(PathBuf),
    /// payload-dumper-go — needs a raw payload.bin.
    Go(PathBuf),
}

impl PayloadDumper {
    pub fn path(&self) -> &Path {
        match self {
            Self::Rust(p) | Self::Go(p) => p,
        }
    }
    pub fn into_path(self) -> PathBuf {
        match self {
            Self::Rust(p) | Self::Go(p) => p,
        }
    }
    pub fn is_rust(&self) -> bool {
        matches!(self, Self::Rust(_))
    }
}

/// Find whichever payload dumper is available, best first.
pub fn find_payload_dumper() -> Option<PayloadDumper> {
    if let Some(p) = find_payload_dumper_rust() {
        return Some(PayloadDumper::Rust(p));
    }
    find_payload_dumper_go().map(PayloadDumper::Go)
}

/// A `payload_dumper` in $PATH that is *not* payload-dumper-rust. Turns
/// "no payload dumper found" into a message that names the actual problem.
pub fn foreign_payload_dumper() -> Option<PathBuf> {
    let p = which::which(PDG_BINARY).ok()?;
    (!is_payload_dumper_rust(&p)).then_some(p)
}

/// Check if payload_dumper is runnable.
fn pdg_is_runnable() -> bool {
    find_payload_dumper_rust().is_some()
}

/// ~/.local/bin, but only when it is already in $PATH — installing there
/// otherwise produces a binary nothing can find.
fn local_bin_in_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let lb = Path::new(&home).join(".local/bin");
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).any(|p| p == lb).then_some(lb)
}

#[cfg(unix)]
fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_exec(_p: &Path) {}

/// The release asset and its expected sha256 for this platform, or `None`
/// where there is no pinned prebuilt binary.
fn pdg_asset() -> Option<(&'static str, &'static str)> {
    PDG_ASSETS
        .iter()
        .find(|(os, arch, _, _)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
        .map(|&(_, _, asset, sha)| (asset, sha))
}

/// Install payload_dumper from GitHub releases.
fn install_payload_dumper(on_log: &dyn Fn(String)) -> DepResult {
    let mut result = DepResult {
        tool: PDG_BINARY.into(),
        already_installed: false,
        installed: false,
        skipped: false,
        error: String::new(),
    };

    if pdg_is_runnable() {
        result.already_installed = true;
        return result;
    }

    let (asset_name, expected_sha256) = match pdg_asset() {
        Some(a) => a,
        None => {
            result.error = format!(
                "No prebuilt binary for {}/{}. Install manually: cargo install payload_dumper",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return result;
        }
    };

    let dl_url = format!(
        "https://github.com/{}/releases/download/payload-dumper-rust-v{}/{}",
        PDG_REPO, PDG_VERSION, asset_name
    );

    on_log(format!("  Downloading {} ...", asset_name));
    on_log(format!("  URL: {}", dl_url));

    // Unique per-run temp dir, auto-removed on drop.
    let tmp_dir = match tempfile::Builder::new().prefix("lfff-pdg-").tempdir() {
        Ok(d) => d,
        Err(e) => {
            result.error = format!("Cannot create temp dir: {}", e);
            return result;
        }
    };
    let archive = tmp_dir.path().join(asset_name);

    // Download (-f: fail on HTTP errors instead of saving an error page)
    let dl_ok = Command::new("curl")
        .args(["-sfL", "-o"])
        .arg(&archive)
        .arg(&dl_url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !dl_ok {
        result.error = format!("Download failed: {}", dl_url);
        return result;
    }

    // Verified before anything is unpacked, let alone run or installed.
    match crate::utils::verify_sha256(&archive, expected_sha256) {
        Ok(true) => on_log("  Checksum verified".into()),
        Ok(false) => {
            result.error = format!(
                "Checksum mismatch for {} — refusing to install it",
                asset_name
            );
            return result;
        }
        Err(e) => {
            result.error = format!("Cannot verify {}: {}", asset_name, e);
            return result;
        }
    }

    // Extract with the zip crate — no external unzip dependency.
    let binary = tmp_dir.path().join("payload_dumper");
    let unzip_result = (|| -> Result<(), String> {
        let f = fs::File::open(&archive).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        let idx = (0..z.len())
            .find(|&i| {
                z.by_index(i)
                    .map(|e| {
                        Path::new(e.name())
                            .file_name()
                            .map(|n| n == "payload_dumper")
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .ok_or_else(|| "payload_dumper binary not found in archive".to_string())?;
        let mut entry = z.by_index(idx).map_err(|e| e.to_string())?;
        let mut out = fs::File::create(&binary).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = unzip_result {
        result.error = format!("Failed to extract archive: {}", e);
        return result;
    }

    set_exec(&binary);

    // Sanity check BEFORE installing: the downloaded binary must actually run
    // (catches wrong-architecture downloads and corrupted archives).
    let runs = Command::new(&binary)
        .arg("--help")
        .output()
        .map(|o| o.status.code().is_some_and(|c| c != 126 && c != 127))
        .unwrap_or(false);
    if !runs {
        result.error =
            "Downloaded payload_dumper does not run (wrong architecture or corrupted download)"
                .into();
        return result;
    }

    // Install. Preferred: ~/.local/bin when already in PATH — no root needed.
    if let Some(local_bin) = local_bin_in_path() {
        fs::create_dir_all(&local_bin).ok();
        let dest = local_bin.join("payload_dumper");
        if fs::copy(&binary, &dest).is_ok() {
            set_exec(&dest);
            on_log(format!("  Installed to {}", dest.display()));
            result.installed = true;
            return result;
        }
    }

    // Otherwise /usr/local/bin via sudo/pkexec.
    let dest = std::path::Path::new("/usr/local/bin/payload_dumper");
    let sc = sudo_cmd();
    let install_ok = Command::new(&sc)
        .args(["cp"])
        .arg(&binary)
        .arg(dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && Command::new(&sc)
            .args(["chmod", "755"])
            .arg(dest)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if install_ok {
        on_log(format!("  Installed to {}", dest.display()));
        result.installed = true;
    } else {
        // Last resort: ~/.local/bin even if it is not in PATH yet.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let local_bin = std::path::Path::new(&home).join(".local/bin");
        fs::create_dir_all(&local_bin).ok();
        let local_dest = local_bin.join("payload_dumper");
        if fs::copy(&binary, &local_dest).is_ok() {
            set_exec(&local_dest);
            on_log(format!(
                "  Installed to {}  (add ~/.local/bin to PATH if needed)",
                local_dest.display()
            ));
            result.installed = true;
        } else {
            result.error = "Failed to install binary".into();
        }
    }

    result
}

// ---------------------------------------------------------------------------
// System package installer
// ---------------------------------------------------------------------------

fn install_via_pkg_manager(tools: &[&str], pm: &str, on_log: &dyn Fn(String)) -> Vec<DepResult> {
    let mut results = Vec::new();
    let mut to_install: Vec<(&str, &str)> = Vec::new();

    for &tool in tools {
        if which::which(tool).is_ok() {
            results.push(DepResult {
                tool: tool.into(),
                already_installed: true,
                installed: false,
                skipped: false,
                error: String::new(),
            });
            continue;
        }
        match pkg_for_tool(pm, tool) {
            Some(pkg) => to_install.push((tool, pkg)),
            None => results.push(DepResult {
                tool: tool.into(),
                already_installed: false,
                installed: false,
                skipped: true,
                error: format!("No package mapping for {} on {}", tool, pm),
            }),
        }
    }

    if to_install.is_empty() {
        return results;
    }

    // Deduplicate packages
    let mut packages: Vec<&str> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (_, pkg) in &to_install {
        if seen.insert(*pkg) {
            packages.push(pkg);
        }
    }

    let mut cmd = install_cmd(pm);

    // nix profile requires full attribute path: nixpkgs#package-name
    let final_packages: Vec<String>;
    let pkg_refs: Vec<&str> = if pm == "nix-profile" {
        final_packages = packages.iter().map(|p| format!("nixpkgs#{}", p)).collect();
        final_packages.iter().map(|s| s.as_str()).collect()
    } else {
        packages
    };

    cmd.extend(pkg_refs.iter());

    let sc = sudo_cmd();
    if needs_sudo(pm) {
        let mut new_cmd: Vec<&str> = vec![&sc];
        new_cmd.extend(cmd.iter());
        cmd = new_cmd;
    }

    on_log(format!("  Running: {}", cmd.join(" ")));

    // Resolve brew to full path if not in PATH (macOS .app launch)
    let cmd0 = if pm == "brew" {
        find_brew().unwrap_or_else(|| cmd[0].to_string())
    } else {
        cmd[0].to_string()
    };
    let mut command = Command::new(&cmd0);
    command.args(&cmd[1..]);
    let ok = run_streaming(command, on_log);

    if ok {
        let is_ostree = pm == "rpm-ostree";
        for (tool, _) in &to_install {
            let found = which::which(tool).is_ok();
            results.push(DepResult {
                tool: tool.to_string(),
                already_installed: false,
                installed: found || is_ostree,
                skipped: false,
                error: if found || !is_ostree {
                    String::new()
                } else {
                    "Layered via rpm-ostree — reboot to make available".into()
                },
            });
        }
    } else {
        for (tool, _) in &to_install {
            results.push(DepResult {
                tool: tool.to_string(),
                already_installed: false,
                installed: false,
                skipped: false,
                error: "Package manager install failed".into(),
            });
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// All tools managed by LFFF.
pub const MANAGED_TOOLS: &[&str] = &["fastboot", "adb", "aria2c", "payload_dumper"];

/// Check and install missing dependencies.
///
/// If `dry_run` is true, only checks and reports without installing.
/// All human-readable output goes through `on_log` — the caller decides how
/// to present it (terminal, GUI log pane, …).
pub fn install_dependencies(
    tools: Option<&[String]>,
    dry_run: bool,
    on_log: &dyn Fn(String),
) -> DepsReport {
    let tool_list: Vec<&str> = tools
        .map(|t| t.iter().map(|s| s.as_str()).collect())
        .unwrap_or_else(|| MANAGED_TOOLS.to_vec());

    let mut report = DepsReport {
        results: Vec::new(),
    };

    let pm = detect_pkg_manager();

    on_log("\n── Dependency check ─────────────────────────────────────".into());

    if pm.is_none() && !dry_run {
        if is_atomic_distro() {
            on_log("  ⚠  Atomic/immutable distro detected.".into());
            on_log("  No compatible package manager found in this session.".into());
            on_log("  Options:".into());
            on_log("    a) Install Homebrew on Linux and re-run 'lfff deps'".into());
            on_log("    b) Use toolbox/distrobox to enter a mutable container:".into());
            on_log("         toolbox enter".into());
            on_log("         sudo dnf install android-tools aria2".into());
            on_log("         lfff deps".into());
            on_log("    c) Manually download binaries and place them in PATH".into());
        } else {
            on_log("  ✗ No supported package manager found.".into());
            on_log("    Install manually: fastboot, adb, aria2c, payload_dumper".into());
        }
        for &t in &tool_list {
            report.results.push(DepResult {
                tool: t.into(),
                already_installed: false,
                installed: false,
                skipped: true,
                error: "No package manager available".into(),
            });
        }
        on_log("────────────────────────────────────────────────────────".into());
        return report;
    }

    if let Some(ref pm_name) = pm {
        on_log(format!("  Package manager : {}", pm_name));
    }

    // Split: payload_dumper installed via cargo, not system package manager
    let pkg_tools: Vec<&str> = tool_list
        .iter()
        .filter(|&&t| t != "payload_dumper")
        .copied()
        .collect();

    if !pkg_tools.is_empty() {
        if dry_run {
            for &t in &pkg_tools {
                if which::which(t).is_ok() {
                    report.results.push(DepResult {
                        tool: t.into(),
                        already_installed: true,
                        installed: false,
                        skipped: false,
                        error: String::new(),
                    });
                } else {
                    let pkg = pm
                        .as_ref()
                        .and_then(|p| pkg_for_tool(p, t))
                        .unwrap_or("unknown");
                    report.results.push(DepResult {
                        tool: t.into(),
                        already_installed: false,
                        installed: false,
                        skipped: true,
                        error: format!(
                            "Would install: {} via {}",
                            pkg,
                            pm.as_deref().unwrap_or("?")
                        ),
                    });
                }
            }
        } else if let Some(ref pm_name) = pm {
            report
                .results
                .extend(install_via_pkg_manager(&pkg_tools, pm_name, on_log));
        }
    }

    // payload_dumper
    if tool_list.contains(&"payload_dumper") {
        if dry_run {
            // payload-dumper-go satisfies the dependency too — extraction works
            // through it, just on the slower route — so report it as present
            // rather than missing, with the caveat spelled out.
            let go = if find_payload_dumper_rust().is_some() {
                None
            } else {
                find_payload_dumper_go()
            };
            if let Some(ref path) = go {
                on_log(format!(
                    "  ⚠  using payload-dumper-go ({}) — it cannot read OTA zips directly; payload-dumper-rust is preferred",
                    path.display()
                ));
            }
            if find_payload_dumper_rust().is_some() || go.is_some() {
                report.results.push(DepResult {
                    tool: PDG_BINARY.into(),
                    already_installed: true,
                    installed: false,
                    skipped: false,
                    error: String::new(),
                });
            } else {
                report.results.push(DepResult {
                    tool: PDG_BINARY.into(),
                    already_installed: false,
                    installed: false,
                    skipped: true,
                    error: "Would install via: cargo install payload_dumper".into(),
                });
            }
        } else {
            let result = install_payload_dumper(on_log);
            // Installing the preferred dumper failed, but extraction is still
            // possible if the Go one is around — say so instead of leaving the
            // report looking fatal.
            if !result.ok()
                && let Some(path) = find_payload_dumper_go()
            {
                on_log(format!(
                    "  ⚠  payload-dumper-go ({}) will be used instead — slower, and it cannot read OTA zips directly",
                    path.display()
                ));
            }
            report.results.push(result);
        }
    }

    // Summary
    for r in &report.results {
        if r.already_installed {
            on_log(format!("  ✓  {:<25} already installed", r.tool));
        } else if r.installed && !r.error.is_empty() {
            on_log(format!("  ✓  {:<25} layered (reboot required)", r.tool));
        } else if r.installed {
            on_log(format!("  ✓  {:<25} installed", r.tool));
        } else if r.skipped {
            on_log(format!("  -  {:<25} skipped", r.tool));
        } else {
            on_log(format!("  ✗  {:<25} FAILED", r.tool));
            on_log(format!("     {}", r.error));
        }
    }

    if report.all_ok() {
        on_log("  ✓  All dependencies are ready".into());
    } else if !report.failed().is_empty() {
        on_log("  ✗  Some dependencies failed to install".into());
    }

    // Reboot notice for rpm-ostree layering
    let needs_reboot = report
        .results
        .iter()
        .any(|r| r.error.contains("rpm-ostree"));
    if needs_reboot {
        on_log("  ⚠  Packages layered via rpm-ostree.".into());
        on_log("     Reboot to make them available in PATH.".into());
    }
    on_log("────────────────────────────────────────────────────────".into());

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// payload-dumper-rust is a clap program: `--version` exits 0 and prints
    /// "<bin> <version>" followed by a banner naming the project.
    #[test]
    fn accepts_payload_dumper_rust() {
        assert!(version_banner_is_rust(
            true,
            b"payload_dumper 0.8.4\n\nProject home: <https://github.com/rhythmcache/payload-dumper-rust>\n"
        ));
        // Older releases print the version line alone.
        assert!(version_banner_is_rust(true, b"payload_dumper 0.8.3\n"));
    }

    /// nixpkgs' `payload_dumper` is vm03's Python script, which has no
    /// `--version`: argparse writes usage to stderr and exits 2. Picking it up
    /// would fail much later, inside extraction, on a protobuf error.
    #[test]
    fn rejects_python_payload_dumper() {
        assert!(!version_banner_is_rust(false, b""));
    }

    /// A binary that exits 0 but is plainly something else must not pass.
    #[test]
    fn rejects_unrelated_binary() {
        assert!(!version_banner_is_rust(true, b"GNU coreutils 9.5\n"));
    }

    /// Standard base64, for comparing against the SRI hashes in flake.nix.
    fn base64(bytes: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn payload_dumper_pin_matches_flake_nix() {
        let flake =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../flake.nix"))
                .expect("read flake.nix");
        assert!(
            flake.contains(&format!("payloadDumperVersion = \"{PDG_VERSION}\";")),
            "flake.nix pins a different payload-dumper-rust than PDG_VERSION"
        );
        for (os, arch, _, sha) in PDG_ASSETS {
            let bytes: Vec<u8> = (0..sha.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&sha[i..i + 2], 16).expect("hex"))
                .collect();
            let line = format!("{os}-{arch} = \"sha256-{}\";", base64(&bytes));
            assert!(
                flake.contains(&line),
                "flake.nix has a different hash for {os}-{arch}; expected `{line}`"
            );
        }
    }

    #[test]
    fn every_pinned_hash_is_a_sha256() {
        for (_, _, asset, sha) in PDG_ASSETS {
            assert_eq!(sha.len(), 64, "{asset}");
            assert!(
                sha.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{asset}"
            );
        }
    }

    // Runs on CI's macOS runners, which have both Homebrew and the /usr/bin/apt
    // Java stub that once got mistaken for Debian's apt.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_installs_with_homebrew_never_apt() {
        let pm = detect_pkg_manager();
        assert_ne!(pm.as_deref(), Some("apt"), "picked the /usr/bin/apt stub");
        if find_brew().is_some() {
            assert_eq!(pm.as_deref(), Some("brew"));
        } else {
            assert_eq!(pm, None, "no Homebrew here, so nothing to install with");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_install_commands_are_brews() {
        assert_eq!(pkg_for_tool("brew", "adb"), Some("android-platform-tools"));
        assert_eq!(
            pkg_for_tool("brew", "fastboot"),
            Some("android-platform-tools")
        );
        assert!(!needs_sudo("brew"), "Homebrew refuses to run under sudo");
    }
}
