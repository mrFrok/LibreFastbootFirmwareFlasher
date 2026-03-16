//! Anti-Rollback (ARB) version checker for Qualcomm-based OnePlus / OPPO devices.
//!
//! Parses xbl_config.img directly using the same algorithm as arbextract:
//!   <https://github.com/koaaN/arbextract>
//!
//! Algorithm (from arbextract.c):
//!   1. Parse ELF64 header → locate program headers
//!   2. Find last PT_NULL segment with filesz > 0 (HASH segment)
//!   3. Scan HASH segment for Hash Table Segment Header
//!   4. Jump to OEM Metadata at header_off + 36 + common_sz + qti_sz
//!   5. Read: major (4B), minor (4B), arb (4B)
//!
//! ARB == 0: hard ARB not enforced (safe).
//! ARB  > 0: hard ARB active — flashing lower version will brick the device.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use regex::Regex;

// ELF constants
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const EI_CLASS: usize = 4;
const PT_NULL: u32 = 0;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Anti-Rollback version information.
#[derive(Debug, Clone)]
pub struct ArbInfo {
    pub version: Option<u32>,
    pub source: String,
    pub oem_major: Option<u32>,
    pub oem_minor: Option<u32>,
}

impl ArbInfo {
    /// True if ARB is active (version > 0).
    pub fn enforced(&self) -> bool {
        matches!(self.version, Some(v) if v > 0)
    }

    fn unknown(source: &str) -> Self {
        Self {
            version: None,
            source: source.to_string(),
            oem_major: None,
            oem_minor: None,
        }
    }
}

impl fmt::Display for ArbInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.version {
            None => write!(f, "ARB version: unknown"),
            Some(0) => write!(f, "ARB version: 0 (hard ARB not enforced)"),
            Some(v) => write!(f, "ARB version: {} (hard ARB ACTIVE)", v),
        }
    }
}

/// Result of comparing firmware vs device ARB.
#[derive(Debug, Clone)]
pub struct ArbCheckResult {
    pub firmware_arb: ArbInfo,
    pub device_arb: ArbInfo,
    pub safe: bool,
    pub warning: String,
    pub detail: String,
}

/// Method used to read ARB from the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceArbMethod {
    Dump,
    Getvar,
    Failed,
}

