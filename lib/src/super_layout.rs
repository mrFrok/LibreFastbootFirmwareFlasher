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
        for p in &self.partitions {
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
