//! An empty `super` partition layout, written in the Android logical
//! partition (LP) metadata format — what `lpmake` produces as
//! super_empty.img and `fastboot wipe-super` installs.
//!
//! Flashing firmware partition by partition into an existing super cannot
//! fix a layout that is already wrong: fastbootd recreates partitions only in
//! group `default`, and OTAs then fail. Writing the layout the firmware
//! describes — its groups, their sizes, and every partition `readonly` in its
//! group — and then flashing into it gives a device the layout it shipped
//! with.
//!
//! The format follows AOSP's system/core/fs_mgr/liblp
//! (`include/liblp/metadata_format.h`, `writer.cpp`, `builder.cpp`). Only the
//! empty layout is written: every partition has size 0 and no extents, and
//! fastbootd grows each one as its image is flashed.
//!
//! Experimental and off by default (`FlashOptions::super_layout`): checked
//! byte for byte against lpmake, but not yet against a factory lpdump.

use sha2::{Digest, Sha256};

const GEOMETRY_MAGIC: u32 = 0x616c_4467;
const GEOMETRY_SIZE: usize = 4096;
const HEADER_MAGIC: u32 = 0x414c_5030;
const MAJOR_VERSION: u16 = 10;
/// Header v1.0 is 128 bytes; v1.2 adds flags and pads to 256.
const HEADER_V1_0_SIZE: u32 = 128;
const HEADER_V1_2_SIZE: u32 = 256;
const MINOR_VERSION_EXPANDED_HEADER: u16 = 2;
const HEADER_FLAG_VIRTUAL_AB_DEVICE: u32 = 0x1;
const SECTOR_SIZE: u64 = 512;
/// Kept free at the start of super, before the first geometry copy.
const PARTITION_RESERVED_BYTES: u64 = 4096;
const NAME_LEN: usize = 36;
/// Space between a group's maximum size and the size of super.
pub const GROUP_HEADROOM: u64 = 4 * 1024 * 1024;

pub const ATTR_READONLY: u32 = 1;

const PARTITION_ENTRY_SIZE: u32 = 52;
const EXTENT_ENTRY_SIZE: u32 = 24;
const GROUP_ENTRY_SIZE: u32 = 48;
const BLOCK_DEVICE_ENTRY_SIZE: u32 = 64;

/// The group every layout has first; it has no size limit.
pub const DEFAULT_GROUP: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    /// Maximum bytes its partitions may take together; 0 for no limit.
    pub maximum_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub group: String,
    pub attributes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperLayout {
    /// Name of the physical partition holding it, normally "super".
    pub super_name: String,
    /// Size of that partition in bytes.
    pub device_size: u64,
    /// Room reserved for one copy of the metadata.
    pub metadata_max_size: u32,
    /// Copies of the metadata, one per slot.
    pub metadata_slots: u32,
    pub logical_block_size: u32,
    pub alignment: u32,
    pub alignment_offset: u32,
    /// Virtual A/B device: sets the header flag (and header v1.2).
    pub virtual_ab: bool,
    /// Groups besides `default`, in order.
    pub groups: Vec<Group>,
    pub partitions: Vec<Partition>,
}

impl SuperLayout {
    /// lpmake's defaults for whatever the caller does not set.
    pub fn new(device_size: u64, metadata_slots: u32) -> Self {
        SuperLayout {
            super_name: "super".into(),
            device_size,
            metadata_max_size: 65536,
            metadata_slots,
            logical_block_size: 4096,
            alignment: 1024 * 1024,
            alignment_offset: 0,
            virtual_ab: false,
            groups: Vec::new(),
            partitions: Vec::new(),
        }
    }

    /// The image `fastboot wipe-super` takes: the geometry block followed by
    /// one copy of the metadata, as lpmake writes super_empty.img.
    pub fn to_image(&self) -> Result<Vec<u8>, String> {
        let mut out = self.geometry();
        out.extend(self.metadata()?);
        Ok(out)
    }

