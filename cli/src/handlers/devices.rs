use colored::Colorize;
use lfff_lib::device::{list_adb_devices, list_fastboot_devices, run_pre_flash_checks};

use crate::output::print_check_report;

/// stdout gets one `MODE<TAB>SERIAL` line per device, for `cut -f2`;
/// the frame and the check report go to stderr.
pub fn run(check: bool, serial: Option<&str>) -> i32 {
    eprintln!(
        "\n{}",
        "── Connected devices ────────────────────────────────────".dimmed()
    );

    let fb_serials = list_fastboot_devices();
    let adb_serials = list_adb_devices();

    if fb_serials.is_empty() && adb_serials.is_empty() {
        eprintln!("  {}", "No devices found via fastboot or adb.".yellow());
        eprintln!(
            "  {}",
            "Make sure USB debugging or fastboot mode is enabled.".dimmed()
        );
        eprintln!(
            "{}\n",
            "────────────────────────────────────────────────────────".dimmed()
        );
        return 1;
    }

    for s in &fb_serials {
        println!("fastboot\t{}", s);
    }
    for s in &adb_serials {
        println!("adb\t{}", s);
    }
    eprintln!(
        "{}\n",
        "────────────────────────────────────────────────────────".dimmed()
    );

    if check {
        eprintln!("Running pre-flash checks …\n");
        let result = run_pre_flash_checks(serial);
        print_check_report(&result);
        return if result.ready() { 0 } else { 1 };
    }

    0
}
