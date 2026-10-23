//! HTTP through `curl`, like the rest of LibreFastbootFirmwareFlasher: no TLS
//! stack to build, so the static musl CLI and Termux keep working.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

// The servers answer within a second or two; one of a server's addresses
// sometimes takes the connection and never answers, and a retry soon
// reaches another.
const TIMEOUT_SECS: &str = "6";

pub fn post(url: &str, headers: &[(&str, String)], body: &str) -> Result<String> {
    let mut cmd = curl(url);
    cmd.args(["-X", "POST", "--data-binary", "@-"]);
    for (name, value) in headers {
        // "name:" would drop the header; "name;" sends it empty.
        if value.is_empty() {
            cmd.arg("-H").arg(format!("{name};"));
        } else {
            cmd.arg("-H").arg(format!("{name}: {value}"));
        }
    }
    run(cmd, Some(body), url)
}

/// Fails on HTTP errors.
pub fn get(url: &str) -> Result<String> {
    let mut cmd = curl(url);
    cmd.arg("--fail");
    run(cmd, None, url)
}

fn curl(url: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sS",
        "-L",
        "--connect-timeout",
        "8",
        "--max-time",
        TIMEOUT_SECS,
        "--compressed",
    ])
    .arg("--url")
    .arg(url);
    cmd
}

fn run(mut cmd: Command, body: Option<&str>, url: &str) -> Result<String> {
    let host = url.split('/').nth(2).unwrap_or(url);
    let mut child = cmd
        .stdin(if body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl is not installed")?;
    if let (Some(body), Some(mut stdin)) = (body, child.stdin.take()) {
        stdin.write_all(body.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{host}: {}", err.trim().trim_start_matches("curl: "));
    }
    String::from_utf8(out.stdout).with_context(|| format!("{host}: answer is not text"))
}
