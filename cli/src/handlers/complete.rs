use clap::Command;
use clap_complete::{Shell as ClapShell, generate};
use clap_complete_nushell::Nushell;

use crate::cli::CompletionShell;

/// Write a completion script for `shell` to stdout.
///
/// The script is the command's only output, so nothing else may be written to
/// stdout on this path — that is why the logger goes to stderr (see `main`).
pub fn run(cmd: &mut Command, shell: CompletionShell) -> i32 {
    let name = cmd.get_name().to_string();
    let out = &mut std::io::stdout();
    match shell {
        CompletionShell::Bash => generate(ClapShell::Bash, cmd, name, out),
        CompletionShell::Zsh => generate(ClapShell::Zsh, cmd, name, out),
        CompletionShell::Fish => generate(ClapShell::Fish, cmd, name, out),
        CompletionShell::PowerShell => generate(ClapShell::PowerShell, cmd, name, out),
        CompletionShell::Elvish => generate(ClapShell::Elvish, cmd, name, out),
        CompletionShell::Nushell => generate(Nushell, cmd, name, out),
    }
    0
}
