use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashHistoryEntry {
    pub timestamp: String,
    pub firmware_name: String,
    pub firmware_path: String,
    pub device_serial: String,
    pub device_product: String,
    pub total_partitions: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub aborted: bool,
    pub duration_s: f64,
    pub end_reason: Option<String>,
    pub failed_partitions: Vec<FailedPartition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedPartition {
    pub name: String,
    pub slot: String,
    pub error: String,
}

fn history_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lfff")
        .join("flash-history.jsonl")
}

pub fn append_entry(entry: &FlashHistoryEntry) -> std::io::Result<()> {
    let path = history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(entry)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(format!("{}\n", line).as_bytes())?;
    Ok(())
}

pub fn load_history() -> Vec<FlashHistoryEntry> {
    let path = history_path();
    if !path.exists() {
        return Vec::new();
    }
    std::fs::read_to_string(&path)
        .ok()
        .map(|content| {
            content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .unwrap_or_default()
}
