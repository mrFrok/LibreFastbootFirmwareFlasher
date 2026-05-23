use colored::Colorize;
use lfff_lib::deps::install_dependencies;

pub fn run(check: bool, tools: &[String]) -> i32 {
    let tool_list = if tools.is_empty() { None } else { Some(tools) };
    let report = install_dependencies(tool_list, check);

    if report.all_ok() {
        println!("{}", "✓ All dependencies are ready".green().bold());
        0
    } else {
        eprintln!("{}", "✗ Some dependencies are missing — see report above".red().bold());
        1
    }
}
