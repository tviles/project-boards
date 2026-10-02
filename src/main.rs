use clap::Parser;
use project_boards::cli::{Cli, Command};
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let name = match cli.command {
        Command::Pane => "pane",
        Command::Open { .. } => "open",
        Command::Doctor { .. } => "doctor",
    };
    eprintln!("project-boards: `{name}` is not built yet in this version");
    ExitCode::FAILURE
}