    fn geometry(&self) -> Vec<u8> {
        let mut g = Vec::with_capacity(GEOMETRY_SIZE);
        g.extend(GEOMETRY_MAGIC.to_le_bytes());
        g.extend(52u32.to_le_bytes());
        g.extend([0u8; 32]);
        g.extend(self.metadata_max_size.to_le_bytes());
        g.extend(self.metadata_slots.to_le_bytes());
        g.extend(self.logical_block_size.to_le_bytes());
        let sum = Sha256::digest(&g);
        g[8..40].copy_from_slice(&sum);
        g.resize(GEOMETRY_SIZE, 0);
        g
    }

    /// First sector logical partitions may use: past the reserved bytes, two
    /// geometry copies and every metadata copy (primary and backup), aligned.
    fn first_logical_sector(&self) -> u64 {
        let reserved = PARTITION_RESERVED_BYTES
            + (GEOMETRY_SIZE as u64 + self.metadata_max_size as u64 * self.metadata_slots as u64)
                * 2;
        let align = if self.alignment != 0 {
            self.alignment as u64
        } else {
            self.logical_block_size as u64
        };
        reserved.div_ceil(align) * align / SECTOR_SIZE
    }

    fn metadata(&self) -> Result<Vec<u8>, String> {
        // Groups: `default` first, then the rest in order.
        let mut groups = vec![Group {
            name: DEFAULT_GROUP.into(),
            maximum_size: 0,
        }];
        for g in &self.groups {
            if g.name == DEFAULT_GROUP || groups.iter().any(|x| x.name == g.name) {
                return Err(format!("group listed twice: {}", g.name));
            }
            groups.push(g.clone());
        }

        let mut partitions = Vec::new();
        for (i, p) in self.partitions.iter().enumerate() {
            // Unique across the whole table, not only within a group: two
            // entries of one name are what made OTAs fail before.
            if self.partitions[..i].iter().any(|q| q.name == p.name) {
                return Err(format!("partition listed twice: {}", p.name));
            }
            let Some(group_index) = groups.iter().position(|g| g.name == p.group) else {
                return Err(format!(
                    "partition {} is in unknown group {}",
                    p.name, p.group
                ));
            };
            if p.attributes & !ATTR_READONLY != 0 {
                return Err(format!("unsupported attributes on {}", p.name));
            }
            let mut e = name_field(&p.name)?;
            e.extend(p.attributes.to_le_bytes());
            e.extend(0u32.to_le_bytes()); // first_extent_index
            e.extend(0u32.to_le_bytes()); // num_extents: empty layout
            e.extend((group_index as u32).to_le_bytes());
            partitions.push(e);
        }

        let mut group_table = Vec::new();
        for g in &groups {
            group_table.extend(name_field(&g.name)?);
            group_table.extend(0u32.to_le_bytes()); // flags
            group_table.extend(g.maximum_size.to_le_bytes());
        }

        let mut block_devices = Vec::new();
        block_devices.extend(self.first_logical_sector().to_le_bytes());
        block_devices.extend(self.alignment.to_le_bytes());
        block_devices.extend(self.alignment_offset.to_le_bytes());
        block_devices.extend(self.device_size.to_le_bytes());
        block_devices.extend(name_field(&self.super_name)?);
        block_devices.extend(0u32.to_le_bytes()); // flags

        let partition_table: Vec<u8> = partitions.concat();
        let tables = [
            partition_table.as_slice(),
            &[],
            group_table.as_slice(),
            block_devices.as_slice(),
        ]
        .concat();

        let (minor, header_size, flags) = if self.virtual_ab {
            (
                MINOR_VERSION_EXPANDED_HEADER,
                HEADER_V1_2_SIZE,
                HEADER_FLAG_VIRTUAL_AB_DEVICE,
            )
        } else {
            (0, HEADER_V1_0_SIZE, 0)
        };
        let mut h = Vec::with_capacity(header_size as usize);
        h.extend(HEADER_MAGIC.to_le_bytes());
        h.extend(MAJOR_VERSION.to_le_bytes());
        h.extend(minor.to_le_bytes());
        h.extend(header_size.to_le_bytes());
        h.extend([0u8; 32]); // header checksum, filled below
        h.extend((tables.len() as u32).to_le_bytes());
        h.extend(Sha256::digest(&tables));
        let descriptors = [
            (0, partitions.len(), PARTITION_ENTRY_SIZE),
            (partition_table.len(), 0, EXTENT_ENTRY_SIZE),
            (partition_table.len(), groups.len(), GROUP_ENTRY_SIZE),
            (
                partition_table.len() + group_table.len(),
                1,
                BLOCK_DEVICE_ENTRY_SIZE,
            ),
        ];
        for (offset, count, size) in descriptors {
            h.extend((offset as u32).to_le_bytes());
            h.extend((count as u32).to_le_bytes());
            h.extend(size.to_le_bytes());
        }
        if self.virtual_ab {
            h.extend(flags.to_le_bytes());
            h.resize(header_size as usize, 0);
        }
        let sum = Sha256::digest(&h);
        h[12..44].copy_from_slice(&sum);

        let mut out = h;
        out.extend(tables);
        if out.len() > self.metadata_max_size as usize {
            return Err(format!(
                "the layout needs {} bytes of metadata, more than the {} reserved",
                out.len(),
                self.metadata_max_size
            ));
        }
        Ok(out)
    }
}

