//! Automatic dependency installer for LFFF.
//!
//! Handles:
//!   - android-tools (fastboot + adb) via system package manager
//!   - aria2c via system package manager
//!   - payload_dumper via `cargo install payload_dumper`
//!
//! Supported package managers: pacman, apt, dnf, zypper, emerge, brew,
//!                        xbps (Void), apk (Alpine), rpm-ostree (atomic Fedora),
//!                        nix-env (NixOS)

use std::fs;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Return the package name for a given tool on a given package manager.
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
            if line.starts_with("ID=") {
                let id = line[3..].trim().trim_matches('"').to_lowercase();
                if atomic_ids.contains(&id.as_str()) {
                    return true;
                }
            }
            // Also match ID_LIKE (e.g. ID_LIKE="fedora" on Silverblue)
            if line.starts_with("ID_LIKE=") {
                let id_like = line[8..].trim().trim_matches('"').to_lowercase();
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
        // Atomic distros: try native layering, then per-user installers
        for pm in &["rpm-ostree", "nix-env", "brew"] {
            if which::which(pm).is_ok() {
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
    None
}

fn sudo_cmd() -> String {
    std::env::var("LFFF_SUDO_CMD").unwrap_or_else(|_| "sudo".to_string())
}

fn needs_sudo(pm: &str) -> bool {
    // Per-user package managers don't need sudo
    if matches!(pm, "brew" | "nix-env") {
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
fn install_payload_dumper() -> DepResult {
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
    println!("  Fetching latest payload_dumper release from GitHub ...");
    let api_result = Command::new("curl")
        .args([
            "-sL",
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

    println!("  Downloading {} ...", asset_name);
    println!("  URL: {}", dl_url);

    let tmp_dir = std::env::temp_dir().join("lfff_pdg_install");
    fs::create_dir_all(&tmp_dir).ok();
    let archive = tmp_dir.join(&asset_name);

    // Download
    let dl_ok = Command::new("curl")
        .args(["-sL", "-o"])
        .arg(&archive)
        .arg(&dl_url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !dl_ok {
        result.error = format!("Download failed: {}", dl_url);
        let _ = fs::remove_dir_all(&tmp_dir);
        return result;
    }

    // unzip may not be installed on minimal systems (e.g. Alpine, Docker)
    if which::which("unzip").is_err() {
        result.error = "unzip not found — install it (e.g. sudo apt install unzip) and re-run".into();
        let _ = fs::remove_dir_all(&tmp_dir);
        return result;
    }

    // Extract ZIP
    let unzip_ok = Command::new("unzip")
        .args(["-o", "-q"])
        .arg(&archive)
        .arg("-d")
        .arg(&tmp_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !unzip_ok {
        result.error = "Failed to unzip archive".into();
        let _ = fs::remove_dir_all(&tmp_dir);
        return result;
    }

    // Find the binary
    let binary = tmp_dir.join("payload_dumper");
    if !binary.exists() {
        result.error = "payload_dumper binary not found in archive".into();
        let _ = fs::remove_dir_all(&tmp_dir);
        return result;
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).ok();
    }

    // Install to /usr/local/bin (with sudo) or ~/.local/bin
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
        println!("  Installed to {}", dest.display());
        result.installed = true;
    } else {
        // Fallback: ~/.local/bin
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let local_bin = std::path::Path::new(&home).join(".local/bin");
        fs::create_dir_all(&local_bin).ok();
        let local_dest = local_bin.join("payload_dumper");
        if fs::copy(&binary, &local_dest).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&local_dest, fs::Permissions::from_mode(0o755)).ok();
            }
            println!(
                "  Installed to {}  (add ~/.local/bin to PATH if needed)",
                local_dest.display()
            );
            result.installed = true;
        } else {
            result.error = "Failed to install binary".into();
        }
    }

    let _ = fs::remove_dir_all(&tmp_dir);
    result
}

// ---------------------------------------------------------------------------
// System package installer
// ---------------------------------------------------------------------------

fn install_via_pkg_manager(tools: &[&str], pm: &str) -> Vec<DepResult> {
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
    cmd.extend(packages.iter());

    let sc = sudo_cmd();
    if needs_sudo(pm) {
        let mut new_cmd: Vec<&str> = vec![&sc];
        new_cmd.extend(cmd.iter());
        cmd = new_cmd;
    }

    println!("  Running: {}", cmd.join(" "));

    let status = Command::new(cmd[0]).args(&cmd[1..]).status();

    match status {
        Ok(s) if s.success() => {
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
        }
        _ => {
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
pub fn install_dependencies(tools: Option<&[String]>, dry_run: bool) -> DepsReport {
    let tool_list: Vec<&str> = tools
        .map(|t| t.iter().map(|s| s.as_str()).collect())
        .unwrap_or_else(|| MANAGED_TOOLS.to_vec());

    let mut report = DepsReport {
        results: Vec::new(),
    };

    let pm = detect_pkg_manager();

    println!("\n── Dependency check ─────────────────────────────────────");

    if pm.is_none() && !dry_run {
        if is_atomic_distro() {
            println!("  ⚠  Atomic/immutable distro detected.");
            println!("  No compatible package manager found in this session.");
            println!();
            println!("  Options:");
            println!("    a) Install Homebrew on Linux and re-run 'lfff deps'");
            println!("    b) Use toolbox/distrobox to enter a mutable container:");
            println!("         toolbox enter");
            println!("         sudo dnf install android-tools aria2");
            println!("         lfff deps");
            println!("    c) Manually download binaries and place them in PATH");
        } else {
            println!("  ✗ No supported package manager found.");
            println!("    Install manually: fastboot, adb, aria2c, payload_dumper");
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
        println!("────────────────────────────────────────────────────────");
        return report;
    }

    if let Some(ref pm_name) = pm {
        println!("  Package manager : {}", pm_name);
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
                .extend(install_via_pkg_manager(&pkg_tools, pm_name));
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
            report.results.push(install_payload_dumper());
        }
    }

    // Print summary
    println!();
    for r in &report.results {
        if r.already_installed {
            println!("  ✓  {:<25} already installed", r.tool);
        } else if r.installed && !r.error.is_empty() {
            println!("  ✓  {:<25} {} {}", r.tool, "layered", "(reboot required)");
        } else if r.installed {
            println!("  ✓  {:<25} installed", r.tool);
        } else if r.skipped {
            println!("  -  {:<25} skipped", r.tool);
        } else {
            println!("  ✗  {:<25} FAILED", r.tool);
            println!("     {}", r.error);
        }
    }
    println!();

    if report.all_ok() {
        println!("{}", "━".repeat(56));
        println!("  ✓  All dependencies are ready");
        println!("{}", "━".repeat(56));
    } else if !report.failed().is_empty() {
        println!("{}", "━".repeat(56));
        println!("  ✗  Some dependencies failed to install");
        println!("{}", "━".repeat(56));
    }

    // Reboot notice for rpm-ostree layering
    let needs_reboot = report.results.iter().any(|r| r.error.contains("rpm-ostree"));
    if needs_reboot {
        println!("{}", "━".repeat(56));
        println!("  ⚠  Packages layered via rpm-ostree.");
        println!("     Reboot to make them available in PATH.");
        println!("{}", "━".repeat(56));
    }
    println!("────────────────────────────────────────────────────────");

    report
}
