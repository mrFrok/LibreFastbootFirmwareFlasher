//! Automatic dependency installer for LFFF.
//!
//! Handles:
//!   - android-tools (fastboot + adb) via system package manager
//!   - aria2c via system package manager
//!   - payload_dumper via `cargo install payload_dumper`
//!
//! Supported package managers: pacman, apt, dnf, zypper, emerge, brew

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
        "emerge" => vec!["emerge"],
        "brew" => vec!["brew", "install"],
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
        }
    }

    Path::new("/ostree").is_dir() || Path::new("/etc/NIXOS").exists()
}

/// Detect the best available package manager.
fn detect_pkg_manager() -> Option<String> {
    if is_atomic_distro() {
        if which::which("brew").is_ok() {
            return Some("brew".into());
        }
        return None;
    }

    for pm in &[
        "pacman", "apt", "apt-get", "dnf", "zypper", "emerge", "brew",
    ] {
        if which::which(pm).is_ok() {
            return Some(if *pm == "apt-get" { "apt" } else { pm }.to_string());
        }
    }
    None
}

fn needs_sudo(pm: &str) -> bool {
    if pm == "brew" {
        return false;
    }
    // Check if running as root via env
    std::env::var("USER").map(|u| u != "root").unwrap_or(true)
}

// ---------------------------------------------------------------------------
// payload_dumper installer (via cargo install)
// ---------------------------------------------------------------------------

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

/// Install payload_dumper via `cargo install payload_dumper`.
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

    // Need cargo to install
    if which::which("cargo").is_err() {
        result.error = "cargo not found. Install Rust: https://rustup.rs".into();
        return result;
    }

    println!("  Installing payload_dumper via cargo install ...");
    let status = Command::new("cargo")
        .args(["install", "payload_dumper"])
        .status();

    match status {
        Ok(s) if s.success() => {
            if which::which(PDG_BINARY).is_ok() {
                result.installed = true;
                println!("  ✓ payload_dumper installed");
            } else {
                result.error =
                    "cargo install succeeded but binary not found in PATH. Check ~/.cargo/bin"
                        .into();
            }
        }
        Ok(s) => {
            result.error = format!("cargo install failed with code {}", s.code().unwrap_or(-1));
        }
        Err(e) => {
            result.error = format!("Failed to run cargo: {}", e);
        }
    }

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

    if needs_sudo(pm) {
        let mut sudo_cmd = vec!["sudo"];
        sudo_cmd.extend(cmd.iter());
        cmd = sudo_cmd;
    }

    println!("  Running: {}", cmd.join(" "));

    let status = Command::new(cmd[0]).args(&cmd[1..]).status();

    match status {
        Ok(s) if s.success() => {
            for (tool, _) in &to_install {
                let found = which::which(tool).is_ok();
                results.push(DepResult {
                    tool: tool.to_string(),
                    already_installed: false,
                    installed: found,
                    skipped: false,
                    error: if found {
                        String::new()
                    } else {
                        "Installed but binary not found in PATH".into()
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
            println!("  System package manager cannot install packages in the current session.");
            println!();
            println!("  Recommended: install Homebrew on Linux, then re-run 'lfff deps'");
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
    println!("────────────────────────────────────────────────────────");

    report
}
