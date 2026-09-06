//! Terminal presentation for the CLI: prompts, reports, and summaries.
//!
//! Everything here used to live in `lfff-lib`; the library now returns data
//! and emits log lines through callbacks, and this module renders them.

use std::io::{self, Write};

use lfff_lib::device::PreFlashCheck;
use lfff_lib::extractor::ExtractionResult;
use lfff_lib::flasher::{FlashResult, FlashSession, is_critical_partition};
use lfff_lib::utils::{check_tools, fastboot, tool_install_hint};

// ---------------------------------------------------------------------------
// Interactive input
// ---------------------------------------------------------------------------

/// Prompt user for input, return default on empty.
pub fn prompt(message: &str, default: &str) -> String {
    let suffix = if default.is_empty() {
        String::new()
    } else {
        format!(" [{}]", default)
    };
    print!("{}{}: ", message, suffix);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Dependency table
// ---------------------------------------------------------------------------

/// Print dependency table, return false if any tool is missing.
pub fn require_tools(tools: &[&str]) -> bool {
    let results = check_tools(tools);
    let mut all_ok = true;
    for &tool in tools {
        let found = results.get(tool).copied().unwrap_or(false);
        let status = if found { "✓" } else { "✗" };
        let hint = if found {
            String::new()
        } else {
            format!("  →  {}", tool_install_hint(tool))
        };
        println!("  {}  {}{}", status, tool, hint);
        if !found {
            all_ok = false;
        }
    }
    all_ok
}

// ---------------------------------------------------------------------------
// Pre-flash check report
// ---------------------------------------------------------------------------

/// Print human-readable pre-flash check report.
pub fn print_check_report(check: &PreFlashCheck) {
    let s = |ok: bool| if ok { "✓" } else { "✗" };
    println!("\n── Pre-flash check report ─────────────────────────────");
    println!("  {:<3} Device detected", s(check.device_found));
    println!("  {:<3} Fastboot communication", s(check.communication_ok));
    println!(
        "  {:<3} Cable speed ({:.2} MB/s)",
        s(check.cable_ok),
        check.cable_result.speed_mbs
    );
    println!(
        "  {:<3} Battery level ({}%)",
        s(check.battery_ok),
        check.device_info.battery_level
    );
    println!("  {:<3} Bootloader unlocked", s(check.unlocked));
    if !check.warnings.is_empty() {
        println!("\n  Warnings:");
        for w in &check.warnings {
            println!("    ⚠  {}", w);
        }
    }
    if !check.errors.is_empty() {
        println!("\n  Errors:");
        for e in &check.errors {
            println!("    ✗  {}", e);
        }
    }
    println!();
    if check.ready() {
        println!("  ✓ Device is ready for flashing.");
    } else {
        println!("  ✗ Device is NOT ready. Fix errors above.");
    }
    println!("────────────────────────────────────────────────────────\n");
}

// ---------------------------------------------------------------------------
// Extraction report
// ---------------------------------------------------------------------------

/// Print extraction result summary to stdout.
pub fn print_extraction_result(result: &ExtractionResult) {
    if !result.success {
        println!("\n✗ Extraction failed: {}", result.error);
        return;
    }
    println!("\n✓ Extracted to: {}", result.output_dir.display());
    let mut sorted: Vec<_> = result.groups.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    for (group, images) in &sorted {
        println!("\n  {}/", group);
        let mut imgs: Vec<_> = images.iter().collect();
        imgs.sort();
        for img in imgs {
            let mb = std::fs::metadata(img)
                .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                .unwrap_or(0.0);
            println!(
                "    {:<45} {:>7.1} MB",
                img.file_name().unwrap_or_default().to_string_lossy(),
                mb
            );
        }
    }
    println!("\n  Total: {} image(s)", result.all_images().len());
    if let Some(arb) = &result.arb_info {
        println!("\n── ARB ──────────────────────────────────────────────────");
        println!("  {}", arb);
        if arb.enforced() {
            println!("  ⚠  Hard ARB is ACTIVE on this firmware.");
        }
        println!("────────────────────────────────────────────────────────");
    }
}

// ---------------------------------------------------------------------------
// Flash failure diagnosis
// ---------------------------------------------------------------------------

/// Print a failed flash result with probable cause and suggested fix.
pub fn report_failure(result: &FlashResult) {
    let is_crit = is_critical_partition(&result.partition);
    let err_lower = result.error.to_lowercase();

    println!();
    println!("{}", "━".repeat(60));
    println!("  ✗  FAILED  {}_{}", result.partition, result.slot);
    println!("  {}", result.error);
    println!();

    if err_lower.contains("resize") || err_lower.contains("not enough space") {
        println!("  Cause: Dynamic partition resize failed.");
        println!("  Fix  : Make sure the device is in fastbootd and retry.");
    } else if err_lower.contains("does not exist") || err_lower.contains("not found") {
        println!("  Cause: Partition not present on this device.");
        println!("  Fix  : This image may not be compatible with your device variant.");
    } else if err_lower.contains("permission denied") || err_lower.contains("not allowed") {
        println!("  Cause: Bootloader is locked.");
        println!("  Fix  : fastboot flashing unlock");
    } else if err_lower.contains("timeout") {
        println!("  Cause: USB timeout.");
        println!("  Fix  : Try a different cable or USB 3.0 port.");
    } else {
        println!("  Possible causes:");
        println!("    • Faulty USB cable — try a different one");
        println!("    • Bootloader is locked  →  fastboot flashing unlock");
        println!("    • Corrupted image — re-download the firmware");
        println!("    • Low battery during flash");
    }

    if is_crit {
        println!();
        println!("  ⚠  CRITICAL partition — do NOT reboot or unplug until resolved.");
    }
    println!("{}", "━".repeat(60));
    println!();
}

// ---------------------------------------------------------------------------
// Session summary + wipe/reboot follow-up
// ---------------------------------------------------------------------------

/// Print session summary. Pure output — no prompts, no device commands.
/// Callers follow up with [`offer_wipe_and_reboot`] on success.
pub fn print_summary(session: &FlashSession) {
    let total = session.results.len();
    let ok = session.succeeded().len();
    let failed_count = session.failed().len();

    println!("\n── Flash session summary ───────────────────────────────");
    println!("  Total      :  {}", total);
    println!("  ✓ OK       :  {}", ok);
    if failed_count > 0 {
        println!("  ✗ Failed   :  {}", failed_count);
    }

    if !session.failed().is_empty() {
        println!("\n  Failed partitions:");
        for r in session.failed() {
            let crit = if is_critical_partition(&r.partition) {
                "  [CRITICAL]"
            } else {
                ""
            };
            println!("    ✗  {}_{}{}", r.partition, r.slot, crit);
            println!("       {}", r.error);
        }
    }

    println!();

    if session.failed().is_empty() && !session.aborted {
        let elapsed_total: f64 = session.succeeded().iter().map(|r| r.duration_s).sum();
        let mins = elapsed_total as u64 / 60;
        let secs = elapsed_total as u64 % 60;
        let time_str = if mins > 0 {
            format!("{}m {:02}s", mins, secs)
        } else {
            format!("{}s", secs)
        };

        println!("{}", "━".repeat(60));
        println!("  ✓  Flash complete!");
        println!("{}", "━".repeat(60));
        println!("  Partitions flashed :  {}", ok);
        println!("  Total flash time   :  {}", time_str);
        println!("{}", "━".repeat(60));
        println!();
    } else if !session.critical_failed().is_empty() {
        println!("{}", "━".repeat(60));
        println!("  ✗  Critical failure");
        println!("  One or more CRITICAL partitions failed to flash.");
        println!("  The device may not boot.");
        println!("  Do NOT reboot or unplug until resolved.");
        println!("{}", "━".repeat(60));
    } else if !session.failed().is_empty() {
        println!("{}", "━".repeat(60));
        println!("  ⚠  Flash completed with errors");
        println!("  Non-critical partitions failed — device should still boot.");
        println!("  Re-flash the failed partitions to complete the update.");
        println!("{}", "━".repeat(60));
    } else if session.aborted {
        println!("{}", "━".repeat(60));
        println!("  ⚠  Flash was aborted");
        println!("{}", "━".repeat(60));
    }
}

/// Interactively offer a userdata wipe, then reboot to system.
/// Follow-up to a successful [`print_summary`].
pub fn offer_wipe_and_reboot(session: &FlashSession) {
    offer_wipe(session);

    println!("\n  Rebooting to system ...");
    let mut args: Vec<&str> = Vec::new();
    if let Some(s) = session.serial.as_deref() {
        args.push("-s");
        args.push(s);
    }
    args.push("reboot");
    fastboot(&args, 30);
}

fn offer_wipe(session: &FlashSession) {
    println!("── Format userdata ──────────────────────────────────────");
    println!("  'fastboot -w' wipes ALL user data (contacts, apps, files).");
    println!("  Recommended after a major version change or cross-region flash.");
    println!();
    println!("  ⚠  ALL DATA WILL BE PERMANENTLY ERASED.");
    println!();

    let answer = prompt("  Wipe userdata now? (yes / no)", "no");
    if answer != "yes" {
        println!("  Skipped. Wipe manually later: fastboot -w");
        println!("────────────────────────────────────────────────────────\n");
        return;
    }

    println!("  Wiping userdata ...");
    let mut args: Vec<&str> = Vec::new();
    if let Some(s) = session.serial.as_deref() {
        args.push("-s");
        args.push(s);
    }
    args.push("-w");
    let r = fastboot(&args, 120);
    if r.code == 0 {
        println!("  ✓ Userdata wiped successfully.");
    } else {
        println!(
            "  ✗ Wipe failed: {}",
            if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            }
        );
    }
    println!("────────────────────────────────────────────────────────\n");
}
