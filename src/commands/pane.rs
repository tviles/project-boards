//! `project-boards pane`: the board TUI that the manifest's pane entrypoint runs.

use crate::config::load_config;
use crate::herdr::env::PluginEnv;
use crate::herdr::repo::detect_repo;
use crate::ui::controller::PaneOptions;
use crate::ui::runtime::run;

pub fn run_pane() -> anyhow::Result<()> {
    let env = PluginEnv::from_system();
    let _log = crate::logging::init(&env.state_dir);
    let (config, warnings) = load_config(&env.config_dir);
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let repo = var("PB_REPO")
        .and_then(|r| r.parse().ok())
        .or_else(|| std::env::current_dir().ok().and_then(|d| detect_repo(&d)));
    let board = var("PB_BOARD").and_then(|b| b.parse().ok());
    let picker = var("PB_PICKER").as_deref() == Some("1");
    tracing::info!(?repo, ?board, picker, "pane starting");
    let options = PaneOptions {
        state_dir: env.state_dir,
        own_pane: env.pane_id,
        config,
        warnings,
        repo,
        board,
        picker,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run(options))
}
