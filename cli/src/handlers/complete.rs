use clap::Command;
use clap_complete::{Shell as ClapShell, generate};
use clap_complete_nushell::Nushell;
use std::io::Write;
use std::process;
const COLOR_RED: &str = "\x1b[31m";
const COLOR_GREEN: &str = "\x1b[32m";
const COLOR_RESET: &str = "\x1b[0m";

type GeneratorFn = fn(&mut Command, String, &mut dyn Write);
pub fn get_generator(shell: &str) -> Option<GeneratorFn> {
    match shell {
        "bash" => Some(generate_bash),
        "zsh" => Some(generate_zsh),
        "fish" => Some(generate_fish),
        "powershell" => Some(generate_powershell),
        "elvish" => Some(generate_elvish),
        "nushell" => Some(generate_nushell),
        _ => None,
    }
}

pub fn handle_completion(cmd: &mut Command, shell: Option<&str>) -> ! {
    let cmd_name = cmd.get_name().to_string();

    match shell {
        Some(s) => {
            if let Some(generator) = get_generator(s) {
                generator(cmd, cmd_name, &mut std::io::stdout());
                process::exit(0);
            } else {
                eprintln!("Unsupported shell: {}", s);
                process::exit(1);
            }
        }
        None => {
            eprintln!(
                "{COLOR_RED}error:{COLOR_RESET} the following required arguments were not provided:"
            );
            eprintln!("  {COLOR_GREEN}<SHELL>{COLOR_RESET}");
            eprintln!();
            eprintln!("Usage: lfff completion {COLOR_GREEN}<SHELL>{COLOR_RESET}");
            eprintln!(
                "Supported {COLOR_GREEN}<SHELL>{COLOR_RESET}: bash, zsh, fish, powershell, elvish, nushell"
            );
            eprintln!("For more information, try '--help'.");
            process::exit(1);
        }
    }
}

fn generate_bash(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(ClapShell::Bash, cmd, name, out);
}
fn generate_zsh(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(ClapShell::Zsh, cmd, name, out);
}
fn generate_fish(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(ClapShell::Fish, cmd, name, out);
}
fn generate_powershell(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(ClapShell::PowerShell, cmd, name, out);
}
fn generate_elvish(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(ClapShell::Elvish, cmd, name, out);
}
fn generate_nushell(cmd: &mut Command, name: String, out: &mut dyn Write) {
    generate(Nushell, cmd, name, out);
}
