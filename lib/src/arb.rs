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

use tracing::{debug, info, warn};

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
            tracing::error!("Cannot read {}: {}", path.display(), e);
            return ArbInfo::unknown(&format!("read error: {}", e));
        }
    };

    if data.len() < 64 {
        return ArbInfo::unknown("file too small for ELF header");
    }
    if data[..4] != ELF_MAGIC || data[EI_CLASS] != ELFCLASS64 {
        return ArbInfo::unknown("not a valid ELF64 file");
    }

    let e_phoff = u64::from_le_bytes(data[0x20..0x28].try_into().unwrap_or([0; 8])) as usize;
    let e_phentsz = u16::from_le_bytes(data[0x36..0x38].try_into().unwrap_or([0; 2])) as usize;
    let e_phnum = u16::from_le_bytes(data[0x38..0x3A].try_into().unwrap_or([0; 2])) as usize;
    
    if e_phentsz == 0 {
        return ArbInfo::unknown("invalid ELF program header entry size");
    }

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
        let p_type = u32::from_le_bytes(
            data[ph..ph + 4].try_into().unwrap_or([0; 4])
        );
        let p_offset = u64::from_le_bytes(
            data[ph + 8..ph + 16].try_into().unwrap_or([0; 8])
        ) as usize;
        let p_filesz = u64::from_le_bytes(
            data[ph + 32..ph + 40].try_into().unwrap_or([0; 8])
        ) as usize;
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
        let version = u32::from_le_bytes(
            seg[off..off + 4].try_into().unwrap_or([0; 4])
        );
        let common_sz = u32::from_le_bytes(
            seg[off + 4..off + 8].try_into().unwrap_or([0; 4])
        ) as usize;
        let qti_sz = u32::from_le_bytes(
            seg[off + 8..off + 12].try_into().unwrap_or([0; 4])
        ) as usize;
        let oem_sz = u32::from_le_bytes(
            seg[off + 12..off + 16].try_into().unwrap_or([0; 4])
        ) as usize;
        let hash_tbl_sz = u32::from_le_bytes(
            seg[off + 16..off + 20].try_into().unwrap_or([0; 4])
        ) as usize;

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
    let common_sz = u32::from_le_bytes(
        seg[header_off + 4..header_off + 8].try_into().unwrap_or([0; 4])
    ) as usize;
    let qti_sz = u32::from_le_bytes(
        seg[header_off + 8..header_off + 12].try_into().unwrap_or([0; 4])
    ) as usize;
    let oem_off = header_off + 36 + common_sz + qti_sz;

    if oem_off + 12 > seg.len() {
        return ArbInfo::unknown("OEM metadata offset out of bounds");
    }

    let oem_major = u32::from_le_bytes(
        seg[oem_off..oem_off + 4].try_into().unwrap_or([0; 4])
    );
    let oem_minor = u32::from_le_bytes(
        seg[oem_off + 4..oem_off + 8].try_into().unwrap_or([0; 4])
    );
    let arb = u32::from_le_bytes(
        seg[oem_off + 8..oem_off + 12].try_into().unwrap_or([0; 4])
    );

    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
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
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_lowercase())
                    .unwrap_or_default();
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
    if let Some(parent) = xbl_path.parent()
        && let Some(config) = find_xbl_config(parent) {
            return extract_arb_from_xbl_config(&config);
        }
    ArbInfo::unknown("xbl_config.img not found next to xbl.img")
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

}
