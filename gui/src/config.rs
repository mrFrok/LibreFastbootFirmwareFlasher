use std::path::{Path, PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub scale: f32,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub output_dir: Option<String>,
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
            output_dir: None,
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