/// The dynamic partition part of a payload, as `payload_dumper --metadata`
/// exports it. Group and partition names carry no slot suffix there.
#[derive(serde::Deserialize)]
struct PayloadMetadata {
    dynamic_partition_metadata: Option<DynamicPartitions>,
    #[serde(default)]
    partitions: Vec<PayloadPartition>,
}

#[derive(serde::Deserialize)]
struct PayloadPartition {
    partition_name: String,
    #[serde(default)]
    size_in_bytes: u64,
}

#[derive(serde::Deserialize)]
struct DynamicPartitions {
    groups: Vec<PayloadGroup>,
    #[serde(default)]
    snapshot_enabled: Option<bool>,
}

#[derive(serde::Deserialize)]
struct PayloadGroup {
    name: String,
    #[serde(default)]
    size: Option<u64>,
    partition_names: Vec<String>,
}

impl SuperLayout {
    /// The layout an A/B firmware describes, for a super of `device_size`
    /// bytes: each payload group as `<group>_a` and `<group>_b` with the
    /// payload's size, and each partition readonly in both, as AOSP builds
    /// super_empty.img for an A/B device. Virtual A/B when the payload says
    /// snapshots are enabled.
    ///
    /// Matches the factory layout of a OnePlus 15 (lpdump): 3 metadata
    /// slots of 64 KiB, Virtual A/B, empty `_b` entries, groups at most
    /// super minus 4 MiB.
    pub fn from_payload_metadata(json: &str, device_size: u64) -> Result<Self, String> {
        let meta: PayloadMetadata =
            serde_json::from_str(json).map_err(|e| format!("payload metadata: {e}"))?;
        let dynamic = meta
            .dynamic_partition_metadata
            .ok_or("the firmware has no dynamic partition metadata")?;
        if dynamic.groups.is_empty() {
            return Err("the firmware lists no partition groups".into());
        }
        let mut layout = SuperLayout::new(device_size, 3);
        layout.virtual_ab = dynamic.snapshot_enabled.unwrap_or(false);
        // A factory OnePlus 15 (CPH2747) sizes its groups at super minus
        // 4 MiB; the same rule caps groups a payload makes too large.
        let usable = device_size.saturating_sub(GROUP_HEADROOM);
        for slot in ["a", "b"] {
            for g in &dynamic.groups {
                let group = format!("{}_{slot}", g.name);
                // Payloads can name a group larger than this device's super
                // (RMX3709 16.0.3.503: 15.9 GiB on a 13.5 GiB super). The
                // limit is then the space super actually has; partitions
                // still have to fit, which flashing reports if they don't.
                let maximum_size = g.size.unwrap_or(0).min(usable);
                layout.groups.push(Group {
                    name: group.clone(),
                    maximum_size,
                });
                for p in &g.partition_names {
                    layout.partitions.push(Partition {
                        name: format!("{p}_{slot}"),
                        group: group.clone(),
                        attributes: ATTR_READONLY,
                    });
                }
            }
        }
        // The images have to fit their group: checked here, before super is
        // wiped, rather than discovered halfway through flashing.
        let size_of = |name: &str| {
            meta.partitions
                .iter()
                .find(|p| p.partition_name == name)
                .map(|p| p.size_in_bytes)
        };
        for g in &dynamic.groups {
            let limit = g.size.unwrap_or(0).min(usable);
            let mut total = 0u64;
            for p in &g.partition_names {
                total += size_of(p).ok_or_else(|| {
                    format!("the firmware lists {p} but has no image size for it")
                })?;
            }
            if limit > 0 && total > limit {
                return Err(format!(
                    "the firmware's partitions need {total} bytes, more than the {limit} bytes group {} can hold on this phone",
                    g.name
                ));
            }
        }
        // Fails early on names or sizes the format cannot hold.
        layout.to_image()?;
        Ok(layout)
    }
}

