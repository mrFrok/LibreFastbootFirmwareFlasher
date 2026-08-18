//! Shared utilities for fastboot-flasher.
//!
//! Centralises subprocess execution, checksum helpers, and dependency checks
//! so every other module can import from one place.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{error, info, warn};
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
/// Enforces timeout if timeout_secs > 0. Never panics — callers decide how to handle failures.
pub fn run_cmd(cmd: &[&str], timeout_secs: u64) -> CmdResult {
    if cmd.is_empty() {
        return CmdResult {
            code: -1,
            stdout: String::new(),
            stderr: "Empty command".into(),
        };
    }
    
    // No timeout: use output() directly (captures stdout/stderr)
    if timeout_secs == 0 {
        match Command::new(cmd[0]).args(&cmd[1..]).output() {
            Ok(output) => {
                return CmdResult {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                };
            }
            Err(e) => {
                return CmdResult {
                    code: -1,
                    stdout: String::new(),
                    stderr: if e.kind() == std::io::ErrorKind::NotFound {
                        format!("Binary not found: {}", cmd[0])
                    } else {
                        format!("Failed to execute {}: {}", cmd[0], e)
                    },
                };
            }
        }
    }
    
    // With timeout: spawn threads to drain stdout/stderr concurrently,
    // then wait for the child with a timeout.  This prevents pipe-deadlock
    // when the subprocess writes more than the OS pipe buffer (~64 KiB).
    let mut child = match Command::new(cmd[0])
        .args(&cmd[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CmdResult {
                code: -1,
                stdout: String::new(),
                stderr: if e.kind() == std::io::ErrorKind::NotFound {
                    format!("Binary not found: {}", cmd[0])
                } else {
                    format!("Failed to execute {}: {}", cmd[0], e)
                },
            };
        }
    };

    // Spawn background threads to read stdout/stderr so the pipe buffers
    // never fill up and block the child process.
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });

    match wait_timeout::ChildExt::wait_timeout(&mut child, Duration::from_secs(timeout_secs)) {
        Ok(Some(status)) => {
            let stdout = stdout_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            let stderr = stderr_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            CmdResult {
                code: status.code().unwrap_or(-1),
                stdout: stdout.trim().to_string(),
                stderr: stderr.trim().to_string(),
            }
        }
        Ok(None) => {
            warn!("Command '{}' exceeded timeout of {}s, killing process", cmd[0], timeout_secs);
            let _ = child.kill();
            let _ = child.wait();
            CmdResult {
                code: -124,
                stdout: String::new(),
                stderr: format!("Command '{}' exceeded timeout of {}s", cmd[0], timeout_secs),
            }
        }
        Err(e) => {
            warn!("Error waiting for command {}: {}", cmd[0], e);
            let _ = child.kill();
            CmdResult {
                code: -1,
                stdout: String::new(),
                stderr: format!("Error waiting for command: {}", e),
            }
        }
    }
}

/// Run a command from owned strings.
pub fn run_cmd_owned(cmd: &[String], timeout_secs: u64) -> CmdResult {
    let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    run_cmd(&refs, timeout_secs)
}

/// Like [`run_cmd`] but monitors a cancel flag.  When the flag becomes true
/// the child process is killed immediately and a "cancelled" result is returned.
pub fn run_cmd_with_cancel(cmd: &[&str], timeout_secs: u64, cancel: &AtomicBool) -> CmdResult {
    if cmd.is_empty() {
        return CmdResult { code: -1, stdout: String::new(), stderr: "Empty command".into() };
    }

    let mut child = match Command::new(cmd[0])
        .args(&cmd[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CmdResult {
                code: -1,
                stdout: String::new(),
                stderr: if e.kind() == std::io::ErrorKind::NotFound {
                    format!("Binary not found: {}", cmd[0])
                } else {
                    format!("Failed to execute {}: {}", cmd[0], e)
                },
            };
        }
    };

    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });

    let poll = Duration::from_millis(500);
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match wait_timeout::ChildExt::wait_timeout(&mut child, poll) {
            Ok(Some(status)) => {
                let stdout = stdout_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
                let stderr = stderr_handle.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
                return CmdResult {
                    code: status.code().unwrap_or(-1),
                    stdout: stdout.trim().to_string(),
                    stderr: stderr.trim().to_string(),
                };
            }
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) {
                    warn!("Cancel requested — killing '{}'", cmd[0]);
                    let _ = child.kill();
                    let _ = child.wait();
                    return CmdResult {
                        code: -125,
                        stdout: String::new(),
                        stderr: format!("Cancelled: {}", cmd[0]),
                    };
                }
                if std::time::Instant::now() >= deadline {
                    warn!("Command '{}' exceeded timeout of {}s, killing process", cmd[0], timeout_secs);
                    let _ = child.kill();
                    let _ = child.wait();
                    return CmdResult {
                        code: -124,
                        stdout: String::new(),
                        stderr: format!("Command '{}' exceeded timeout of {}s", cmd[0], timeout_secs),
                    };
                }
            }
            Err(e) => {
                warn!("Error waiting for command {}: {}", cmd[0], e);
                let _ = child.kill();
                return CmdResult {
                    code: -1,
                    stdout: String::new(),
                    stderr: format!("Error waiting for command: {}", e),
                };
            }
        }
    }
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
    // digest 0.11: the output array no longer implements LowerHex directly.
    use std::fmt::Write as _;
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(&mut hex, "{:02x}", b);
    }
    Ok(hex)
}

/// Return true if file digest matches expected.
pub fn verify_sha256(file_path: &Path, expected: &str) -> Result<bool> {
    let actual = compute_sha256(file_path)?;
    if actual != expected {
        error!(
            "Checksum mismatch for {}: expected {}, got {}",
            file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown"),
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

/// Is the tool usable? `payload_dumper` needs more than a $PATH hit: the name
/// is shared with an unrelated Python script on some distros, so it goes
/// through the resolver that tells the two apart.
fn tool_present(tool: &str) -> bool {
    if tool == "payload_dumper" {
        return crate::deps::find_payload_dumper_rust().is_some();
    }
    which::which(tool).is_ok()
}

/// Check whether each tool is available in $PATH.
pub fn check_tools(tools: &[&str]) -> HashMap<String, bool> {
    tools
        .iter()
        .map(|&t| (t.to_string(), tool_present(t)))
        .collect()
}

/// Names of the tools from `tools` that are missing from $PATH.
pub fn missing_tools(tools: &[&str]) -> Vec<String> {
    tools
        .iter()
        .filter(|&&t| !tool_present(t))
        .map(|&t| t.to_string())
        .collect()
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
