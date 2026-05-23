//! Structured error types for better error handling and diagnostics.
//!
//! Uses `thiserror` for concise derive-based Display impls.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Top-level error
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum FlashError {
    #[error(transparent)]
    Device(#[from] DeviceError),
    #[error(transparent)]
    Firmware(#[from] FirmwareError),
    #[error(transparent)]
    Flash(#[from] FlashOperationError),
    #[error(transparent)]
    Partition(#[from] PartitionError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("{context}: {details}")]
    Generic { context: String, details: String },
}

// ---------------------------------------------------------------------------
// Device errors
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum DeviceError {
    #[error("Device not found ({searched_for}): {context}")]
    NotFound { searched_for: String, context: String },
    #[error("Communication with device failed: {reason} ({last_error})")]
    CommunicationFailed { reason: String, last_error: String },
    #[error("Device {device} is locked: {suggestion}")]
    Locked { device: String, suggestion: String },
    #[error("Battery level too low: {current}% (minimum: {minimum}%)")]
    BatteryLow { current: i32, minimum: i32 },
    #[error("USB connection too slow: {speed_mbs:.2} MB/s (minimum: {minimum_mbs:.2} MB/s)")]
    SlowConnection { speed_mbs: f64, minimum_mbs: f64 },
    #[error("Could not detect device type: {reason}")]
    TypeDetectionFailed { reason: String },
}

// ---------------------------------------------------------------------------
// Firmware errors
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum FirmwareError {
    #[error("File not found: {path} ({description})")]
    FileNotFound { path: PathBuf, description: String },
    #[error("Invalid firmware format in {file}: expected {expected}")]
    InvalidFormatNoFound {
        file: String,
        expected: String,
    },
    #[error("Invalid firmware format in {file}: expected {expected}, got {found}")]
    InvalidFormat {
        file: String,
        expected: String,
        found: String,
    },
    #[error("Checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    #[error("ARB violation: cannot flash firmware v{firmware_version} to device with ARB v{device_version} ({detail})")]
    ArbViolation {
        firmware_version: u32,
        device_version: u32,
        detail: String,
    },
    #[error("Failed to extract {file}: {reason}")]
    ExtractionFailed { reason: String, file: String },
}

// ---------------------------------------------------------------------------
// Partition errors
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum PartitionError {
    #[error("Partition '{partition}' not found. Available: {available}")]
    NotFound {
        partition: String,
        available: String,
    },
    #[error("Failed to flash partition '{partition}'{slot}{crit}: {reason}")]
    FlashFailed {
        partition: String,
        slot: String,
        crit: String,
        reason: String,
    },
    #[error("Invalid slot '{slot}'. Valid slots: {valid}")]
    InvalidSlot { slot: String, valid: String },
}

// ---------------------------------------------------------------------------
// Flash operation errors
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum FlashOperationError {
    #[error("Device reboot timeout: did not reach {target_mode} mode within {timeout_secs}s")]
    RebootTimeout {
        target_mode: String,
        timeout_secs: u64,
    },
    #[error("Flash operation aborted: {reason}")]
    Aborted { reason: String },
    #[error("Flash operation incomplete: {completed}/{total} partitions completed (last error: {last_error})")]
    Incomplete {
        completed: usize,
        total: usize,
        last_error: String,
    },
    #[error("Device in unexpected mode: expected {expected}, got {actual}")]
    UnexpectedMode { expected: String, actual: String },
}

// ---------------------------------------------------------------------------
// Command errors
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug, Clone)]
pub enum CommandError {
    #[error("Command not found: '{command}' ({install_hint})")]
    NotFound {
        command: String,
        install_hint: String,
    },
    #[error("Command failed: '{command}' exited with code {exit_code} ({stderr})")]
    ExecutionFailed {
        command: String,
        exit_code: i32,
        stderr: String,
    },
    #[error("Command timeout: '{command}' exceeded {timeout_secs}s")]
    Timeout {
        command: String,
        timeout_secs: u64,
    },
    #[error("Invalid arguments for '{command}': {reason}")]
    InvalidArgs { command: String, reason: String },
}