impl SuperLayout {
    /// Logical partitions the device has (as fastbootd lists them, with
    /// slot suffix) that this layout would not keep: rebuilding would delete
    /// them and their contents. On a OnePlus 15 my_company and my_preload
    /// come from the factory, not from the firmware.
    pub fn missing_from(&self, device_partitions: &[String]) -> Vec<String> {
        let mut missing: Vec<String> = device_partitions
            .iter()
            .filter(|d| !d.ends_with("-cow") && !d.ends_with("_cow") && !d.ends_with("-cow-img"))
            .filter(|d| !self.partitions.iter().any(|p| &p.name == *d))
            .map(|d| d.trim_end_matches("_a").trim_end_matches("_b").to_string())
            .collect();
        missing.sort();
        missing.dedup();
        missing
    }
}

/// What an LP image says, read back: enough to check it against the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLayout {
    pub metadata_max_size: u32,
    pub metadata_slots: u32,
    pub virtual_ab: bool,
    pub device_size: u64,
    pub super_name: String,
    pub groups: Vec<Group>,
    pub partitions: Vec<Partition>,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]))
        .ok_or_else(|| "image too short".to_string())
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|x| u32::from_le_bytes(x.try_into().unwrap()))
        .ok_or_else(|| "image too short".to_string())
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8)
        .map(|x| u64::from_le_bytes(x.try_into().unwrap()))
        .ok_or_else(|| "image too short".to_string())
}
fn name_at(b: &[u8], o: usize) -> Result<String, String> {
    let raw = b.get(o..o + NAME_LEN).ok_or("image too short")?;
    let end = raw.iter().position(|&c| c == 0).unwrap_or(NAME_LEN);
    String::from_utf8(raw[..end].to_vec()).map_err(|_| "a name is not text".to_string())
}

