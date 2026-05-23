use colored::Colorize;
use lfff_lib::device::{
    list_adb_devices, list_fastboot_devices, print_check_report, run_pre_flash_checks,
};

pub fn run(check: bool, serial: Option<&str>) -> i32 {
    println!("\n{}", "── Connected devices ────────────────────────────────────".dimmed());

    let fb_serials = list_fastboot_devices();
    let adb_serials = list_adb_devices();

    if fb_serials.is_empty() && adb_serials.is_empty() {
        println!("  {}", "No devices found via fastboot or adb.".yellow());
        println!("  {}", "Make sure USB debugging or fastboot mode is enabled.".dimmed());
        println!("{}\n", "────────────────────────────────────────────────────────".dimmed());
        return 1;
    }

    for s in &fb_serials {
        println!("  {} {}", "fastboot".green().bold(), s);
    }
    for s in &adb_serials {
        println!("  {} {}", "adb".blue().bold(), s);
    }
    println!("{}\n", "────────────────────────────────────────────────────────".dimmed());

    if check {
        println!("Running pre-flash checks …\n");
        let result = run_pre_flash_checks(serial);
        print_check_report(&result);
        return if result.ready() { 0 } else { 1 };
    }

    0
}
