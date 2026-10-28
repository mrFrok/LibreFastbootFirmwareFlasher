use colored::Colorize;
use lfff_lib::device::{RebootTarget, Via, reboot};
use lfff_lib::flasher::{DeviceMode, detect_mode};

use crate::cli::RebootTo;

pub fn run(to: RebootTo, serial: Option<&str>) -> i32 {
    let target = match to {
        RebootTo::System => RebootTarget::System,
        RebootTo::Recovery => RebootTarget::Recovery,
        RebootTo::Bootloader => RebootTarget::Bootloader,
        RebootTo::Fastbootd => RebootTarget::Fastbootd,
    };
    // The tool that sees the phone sends the reboot. A recovery is not
    // listed as an adb "device", so anything fastboot does not see goes to
    // adb, which gives its own error when there is nothing there.
    let via = match detect_mode(serial) {
        DeviceMode::Bootloader | DeviceMode::Fastbootd => Via::Fastboot,
        DeviceMode::System | DeviceMode::Unknown => Via::Adb,
    };
    match reboot(serial, via, target) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "{} {}",
                "✗".red().bold(),
                format!("Reboot failed: {}", e.trim()).red()
            );
            1
        }
    }
}
