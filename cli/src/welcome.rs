pub fn print() {
    println!();
    println!("  ██╗     ███████╗███████╗███████╗");
    println!("  ██║     ██╔════╝██╔════╝██╔════╝");
    println!("  ██║     █████╗  █████╗  █████╗  ");
    println!("  ██║     ██╔══╝  ██╔══╝  ██╔══╝  ");
    println!("  ███████╗██║     ██║     ██║     ");
    println!("  ╚══════╝╚═╝     ╚═╝     ╚═╝     ");
    println!();
    println!(
        "  LibreFastbootFirmwareFlasher  v{}",
        env!("CARGO_PKG_VERSION")
    );
    println!("  Flash Android firmware via fastboot — free, open, no bloat.");
    println!();
    println!("  Quick start:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  1.  lfff deps                       install tools");
    println!("  2.  lfff download <url>              download OTA zip");
    println!("  3.  lfff extract firmware.zip        unpack images");
    println!("  4.  lfff devices --check             verify setup");
    println!("  5.  lfff flash ./firmwares/<dir>     flash device");
    println!();
    println!("  Commands:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  arb               compare Anti-Rollback version");
    println!("  completion        generate shell completion");
    println!("  deps              install & verify external tools");
    println!("  devices           list devices, run pre-flash checks");
    println!("  download          download OTA firmware zip");
    println!("  extract           extract .zip into partition images");
    println!("  flash             flash full firmware (A/B, super)");
    println!("  flash-partition   flash a single partition by name");

    println!("  Links:");
    println!("  ─────────────────────────────────────────────────────");
    println!("  GitHub     https://github.com/mrFrok/LibreFastbootFirmwareFlasher");
    println!("  Telegram   https://t.me/gt3neo5hub");
    println!("  Author     https://t.me/mrFrok228");
    println!();
    println!("  -v / --verbose    debug output");
    println!("  <command> --help  command help");
    println!();
    println!("  OnePlus · OPPO · Realme · Qualcomm A/B · Dynamic partitions");
    println!();
}
