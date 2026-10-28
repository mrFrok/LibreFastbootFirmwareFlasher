use std::path::PathBuf;

use lfff_lib::arb::{
    ArbCompare, DeviceArbError, compare_arb, device_arb, extract_arb_from_xbl, find_xbl_config,
};

pub fn run(
    xbl: Option<&PathBuf>,
    firmware_dir: Option<&PathBuf>,
    device: bool,
    serial: Option<&str>,
) -> i32 {
    let xbl_path = match (xbl, firmware_dir) {
        (Some(path), _) => Some(path.clone()),
        (None, Some(dir)) => match find_xbl_config(dir) {
            Some(p) => Some(p),
            None => {
                eprintln!("✗ xbl_config.img not found in the given firmware directory.");
                return 1;
            }
        },
        (None, None) if device => None,
        // clap requires one of --xbl, --firmware-dir, --device.
        _ => unreachable!("clap requires a source for the ARB"),
    };

    let mut device_version = None;
    if device {
        eprintln!("\n  Reading xbl_config from the phone over adb (root)...");
        match device_arb(serial) {
            Ok(d) => {
                let slot = if d.slot.is_empty() {
                    String::new()
                } else {
                    format!(" (slot {})", d.slot.to_uppercase())
                };
                println!("  Phone     : {}{}", d.info, slot);
                device_version = d.info.version;
            }
            Err(e) => {
                eprintln!("  ✗ {}", e);
                match e {
                    DeviceArbError::NoDevice => eprintln!(
                        "     Boot the phone into Android with USB debugging on, or into a recovery with adb."
                    ),
                    DeviceArbError::NoRoot => eprintln!(
                        "     Magisk: allow the prompt on the phone. KernelSU / APatch: grant root to Shell in the manager's superuser list. Or use a recovery with root adb. fastbootd cannot read xbl_config."
                    ),
                    _ => {}
                }
                if xbl_path.is_none() {
                    return 1;
                }
            }
        }
    }

    let Some(xbl_path) = xbl_path else {
        return 0;
    };

    let firmware_arb = extract_arb_from_xbl(&xbl_path);
    println!("\n  Firmware  : {}", firmware_arb);

    if let (Some(dev), Some(fw)) = (device_version, firmware_arb.version) {
        match compare_arb(dev, fw) {
            ArbCompare::Same => {
                println!(
                    "  ✓  Same ARB as the phone ({}): the counter will not change.",
                    dev
                );
            }
            ArbCompare::Raises => {
                println!(
                    "  ⚠  Firmware ARB {} is higher than the phone's {}.",
                    fw, dev
                );
                eprintln!("     Flashing will permanently raise the anti-rollback counter.");
                eprintln!(
                    "     You will NOT be able to go back to firmware with ARB {}.",
                    dev
                );
            }
            ArbCompare::Lower => {
                println!(
                    "  ✗  Firmware ARB {} is LOWER than the phone's {}.",
                    fw, dev
                );
                eprintln!(
                    "     The bootloader will reject it — flashing this firmware bricks the phone."
                );
                return 1;
            }
        }
        return 0;
    }

    if firmware_arb.version.is_none() {
        println!("  ⚠  The firmware's ARB could not be read — treat it as unknown, not as 0.");
        return 1;
    }
    if firmware_arb.enforced() {
        println!("  ⚠  Hard ARB is ACTIVE on this firmware (ARB > 0).");
        eprintln!("     Flashing will permanently raise the anti-rollback counter.");
        eprintln!("     You will NOT be able to downgrade afterwards.");
    } else {
        println!("  ✓  Hard ARB is not enforced (version = 0).");
    }

    0
}
