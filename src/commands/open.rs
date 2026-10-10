//! `project-boards open`: run by herdr actions. Focuses the board's pane if one is open,
//! otherwise opens a new board pane, passing what it knows through PB_* variables.

use crate::cli::Placement;
use crate::config::Config;
use crate::herdr::cli::{HerdrCli, OpenRequest, action_cwd, focus_plugin_pane, open_plugin_pane};
use crate::herdr::env::PluginEnv;
use crate::herdr::registry::PaneRegistry;
use crate::model::{BoardRef, RepoSlug};
use crate::state::State;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OpenArgs {
    pub project: Option<BoardRef>,
    pub placement: Option<Placement>,
    pub picker: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenOutcome {
    Focused(String),
    Opened(String),
}

pub fn open(
    env: &PluginEnv,
    cli: &dyn HerdrCli,
    config: &Config,
    state: &State,
    args: &OpenArgs,
    detect_repo: impl Fn(&Path) -> Option<RepoSlug>,
) -> anyhow::Result<OpenOutcome> {
    let ctx = &env.context;
    let cwd = action_cwd(env, cli);
    let repo = cwd.as_deref().and_then(|d| detect_repo(Path::new(d)));
    let board = args.project.clone().or_else(|| {
        if args.picker {
            None
        } else {
            repo.as_ref().and_then(|r| state.remembered_board(r))
        }
    });

    if let (Some(board), false) = (&board, args.picker)
        && let Some(pane) = PaneRegistry::new(&env.state_dir).live_pane(cli, board)
    {
        focus_plugin_pane(cli, &pane)?;
        return Ok(OpenOutcome::Focused(pane));
    }

    let placement = args.placement.unwrap_or(config.placement);
    let needs_target = matches!(placement, Placement::Split | Placement::Zoomed);
    if needs_target && ctx.focused_pane_id.is_none() {
        anyhow::bail!(
            "the {} placement needs a focused pane to open next to",
            placement.as_str()
        );
    }
    let mut vars = Vec::new();
    if let Some(r) = &repo {
        vars.push(("PB_REPO".to_string(), r.to_string()));
    }
    if let Some(b) = &board {
        vars.push(("PB_BOARD".to_string(), b.to_string()));
    }
    if args.picker {
        vars.push(("PB_PICKER".to_string(), "1".to_string()));
    }
    let request = OpenRequest {
        placement,
        workspace: ctx
            .workspace_id
            .clone()
            .or_else(|| env.workspace_id.clone()),
        target_pane: if needs_target {
            ctx.focused_pane_id.clone()
        } else {
            None
        },
        cwd,
        env: vars,
    };
    Ok(OpenOutcome::Opened(open_plugin_pane(
        cli,
        &env.plugin_id,
        &request,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::cli::FakeHerdr;
    use crate::herdr::env::{PluginContext, PluginEnv};
    use serde_json::json;

    fn env(state: &Path) -> PluginEnv {
        PluginEnv {
            in_herdr: true,
            plugin_id: "tviles.project-boards".into(),
            config_dir: state.into(),
            state_dir: state.into(),
            pane_id: None,
            workspace_id: Some("w1".into()),
            context: PluginContext {
                workspace_id: Some("w1".into()),
                focused_pane_id: Some("p1".into()),
                ..Default::default()
            },
        }
    }

    fn fake_with_cwd() -> FakeHerdr {
        let fake = FakeHerdr::default();
        fake.respond(
            &["pane", "list"],
            Ok(json!({"panes": [{"pane_id": "p1", "foreground_cwd": "/code/app"}]})),
        );
        fake.respond(
            &["plugin", "pane", "open"],
            Ok(json!({"plugin_pane": {"pane": {"pane_id": "new1"}}})),
        );
        fake.respond(&["plugin", "pane", "focus"], Ok(json!({})));
        fake
    }

    fn app_repo(_: &Path) -> Option<RepoSlug> {
        Some("tviles/app".parse().unwrap())
    }

    fn open_call(fake: &FakeHerdr) -> String {
        fake.calls()
            .into_iter()
            .find(|c| c.starts_with(&["plugin".into(), "pane".into(), "open".into()]))
            .unwrap()
            .join(" ")
    }

    #[test]
    fn opens_a_tab_with_repo_from_the_live_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_with_cwd();
        let out = open(
            &env(dir.path()),
            &fake,
            &Config::default(),
            &State::default(),
            &OpenArgs::default(),
            app_repo,
        )
        .unwrap();
        assert_eq!(out, OpenOutcome::Opened("new1".into()));
        let call = open_call(&fake);
        assert!(call.contains("--placement tab"));
        assert!(call.contains("--cwd /code/app"));
        assert!(call.contains("--env PB_REPO=tviles/app"));
        assert!(!call.contains("--target-pane"));
    }

    #[test]
    fn focuses_the_existing_pane_for_a_remembered_board() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::default();
        let board: BoardRef = "tviles/3".parse().unwrap();
        state.remember_board(&"tviles/app".parse().unwrap(), &board);
        PaneRegistry::new(dir.path())
            .register(&board, "old7")
            .unwrap();
        let fake = fake_with_cwd();
        fake.respond(&["pane", "get", "old7"], Ok(json!({})));
        let out = open(
            &env(dir.path()),
            &fake,
            &Config::default(),
            &state,
            &OpenArgs::default(),
            app_repo,
        )
        .unwrap();
        assert_eq!(out, OpenOutcome::Focused("old7".into()));
    }

    #[test]
    fn split_targets_the_focused_pane_and_picker_is_passed_on() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_with_cwd();
        let args = OpenArgs {
            project: None,
            placement: Some(Placement::Split),
            picker: true,
        };
        open(
            &env(dir.path()),
            &fake,
            &Config::default(),
            &State::default(),
            &args,
            app_repo,
        )
        .unwrap();
        let call = open_call(&fake);
        assert!(
            call.contains("--placement split --workspace w1 --target-pane p1 --direction right")
        );
        assert!(call.contains("--env PB_PICKER=1"));
    }

    #[test]
    fn split_without_a_focused_pane_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = env(dir.path());
        e.context.focused_pane_id = None;
        let args = OpenArgs {
            project: None,
            placement: Some(Placement::Zoomed),
            picker: false,
        };
        let err = open(
            &e,
            &fake_with_cwd(),
            &Config::default(),
            &State::default(),
            &args,
            |_| None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("zoomed"));
    }
}
