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
    let fname = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    extract_arb_from_bytes(&data, fname)
}

/// Parse an xbl_config image already in memory; `name` only labels the
/// result's `source`.
pub fn extract_arb_from_bytes(data: &[u8], name: &str) -> ArbInfo {
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
        let p_type = u32::from_le_bytes(data[ph..ph + 4].try_into().unwrap_or([0; 4]));
        let p_offset =
            u64::from_le_bytes(data[ph + 8..ph + 16].try_into().unwrap_or([0; 8])) as usize;
        let p_filesz =
            u64::from_le_bytes(data[ph + 32..ph + 40].try_into().unwrap_or([0; 8])) as usize;
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
        let version = u32::from_le_bytes(seg[off..off + 4].try_into().unwrap_or([0; 4]));
        let common_sz =
            u32::from_le_bytes(seg[off + 4..off + 8].try_into().unwrap_or([0; 4])) as usize;
        let qti_sz =
            u32::from_le_bytes(seg[off + 8..off + 12].try_into().unwrap_or([0; 4])) as usize;
        let oem_sz =
            u32::from_le_bytes(seg[off + 12..off + 16].try_into().unwrap_or([0; 4])) as usize;
        let hash_tbl_sz =
            u32::from_le_bytes(seg[off + 16..off + 20].try_into().unwrap_or([0; 4])) as usize;

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
        seg[header_off + 4..header_off + 8]
            .try_into()
            .unwrap_or([0; 4]),
    ) as usize;
    let qti_sz = u32::from_le_bytes(
        seg[header_off + 8..header_off + 12]
            .try_into()
            .unwrap_or([0; 4]),
    ) as usize;
    let oem_off = header_off + 36 + common_sz + qti_sz;

    if oem_off + 12 > seg.len() {
        return ArbInfo::unknown("OEM metadata offset out of bounds");
    }

    let oem_major = u32::from_le_bytes(seg[oem_off..oem_off + 4].try_into().unwrap_or([0; 4]));
    let oem_minor = u32::from_le_bytes(seg[oem_off + 4..oem_off + 8].try_into().unwrap_or([0; 4]));
    let arb = u32::from_le_bytes(seg[oem_off + 8..oem_off + 12].try_into().unwrap_or([0; 4]));

    info!(
        "OEM Metadata Major={} Minor={} ARB={} (from {})",
        oem_major, oem_minor, arb, name
    );

    ArbInfo {
        version: Some(arb),
        source: format!("xbl_config ELF OEM metadata ({})", name),
        oem_major: Some(oem_major),
        oem_minor: Some(oem_minor),
    }
}

// ---------------------------------------------------------------------------
// Verdict for a firmware directory
// ---------------------------------------------------------------------------

/// What is known about a firmware directory's ARB — the one answer every
/// frontend bases its warning on.
///
/// "Could not tell" is kept apart from zero on purpose: zero means the
/// anti-rollback counter will not rise, while an unknown means nothing at all,
/// and presenting one as the other is how a user talks themselves into a brick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbVerdict {
    /// Read from xbl_config's OEM metadata.
    Known(u32),
    /// There is no xbl_config.img anywhere under the directory.
    NotFound,
    /// xbl_config.img exists but could not be parsed.
    Unparsed,
}

/// Decide the ARB of the firmware extracted into `dir`.
///
/// Reads the xbl_config that flashing will use — picked by the same
/// `collect_images` — so a folder with several (`xbl_config_a.img` and
/// `XBL_CONFIG.img`, say) cannot show the ARB of one and flash the other.
pub fn firmware_arb(dir: &Path) -> ArbVerdict {
    match crate::flasher::collect_images(dir).get("xbl_config") {
        None => ArbVerdict::NotFound,
        Some(xbl) => match extract_arb_from_xbl_config(xbl).version {
            Some(v) => ArbVerdict::Known(v),
            None => ArbVerdict::Unparsed,
        },
    }
}

// ---------------------------------------------------------------------------
// ARB of the firmware installed on the phone
// ---------------------------------------------------------------------------

/// Why the installed firmware's ARB could not be read.
///
/// fastbootd cannot help here: its `fetch` is compiled out of user builds,
/// refused on locked devices and limited to vendor_boot. The partition can
/// only be read from Android or a recovery over adb, as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceArbError {
    /// No phone in adb mode (booted Android with USB debugging, or recovery).
    NoDevice,
    /// The adb shell is not root and `su` was refused or is missing.
    NoRoot,
    /// Root works but the partition could not be read.
    ReadFailed(String),
    /// The partition was read but holds no ARB this parser understands.
    Unparsed,
}

impl fmt::Display for DeviceArbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no phone found over adb"),
            Self::NoRoot => write!(f, "root is required to read xbl_config from the phone"),
            Self::ReadFailed(e) => write!(f, "could not read xbl_config from the phone: {e}"),
            Self::Unparsed => write!(f, "xbl_config from the phone could not be parsed"),
        }
    }
}

/// ARB read from the phone's active slot.
#[derive(Debug, Clone)]
pub struct DeviceArb {
    /// `a`, `b`, or empty on a non-A/B phone.
    pub slot: String,
    pub info: ArbInfo,
}

