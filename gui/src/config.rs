use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scale: f32,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Colour scheme index into `AppScheme` in globals/lfff-scheme.slint:
    /// 0 blue, 1 indigo, 2 teal, 3 green, 4 amber, 5 rose, 6 violet.
    #[serde(default)]
    pub scheme: i32,
    /// Alpha-blended surfaces, for compositors that blur behind windows.
    #[serde(default)]
    pub translucent: bool,
    #[serde(default)]
    pub output_dir: Option<String>,
    /// Chip the user chose per device product ("snapdragon" or "mediatek").
    /// Never guessed: an entry exists only after the user picked it.
    #[serde(default)]
    pub chips: BTreeMap<String, String>,
    /// Tools pinned to the sidebar: "partition", "flash", "device", "download"
    /// (ids 1-4 of the Tools page, in that order).
    #[serde(default)]
    pub pinned_tools: Vec<String>,
    /// The pre-2.10 page layout, kept for a transition period.
    #[serde(default)]
    pub classic_ui: bool,
    /// The user closed the first-launch notice (supported phones, own risk);
    /// it is not shown again.
    #[serde(default)]
    pub notice_accepted: bool,
}

fn default_lang() -> String {
    "en".to_string()
}
fn default_theme() -> String {
    "dark".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scale: 1.0,
            lang: default_lang(),
            theme: default_theme(),
            scheme: 0,
            translucent: false,
            output_dir: None,
            chips: BTreeMap::new(),
            pinned_tools: Vec::new(),
            classic_ui: false,
            notice_accepted: false,
        }
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lfff")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(config: &Config) {
    if let Some(d) = config_path().parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(s) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(config_path(), s);
    }
}

pub fn save_scale(scale: f32) {
    let mut config = load_config();
    config.scale = scale;
    save_config(&config);
}

pub fn get_output_dir() -> PathBuf {
    let dir = load_config()
        .output_dir
        .filter(|p| Path::new(p).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("lfff")
                .join("firmwares")
        });
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub fn set_output_dir(path: &str) {
    let mut config = load_config();
    config.output_dir = Some(path.to_string());
    save_config(&config);
}

pub fn set_chip(product: &str, chip: &str) {
    let mut config = load_config();
    config.chips.insert(product.to_string(), chip.to_string());
    save_config(&config);
}

pub fn remove_chip(product: &str) {
    let mut config = load_config();
    config.chips.remove(product);
    save_config(&config);
}

pub fn clear_chips() {
    let mut config = load_config();
    config.chips.clear();
    save_config(&config);
}

#[cfg(test)]
mod tests {
    use super::*;

    // A config written by an older version has none of the newer fields and
    // must still load with every setting it does have.
    #[test]
    fn an_old_config_file_still_loads() {
        let old = r#"{"scale":1.25,"lang":"ru","theme":"light","scheme":2,"translucent":true,"output_dir":"/tmp/fw"}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert_eq!(c.lang, "ru");
        assert_eq!(c.scheme, 2);
        assert!(c.chips.is_empty());
        assert!(c.pinned_tools.is_empty());
        assert!(!c.classic_ui);
        // Existing users see the first-launch notice once after updating.
        assert!(!c.notice_accepted);
    }
}
