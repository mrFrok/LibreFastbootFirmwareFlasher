//! Shared utilities for fastboot-flasher.
//!
//! Centralises subprocess execution, checksum helpers, and dependency checks
//! so every other module can import from one place.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use log::{error, info};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Command result
// ---------------------------------------------------------------------------

/// Result of an external command invocation.
#[derive(Debug, Clone)]
pub struct CmdResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdResult {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

// ---------------------------------------------------------------------------
// Subprocess helpers
// ---------------------------------------------------------------------------

/// Run an external command and return (code, stdout, stderr).
/// Never panics — callers decide how to handle failures.
pub fn run_cmd(cmd: &[&str], _timeout_secs: u64) -> CmdResult {
    if cmd.is_empty() {
        return CmdResult {
            code: -1,
            stdout: String::new(),
            stderr: "Empty command".into(),
        };
    }
    // TODO: real timeout via wait-timeout crate or tokio
    match Command::new(cmd[0]).args(&cmd[1..]).output() {
        Ok(output) => CmdResult {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(e) => CmdResult {
            code: -1,
            stdout: String::new(),
            stderr: if e.kind() == std::io::ErrorKind::NotFound {
                format!("Binary not found: {}", cmd[0])
            } else {
                format!("Failed to execute {}: {}", cmd[0], e)
            },
        },
    }
}

/// Run a command from owned strings.
pub fn run_cmd_owned(cmd: &[String], timeout_secs: u64) -> CmdResult {
    let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    run_cmd(&refs, timeout_secs)
}

/// Thin wrapper for fastboot invocations.
pub fn fastboot(args: &[&str], timeout_secs: u64) -> CmdResult {
    let mut cmd = vec!["fastboot"];
    cmd.extend_from_slice(args);
    run_cmd(&cmd, timeout_secs)
}

/// Thin wrapper for adb invocations.
pub fn adb(args: &[&str], timeout_secs: u64) -> CmdResult {
    let mut cmd = vec!["adb"];
    cmd.extend_from_slice(args);
    run_cmd(&cmd, timeout_secs)
}

// ---------------------------------------------------------------------------
// Integrity
// ---------------------------------------------------------------------------

/// Compute SHA-256 hex digest of a file.
pub fn compute_sha256(file_path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(file_path)
        .with_context(|| format!("Cannot open file: {}", file_path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Read error: {}", file_path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Return true if file digest matches expected.
pub fn verify_sha256(file_path: &Path, expected: &str) -> Result<bool> {
    let actual = compute_sha256(file_path)?;
    if actual != expected {
        error!(
            "Checksum mismatch for {}: expected {}, got {}",
            file_path.file_name().unwrap_or_default().to_string_lossy(),
            expected,
            actual
        );
        return Ok(false);
    }
    info!("Checksum OK (sha256): {}", actual);
    Ok(true)
}

// ---------------------------------------------------------------------------
// Dependency checks
// ---------------------------------------------------------------------------

/// Return install hint for a given tool name.
pub fn tool_install_hint(tool: &str) -> &'static str {
    match tool {
        "fastboot" | "adb" => "android-tools  (apt / brew / pacman)",
        "payload_dumper" => "cargo install payload_dumper",
        "aria2c" => "aria2  (apt / brew / pacman)",
        "curl" => "curl  (apt / brew / pacman)",
        _ => "not found",
    }
}

/// Check whether each tool is available in $PATH.
pub fn check_tools(tools: &[&str]) -> HashMap<String, bool> {
    tools
        .iter()
        .map(|&t| (t.to_string(), which::which(t).is_ok()))
        .collect()
}

/// Print dependency table, return false if any tool is missing.
pub fn require_tools(tools: &[&str]) -> bool {
    let results = check_tools(tools);
    let mut all_ok = true;
    for &tool in tools {
        let found = results.get(tool).copied().unwrap_or(false);
        let status = if found { "✓" } else { "✗" };
        let hint = if found {
            String::new()
        } else {
            format!("  →  {}", tool_install_hint(tool))
        };
        println!("  {}  {}{}", status, tool, hint);
        if !found {
            all_ok = false;
        }
    }
    all_ok
}

// ---------------------------------------------------------------------------
// Interactive input
// ---------------------------------------------------------------------------

/// Prompt user for input, return default on empty.
pub fn prompt(message: &str, default: &str) -> String {
    use std::io::{self, Write};
    let suffix = if default.is_empty() {
        String::new()
    } else {
        format!(" [{}]", default)
    };
    print!("{}{}: ", message, suffix);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_cmd_echo() {
        let r = run_cmd(&["echo", "hello"], 10);
        assert_eq!(r.code, 0);
        assert_eq!(r.stdout, "hello");
    }

    #[test]
    fn test_run_cmd_not_found() {
        let r = run_cmd(&["__nonexistent_binary__"], 10);
        assert_eq!(r.code, -1);
    }

    #[test]
    fn test_check_tools_mixed() {
        let r = check_tools(&["echo", "__missing__"]);
        assert_eq!(r.get("echo"), Some(&true));
        assert_eq!(r.get("__missing__"), Some(&false));
    }

    #[test]
    fn test_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("test.txt");
        std::fs::write(&f, b"hello world").unwrap();
        let hash = compute_sha256(&f).unwrap();
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
