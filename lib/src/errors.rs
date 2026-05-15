//! Structured error types for better error handling and diagnostics.
//!
//! Provides detailed error context for different failure scenarios,
//! making it easier for users and developers to understand what went wrong.

use std::fmt;
use std::path::PathBuf;

/// High-level error categories for firmware flashing
#[derive(Debug, Clone)]
pub enum FlashError {
    /// Device-related errors
    Device(DeviceError),
    /// Firmware/image related errors
    Firmware(FirmwareError),
    /// Flash operation errors
    Flash(FlashOperationError),
    /// Partition-related errors
    Partition(PartitionError),
    /// Command execution errors
    Command(CommandError),
    /// Generic error with context
    Generic {
        context: String,
        details: String,
    },
}

/// Device-related errors
#[derive(Debug, Clone)]
pub enum DeviceError {
    /// Device not found in fastboot/adb
    NotFound {
        searched_for: String,
        context: String,
    },
    /// Communication with device failed
    CommunicationFailed {
        reason: String,
        last_error: String,
    },
    /// Device is locked (bootloader not unlocked)
    Locked {
        device: String,
        suggestion: String,
    },
    /// Insufficient battery level
    BatteryLow {
        current: i32,
        minimum: i32,
    },
    /// Cable/connection speed too slow
    SlowConnection {
        speed_mbs: f64,
        minimum_mbs: f64,
    },
    /// Device type detection failed
    TypeDetectionFailed {
        reason: String,
    },
}

/// Firmware/image related errors
#[derive(Debug, Clone)]
pub enum FirmwareError {
    /// File not found
    FileNotFound {
        path: PathBuf,
        description: String,
    },
    /// Invalid firmware format
    InvalidFormat {
        file: String,
        expected: String,
        found: Option<String>,
    },
    /// Checksum mismatch
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// ARB version check failed - would result in device brick
    ArbViolation {
        firmware_version: u32,
        device_version: u32,
        detail: String,
    },
    /// Extraction failed
    ExtractionFailed {
        reason: String,
        file: String,
    },
}

/// Partition-related errors
#[derive(Debug, Clone)]
pub enum PartitionError {
    /// Partition not found in image
    NotFound {
        partition: String,
        available: Vec<String>,
    },
    /// Partition flash failed
    FlashFailed {
        partition: String,
        slot: Option<String>,
        reason: String,
        critical: bool,
    },
    /// Invalid partition slot
    InvalidSlot {
        slot: String,
        valid: Vec<String>,
    },
}

/// Flash operation errors
#[derive(Debug, Clone)]
pub enum FlashOperationError {
    /// Device reboot timeout
    RebootTimeout {
        target_mode: String,
        timeout_secs: u64,
    },
    /// Flash operation aborted by user
    Aborted {
        reason: String,
    },
    /// Operation incomplete
    Incomplete {
        completed: usize,
        total: usize,
        last_error: String,
    },
    /// Device in unexpected mode
    UnexpectedMode {
        expected: String,
        actual: String,
    },
}

/// Command execution errors
#[derive(Debug, Clone)]
pub enum CommandError {
    /// Command not found in PATH
    NotFound {
        command: String,
        install_hint: String,
    },
    /// Command execution failed
    ExecutionFailed {
        command: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    /// Command timed out
    Timeout {
        command: String,
        timeout_secs: u64,
    },
    /// Invalid command arguments
    InvalidArgs {
        command: String,
        reason: String,
    },
}

impl fmt::Display for FlashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlashError::Device(e) => write!(f, "{}", e),
            FlashError::Firmware(e) => write!(f, "{}", e),
            FlashError::Flash(e) => write!(f, "{}", e),
            FlashError::Partition(e) => write!(f, "{}", e),
            FlashError::Command(e) => write!(f, "{}", e),
            FlashError::Generic { context, details } => {
                write!(f, "{}: {}", context, details)
            }
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceError::NotFound {
                searched_for,
                context,
            } => {
                write!(f, "Device not found ({}): {}", searched_for, context)
            }
            DeviceError::CommunicationFailed {
                reason,
                last_error,
            } => {
                write!(
                    f,
                    "Communication with device failed: {} ({})",
                    reason, last_error
                )
            }
            DeviceError::Locked {
                device,
                suggestion,
            } => {
                write!(f, "Device {} is locked: {}", device, suggestion)
            }
            DeviceError::BatteryLow { current, minimum } => {
                write!(
                    f,
                    "Battery level too low: {}% (minimum: {}%)",
                    current, minimum
                )
            }
            DeviceError::SlowConnection {
                speed_mbs,
                minimum_mbs,
            } => {
                write!(
                    f,
                    "USB connection too slow: {:.2} MB/s (minimum: {:.2} MB/s)",
                    speed_mbs, minimum_mbs
                )
            }
            DeviceError::TypeDetectionFailed { reason } => {
                write!(f, "Could not detect device type: {}", reason)
            }
        }
    }
}

