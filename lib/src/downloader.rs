//! OTA firmware downloader for OnePlus / OPPO / Realme devices.
//!
//! Resolves OTA download links (including 4PDA redirects) to direct CDN URLs
//! and downloads them via aria2c for maximum speed.
//!
//! All progress data is surfaced through [`DownloadProgress`] and passed to
//! the Slint GUI via an `on_progress` closure.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::utils::{missing_tools, run_cmd};
use tracing::info;

const OTA_HEADERS: &[&str] = &[
    "userId: oplus-ota|16002018",
    "User-Agent: okhttp/4.9.2",
    "Accept: application/json",
];

// ─── Result ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub success: bool,
    pub url: String,
    pub cdn_url: String,
    pub output_path: Option<PathBuf>,
    pub error: String,
}

impl DownloadResult {
    fn fail(url: &str, error: &str) -> Self {
        Self {
            success: false,
            url: url.into(),
            cdn_url: String::new(),
            output_path: None,
            error: error.into(),
        }
    }
}

// ─── Progress ────────────────────────────────────────────────────────────────

/// All information the GUI needs to render a download screen.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    // --- aria2c progress ---
    /// 0.0 – 100.0
    pub percent: f32,
    /// e.g. "45.2MiB/s"
    pub speed: String,
    /// e.g. "1m 22s"
    pub eta: String,
    /// e.g. "245 MiB"
    pub downloaded: String,
    /// e.g. "1.2 GiB"
    pub total_size: String,

    // --- link lifetime ---
    /// How long the CDN link is still valid, e.g. "2h 15m" or "EXPIRED"
    pub link_expires_in: String,
    /// Unix timestamp when the link expires (0 = unknown)
    pub link_expires_ts: u64,

    // --- raw line for log area ---
    pub raw_line: String,
}

impl Default for DownloadProgress {
    fn default() -> Self {
        Self {
            percent: 0.0,
            speed: String::new(),
            eta: String::new(),
            downloaded: String::new(),
            total_size: String::new(),
            link_expires_in: String::new(),
            link_expires_ts: 0,
            raw_line: String::new(),
        }
    }
}

// ─── URL helpers ─────────────────────────────────────────────────────────────

/// Unwrap 4PDA redirect to get real OTA endpoint. Two forms are in use:
/// `4pda.to/goto/?u=<url>` and `4pda.to/stat/go?url=<url>`.
fn extract_real_url(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url)
        && parsed
            .host_str()
            .map(|h| h.contains("4pda.to"))
            .unwrap_or(false)
    {
        for (k, v) in parsed.query_pairs() {
            if k == "u" || k == "url" {
                info!("Unwrapped 4PDA redirect -> {}", v);
                return v.to_string();
            }
        }
    }
    url.to_string()
}

/// Accept only an absolute `http`/`https` URL.
///
/// Every URL handed to curl or aria2c passes through here first: the pasted
/// one, the target of a 4PDA redirect, and the `Location` a server answers
/// with. All three are someone else's text, and as a bare argument a value
/// like `--enable-rpc=true` would be read as an option — aria2c would start an
/// unauthenticated RPC server that any local process or web page can use to
/// write files. A parsed http(s) URL always begins with its scheme, so it can
/// only ever be read as a URL.
fn web_url(raw: &str) -> Result<url::Url, String> {
    let shown = &raw[..raw.len().min(100)];
    let parsed = url::Url::parse(raw.trim()).map_err(|_| format!("Not a valid URL: {shown}"))?;
    match parsed.scheme() {
        "http" | "https" if parsed.has_host() => Ok(parsed),
        _ => Err(format!("Only http(s) links can be downloaded: {shown}")),
    }
}