impl fmt::Display for DeviceArbMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dump => write!(f, "dump"),
            Self::Getvar => write!(f, "getvar"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// ELF parser — extract ARB from xbl_config.img
// ---------------------------------------------------------------------------

/// Parse xbl_config.img and extract its ARB version.
/// Mirrors the algorithm from arbextract.c.
pub fn extract_arb_from_xbl_config(path: &Path) -> ArbInfo {
    if !path.exists() {
        warn!("xbl_config.img not found: {}", path.display());
        return ArbInfo::unknown("file not found");
    }

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            log::error!("Cannot read {}: {}", path.display(), e);
            return ArbInfo::unknown(&format!("read error: {}", e));
        }
    };

    if data.len() < 64 {
        return ArbInfo::unknown("file too small for ELF header");
    }
    if data[..4] != ELF_MAGIC || data[EI_CLASS] != ELFCLASS64 {
        return ArbInfo::unknown("not a valid ELF64 file");
    }

    let e_phoff = u64::from_le_bytes(data[0x20..0x28].try_into().unwrap()) as usize;
    let e_phentsz = u16::from_le_bytes(data[0x36..0x38].try_into().unwrap()) as usize;
    let e_phnum = u16::from_le_bytes(data[0x38..0x3A].try_into().unwrap()) as usize;

    debug!(
        "ELF64: e_phoff={:#x} e_phentsz={} e_phnum={}",
        e_phoff, e_phentsz, e_phnum
    );

    // Find last PT_NULL segment with filesz > 0 (HASH segment)
    let (mut hash_off, mut hash_size) = (0usize, 0usize);
    for i in (0..e_phnum).rev() {
        let ph = e_phoff + i * e_phentsz;
        if ph + 56 > data.len() {
            continue;
        }
        let p_type = u32::from_le_bytes(data[ph..ph + 4].try_into().unwrap());
        let p_offset = u64::from_le_bytes(data[ph + 8..ph + 16].try_into().unwrap()) as usize;
        let p_filesz = u64::from_le_bytes(data[ph + 32..ph + 40].try_into().unwrap()) as usize;
        if p_type == PT_NULL && p_filesz > 0 {
            hash_off = p_offset;
            hash_size = p_filesz;
            debug!("HASH segment: offset={:#x} size={:#x}", hash_off, hash_size);
            break;
        }
    }

    if hash_size == 0 {
        return ArbInfo::unknown("HASH segment not found in ELF");
    }
    if hash_off + hash_size > data.len() {
        return ArbInfo::unknown("HASH segment extends beyond file");
    }

    let seg = &data[hash_off..hash_off + hash_size];

    // Scan for Hash Table Segment Header
    let scan_limit = std::cmp::min(0x1000, seg.len().saturating_sub(36));
    let mut header_off: Option<usize> = None;
    let mut off = 0;
    while off < scan_limit {
        if off + 20 > seg.len() {
            break;
        }
        let version = u32::from_le_bytes(seg[off..off + 4].try_into().unwrap());
        let common_sz = u32::from_le_bytes(seg[off + 4..off + 8].try_into().unwrap()) as usize;
        let qti_sz = u32::from_le_bytes(seg[off + 8..off + 12].try_into().unwrap()) as usize;
        let oem_sz = u32::from_le_bytes(seg[off + 12..off + 16].try_into().unwrap()) as usize;
        let hash_tbl_sz = u32::from_le_bytes(seg[off + 16..off + 20].try_into().unwrap()) as usize;

        if (1..=10).contains(&version)
            && common_sz <= 0x1000
            && oem_sz <= 0x4000
            && hash_tbl_sz <= 0x4000
            && off + 36 + common_sz + qti_sz + oem_sz <= seg.len()
        {
            debug!(
                "Hash table header at seg+{:#x}: ver={} common={} qti={} oem={}",
                off, version, common_sz, qti_sz, oem_sz
            );
            header_off = Some(off);
            break;
        }
        off += 4;
    }

    let header_off = match header_off {
        Some(o) => o,
        None => return ArbInfo::unknown("hash table header not found in HASH segment"),
    };

    // Read OEM metadata
    let common_sz =
        u32::from_le_bytes(seg[header_off + 4..header_off + 8].try_into().unwrap()) as usize;
    let qti_sz =
        u32::from_le_bytes(seg[header_off + 8..header_off + 12].try_into().unwrap()) as usize;
    let oem_off = header_off + 36 + common_sz + qti_sz;

    if oem_off + 12 > seg.len() {
        return ArbInfo::unknown("OEM metadata offset out of bounds");
    }

    let oem_major = u32::from_le_bytes(seg[oem_off..oem_off + 4].try_into().unwrap());
    let oem_minor = u32::from_le_bytes(seg[oem_off + 4..oem_off + 8].try_into().unwrap());
    let arb = u32::from_le_bytes(seg[oem_off + 8..oem_off + 12].try_into().unwrap());

    let fname = path.file_name().unwrap_or_default().to_string_lossy();
    info!(
        "OEM Metadata Major={} Minor={} ARB={} (from {})",
        oem_major, oem_minor, arb, fname
    );

    ArbInfo {
        version: Some(arb),
        source: format!("xbl_config ELF OEM metadata ({})", fname),
        oem_major: Some(oem_major),
        oem_minor: Some(oem_minor),
    }
}

// ---------------------------------------------------------------------------
// File locators
// ---------------------------------------------------------------------------

