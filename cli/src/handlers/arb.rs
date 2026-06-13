use std::path::PathBuf;

use lfff_lib::arb::{extract_arb_from_xbl, find_xbl_config};

pub fn run(xbl: Option<&PathBuf>, firmware_dir: Option<&PathBuf>) -> i32 {
    let xbl_path = match (xbl, firmware_dir) {
        (Some(path), _) => path.clone(),
        (None, Some(dir)) => match find_xbl_config(dir) {
            Some(p) => p,
            None => {
                println!("✗ xbl_config.img not found in the given firmware directory.");
                return 1;
            }
        },
        _ => {
            println!("✗ Provide either --xbl <path> or --firmware-dir <dir>.");
            return 1;
        }
    };

    let firmware_arb = extract_arb_from_xbl(&xbl_path);
    println!("\n  Firmware  : {}", firmware_arb);

    if firmware_arb.enforced() {
        println!("  ⚠  Hard ARB is ACTIVE on this firmware (ARB > 0).");
        println!("     Flashing will permanently raise the anti-rollback counter.");
        println!("     You will NOT be able to downgrade afterwards.");
    } else {
        println!("  ✓  Hard ARB is not enforced (version = 0).");
    }

    0
}
