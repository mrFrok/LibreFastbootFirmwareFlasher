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
use std::path::Path;
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
        "pacman", "apt", "apt-get", "dnf", "zypper", "emerge",
        "xbps-install", "apk", "brew",
    ] {
        if which::which(pm).is_ok() {
            return Some(match *pm {
                "apt-get" => "apt",
                "xbps-install" => "xbps",
                other => other,
            }.to_string());
        }
    }

    // macOS: GUI launched from .app may not have /opt/homebrew/bin in PATH
    if cfg!(target_os = "macos") && find_brew().is_some() {
        return Some("brew".into());
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

/// Check if payload_dumper is runnable.
fn pdg_is_runnable() -> bool {
    if which::which(PDG_BINARY).is_err() {
        return false;
    }
    Command::new(PDG_BINARY)
        .arg("--help")
        .output()
        .map(|o| o.status.code() != Some(126))
        .unwrap_or(false)
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

/// Pick the correct asset name for the current platform.
///
/// Asset naming: `payload_dumper-{os}-{arch}.zip`
/// Examples: payload_dumper-linux-x86_64.zip, payload_dumper-macos-aarch64.zip
fn pdg_asset_name() -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "armv7",
        _ => return None,
    };
    Some(format!("payload_dumper-{}-{}.zip", os, arch))
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

    let asset_name = match pdg_asset_name() {
        Some(n) => n,
        None => {
            result.error = format!(
                "No prebuilt binary for {}/{}. Install manually: cargo install payload_dumper",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return result;
        }
    };

    // Fetch latest release tag from GitHub API
    on_log("  Fetching latest payload_dumper release from GitHub ...".into());
    let api_result = Command::new("curl")
        .args([
            "-sfL",
            &format!("https://api.github.com/repos/{}/releases/latest", PDG_REPO),
            "-H",
            "User-Agent: lfff/0.2",
        ])
        .output();

    let tag = match api_result {
        Ok(output) if output.status.success() => {
            let json = String::from_utf8_lossy(&output.stdout);
            // JSON line:   "tag_name": "payload-dumper-rust-v0.8.2",
            // Find the value between the last pair of quotes on the line
            json.lines()
                .find(|l| l.contains("\"tag_name\""))
                .and_then(|l| {
                    // Skip past "tag_name" key — find value after the colon
                    let after_key = l.split("\"tag_name\"").nth(1)?;
                    // Now extract string between quotes: : "value",
                    let first_quote = after_key.find('"')? + 1;
                    let rest = &after_key[first_quote..];
                    let end_quote = rest.find('"')?;
                    let val = &rest[..end_quote];
                    if val.is_empty() {
                        None
                    } else {
                        Some(val.to_string())
                    }
                })
                .unwrap_or_else(|| "payload-dumper-rust-v0.8.2".to_string())
        }
        _ => "payload-dumper-rust-v0.8.2".to_string(),
    };

    let dl_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        PDG_REPO, tag, asset_name
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
    let archive = tmp_dir.path().join(&asset_name);

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
            if which::which(PDG_BINARY).is_ok() {
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
            report.results.push(install_payload_dumper(on_log));
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
    let needs_reboot = report.results.iter().any(|r| r.error.contains("rpm-ostree"));
    if needs_reboot {
        on_log("  ⚠  Packages layered via rpm-ostree.".into());
        on_log("     Reboot to make them available in PATH.".into());
    }
    on_log("────────────────────────────────────────────────────────".into());

    report
}
