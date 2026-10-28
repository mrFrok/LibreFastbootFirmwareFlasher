use colored::Colorize;
use lfff_lib::device::list_fastboot_devices;
use lfff_lib::flasher::set_active_slot;

use crate::cli::Slot;

pub fn run(slot: Slot, serial: Option<&str>) -> i32 {
    let fastboot = list_fastboot_devices();
    let present = match serial {
        Some(s) => fastboot.iter().any(|d| d == s),
        None => !fastboot.is_empty(),
    };
    if !present {
        eprintln!(
            "{} {}",
            "✗".red().bold(),
            "No device in fastboot or fastbootd: reboot it there first (lfff reboot bootloader)."
                .red()
        );
        return 1;
    }
    let slot = match slot {
        Slot::A => "a",
        Slot::B => "b",
    };
    match set_active_slot(serial, slot) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "{} {}",
                "✗".red().bold(),
                format!("set_active {slot} failed: {e}").red()
            );
            1
        }
    }
}