/// Follow OTA server 302 redirect to get CDN URL.
/// If no redirect found, assume URL is already a direct download link.
fn resolve_cdn(url: &url::Url) -> Result<url::Url, String> {
    let mut parts: Vec<String> = vec![
        "curl".into(),
        "-s".into(),
        "-o".into(),
        "/dev/null".into(),
        "-D".into(),
        "-".into(),
        "--max-redirs".into(),
        "0".into(),
        "--connect-timeout".into(),
        "15".into(),
    ];
    for h in OTA_HEADERS {
        parts.push("-H".into());
        parts.push(h.to_string());
    }
    parts.push(url.as_str().into());
    let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
    let r = run_cmd(&refs, 30);
    match location_header(&r.stdout) {
        Some(loc) => {
            // A relative Location is resolved against the URL that sent it.
            let cdn = url
                .join(loc)
                .map_err(|_| format!("Server redirected to an invalid URL: {loc}"))?;
            let cdn = web_url(cdn.as_str())?;
            info!("CDN URL resolved: {}", cdn);
            Ok(cdn)
        }
        None => {
            info!(
                "No redirect found, using URL directly: {}",
                &url.as_str()[..url.as_str().len().min(100)]
            );
            Ok(url.clone())
        }
    }
}

/// The `Location` value from a block of response headers, if any.
fn location_header(headers: &str) -> Option<&str> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim())
            .filter(|v| !v.is_empty())
    })
}

/// Parse `?e=<unix_ts>` expiry from a CDN URL.
/// Returns `(expires_ts, human_string)`.
fn parse_link_expiry(cdn_url: &str) -> (u64, String) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if let Ok(parsed) = url::Url::parse(cdn_url) {
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        if let Some(ts_str) = params.get("e")
            && let Ok(ts) = ts_str.parse::<u64>()
        {
            if ts > now {
                let diff = ts - now;
                let h = diff / 3600;
                let m = (diff % 3600) / 60;
                let label = if h > 0 {
                    format!("{}h {}m", h, m)
                } else {
                    format!("{}m", m)
                };
                return (ts, label);
            } else {
                return (ts, "EXPIRED".into());
            }
        }
    }
    (0, String::new())
}

// ─── aria2c progress parser ───────────────────────────────────────────────────

/// Parse an aria2c progress line.
///
/// aria2c --show-console-readout writes to stdout lines like:
/// `#da9bea 210MiB/7.8GiB(2%) CN:16 DL:28MiB ETA:4m37s]`
/// (note: no leading `[`, GID starts with `#`, DL value has no `/s`)
fn parse_aria2c_progress(line: &str, expires_ts: u64) -> Option<DownloadProgress> {
    // Must look like a progress line: contains GID marker and percentage
    if !line.contains('%') {
        return None;
    }

    // Percentage — e.g. "(2%)"
    let percent = if let Some(start) = line.find('(') {
        if let Some(end) = line[start..].find('%') {
            line[start + 1..start + end].parse::<f32>().ok()
        } else {
            None
        }
    } else {
        None
    };

    // DL speed — e.g. "DL:28MiB" or "DL:28MiB/s"
    let speed = if let Some(pos) = line.find("DL:") {
        let rest = &line[pos + 3..];
        let token = rest.split_whitespace().next().unwrap_or("");
        // strip trailing ']' if present
        token.trim_end_matches(']').to_string()
    } else {
        String::new()
    };

    // ETA — e.g. "ETA:4m37s]" or "ETA:22s"
    let eta = if let Some(pos) = line.find("ETA:") {
        let rest = &line[pos + 4..];
        rest.split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(']')
            .to_string()
    } else {
        String::new()
    };

    // Sizes — "210MiB/7.8GiB(2%)" appears after the GID token (first space)
    let (downloaded, total_size) = parse_sizes(line);

    // Recompute live expiry label every tick
    let (_, link_expires_in) = if expires_ts > 0 {
        parse_link_expiry(&format!("https://x.invalid/?e={}", expires_ts))
    } else {
        (0, String::new())
    };

    let percent = percent?;
    Some(DownloadProgress {
        percent,
        speed,
        eta,
        downloaded,
        total_size,
        link_expires_in,
        link_expires_ts: expires_ts,
        raw_line: line.to_string(),
    })
}

