//! `lfff root` and `lfff flash --root`: patch init_boot (or boot) with
//! KernelSU, KernelSU Next or SukiSU Ultra and flash it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use colored::Colorize;
use lfff_lib::device::{RebootTarget, Via, reboot};
use lfff_lib::flasher::{
    DeviceMode, collect_images, detect_mode, flash_partition, get_active_slot,
    wait_for_any_fastboot, wait_for_fastbootd,
};
use lfff_lib::root::{Patched, RootFork, install_manager, patch_firmware};

use crate::output::{interactive, prompt, require_tools};

fn fail(msg: impl std::fmt::Display) {
    eprintln!("{} {}", "✗".red().bold(), msg.to_string().red());
}

/// Patch the firmware's image for `fork`, logging each step. `None` after
/// printing why, when it cannot be done.
pub fn patch(
    fork: RootFork,
    images: &HashMap<String, PathBuf>,
    serial: Option<&str>,
) -> Option<Patched> {
    eprintln!(
        "\n{}",
        format!(
            "── Root: {} ─────────────────────────────────────",
            fork.name()
        )
        .dimmed()
    );
    let on_log = |m: String| eprintln!("  {m}");
    match patch_firmware(fork, images, serial, &on_log) {
        Ok(p) => {
            eprintln!(
                "  {} {} patched with {} {} ({})",
                "✓".green(),
                p.partition,
                fork.name(),
                p.release.tag_name,
                p.kmi
            );
            Some(p)
        }
        Err(e) => {
            fail(format!("Root: {e}. Nothing was flashed."));
            None
        }
    }
}

/// The warnings every root install shows before its confirmation.
pub fn print_warnings(p: &Patched) {
    eprintln!(
        "  {}",
        format!(
            "Root: {} is flashed patched with {}. Play Integrity will fail while it is rooted.",
            p.partition,
            p.fork.name()
        )
        .yellow()
    );
    eprintln!(
        "  {}",
        format!(
            "Never lock the bootloader with a patched {}: flash the stock one first, or the phone may not start.",
            p.partition
        )
        .red()
        .bold()
    );
}

/// After the phone has started: offer the fork's manager app over adb.
pub fn offer_manager(p: &Patched, serial: Option<&str>) {
    if !interactive() {
        return;
    }
    let ans = prompt(
        &format!(
            "\n  Install the {} manager app ({}) when the phone has started? (yes / no)",
            p.fork.name(),
            p.release.tag_name
        ),
        "no",
    );
    if ans != "yes" {
        eprintln!(
            "  Skipped. Install it from the {} releases later.",
            p.fork.name()
        );
        return;
    }
    eprintln!("  Waiting for the phone in Android (USB debugging on)...");
    let mut wait: Vec<&str> = Vec::new();
    if let Some(s) = serial {
        wait.extend(["-s", s]);
    }
    wait.push("wait-for-device");
    if !lfff_lib::utils::adb(&wait, 180).success() {
        fail("The phone did not show up over adb; install the manager yourself.");
        return;
    }
    let on_log = |m: String| eprintln!("  {m}");
    match install_manager(p.fork, &p.release, serial, &on_log) {
        Ok(()) => eprintln!("  {} Manager installed.", "✓".green()),
        Err(e) => fail(e),
    }
}

/// When the phone was patched from Android, take it to fastbootd for the
/// flash. False when it does not get there.
pub fn to_fastbootd_if_in_android(serial: Option<&str>) -> bool {
    if detect_mode(serial) != DeviceMode::System {
        return true;
    }
    eprintln!("  Rebooting the phone to fastbootd...");
    if let Err(e) = reboot(serial, Via::Adb, RebootTarget::Fastbootd) {
        fail(format!("Could not reboot to fastbootd: {}", e.trim()));
        return false;
    }
    if !wait_for_fastbootd(serial, 120) {
        fail("The phone did not come up in fastbootd.");
        return false;
    }
    true
}

/// `lfff root`: patch the image of the firmware the phone runs and flash it
/// to the active slot.
pub fn run(
    dir: &Path,
    fork: RootFork,
    serial: Option<&str>,
    dry_run: bool,
    no_manager: bool,
) -> i32 {
    if !dir.is_dir() {
        fail(format!("Not a directory: {}", dir.display()));
        return 1;
    }
    if !dry_run && !interactive() {
        fail(
            "Rooting asks for confirmation and needs a terminal; run it there (or use --dry-run).",
        );
        return 2;
    }
    if !require_tools(&["fastboot", "adb", "curl"]) {
        fail("fastboot, adb and curl are needed. Aborting.");
        return 1;
    }
    let images = collect_images(dir);
    if images.is_empty() {
        fail(format!("No .img files found in {}", dir.display()));
        return 1;
    }
    let Some(p) = patch(fork, &images, serial) else {
        return 1;
    };
    if dry_run {
        eprintln!("  Patched image: {}", p.image.display());
        eprintln!("  {}", "--dry-run: nothing was flashed.".dimmed());
        return 0;
    }

    eprintln!(
        "\n{}",
        "── Final warning ────────────────────────────────────────".dimmed()
    );
    eprintln!(
        "  {}",
        format!(
            "The firmware must be the same build the phone runs now: a {} from another build can stop it from starting.",
            p.partition
        )
        .red()
    );
    print_warnings(&p);
    eprintln!(
        "  {}",
        format!("{} is flashed to the active slot only.", p.partition).dimmed()
    );
    if prompt("\n  Type ROOT to flash it, anything else to abort", "") != "ROOT" {
        eprintln!("{}", "Aborted by user.".yellow());
        return 1;
    }

    match detect_mode(serial) {
        DeviceMode::System => {
            eprintln!("  Rebooting the phone to the bootloader...");
            if let Err(e) = reboot(serial, Via::Adb, RebootTarget::Bootloader) {
                fail(format!("Could not reboot to the bootloader: {}", e.trim()));
                return 1;
            }
            if !wait_for_any_fastboot(serial, 120) {
                fail("The phone did not come up in fastboot.");
                return 1;
            }
        }
        DeviceMode::Bootloader | DeviceMode::Fastbootd => {}
        DeviceMode::Unknown => {
            fail("No phone found (adb or fastboot).");
            return 1;
        }
    }
    let Some(slot) = get_active_slot(serial) else {
        fail("Could not read the active slot. Nothing was flashed.");
        return 1;
    };
    eprintln!("  Flashing {}_{slot}...", p.partition);
    let r = flash_partition(&p.image, &p.partition, &slot, serial);
    if !r.success {
        fail(format!(
            "Flashing {} failed: {}",
            p.partition,
            r.error.trim()
        ));
        return 1;
    }
    eprintln!("  {} {}_{slot} flashed.", "✓".green(), p.partition);
    if let Err(e) = reboot(serial, Via::Fastboot, RebootTarget::System) {
        fail(format!("Reboot failed: {}", e.trim()));
    }
    if !no_manager {
        offer_manager(&p, serial);
    }
    eprintln!("\n{}", "✓ Root installed".green().bold());
    0
}
