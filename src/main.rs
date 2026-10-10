use clap::Parser;
use project_boards::cli::{Cli, Command};
use project_boards::commands::open::{OpenArgs, OpenOutcome, open};
use project_boards::config::load_config;
use project_boards::herdr::cli::ProcessHerdr;
use project_boards::herdr::env::PluginEnv;
use project_boards::herdr::repo::detect_repo;
use project_boards::state::State;
use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Open {
            project,
            placement,
            picker,
        } => {
            let env = PluginEnv::from_system();
            let (config, _) = load_config(&env.config_dir);
            let state = State::load(&env.state_dir);
            let project = match project
                .as_deref()
                .map(str::parse::<project_boards::model::BoardRef>)
                .transpose()
            {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("project-boards: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let args = OpenArgs {
                project,
                placement,
                picker,
            };
            match open(
                &env,
                &ProcessHerdr::from_env(),
                &config,
                &state,
                &args,
                detect_repo,
            ) {
                Ok(OpenOutcome::Focused(p)) => println!("focused {p}"),
                Ok(OpenOutcome::Opened(p)) => println!("opened {p}"),
                Err(e) => {
                    eprintln!("project-boards: {e:#}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Command::Pane => match project_boards::commands::pane::run_pane() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "project-boards: {e:#}");
                ExitCode::FAILURE
            }
        },
        Command::Doctor { .. } => {
            eprintln!("project-boards: this subcommand is not built yet in this version");
            ExitCode::FAILURE
        }
    }
}