/// Locate xbl_config.img (or _a/_b variant) under search_dir, recursively.
pub fn find_xbl_config(search_dir: &Path) -> Option<PathBuf> {
    for name in &["xbl_config.img", "xbl_config_a.img", "xbl_config_b.img"] {
        let direct = search_dir.join(name);
        if direct.exists() {
            return Some(direct);
        }
    }
    // Recursive search
    if let Ok(entries) = fs::read_dir(search_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(found) = find_xbl_config(&p) {
                    return Some(found);
                }
            } else {
                let fname = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                if fname.starts_with("xbl_config") && fname.ends_with(".img") {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Backward compat — if given xbl.img, look for xbl_config nearby.
pub fn extract_arb_from_xbl(xbl_path: &Path) -> ArbInfo {
    if xbl_path
        .file_stem()
        .map(|s| s.to_string_lossy().starts_with("xbl_config"))
        .unwrap_or(false)
    {
        return extract_arb_from_xbl_config(xbl_path);
    }
    if let Some(parent) = xbl_path.parent() {
        if let Some(config) = find_xbl_config(parent) {
            return extract_arb_from_xbl_config(&config);
        }
    }
    ArbInfo::unknown("xbl_config.img not found next to xbl.img")
}

// ---------------------------------------------------------------------------
// Device ARB via fastboot
// ---------------------------------------------------------------------------

fn fastboot_dump(partition: &str, dest: &Path, serial: Option<&str>) -> bool {
    let mut base: Vec<String> = vec!["fastboot".into()];
    if let Some(s) = serial {
        base.push("-s".into());
        base.push(s.into());
    }
    let dest_str: String = dest.to_string_lossy().into();

    for method in &["dump_partition", "fetch"] {
        let mut cmd = base.clone();
        cmd.extend([method.to_string(), partition.into(), dest_str.clone()]);
        let r = crate::utils::run_cmd_owned(&cmd, 60);
        if r.success() && dest.exists() && fs::metadata(dest).map(|m| m.len() > 0).unwrap_or(false)
        {
            debug!("{} succeeded for {}", method, partition);
            return true;
        }
    }
    warn!("Could not dump partition: {}", partition);
    false
}

/// Dump xbl_config from connected device. Tries slots: _a, _b, bare.
pub fn dump_xbl_config_from_device(
    serial: Option<&str>,
    dest_dir: Option<&Path>,
) -> Option<PathBuf> {
    let out = match dest_dir {
        Some(d) => d.to_path_buf(),
        None => {
            let t = std::env::temp_dir().join("lfff_arb");
            fs::create_dir_all(&t).ok()?;
            t
        }
    };
    for part in &["xbl_config_a", "xbl_config_b", "xbl_config"] {
        let dest = out.join(format!("{}.img", part));
        info!("Attempting to dump {} from device ...", part);
        if fastboot_dump(part, &dest, serial) {
            return Some(dest);
        }
    }
    None
}

/// Read ARB version from connected device (dump → getvar fallback).
pub fn get_device_arb_version(serial: Option<&str>) -> (ArbInfo, DeviceArbMethod) {
    // Step 1: dump xbl_config
    if let Some(dumped) = dump_xbl_config_from_device(serial, None) {
        let mut arb = extract_arb_from_xbl_config(&dumped);
        if arb.version.is_some() {
            arb.source = format!(
                "dumped {} from device",
                dumped.file_name().unwrap_or_default().to_string_lossy()
            );
            let _ = fs::remove_file(&dumped);
            return (arb, DeviceArbMethod::Dump);
        }
    }

    // Step 2: fastboot getvar fallback
    info!("xbl_config dump failed — falling back to fastboot getvar");
    let mut cmd: Vec<String> = vec!["fastboot".into()];
    if let Some(s) = serial {
        cmd.push("-s".into());
        cmd.push(s.into());
    }
    cmd.push("getvar".into());
    cmd.push("anti-rollback-version".into());

    let r = crate::utils::run_cmd_owned(&cmd, 15);
    if r.code != -1 {
        let output = format!("{}\n{}", r.stdout, r.stderr);
        for var in &["anti-rollback-version", "version-anti-rollback"] {
            let pat = format!(r"(?i){}:\s*(\d+)", regex::escape(var));
            if let Ok(re) = Regex::new(&pat) {
                if let Some(caps) = re.captures(&output) {
                    if let Some(m) = caps.get(1) {
                        if let Ok(v) = m.as_str().parse::<u32>() {
                            info!("Device ARB version (fastboot getvar): {}", v);
                            return (
                                ArbInfo {
                                    version: Some(v),
                                    source: format!("fastboot getvar {}", var),
                                    oem_major: None,
                                    oem_minor: None,
                                },
                                DeviceArbMethod::Getvar,
                            );
                        }
                    }
                }
            }
        }
    }
    (
        ArbInfo::unknown("could not read ARB from device"),
        DeviceArbMethod::Failed,
    )
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// Compare firmware ARB vs device ARB and produce a safety assessment.
pub fn compare_arb_versions(firmware_arb: &ArbInfo, device_arb: &ArbInfo) -> ArbCheckResult {
    let fw = firmware_arb.version;
    let dev = device_arb.version;

    if fw == Some(0) {
        return ArbCheckResult { firmware_arb: firmware_arb.clone(), device_arb: device_arb.clone(), safe: true, warning: String::new(),
            detail: "This firmware has ARB = 0 (hard ARB not enforced).\nHowever, if your device previously had ARB > 0, the fuse may already be set.".into() };
    }
    if fw.is_none() || dev.is_none() {
        let mut u = Vec::new();
        if fw.is_none() {
            u.push("firmware");
        }
        if dev.is_none() {
            u.push("device");
        }
        return ArbCheckResult {
            firmware_arb: firmware_arb.clone(),
            device_arb: device_arb.clone(),
            safe: false,
            warning: format!("Could not determine ARB version for: {}.", u.join(", ")),
            detail: "Proceed only if you are sure the firmware is not a downgrade.".into(),
        };
    }
    let (fw, dev) = (fw.unwrap(), dev.unwrap());
    if fw == dev {
        return ArbCheckResult {
            firmware_arb: firmware_arb.clone(),
            device_arb: device_arb.clone(),
            safe: true,
            warning: String::new(),
            detail: format!("ARB versions match ({}). Safe to flash.", fw),
        };
    }
    if fw > dev {
        return ArbCheckResult {
            firmware_arb: firmware_arb.clone(),
            device_arb: device_arb.clone(),
            safe: false,
            warning: format!(
                "Firmware ARB ({}) is HIGHER than device ARB ({}).\nRolling back to ARB < {} will be IMPOSSIBLE after flashing.",
                fw, dev, fw
            ),
            detail: "You can still flash, but downgrading afterwards will not be possible.".into(),
        };
    }
    // fw < dev — DANGER
    ArbCheckResult {
        firmware_arb: firmware_arb.clone(),
        device_arb: device_arb.clone(),
        safe: false,
        warning: format!(
            "DANGER: Firmware ARB ({}) is LOWER than device ARB ({}).\nFlashing this firmware WILL BRICK the device.",
            fw, dev
        ),
        detail: "Do NOT flash unless you fully understand the consequences.".into(),
    }
}

// ---------------------------------------------------------------------------
// Interactive confirmation gate
// ---------------------------------------------------------------------------

/// Display ARB check results and prompt user for confirmation.
/// Returns true if the user confirmed to proceed.
pub fn arb_confirmation_gate(result: &ArbCheckResult, device_method: &str) -> bool {
    use colored::Colorize;
    use std::io::{self, Write};

    let fw = &result.firmware_arb;
    let dev = &result.device_arb;

    println!();
    println!(
        "{}",
        format!("── ARB (Anti-Rollback) check {}", "─".repeat(33)).dimmed()
    );

    let fw_ver = fw
        .version
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".yellow().to_string());
    println!(
        "  {}  ARB {}  {}",
        "Firmware :".dimmed(),
        fw_ver.bold(),
        format!("({})", fw.source).dimmed()
    );

    if device_method != "none" {
        let dev_ver = dev
            .version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".yellow().to_string());
        let label = match device_method {
            "dump" => "(dumped xbl_config from device)".dimmed().to_string(),
            "getvar" => "(fastboot getvar)".dimmed().to_string(),
            "failed" => "(could not read from device)".yellow().to_string(),
            _ => String::new(),
        };
        println!(
            "  {}  ARB {}  {}",
            "Device   :".dimmed(),
            dev_ver.bold(),
            label
        );
    }

    if result.safe {
        if fw.version == Some(0) {
            println!();
            for line in result.detail.lines() {
                println!("  {}", line.trim().yellow());
            }
            println!("{}", "─".repeat(60).dimmed());
            print!("\n  {} ", "Understood, continue? (yes / no):".bold());
            io::stdout().flush().ok();
            let mut ans = String::new();
            io::stdin().read_line(&mut ans).ok();
            return ans.trim().to_lowercase() == "yes";
        }
        println!("  {}  {}", "✓".green(), result.detail);
        println!("{}", "─".repeat(60).dimmed());
        return true;
    }

    println!();
    for line in result.warning.lines() {
        if line.contains("HIGHER") {
            println!("  {}", line.truecolor(255, 165, 0));
        } else {
            println!("  {}", line.red());
        }
    }
    if !result.detail.is_empty() {
        println!("  {}", result.detail.dimmed());
    }
    println!("{}", "─".repeat(60).dimmed());
    print!(
        "\n  {} ",
        "Type YES to proceed anyway, anything else to abort:".bold()
    );
    io::stdout().flush().ok();
    let mut ans = String::new();
    io::stdin().read_line(&mut ans).ok();
    ans.trim() == "YES"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arb_display() {
        assert!(
            ArbInfo {
                version: Some(3),
                source: "t".into(),
                oem_major: None,
                oem_minor: None
            }
            .enforced()
        );
        assert!(
            !ArbInfo {
                version: Some(0),
                source: "t".into(),
                oem_major: None,
                oem_minor: None
            }
            .enforced()
        );
    }

    #[test]
    fn test_compare_same() {
        let a = ArbInfo {
            version: Some(5),
            source: "fw".into(),
            oem_major: None,
            oem_minor: None,
        };
        let b = ArbInfo {
            version: Some(5),
            source: "dev".into(),
            oem_major: None,
            oem_minor: None,
        };
        assert!(compare_arb_versions(&a, &b).safe);
    }

    #[test]
    fn test_compare_downgrade() {
        let fw = ArbInfo {
            version: Some(3),
            source: "fw".into(),
            oem_major: None,
            oem_minor: None,
        };
        let dev = ArbInfo {
            version: Some(5),
            source: "dev".into(),
            oem_major: None,
            oem_minor: None,
        };
        let r = compare_arb_versions(&fw, &dev);
        assert!(!r.safe);
        assert!(r.warning.contains("DANGER"));
    }
}