/// Extract downloaded and total sizes from an aria2c progress line.
/// Handles both `[#gid size/total(pct)` and `#gid size/total(pct)` formats.
fn parse_sizes(line: &str) -> (String, String) {
    // Skip the GID token (starts with optional '[' then '#'), then take the size token
    let body = line.trim_start_matches('[');
    // Find first space after the GID
    if let Some(sp) = body.find(' ') {
        let chunk = body[sp..].trim_start();
        if let Some(slash) = chunk.find('/') {
            let downloaded = chunk[..slash].to_string();
            let rest = &chunk[slash + 1..];
            let end = rest.find('(').unwrap_or(rest.len());
            let total = rest[..end].to_string();
            return (downloaded, total);
        }
    }
    (String::new(), String::new())
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Token passed to [`download_firmware_with_progress`] to cancel an in-flight download.
///
/// ```rust,no_run
/// use lfff_lib::downloader::{CancelToken, download_firmware_with_progress};
/// let token = CancelToken::new();
/// let t = token.clone();
/// std::thread::spawn(move || {
///     let _ = download_firmware_with_progress("https://example.com/fw.zip", None, 16, token, |_| {});
/// });
/// // Later, from the GUI:
/// t.cancel();
/// ```
#[derive(Clone, Default)]
pub struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Signal the download to stop.
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    /// Returns true if cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Download firmware with real-time progress callback suitable for Slint GUI.
///
/// aria2c writes progress lines to **stdout** (not stderr) when stdout is piped,
/// so we pipe stdout and read it line-by-line. stderr is inherited so aria2c
/// error messages still appear in the terminal.
///
/// Pass a [`CancelToken`] to support cancellation from the GUI:
/// ```rust,no_run
/// use lfff_lib::downloader::{CancelToken, download_firmware_with_progress};
/// let token = CancelToken::new();
/// let t = token.clone();
/// std::thread::spawn(move || {
///     let _ = download_firmware_with_progress("https://example.com/fw.zip", None, 16, token, |p| {
///         println!("Progress: {:.1}% speed={} eta={}", p.percent, p.speed, p.eta);
///     });
/// });
/// // Later, from the GUI:
/// t.cancel();
/// ```
pub fn download_firmware_with_progress<F>(
    url: &str,
    output_dir: Option<&Path>,
    connections: u32,
    cancel: CancelToken,
    on_progress: F,
) -> DownloadResult
where
    F: Fn(DownloadProgress) + Send + 'static,
{
    use std::process::Stdio;
    use std::sync::mpsc;

    let missing = missing_tools(&["aria2c", "curl"]);
    if !missing.is_empty() {
        return DownloadResult::fail(url, &format!("Missing tools: {}", missing.join(", ")));
    }

    let real_url = extract_real_url(url);
    let endpoint = match web_url(&real_url) {
        Ok(u) => u,
        Err(e) => return DownloadResult::fail(url, &e),
    };
    info!("OTA endpoint: {}", endpoint);

    on_progress(DownloadProgress {
        raw_line: "Resolving CDN link...".into(),
        ..Default::default()
    });

    let cdn_url = match resolve_cdn(&endpoint) {
        Ok(c) => c.to_string(),
        Err(e) => return DownloadResult::fail(&real_url, &e),
    };
    info!("CDN URL: {}", cdn_url);

    // Try original URL first, then CDN URL for expiry info
    let (mut expires_ts, mut expires_label) = parse_link_expiry(url);
    if expires_ts == 0 {
        (expires_ts, expires_label) = parse_link_expiry(&cdn_url);
    }
    on_progress(DownloadProgress {
        link_expires_in: expires_label.clone(),
        link_expires_ts: expires_ts,
        raw_line: format!("CDN resolved. Link valid: {}", expires_label),
        ..Default::default()
    });

    let mut cmd = build_aria2c_cmd(&cdn_url, output_dir, connections);
    // Pipe stdout — aria2c writes --show-console-readout progress lines there.
    // Stderr is inherited so error messages appear in terminal.
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return DownloadResult::fail(&real_url, &format!("aria2c: {}", e)),
    };

    let (ptx, prx) = mpsc::channel::<DownloadProgress>();
    let stdout = child.stdout.take();

    let reader_handle = std::thread::spawn(move || {
        use std::io::Read;
        if let Some(mut stdout) = stdout {
            let mut buf = String::new();
            let mut byte = [0u8; 1];
            loop {
                match stdout.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {
                        let ch = char::from(byte[0]);
                        if ch == '\r' || ch == '\n' {
                            let trimmed = buf.trim().to_string();
                            if !trimmed.is_empty() {
                                let p = if trimmed.contains("DL:")
                                    || (trimmed.contains('%')
                                        && (trimmed.starts_with('#') || trimmed.starts_with("[#")))
                                {
                                    parse_aria2c_progress(&trimmed, expires_ts).unwrap_or_else(
                                        || DownloadProgress {
                                            link_expires_ts: expires_ts,
                                            raw_line: trimmed.clone(),
                                            ..Default::default()
                                        },
                                    )
                                } else {
                                    DownloadProgress {
                                        link_expires_ts: expires_ts,
                                        raw_line: trimmed,
                                        ..Default::default()
                                    }
                                };
                                let _ = ptx.send(p);
                            }
                            buf.clear();
                        } else {
                            buf.push(ch);
                        }
                    }
                    Err(_) => break,
                }
            }
            // flush remaining
            let trimmed = buf.trim().to_string();
            if !trimmed.is_empty() {
                let _ = ptx.send(DownloadProgress {
                    link_expires_ts: expires_ts,
                    raw_line: trimmed,
                    ..Default::default()
                });
            }
        }
    });

    // Poll progress + handle cancel
    loop {
        while let Ok(p) = prx.try_recv() {
            on_progress(p);
        }
        if cancel.is_cancelled() {
            info!("Download cancelled by user");
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader_handle.join();
            on_progress(DownloadProgress {
                raw_line: "Download cancelled".into(),
                ..Default::default()
            });
            return DownloadResult {
                success: false,
                url: real_url,
                cdn_url,
                output_path: None,
                error: "Cancelled".into(),
            };
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    while let Ok(p) = prx.try_recv() {
        on_progress(p);
    }

    let exit_status = child.wait();
    let _ = reader_handle.join();

    let (success, exit_code) = match &exit_status {
        Ok(s) => (s.success(), s.code().unwrap_or(-1)),
        Err(_) => (false, -1),
    };

    if !success {
        return DownloadResult {
            success: false,
            url: real_url,
            cdn_url,
            output_path: None,
            error: format!("aria2c exited {}", exit_code),
        };
    }

    on_progress(DownloadProgress {
        percent: 100.0,
        eta: "done".into(),
        link_expires_in: expires_label,
        link_expires_ts: expires_ts,
        raw_line: "Download complete".into(),
        ..Default::default()
    });

    let out = resolve_output_path(&cdn_url, output_dir);
    DownloadResult {
        success: true,
        url: real_url,
        cdn_url,
        output_path: if out.exists() { Some(out) } else { None },
        error: String::new(),
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Build an aria2c `Command` with shared flags.
fn build_aria2c_cmd(cdn_url: &str, output_dir: Option<&Path>, connections: u32) -> Command {
    let mut cmd = Command::new("aria2c");
    cmd.arg(format!("-x{}", connections))
        .arg(format!("-s{}", connections))
        .arg("-k")
        .arg("1M")
        .arg("--file-allocation=none")
        .arg("--summary-interval=1") // summary line every second to stderr
        .arg("--human-readable=true")
        .arg("--console-log-level=notice") // print [NOTICE] lines to stderr
        .arg("--show-console-readout=true") // force progress lines even when not a TTY
        .arg("--download-result=default")
        // A server must not be able to turn a firmware download into a
        // BitTorrent or Metalink session by answering with a .torrent file.
        .arg("--follow-torrent=false")
        .arg("--follow-metalink=false");

    if let Some(dir) = output_dir {
        std::fs::create_dir_all(dir).ok();
        cmd.arg("-d").arg(dir);
    }
    cmd.arg(cdn_url);
    cmd
}

/// Derive expected output path from CDN URL + output directory.
fn resolve_output_path(cdn_url: &str, output_dir: Option<&Path>) -> PathBuf {
    let filename = cdn_url
        .split('/')
        .next_back()
        .unwrap_or("firmware.zip")
        .split('?')
        .next()
        .unwrap_or("firmware.zip");

    output_dir
        .map(|d| d.join(filename))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(filename))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_4pda() {
        assert_eq!(
            extract_real_url("https://4pda.to/goto/?u=https%3A%2F%2Fexample.com%2Ffw.zip"),
            "https://example.com/fw.zip"
        );
    }

    #[test]
    fn test_extract_direct() {
        assert_eq!(
            extract_real_url("https://example.com/fw.zip"),
            "https://example.com/fw.zip"
        );
    }

    #[test]
    fn only_absolute_http_urls_get_through() {
        for ok in [
            "https://example.com/fw.zip",
            "http://ota.example.com/a?b=c",
            "  https://example.com/x  ",
        ] {
            let u = web_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(u.as_str().starts_with("http"), "{ok}");
        }
        for bad in [
            "--enable-rpc=true",
            "-i/tmp/list",
            "--conf-path=/tmp/x",
            "file:///etc/passwd",
            "magnet:?xt=urn:btih:abc",
            "ftp://example.com/fw.zip",
            "javascript:alert(1)",
            "/tmp/local.torrent",
            "https://",
            "",
        ] {
            assert!(web_url(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn test_extract_4pda_stat_link() {
        assert_eq!(
            extract_real_url(
                "https://4pda.to/stat/go?url=https%3A%2F%2Fgauss-opexcostmanual-sg.allawnofs.com%2Fremove-78df%2Fcomponent-ota%2Ffw.zip"
            ),
            "https://gauss-opexcostmanual-sg.allawnofs.com/remove-78df/component-ota/fw.zip"
        );
    }

    #[test]
    fn a_4pda_link_cannot_smuggle_an_option() {
        // Decodes to `--enable-rpc=true`: an aria2c option, not a URL.
        let unwrapped = extract_real_url("https://4pda.to/goto/?u=--enable-rpc%3Dtrue");
        assert_eq!(unwrapped, "--enable-rpc=true");
        assert!(web_url(&unwrapped).is_err());
    }

    #[test]
    fn location_header_is_found_case_insensitively() {
        let h = "HTTP/1.1 302 Found\r\nServer: x\r\nLOCATION: https://cdn.example.com/fw.zip\r\n";
        assert_eq!(location_header(h), Some("https://cdn.example.com/fw.zip"));
        assert_eq!(
            location_header("HTTP/1.1 200 OK\r\nContent-Length: 1\r\n"),
            None
        );
        assert_eq!(location_header("Location:   \r\n"), None);
    }

    #[test]
    fn a_redirect_target_must_also_be_http() {
        let base = web_url("https://ota.example.com/get/fw").expect("base");
        // Relative targets resolve against the sender and stay on http(s).
        let rel = base.join("/cdn/fw.zip").expect("join");
        assert_eq!(
            web_url(rel.as_str()).expect("relative").as_str(),
            "https://ota.example.com/cdn/fw.zip"
        );
        for loc in ["file:///etc/passwd", "magnet:?xt=urn:btih:abc", "ftp://x/y"] {
            let joined = base.join(loc).expect("join");
            assert!(web_url(joined.as_str()).is_err(), "{loc}");
        }
        // `--x` as a Location is just a relative path, so it can never come
        // out as an option.
        let dash = base.join("--enable-rpc=true").expect("join");
        assert!(dash.as_str().starts_with("https://"));
    }

    #[test]
    fn test_parse_sizes() {
        let line = "[#a1b2c3 245MiB/1.2GiB(19%) CN:8 DL:45MiB/s ETA:22s]";
        let (dl, total) = parse_sizes(line);
        assert_eq!(dl, "245MiB");
        assert_eq!(total, "1.2GiB");
    }

    #[test]
    fn test_parse_progress_percent() {
        let line = "[#a1b2c3 245MiB/1.2GiB(19%) CN:8 DL:45MiB/s ETA:22s]";
        let p = parse_aria2c_progress(line, 0).expect("failed to parse progress");
        assert!((p.percent - 19.0).abs() < 0.01);
        assert_eq!(p.speed, "45MiB/s");
        assert_eq!(p.eta, "22s");
    }

    #[test]
    fn test_link_expiry_future() {
        let future_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time error")
            .as_secs()
            + 7500; // ~2h 5m
        let url = format!("https://cdn.example.com/fw.zip?e={}&sig=abc", future_ts);
        let (ts, label) = parse_link_expiry(&url);
        assert_eq!(ts, future_ts);
        assert!(
            label.contains('h'),
            "expected hours in label, got: {}",
            label
        );
    }

    #[test]
    fn test_link_expiry_expired() {
        let url = "https://cdn.example.com/fw.zip?e=1000&sig=abc";
        let (_, label) = parse_link_expiry(url);
        assert_eq!(label, "EXPIRED");
    }
}