/// Read an image as `fastboot wipe-super` takes it (geometry, then one
/// metadata copy), checking magics, sizes and all three checksums, the way
/// liblp reads it.
pub fn parse_image(img: &[u8]) -> Result<ParsedLayout, String> {
    if u32_at(img, 0)? != GEOMETRY_MAGIC {
        return Err("not an LP image: bad geometry magic".into());
    }
    let mut geom = img.get(..52).ok_or("image too short")?.to_vec();
    let want = geom[8..40].to_vec();
    geom[8..40].fill(0);
    if Sha256::digest(&geom).as_slice() != want.as_slice() {
        return Err("geometry checksum mismatch".into());
    }
    let metadata_max_size = u32_at(img, 40)?;
    let metadata_slots = u32_at(img, 44)?;

    let m = img.get(GEOMETRY_SIZE..).ok_or("image too short")?;
    if u32_at(m, 0)? != HEADER_MAGIC || u16_at(m, 4)? != MAJOR_VERSION {
        return Err("bad metadata header".into());
    }
    let minor = u16_at(m, 6)?;
    let header_size = u32_at(m, 8)? as usize;
    let tables_size = u32_at(m, 44)? as usize;
    let header = m.get(..header_size).ok_or("image too short")?;
    let tables = m
        .get(header_size..header_size + tables_size)
        .ok_or("image too short")?;
    let mut h = header.to_vec();
    h[12..44].fill(0);
    if Sha256::digest(&h).as_slice() != &header[12..44] {
        return Err("header checksum mismatch".into());
    }
    if Sha256::digest(tables).as_slice() != &header[48..80] {
        return Err("tables checksum mismatch".into());
    }
    let desc = |i: usize| -> Result<(usize, usize, usize), String> {
        let o = 80 + i * 12;
        Ok((
            u32_at(header, o)? as usize,
            u32_at(header, o + 4)? as usize,
            u32_at(header, o + 8)? as usize,
        ))
    };
    let (p_off, p_n, p_sz) = desc(0)?;
    let (g_off, g_n, g_sz) = desc(2)?;
    let (b_off, b_n, b_sz) = desc(3)?;
    if b_n != 1 || b_sz != BLOCK_DEVICE_ENTRY_SIZE as usize {
        return Err(format!(
            "expected one block device entry, found {b_n} of {b_sz} bytes"
        ));
    }
    let mut groups = Vec::new();
    for i in 0..g_n {
        let o = g_off + i * g_sz;
        groups.push(Group {
            name: name_at(tables, o)?,
            maximum_size: u64_at(tables, o + 40)?,
        });
    }
    let mut partitions = Vec::new();
    for i in 0..p_n {
        let o = p_off + i * p_sz;
        let gi = u32_at(tables, o + 48)? as usize;
        partitions.push(Partition {
            name: name_at(tables, o)?,
            group: groups
                .get(gi)
                .map(|g| g.name.clone())
                .ok_or("a partition points at no group")?,
            attributes: u32_at(tables, o + 36)?,
        });
    }
    let flags = if minor >= MINOR_VERSION_EXPANDED_HEADER {
        u32_at(header, 128)?
    } else {
        0
    };
    Ok(ParsedLayout {
        metadata_max_size,
        metadata_slots,
        virtual_ab: flags & HEADER_FLAG_VIRTUAL_AB_DEVICE != 0,
        device_size: u64_at(tables, b_off + 16)?,
        super_name: name_at(tables, b_off + 24)?,
        groups,
        partitions,
    })
}

impl SuperLayout {
    /// The image, read back and compared with this plan before it is
    /// returned: what goes to the phone is what was meant, checksums and all.
    pub fn to_checked_image(&self) -> Result<Vec<u8>, String> {
        let img = self.to_image()?;
        let got =
            parse_image(&img).map_err(|e| format!("the built layout does not read back: {e}"))?;
        let mut groups = vec![Group {
            name: DEFAULT_GROUP.into(),
            maximum_size: 0,
        }];
        groups.extend(self.groups.iter().cloned());
        let want = ParsedLayout {
            metadata_max_size: self.metadata_max_size,
            metadata_slots: self.metadata_slots,
            virtual_ab: self.virtual_ab,
            device_size: self.device_size,
            super_name: self.super_name.clone(),
            groups,
            partitions: self.partitions.clone(),
        };
        if got != want {
            return Err("the built layout does not match the plan".into());
        }
        Ok(img)
    }

    /// The plan a payload describes, before the phone's super size is known
    /// (it is read in fastbootd): groups at the payload's sizes.
    pub fn plan_from_payload_metadata(json: &str) -> Result<String, String> {
        Self::from_payload_metadata(json, u64::MAX / 2).map(|l| l.summary())
    }