/// Read the ARB of the firmware installed on the phone, from the active
/// slot's xbl_config, over adb as root.
pub fn device_arb(serial: Option<&str>) -> Result<DeviceArb, DeviceArbError> {
    use crate::device::{AdbReadError, AdbRoot};
    let to_arb = |e: AdbReadError| match e {
        AdbReadError::NoDevice => DeviceArbError::NoDevice,
        AdbReadError::NoRoot => DeviceArbError::NoRoot,
        AdbReadError::ReadFailed(e) => DeviceArbError::ReadFailed(e),
    };
    let root = AdbRoot::open(serial).map_err(to_arb)?;
    let part = format!("xbl_config{}", root.slot_suffix);
    let data = root.read(&part).map_err(to_arb)?;
    debug!("Read {} bytes of {}", data.len(), part);
    if !data.starts_with(&ELF_MAGIC) {
        return Err(DeviceArbError::Unparsed);
    }
    let info = extract_arb_from_bytes(&data, &format!("{part} on the phone"));
    if info.version.is_none() {
        return Err(DeviceArbError::Unparsed);
    }
    Ok(DeviceArb {
        slot: root.slot_suffix.trim_start_matches('_').to_string(),
        info,
    })
}

/// What flashing a firmware does to a phone's anti-rollback, from the ARB
/// installed on the phone and the firmware's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbCompare {
    /// Same ARB: nothing changes.
    Same,
    /// The firmware's is higher: the counter rises and older firmware is
    /// locked out for good.
    Raises,
    /// The firmware's is lower: the bootloader rejects it — a hard brick on
    /// Qualcomm.
    Lower,
}

