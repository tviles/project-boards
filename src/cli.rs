use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "project-boards",
    version,
    about = "GitHub Projects boards in herdr"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, PartialEq)]
pub enum Command {
    /// Run the board TUI (what the herdr pane runs).
    Pane,
    /// Open a board pane through herdr, or focus it if it is already open.
    Open {
        /// Board to open, as OWNER/NUMBER.
        #[arg(long)]
        project: Option<String>,
        /// Where to open the pane.
        #[arg(long, value_enum)]
        placement: Option<Placement>,
        /// Always show the board picker.
        #[arg(long)]
        picker: bool,
    },
    /// Check token, scopes, herdr, network, repository detection and key bindings.
    Doctor {
        /// Also show a herdr notification with the summary.
        #[arg(long)]
        notify: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    #[default]
    Tab,
    Overlay,
    Split,
    Zoomed,
}

impl Placement {
    pub fn as_str(&self) -> &'static str {
        match self {
            Placement::Tab => "tab",
            Placement::Overlay => "overlay",
            Placement::Split => "split",
            Placement::Zoomed => "zoomed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_with_project_and_placement() {
        let cli = Cli::try_parse_from([
            "project-boards",
            "open",
            "--project",
            "tviles/3",
            "--placement",
            "overlay",
        ])
        .unwrap();
        assert_eq!(
            cli.command,
            Command::Open {
                project: Some("tviles/3".into()),
                placement: Some(Placement::Overlay),
                picker: false
            }
        );
    }

    #[test]
    fn rejects_popup_placement() {
        let result = Cli::try_parse_from(["project-boards", "open", "--placement", "popup"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_pane_and_doctor() {
        assert_eq!(
            Cli::try_parse_from(["project-boards", "pane"])
                .unwrap()
                .command,
            Command::Pane
        );
        assert_eq!(
            Cli::try_parse_from(["project-boards", "doctor", "--notify"])
                .unwrap()
                .command,
            Command::Doctor { notify: true }
        );
    }

    #[test]
    fn placement_strings_match_herdr() {
        let all = [
            Placement::Tab,
            Placement::Overlay,
            Placement::Split,
            Placement::Zoomed,
        ];
        let names: Vec<_> = all.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, ["tab", "overlay", "split", "zoomed"]);
    }
}