impl fmt::Display for FirmwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FirmwareError::FileNotFound { path, description } => {
                write!(f, "File not found: {} ({})", path.display(), description)
            }
            FirmwareError::InvalidFormat {
                file,
                expected,
                found,
            } => {
                match found {
                    Some(f_type) => {
                        write!(f, "Invalid firmware format in {}: expected {}, got {}", file, expected, f_type)
                    }
                    None => {
                        write!(f, "Invalid firmware format in {}: expected {}", file, expected)
                    }
                }
            }
            FirmwareError::ChecksumMismatch {
                file,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Checksum mismatch for {}: expected {}, got {}",
                    file, expected, actual
                )
            }
            FirmwareError::ArbViolation {
                firmware_version,
                device_version,
                detail,
            } => {
                write!(
                    f,
                    "ARB violation: cannot flash firmware v{} to device with ARB v{} ({})",
                    firmware_version, device_version, detail
                )
            }
            FirmwareError::ExtractionFailed { reason, file } => {
                write!(f, "Failed to extract {}: {}", file, reason)
            }
        }
    }
}

impl fmt::Display for PartitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartitionError::NotFound {
                partition,
                available,
            } => {
                write!(
                    f,
                    "Partition '{}' not found. Available: {}",
                    partition,
                    available.join(", ")
                )
            }
            PartitionError::FlashFailed {
                partition,
                slot,
                reason,
                critical,
            } => {
                let slot_str = slot.as_ref().map_or(String::new(), |s| format!(" (slot {})", s));
                let crit = if *critical { "[CRITICAL]" } else { "" };
                write!(
                    f,
                    "Failed to flash partition '{}'{}{}: {}",
                    partition, slot_str, crit, reason
                )
            }
            PartitionError::InvalidSlot { slot, valid } => {
                write!(
                    f,
                    "Invalid slot '{}'. Valid slots: {}",
                    slot,
                    valid.join(", ")
                )
            }
        }
    }
}

impl fmt::Display for FlashOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlashOperationError::RebootTimeout {
                target_mode,
                timeout_secs,
            } => {
                write!(
                    f,
                    "Device reboot timeout: did not reach {} mode within {}s",
                    target_mode, timeout_secs
                )
            }
            FlashOperationError::Aborted { reason } => {
                write!(f, "Flash operation aborted: {}", reason)
            }
            FlashOperationError::Incomplete {
                completed,
                total,
                last_error,
            } => {
                write!(
                    f,
                    "Flash operation incomplete: {}/{} partitions completed (last error: {})",
                    completed, total, last_error
                )
            }
            FlashOperationError::UnexpectedMode {
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Device in unexpected mode: expected {}, got {}",
                    expected, actual
                )
            }
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::NotFound {
                command,
                install_hint,
            } => {
                write!(f, "Command not found: '{}' ({})", command, install_hint)
            }
            CommandError::ExecutionFailed {
                command,
                exit_code,
                stdout: _,
                stderr,
            } => {
                write!(
                    f,
                    "Command failed: '{}' exited with code {} ({})",
                    command, exit_code, stderr
                )
            }
            CommandError::Timeout {
                command,
                timeout_secs,
            } => {
                write!(
                    f,
                    "Command timeout: '{}' exceeded {}s",
                    command, timeout_secs
                )
            }
            CommandError::InvalidArgs {
                command,
                reason,
            } => {
                write!(f, "Invalid arguments for '{}': {}", command, reason)
            }
        }
    }
}

impl std::error::Error for FlashError {}
impl std::error::Error for DeviceError {}
impl std::error::Error for FirmwareError {}
impl std::error::Error for PartitionError {}
impl std::error::Error for FlashOperationError {}
impl std::error::Error for CommandError {}
