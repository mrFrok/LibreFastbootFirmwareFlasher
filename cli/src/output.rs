//! Terminal presentation for the CLI: prompts, reports, and summaries.
//!
//! Everything here used to live in `lfff-lib`; the library now returns data
//! and emits log lines through callbacks, and this module renders them.
//!
//! All of it goes to stderr. stdout carries only a command's result — a
//! path, a link, a list — so that `$(lfff …)` and pipes get just that.

use std::io::{self, IsTerminal, Write};

use lfff_lib::device::PreFlashCheck;
use lfff_lib::device::{RebootTarget, Via, reboot, wipe_userdata};
use lfff_lib::extractor::ExtractionResult;
use lfff_lib::flasher::{FlashResult, FlashSession, is_critical_partition};
use lfff_lib::utils::{check_tools, tool_install_hint};

// ---------------------------------------------------------------------------
// Interactive input
// ---------------------------------------------------------------------------

/// Whether there is a person to answer: stdin is a terminal.
pub fn interactive() -> bool {
    io::stdin().is_terminal()
}

/// Prompt user for input, return default on empty. With no terminal on
/// stdin nobody can answer, so the default is taken without asking.
pub fn prompt(message: &str, default: &str) -> String {
    if !interactive() {
        return default.to_string();
    }
    let suffix = if default.is_empty() {
        String::new()
    } else {
        format!(" [{}]", default)
    };
    eprint!("{}{}: ", message, suffix);
    io::stderr().flush().ok();
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
        eprintln!("  {}  {}{}", status, tool, hint);
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
    eprintln!("\n── Pre-flash check report ─────────────────────────────");
    eprintln!("  {:<3} Device detected", s(check.device_found));
    eprintln!("  {:<3} Fastboot communication", s(check.communication_ok));
    eprintln!(
        "  {:<3} Cable speed ({:.2} MB/s)",
        s(check.cable_ok),
        check.cable_result.speed_mbs
    );
    eprintln!(
        "  {:<3} Battery level ({}%)",
        s(check.battery_ok),
        check.device_info.battery_level
    );
    eprintln!("  {:<3} Bootloader unlocked", s(check.unlocked));
    if !check.warnings.is_empty() {
        eprintln!("\n  Warnings:");
        for w in &check.warnings {
            eprintln!("    ⚠  {}", w);
        }
    }
    if !check.errors.is_empty() {
        eprintln!("\n  Errors:");
        for e in &check.errors {
            eprintln!("    ✗  {}", e);
        }
    }
    eprintln!();
    if check.ready() {
        eprintln!("  ✓ Device is ready for flashing.");
    } else {
        eprintln!("  ✗ Device is NOT ready. Fix errors above.");
    }
    eprintln!("────────────────────────────────────────────────────────\n");
}

// ---------------------------------------------------------------------------
// Extraction report
// ---------------------------------------------------------------------------

/// Print extraction result summary.
pub fn print_extraction_result(result: &ExtractionResult) {
    if !result.success {
        eprintln!("\n✗ Extraction failed: {}", result.error);
        return;
    }
    eprintln!("\n✓ Extracted to: {}", result.output_dir.display());
    let mut sorted: Vec<_> = result.groups.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    for (group, images) in &sorted {
        eprintln!("\n  {}/", group);
        let mut imgs: Vec<_> = images.iter().collect();
        imgs.sort();
        for img in imgs {
            let mb = std::fs::metadata(img)
                .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                .unwrap_or(0.0);
            eprintln!(
                "    {:<45} {:>7.1} MB",
                img.file_name().unwrap_or_default().to_string_lossy(),
                mb
            );
        }
    }
    eprintln!("\n  Total: {} image(s)", result.all_images().len());
    if let Some(arb) = &result.arb_info {
        eprintln!("\n── ARB ──────────────────────────────────────────────────");
        eprintln!("  {}", arb);
        if arb.enforced() {
            eprintln!("  ⚠  Hard ARB is ACTIVE on this firmware.");
        }
        eprintln!("────────────────────────────────────────────────────────");
    }
}

// ---------------------------------------------------------------------------
// Flash failure diagnosis
// ---------------------------------------------------------------------------