    /// One line per group — name, size limit, partitions — short enough
    /// for the confirmation sheet.
    pub fn summary(&self) -> String {
        self.groups
            .iter()
            .map(|g| {
                let parts: Vec<&str> = self
                    .partitions
                    .iter()
                    .filter(|p| p.group == g.name)
                    .map(|p| p.name.trim_end_matches("_a").trim_end_matches("_b"))
                    .collect();
                format!(
                    "{} · {:.2} GiB · {} × readonly: {}",
                    g.name,
                    g.maximum_size as f64 / (1u64 << 30) as f64,
                    parts.len(),
                    parts.join(" ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A NUL-padded 36-byte name, as every LP table stores names.
fn name_field(name: &str) -> Result<Vec<u8>, String> {
    if name.is_empty() || name.len() > NAME_LEN || !name.is_ascii() {
        return Err(format!("not a valid LP name: {name:?}"));
    }
    let mut f = name.as_bytes().to_vec();
    f.resize(NAME_LEN, 0);
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(virtual_ab: bool) -> SuperLayout {
        let mut l = SuperLayout::new(12_884_901_888, 3);
        l.virtual_ab = virtual_ab;
        for slot in ["a", "b"] {
            l.groups.push(Group {
                name: format!("qti_dynamic_partitions_{slot}"),
                maximum_size: 12_880_707_584,
            });
            for p in ["system", "vendor", "odm", "product"] {
                l.partitions.push(Partition {
                    name: format!("{p}_{slot}"),
                    group: format!("qti_dynamic_partitions_{slot}"),
                    attributes: ATTR_READONLY,
                });
            }
        }
        l
    }

    #[test]
    fn checksums_and_sizes_are_consistent() {
        let img = sample(true).to_image().unwrap();
        let geom = &img[..GEOMETRY_SIZE];
        let mut zeroed = geom[..52].to_vec();
        zeroed[8..40].fill(0);
        assert_eq!(&geom[8..40], Sha256::digest(&zeroed).as_slice());

        let meta = &img[GEOMETRY_SIZE..];
        assert_eq!(
            u32::from_le_bytes(meta[0..4].try_into().unwrap()),
            HEADER_MAGIC
        );
        let header_size = u32::from_le_bytes(meta[8..12].try_into().unwrap()) as usize;
        assert_eq!(header_size, 256);
        let tables_size = u32::from_le_bytes(meta[44..48].try_into().unwrap()) as usize;
        assert_eq!(meta.len(), header_size + tables_size);
        assert_eq!(
            &meta[48..80],
            Sha256::digest(&meta[header_size..]).as_slice()
        );
        let mut h = meta[..header_size].to_vec();
        h[12..44].fill(0);
        assert_eq!(&meta[12..44], Sha256::digest(&h).as_slice());
    }

    #[test]
    fn a_real_payload_gives_both_slots() {
        let json = include_str!("../testdata/rmx3709-payload-metadata.json");
        let l = SuperLayout::from_payload_metadata(json, 17_179_869_184).unwrap();
        assert!(l.virtual_ab);
        let names: Vec<&str> = l.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["qti_dynamic_partitions_a", "qti_dynamic_partitions_b"]
        );
        assert!(l.groups.iter().all(|g| g.maximum_size == 17_058_234_368));
        let odm_b = l.partitions.iter().find(|p| p.name == "odm_b").unwrap();
        assert_eq!(odm_b.group, "qti_dynamic_partitions_b");
        assert_eq!(odm_b.attributes, ATTR_READONLY);
        assert_eq!(l.partitions.len(), 30);
    }

    /// The factory layout of a OnePlus 15 (lpdump, 2026-10-22): super of
    /// 17 062 428 672 bytes, groups of 17 058 234 368 — what its payload says.
    #[test]
    fn a_payload_sized_group_is_kept() {
        let json = include_str!("../testdata/rmx3709-payload-metadata.json");
        let l = SuperLayout::from_payload_metadata(json, 17_062_428_672).unwrap();
        assert!(l.groups.iter().all(|g| g.maximum_size == 17_058_234_368));
    }

    #[test]
    fn a_group_larger_than_super_is_capped_to_it() {
        let json = include_str!("../testdata/rmx3709-payload-metadata.json");
        // The real super of an RMX3706, smaller than the payload's group.
        let super_size = 0x3_6000_0000;
        let l = SuperLayout::from_payload_metadata(json, super_size).unwrap();
        assert!(
            l.groups
                .iter()
                .all(|g| g.maximum_size == super_size - GROUP_HEADROOM)
        );
    }

    #[test]
    fn partitions_the_firmware_lacks_are_reported() {
        let json = include_str!("../testdata/rmx3709-payload-metadata.json");
        let l = SuperLayout::from_payload_metadata(json, 17_062_428_672).unwrap();
        let device: Vec<String> = [
            "system_a",
            "my_company_a",
            "my_company_b",
            "my_preload_a",
            "odm_a-cow",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(l.missing_from(&device), ["my_company", "my_preload"]);
    }

    #[test]
    fn a_built_image_reads_back_as_planned() {
        for vab in [false, true] {
            let l = sample(vab);
            let parsed = parse_image(&l.to_checked_image().unwrap()).unwrap();
            assert_eq!(parsed.virtual_ab, vab);
            assert_eq!(parsed.partitions, l.partitions);
            assert_eq!(parsed.groups.len(), l.groups.len() + 1);
        }
    }

    #[test]
    fn a_damaged_image_is_refused() {
        let mut img = sample(true).to_image().unwrap();
        let last = img.len() - 1;
        img[last] ^= 1;
        assert!(parse_image(&img).unwrap_err().contains("checksum"));
    }

    #[test]
    fn a_partition_named_twice_is_refused() {
        let mut l = sample(false);
        l.partitions.push(l.partitions[0].clone());
        assert!(l.to_image().unwrap_err().contains("twice"));
    }

    #[test]
    fn firmware_too_big_for_the_phone_is_refused() {
        let json = include_str!("../testdata/rmx3709-payload-metadata.json");
        // 11.5 GB of images against an 8 GiB super.
        let err = SuperLayout::from_payload_metadata(json, 8 << 30).unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }

    #[test]
    fn an_unknown_group_is_refused() {
        let mut l = sample(false);
        l.partitions[0].group = "nope".into();
        assert!(l.to_image().is_err());
    }

    /// Byte for byte the same as AOSP's lpmake. Runs only where lpmake is
    /// installed (android-tools); the checksum test above runs everywhere.
    #[test]
    fn matches_lpmake() {
        for virtual_ab in [false, true] {
            let l = sample(virtual_ab);
            let tmp = tempfile::tempdir().unwrap();
            let out = tmp.path().join("super_empty.img");
            let mut args: Vec<String> = vec![
                "--device-size".into(),
                l.device_size.to_string(),
                "--metadata-size".into(),
                l.metadata_max_size.to_string(),
                "--metadata-slots".into(),
                l.metadata_slots.to_string(),
                "--output".into(),
                out.display().to_string(),
            ];
            if virtual_ab {
                args.push("--virtual-ab".into());
            }
            for g in &l.groups {
                args.push("--group".into());
                args.push(format!("{}:{}", g.name, g.maximum_size));
            }
            for p in &l.partitions {
                args.push("--partition".into());
                args.push(format!("{}:readonly:0:{}", p.name, p.group));
            }
            let Ok(status) = std::process::Command::new("lpmake")
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            else {
                eprintln!("lpmake not installed; skipping");
                return;
            };
            assert!(status.success(), "lpmake failed");
            let theirs = std::fs::read(&out).unwrap();
            let ours = l.to_image().unwrap();
            assert_eq!(ours.len(), theirs.len(), "virtual_ab={virtual_ab}");
            assert!(ours == theirs, "virtual_ab={virtual_ab}: bytes differ");
        }
    }
}