pub fn compare_arb(device: u32, firmware: u32) -> ArbCompare {
    use std::cmp::Ordering::*;
    match firmware.cmp(&device) {
        Equal => ArbCompare::Same,
        Greater => ArbCompare::Raises,
        Less => ArbCompare::Lower,
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
        && let Some(config) = find_xbl_config(parent)
    {
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

    // A minimal xbl_config, built byte by byte in the layout the parser
    // expects. It is laid out so that every step of the algorithm has to be
    // right for the ARB to come out: like a real Qualcomm image it opens with
    // a PT_NULL covering the headers, so only taking the *last* PT_NULL finds
    // the hash segment; the table header sits behind zero padding the scanner
    // has to step over; and the common and QTI blocks are non-empty, so the
    // OEM offset arithmetic is actually exercised.

    const PHENT: usize = 56;
    const PT_LOAD: u32 = 1;
    // Not a multiple of 8, so only the scanner's real 4-byte stride lands on it.
    const PADDING: usize = 12;

    struct Synth {
        arb: u32,
        common_sz: u32,
        qti_sz: u32,
    }

    impl Default for Synth {
        fn default() -> Self {
            Self {
                arb: 3,
                common_sz: 0x40,
                qti_sz: 0x80,
            }
        }
    }

    fn hash_segment(s: &Synth) -> Vec<u8> {
        let mut seg = vec![0u8; PADDING];
        let header = seg.len();
        seg.extend(3u32.to_le_bytes()); // version
        seg.extend(s.common_sz.to_le_bytes());
        seg.extend(s.qti_sz.to_le_bytes());
        seg.extend(12u32.to_le_bytes()); // oem_sz: major + minor + arb
        seg.extend(0u32.to_le_bytes()); // hash_tbl_sz
        seg.resize(header + 36, 0);
        seg.resize(seg.len() + (s.common_sz + s.qti_sz) as usize, 0xAA);
        seg.extend(7u32.to_le_bytes()); // oem major
        seg.extend(9u32.to_le_bytes()); // oem minor
        seg.extend(s.arb.to_le_bytes());
        seg
    }

    fn elf_with(seg: &[u8]) -> Vec<u8> {
        let phoff = 64;
        let seg_off = phoff + 2 * PHENT;
        let mut data = vec![0u8; seg_off];
        data[..4].copy_from_slice(&ELF_MAGIC);
        data[EI_CLASS] = ELFCLASS64;
        data[0x20..0x28].copy_from_slice(&(phoff as u64).to_le_bytes());
        data[0x36..0x38].copy_from_slice(&(PHENT as u16).to_le_bytes());
        data[0x38..0x3A].copy_from_slice(&2u16.to_le_bytes());

        // Decoy: a PT_NULL over the ELF header, which holds no hash table.
        let decoy = phoff;
        data[decoy..decoy + 4].copy_from_slice(&PT_NULL.to_le_bytes());
        data[decoy + 32..decoy + 40].copy_from_slice(&(phoff as u64).to_le_bytes());

        let null = phoff + PHENT;
        data[null..null + 4].copy_from_slice(&PT_NULL.to_le_bytes());
        data[null + 8..null + 16].copy_from_slice(&(seg_off as u64).to_le_bytes());
        data[null + 32..null + 40].copy_from_slice(&(seg.len() as u64).to_le_bytes());

        data.extend_from_slice(seg);
        data
    }

    fn parse(bytes: &[u8]) -> ArbInfo {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("xbl_config.img");
        std::fs::write(&path, bytes).expect("write synthetic image");
        extract_arb_from_xbl_config(&path)
    }

    #[test]
    fn reads_arb_and_oem_version_from_a_synthetic_image() {
        let info = parse(&elf_with(&hash_segment(&Synth::default())));
        assert_eq!(info.version, Some(3));
        assert_eq!(info.oem_major, Some(7));
        assert_eq!(info.oem_minor, Some(9));
        assert!(info.enforced());
    }

    #[test]
    fn arb_zero_is_a_known_value_not_an_unknown_one() {
        let info = parse(&elf_with(&hash_segment(&Synth {
            arb: 0,
            ..Synth::default()
        })));
        assert_eq!(info.version, Some(0));
        assert!(!info.enforced());
    }

    #[test]
    fn oem_offset_follows_the_common_and_qti_sizes() {
        for (common_sz, qti_sz) in [(0, 0), (0x10, 0), (0, 0x10), (0x200, 0x300)] {
            let info = parse(&elf_with(&hash_segment(&Synth {
                arb: 5,
                common_sz,
                qti_sz,
            })));
            assert_eq!(
                info.version,
                Some(5),
                "common={common_sz:#x} qti={qti_sz:#x}"
            );
        }
    }

    #[test]
    fn damaged_images_are_unknown_rather_than_zero() {
        let good = elf_with(&hash_segment(&Synth::default()));

        let truncated = &good[..good.len() - 6]; // cut through the ARB word
        let mut elf32 = good.clone();
        elf32[EI_CLASS] = 1;
        let mut no_null = good.clone();
        for ph in [64, 64 + PHENT] {
            no_null[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        }
        let mut no_header = good.clone();
        let seg_start = 64 + 2 * PHENT;
        no_header[seg_start..].fill(0);

        for (what, bytes) in [
            ("truncated", truncated),
            ("not ELF", &b"definitely not an ELF image, just text"[..]),
            ("too small", &good[..32]),
            ("ELF32", &elf32[..]),
            ("no PT_NULL segment", &no_null[..]),
            ("no hash table header", &no_header[..]),
        ] {
            assert_eq!(parse(bytes).version, None, "{what}");
        }
    }

    #[test]
    fn verdict_separates_zero_from_both_kinds_of_unknown() {
        let dir = |bytes: Option<&[u8]>| {
            let d = tempfile::tempdir().expect("tempdir");
            if let Some(b) = bytes {
                std::fs::write(d.path().join("xbl_config.img"), b).expect("write");
            }
            d
        };
        let zero = elf_with(&hash_segment(&Synth {
            arb: 0,
            ..Synth::default()
        }));
        let three = elf_with(&hash_segment(&Synth::default()));

        assert_eq!(firmware_arb(dir(Some(&zero)).path()), ArbVerdict::Known(0));
        assert_eq!(firmware_arb(dir(Some(&three)).path()), ArbVerdict::Known(3));
        assert_eq!(firmware_arb(dir(None).path()), ArbVerdict::NotFound);
        assert_eq!(
            firmware_arb(dir(Some(b"not an elf")).path()),
            ArbVerdict::Unparsed
        );
    }

    #[test]
    fn the_warning_reads_the_xbl_config_that_gets_flashed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zero = elf_with(&hash_segment(&Synth {
            arb: 0,
            ..Synth::default()
        }));
        let three = elf_with(&hash_segment(&Synth::default()));
        // The old lookup preferred xbl_config_a.img by name; flashing picks
        // by collect_images's rule. Both must now land on the same file.
        std::fs::write(dir.path().join("xbl_config_a.img"), &zero).expect("write");
        std::fs::write(dir.path().join("XBL_CONFIG.img"), &three).expect("write");

        let flashed = crate::flasher::collect_images(dir.path())
            .remove("xbl_config")
            .expect("xbl_config");
        let shown = firmware_arb(dir.path());
        assert_eq!(
            ArbVerdict::Known(extract_arb_from_xbl_config(&flashed).version.expect("arb")),
            shown
        );
        assert_eq!(shown, ArbVerdict::Known(3));
    }

    #[test]
    fn a_missing_file_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let info = extract_arb_from_xbl_config(&dir.path().join("xbl_config.img"));
        assert_eq!(info.version, None);
    }

    // A lower firmware ARB than the phone's is the brick case; it must never
    // come out as Same or Raises.
    #[test]
    fn compare_arb_flags_a_lower_firmware() {
        assert_eq!(compare_arb(1, 0), ArbCompare::Lower);
        assert_eq!(compare_arb(2, 1), ArbCompare::Lower);
        assert_eq!(compare_arb(1, 1), ArbCompare::Same);
        assert_eq!(compare_arb(0, 1), ArbCompare::Raises);
    }

    #[test]
    fn bytes_that_are_not_elf_have_no_arb() {
        assert_eq!(
            extract_arb_from_bytes(b"/system/bin/sh: su: not found", "x").version,
            None
        );
        assert_eq!(extract_arb_from_bytes(&[], "x").version, None);
    }
}
