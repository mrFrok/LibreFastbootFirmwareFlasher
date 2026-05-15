use std::env;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Partition lists that can be provided by a JSON config file.
#[derive(Debug, Clone, Deserialize)]
pub struct Partitions {
    #[serde(default)]
    pub super_partitions: Vec<String>,
    #[serde(default)]
    pub bootloader_partitions: Vec<String>,
    #[serde(default)]
    pub critical_partitions: Vec<String>,
}

impl Partitions {
    pub fn default() -> Self {
        Self {
            super_partitions: vec![
                "system".into(),
                "system_ext".into(),
                "system_dlkm".into(),
                "product".into(),
                "odm".into(),
                "odm_dlkm".into(),
                "vendor".into(),
                "vendor_dlkm".into(),
                "my_bigball".into(),
                "my_carrier".into(),
                "my_engineering".into(),
                "my_heytap".into(),
                "my_manifest".into(),
                "my_product".into(),
                "my_region".into(),
                "my_stock".into(),
            ],
            bootloader_partitions: vec!["modem".into()],
            critical_partitions: vec![
                "abl".into(),
                "xbl".into(),
                "xbl_config".into(),
                "xbl_ramdump".into(),
                "aop".into(),
                "aop_config".into(),
                "devcfg".into(),
                "shrm".into(),
                "tz".into(),
                "hyp".into(),
                "multiimgoem".into(),
                "multiimgqti".into(),
                "qupfw".into(),
                "uefisecapp".into(),
                "imagefv".into(),
                "cpucp".into(),
                "boot".into(),
                "init_boot".into(),
                "vendor_boot".into(),
                "modem".into(),
            ],
        }
    }
}

/// Try to load partitions config from:
/// 1. $LFFF_PARTITIONS_FILE
/// 2. ./partitions.json (cwd)
/// 3. /etc/lfff/partitions.json
/// Falls back to builtin defaults when nothing found or parse fails.
pub fn load_partitions() -> Partitions {
    if let Ok(p) = env::var("LFFF_PARTITIONS_FILE") {
        if let Ok(s) = fs::read_to_string(&p) {
            if let Ok(cfg) = serde_json::from_str::<Partitions>(&s) {
                return cfg;
            }
        }
    }

    let cwd = Path::new("./partitions.json");
    if cwd.exists() {
        if let Ok(s) = fs::read_to_string(cwd) {
            if let Ok(cfg) = serde_json::from_str::<Partitions>(&s) {
                return cfg;
            }
        }
    }

    let etc = Path::new("/etc/lfff/partitions.json");
    if etc.exists() {
        if let Ok(s) = fs::read_to_string(etc) {
            if let Ok(cfg) = serde_json::from_str::<Partitions>(&s) {
                return cfg;
            }
        }
    }

    Partitions::default()
}
