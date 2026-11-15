//! Each session's log, saved to disk: what was on screen, so that a report
//! from an untested phone comes with what happened (#18 was closed without
//! one). Plain text, colours stripped, one file per session, the newest
//! [`KEEP`] kept.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How many session logs are kept; older ones are deleted.
pub const KEEP: usize = 30;

/// `~/.local/state/lfff/logs` (Linux), the local data folder elsewhere.
pub fn log_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("lfff")
        .join("logs")
}

pub struct SessionLog {
    file: File,
    pub path: PathBuf,
}

impl SessionLog {
    /// Start `lfff-<kind>-<time>.log` in [`log_dir`], headed with the
    /// version and the system. `None` when it cannot be written: the
    /// session goes on without it.
    pub fn create(kind: &str, version: &str) -> Option<Self> {
        Self::create_in(&log_dir(), kind, version)
    }

    pub fn create_in(dir: &Path, kind: &str, version: &str) -> Option<Self> {
        fs::create_dir_all(dir).ok()?;
        prune(dir, KEEP.saturating_sub(1));
        let now = chrono::Local::now();
        let path = dir.join(format!(
            "lfff-{kind}-{}.log",
            now.format("%Y-%m-%d_%H-%M-%S")
        ));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        writeln!(
            file,
            "LibreFastbootFirmwareFlasher {version} — {kind} — {}\nSystem: {} {}\n{}",
            now.format("%Y-%m-%d %H:%M:%S %:z"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            "=".repeat(60)
        )
        .ok()?;
        Some(Self { file, path })
    }

    /// Append one line (or several), colours stripped.
    pub fn line(&mut self, text: &str) {
        let _ = writeln!(self.file, "{}", strip_ansi(text));
    }
}

/// Text without its ANSI escape sequences (colours, cursor moves).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Parameters, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Delete the oldest `lfff-*.log` until `keep` are left.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("lfff-") && n.ends_with(".log"))
        })
        .collect();
    if logs.len() <= keep {
        return;
    }
    // The name carries the time, so it sorts oldest first.
    logs.sort();
    for p in &logs[..logs.len() - keep] {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_stripped() {
        assert_eq!(
            strip_ansi("\x1b[1;31m✗ failed\x1b[0m: \x1b[2Kok"),
            "✗ failed: ok"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn a_log_has_its_header_and_old_ones_go() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..KEEP + 5 {
            fs::write(
                tmp.path()
                    .join(format!("lfff-flash-2026-01-01_00-00-{i:02}.log")),
                "",
            )
            .unwrap();
        }
        fs::write(tmp.path().join("other.txt"), "").unwrap();
        let mut log = SessionLog::create_in(tmp.path(), "flash", "9.9.9").unwrap();
        log.line("\x1b[32m✓ boot_a\x1b[0m");
        let text = fs::read_to_string(&log.path).unwrap();
        assert!(text.starts_with("LibreFastbootFirmwareFlasher 9.9.9 — flash — "));
        assert!(text.ends_with("✓ boot_a\n"));
        let n = fs::read_dir(tmp.path()).unwrap().count();
        assert_eq!(n, KEEP + 1, "{KEEP} logs and other.txt");
        assert!(
            !tmp.path()
                .join("lfff-flash-2026-01-01_00-00-00.log")
                .exists()
        );
    }
}