/// Print a failed flash result with probable cause and suggested fix.
pub fn report_failure(result: &FlashResult) {
    let is_crit = is_critical_partition(&result.partition);
    let err_lower = result.error.to_lowercase();

    eprintln!();
    eprintln!("{}", "━".repeat(60));
    eprintln!("  ✗  FAILED  {}_{}", result.partition, result.slot);
    eprintln!("  {}", result.error);
    eprintln!();

    if err_lower.contains("resize") || err_lower.contains("not enough space") {
        eprintln!("  Cause: Dynamic partition resize failed.");
        eprintln!("  Fix  : Make sure the device is in fastbootd and retry.");
    } else if err_lower.contains("does not exist") || err_lower.contains("not found") {
        eprintln!("  Cause: Partition not present on this device.");
        eprintln!("  Fix  : This image may not be compatible with your device variant.");
    } else if err_lower.contains("permission denied") || err_lower.contains("not allowed") {
        eprintln!("  Cause: Bootloader is locked.");
        eprintln!("  Fix  : fastboot flashing unlock");
    } else if err_lower.contains("timeout") {
        eprintln!("  Cause: USB timeout.");
        eprintln!("  Fix  : Try a different cable or USB 3.0 port.");
    } else {
        eprintln!("  Possible causes:");
        eprintln!("    • Faulty USB cable — try a different one");
        eprintln!("    • Bootloader is locked  →  fastboot flashing unlock");
        eprintln!("    • Corrupted image — re-download the firmware");
        eprintln!("    • Low battery during flash");
    }

    if is_crit {
        eprintln!();
        eprintln!("  ⚠  CRITICAL partition — do NOT reboot or unplug until resolved.");
    }
    eprintln!("{}", "━".repeat(60));
    eprintln!();
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

    eprintln!("\n── Flash session summary ───────────────────────────────");
    eprintln!("  Total      :  {}", total);
    eprintln!("  ✓ OK       :  {}", ok);
    if failed_count > 0 {
        eprintln!("  ✗ Failed   :  {}", failed_count);
    }

    if !session.failed().is_empty() {
        eprintln!("\n  Failed partitions:");
        for r in session.failed() {
            let crit = if is_critical_partition(&r.partition) {
                "  [CRITICAL]"
            } else {
                ""
            };
            eprintln!("    ✗  {}_{}{}", r.partition, r.slot, crit);
            eprintln!("       {}", r.error);
        }
    }

    eprintln!();

    if session.failed().is_empty() && !session.aborted {
        let elapsed_total: f64 = session.succeeded().iter().map(|r| r.duration_s).sum();
        let mins = elapsed_total as u64 / 60;
        let secs = elapsed_total as u64 % 60;
        let time_str = if mins > 0 {
            format!("{}m {:02}s", mins, secs)
        } else {
            format!("{}s", secs)
        };

        eprintln!("{}", "━".repeat(60));
        eprintln!("  ✓  Flash complete!");
        eprintln!("{}", "━".repeat(60));
        eprintln!("  Partitions flashed :  {}", ok);
        eprintln!("  Total flash time   :  {}", time_str);
        eprintln!("{}", "━".repeat(60));
        eprintln!();
        if !session.outside_group.is_empty() {
            eprintln!(
                "  ⚠  {} partition(s) were created outside their group.",
                session.outside_group.len()
            );
            eprintln!("     Before each of the next two system updates, run");
            eprintln!("     \"Repair updates (OTA)\" in lfff-gui, or the second may fail.");
            eprintln!();
        }
    } else if !session.critical_failed().is_empty() {
        eprintln!("{}", "━".repeat(60));
        eprintln!("  ✗  Critical failure");
        eprintln!("  One or more CRITICAL partitions failed to flash.");
        eprintln!("  The device may not boot.");
        eprintln!("  Do NOT reboot or unplug until resolved.");
        eprintln!("{}", "━".repeat(60));
    } else if !session.failed().is_empty() {
        eprintln!("{}", "━".repeat(60));
        eprintln!("  ⚠  Flash completed with errors");
        eprintln!("  Non-critical partitions failed — device should still boot.");
        eprintln!("  Re-flash the failed partitions to complete the update.");
        eprintln!("{}", "━".repeat(60));
    } else if session.aborted {
        eprintln!("{}", "━".repeat(60));
        eprintln!("  ⚠  Flash was aborted");
        eprintln!("{}", "━".repeat(60));
    }
}

/// Interactively offer a userdata wipe, then reboot to system.
/// Follow-up to a successful [`print_summary`].
pub fn offer_wipe_and_reboot(session: &FlashSession) {
    offer_wipe(session);

    eprintln!("\n  Rebooting to system ...");
    if let Err(e) = reboot(
        session.serial.as_deref(),
        Via::Fastboot,
        RebootTarget::System,
    ) {
        eprintln!("  ✗ Reboot failed: {}", e.trim());
    }
}

fn offer_wipe(session: &FlashSession) {
    eprintln!("── Format userdata ──────────────────────────────────────");
    eprintln!("  'fastboot -w' wipes ALL user data (contacts, apps, files).");
    eprintln!("  Recommended after a major version change or cross-region flash.");
    eprintln!();
    eprintln!("  ⚠  ALL DATA WILL BE PERMANENTLY ERASED.");
    eprintln!();

    let answer = prompt("  Wipe userdata now? (yes / no)", "no");
    if answer != "yes" {
        eprintln!("  Skipped. Wipe manually later: fastboot -w");
        eprintln!("────────────────────────────────────────────────────────\n");
        return;
    }

    eprintln!("  Wiping userdata ...");
    match wipe_userdata(session.serial.as_deref()) {
        Ok(()) => eprintln!("  ✓ Userdata wiped successfully."),
        Err(e) => eprintln!("  ✗ Wipe failed: {}", e.trim()),
    }
    eprintln!("────────────────────────────────────────────────────────\n");
}
