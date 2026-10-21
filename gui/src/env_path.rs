// src/env_path.rs — give a GUI launch the PATH a terminal would have
//
// On macOS an app started from Finder, Launchpad or the Dock inherits
// launchd's PATH, /usr/bin:/bin:/usr/sbin:/sbin, not the one the user's shell
// sets up. fastboot and adb from Homebrew (/opt/homebrew/bin) or the Android
// SDK are then invisible to the GUI even though they work in Terminal — which
// is exactly how it looks to the user: "the drivers are installed, but LFFF
// does not see them". The CLI is run from a shell and never has this problem.

// Only macOS calls into this; elsewhere it is compiled for the tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wraps the PATH the login shell prints, so whatever its startup files
/// write to stdout (banners, `echo`s in .zshrc) cannot be mistaken for it.
const MARKER: &str = "__LFFF_PATH__";

/// Where tools commonly live on macOS whether or not the shell knows it:
/// Homebrew (Apple silicon and Intel), MacPorts, and Android Studio's SDK.
fn well_known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
        .iter()
        .map(PathBuf::from)
        .collect();
    if let Some(h) = home {
        dirs.push(h.join("Library/Android/sdk/platform-tools"));
    }
    dirs
}

/// The PATH between the two markers in a shell's output, if it is there.
fn parse_marked_path(stdout: &str) -> Option<&str> {
    let start = stdout.find(MARKER)? + MARKER.len();
    let len = stdout[start..].find(MARKER)?;
    let path = &stdout[start..start + len];
    (!path.is_empty()).then_some(path)
}

/// `current`, followed by every directory of `extra` it does not already
/// contain, in order. The current entries keep their precedence.
fn merge_paths(current: &[PathBuf], extra: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut merged = current.to_vec();
    for dir in extra {
        if !dir.as_os_str().is_empty() && !merged.contains(&dir) {
            merged.push(dir);
        }
    }
    merged
}

/// Ask the user's login shell for its PATH. Interactive as well as login, so
/// both .zprofile (where Homebrew's installer puts `brew shellenv`) and
/// .zshrc are read. Bounded by a timeout: a startup file that waits for
/// input must not hold up the window.
fn login_shell_path(shell: &str, timeout: Duration) -> Option<String> {
    let script = format!("printf '{MARKER}%s{MARKER}' \"$PATH\"");
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", &script]);
    let out = stdout_within(cmd, timeout)?;
    parse_marked_path(&out).map(str::to_owned)
}

/// Run `cmd` with no stdin and return its stdout, or `None` if it cannot be
/// started or is still running after `timeout` — in which case it is killed.
fn stdout_within(mut cmd: Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// The PATH the GUI should run with: its own, then the login shell's, then
/// the well-known tool directories.
fn resolved_path(
    current: Option<OsString>,
    shell: Option<String>,
    home: Option<&Path>,
) -> OsString {
    let current: Vec<PathBuf> = current
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let from_shell: Vec<PathBuf> = shell
        .and_then(|s| login_shell_path(&s, Duration::from_secs(3)))
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let merged = merge_paths(
        &current,
        from_shell.into_iter().chain(well_known_dirs(home)),
    );
    std::env::join_paths(merged).unwrap_or_default()
}

/// Extend this process's PATH as described at the top of the file. Call
/// first thing in `main`, before any thread starts.
#[cfg(target_os = "macos")]
pub fn inherit_shell_path() {
    let path = resolved_path(
        std::env::var_os("PATH"),
        std::env::var("SHELL").ok(),
        dirs::home_dir().as_deref(),
    );
    log::info!("PATH: {}", path.to_string_lossy());
    // SAFETY: called at the very start of main, while the process is still
    // single-threaded.
    unsafe {
        std::env::set_var("PATH", path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_path_ignores_whatever_the_shell_prints_around_it() {
        let out = format!("Welcome back!\nlast login: today\n{MARKER}/a:/b{MARKER}\n");
        assert_eq!(parse_marked_path(&out), Some("/a:/b"));
        assert_eq!(parse_marked_path("no markers here"), None);
        assert_eq!(
            parse_marked_path(&format!("{MARKER}/only/one/marker")),
            None
        );
        assert_eq!(parse_marked_path(&format!("{MARKER}{MARKER}")), None);
    }

    #[test]
    fn merging_keeps_the_current_order_and_drops_duplicates() {
        let current: Vec<PathBuf> = ["/usr/bin", "/bin"].iter().map(PathBuf::from).collect();
        let extra = [
            "/opt/homebrew/bin",
            "/usr/bin",
            "",
            "/opt/homebrew/bin",
            "/x",
        ]
        .iter()
        .map(PathBuf::from);
        let merged = merge_paths(&current, extra);
        let got: Vec<&str> = merged.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(got, ["/usr/bin", "/bin", "/opt/homebrew/bin", "/x"]);
    }

    #[test]
    fn launchd_path_gains_homebrew_and_the_android_sdk() {
        let path = resolved_path(
            Some("/usr/bin:/bin:/usr/sbin:/sbin".into()),
            None,
            Some(Path::new("/Users/me")),
        );
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(
            dirs[0],
            Path::new("/usr/bin"),
            "existing entries keep precedence"
        );
        for want in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Users/me/Library/Android/sdk/platform-tools",
        ] {
            assert!(dirs.iter().any(|d| d == Path::new(want)), "{want} missing");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_path_from_a_real_shell() {
        // sh stands in for the user's shell: same flags, same marker protocol.
        let got = login_shell_path("/bin/sh", Duration::from_secs(5));
        assert!(got.is_some_and(|p| !p.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_that_hangs_is_given_up_on() {
        // `sleep 30` stands in for a startup file that waits for input. An
        // existing binary rather than a freshly written script: exec right
        // after write races with forks in parallel tests (ETXTBSY).
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let started = Instant::now();
        assert_eq!(stdout_within(cmd, Duration::from_millis(300)), None);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(300),
            "gave up early: {took:?}"
        );
        assert!(
            took < Duration::from_secs(5),
            "waited for the shell: {took:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_quick_command_returns_its_output() {
        let mut cmd = Command::new("printf");
        cmd.arg("hello");
        assert_eq!(
            stdout_within(cmd, Duration::from_secs(5)).as_deref(),
            Some("hello")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_shell_is_not_an_error() {
        assert_eq!(
            login_shell_path("/nonexistent/shell", Duration::from_secs(1)),
            None
        );
    }
}
