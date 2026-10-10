//! Every decision the board pane makes, with no terminal and no network: inputs in,
//! effects out. The runtime shell (runtime.rs) feeds it and carries out its effects.

use crate::board_choice::{AfterRepo, FirstStep, after_repo_projects, first_step};
use crate::config::Config;
use crate::github::GithubError;
use crate::herdr::cli::{HerdrCli, focus_plugin_pane};
use crate::herdr::registry::PaneRegistry;
use crate::model::*;
use crate::state::{LayoutOverride, State};
use crate::store::MemoryStore;
use crate::store::cache::{cache_path, load_cache, save_cache};
use crate::sync::scheduler::{PollConfig, PollKind, Scheduler};
use crate::sync::{IncrementalMode, SyncEvent, SyncTask};
use crate::ui::app::{App, Command, Mode, setup_message};
use crate::ui::keymap::Keymap;
use crate::ui::picker::PickerState;
use crate::ui::theme::Theme;
use crossterm::event::KeyEvent;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct PaneOptions {
    pub state_dir: PathBuf,
    /// `HERDR_PANE_ID`; `None` when run outside herdr.
    pub own_pane: Option<String>,
    pub config: Config,
    pub warnings: Vec<String>,
    pub repo: Option<RepoSlug>,
    pub board: Option<BoardRef>,
    pub picker: bool,
}

// One short-lived value per event; boxing would change the public `Input::Sync` shape.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Key(KeyEvent),
    Focus(bool),
    Tick,
    /// `generation` is the controller's generation when the job was spawned; events from an
    /// earlier board are dropped.
    Sync {
        generation: u64,
        event: SyncEvent,
    },
}

/// One unit of network work for the shell to spawn on the Syncer.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncJob {
    /// Resolve the board, then load every item.
    Resolve(BoardRef),
    /// Refresh the schema and views, then load every item.
    Refresh(Project),
    Incremental {
        project: ProjectId,
        since: String,
    },
    ViewIds {
        project: ProjectId,
        view: ViewId,
        filter: String,
        known: HashSet<ItemId>,
        hydrate: bool,
    },
    Detail {
        item: ItemId,
        before: Option<String>,
    },
    RepoProjects(RepoSlug),
    Projects(Option<RepoSlug>),
}

impl SyncJob {
    /// The job's name, for logs; `Refresh` carries the whole board schema.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Resolve(_) => "Resolve",
            Self::Refresh(_) => "Refresh",
            Self::Incremental { .. } => "Incremental",
            Self::ViewIds { .. } => "ViewIds",
            Self::Detail { .. } => "Detail",
            Self::RepoProjects(_) => "RepoProjects",
            Self::Projects(_) => "Projects",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// `generation` is the controller's generation when the effect was produced; the shell
    /// tags the job's events with it.
    Spawn {
        job: SyncJob,
        generation: u64,
    },
    OpenUrl(String),
    /// Resolve the token again and call `start` (after Retry).
    ResolveToken,
    Exit,
}

/// `fetched_at` minus two minutes, so a local clock running ahead of GitHub's never skips
/// an update. Re-fetching a few items is harmless; upserts are idempotent.
pub fn poll_since(fetched_at: &str) -> String {
    use time::format_description::well_known::Rfc3339;
    match time::OffsetDateTime::parse(fetched_at, &Rfc3339) {
        Ok(t) => (t - time::Duration::minutes(2))
            .format(&Rfc3339)
            .unwrap_or_else(|_| fetched_at.to_string()),
        Err(_) => fetched_at.to_string(),
    }
}

/// Whether `url` may be handed to `open`/`xdg-open`. Links in issue bodies and comments are
/// untrusted: only `http://` and `https://` URLs are opened, never relative paths, `file:`,
/// other schemes, or text an opener would read as an option.
pub fn is_web_link(url: &str) -> bool {
    ["https://", "http://"]
        .iter()
        .find_map(|scheme| {
            url.get(..scheme.len())
                .filter(|s| s.eq_ignore_ascii_case(scheme))
                .map(|_| &url[scheme.len()..])
        })
        .is_some_and(|rest| {
            !rest.is_empty() && !rest.chars().any(|c| c.is_whitespace() || c.is_control())
        })
}

pub const NOT_A_WEB_LINK: &str = "not opened: not a web link";
/// Shown when `r` is pressed while a load or poll is already running.
pub const REFRESHING: &str = "refreshing…";

pub struct Controller {
    pub app: App,
    options: PaneOptions,
    herdr: Arc<dyn HerdrCli>,
    mode: IncrementalMode,
    scheduler: Scheduler,
    board: Option<BoardRef>,
    full_load_in_flight: bool,
    awaiting_repo_projects: bool,
    /// Bumped whenever the shown board changes, so late events of the old board are dropped.
    generation: u64,
}

impl Controller {
    pub fn new(
        options: PaneOptions,
        herdr: Arc<dyn HerdrCli>,
        mode: IncrementalMode,
        now: Instant,
    ) -> Self {
        let (keymap, key_warnings) = Keymap::with_overrides(&options.config.keys);
        let mut app = App::new(
            Box::new(MemoryStore::new(None)),
            keymap,
            Theme::from_env(|k| std::env::var(k).ok()),
        );
        app.status.notes = options
            .warnings
            .iter()
            .cloned()
            .chain(key_warnings)
            .collect();
        let scheduler = Scheduler::new(PollConfig::from_config(&options.config), now);
        Self {
            app,
            options,
            herdr,
            mode,
            scheduler,
            board: None,
            full_load_in_flight: false,
            awaiting_repo_projects: false,
            generation: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn spawn(&self, job: SyncJob) -> Effect {
        Effect::Spawn {
            job,
            generation: self.generation,
        }
    }

    pub fn board(&self) -> Option<&BoardRef> {
        self.board.as_ref()
    }

    /// Called once the shell has tried to resolve a token, and again after Retry.
    pub fn start(&mut self, token: Result<(), GithubError>, now: Instant) -> Vec<Effect> {
        if let Err(e) = token {
            self.show_setup(&e);
            return Vec::new();
        }
        self.app.setup = None;
        if self.app.mode == Mode::Setup {
            self.app.mode = Mode::Normal;
        }
        let state = State::load(&self.options.state_dir);
        let remembered = self
            .options
            .repo
            .as_ref()
            .and_then(|r| state.remembered_board(r));
        match first_step(
            self.options.board.clone(),
            self.options.picker,
            remembered,
            self.options.repo.clone(),
        ) {
            FirstStep::Use(board) => self.open_board(board, now),
            FirstStep::FetchRepoProjects(repo) => {
                self.awaiting_repo_projects = true;
                vec![self.spawn(SyncJob::RepoProjects(repo))]
            }
            FirstStep::PickAll => self.show_picker(true),
        }
    }

    fn show_setup(&mut self, error: &GithubError) {
        self.app.setup = setup_message(error).or_else(|| Some(error.to_string()));
        self.app.mode = Mode::Setup;
    }

    fn show_picker(&mut self, required: bool) -> Vec<Effect> {
        self.app.picker = Some(PickerState::loading(required));
        self.app.mode = Mode::Picker;
        vec![self.spawn(SyncJob::Projects(self.options.repo.clone()))]
    }

    /// Shows `board` in this pane, unless another live pane already shows it.
    fn open_board(&mut self, board: BoardRef, now: Instant) -> Vec<Effect> {
        let registry = PaneRegistry::new(&self.options.state_dir);
        let own = self.options.own_pane.clone();
        if let Some(other) = registry
            .live_pane(self.herdr.as_ref(), &board)
            .filter(|p| Some(p) != own.as_ref())
        {
            match focus_plugin_pane(self.herdr.as_ref(), &other) {
                Ok(()) => {
                    return if self.board.is_none() {
                        vec![Effect::Exit]
                    } else {
                        Vec::new()
                    };
                }
                // Better a second pane on this board than none: open it here.
                Err(e) => {
                    tracing::warn!(error = %e, "could not focus the pane showing this board; opening it here")
                }
            }
        }
        self.release();
        self.generation += 1;
        self.awaiting_repo_projects = false;
        if let Some(own) = &own
            && let Err(e) = registry.register(&board, own)
        {
            tracing::warn!(error = %e, "could not record this pane in the registry");
        }
        let dir = self.options.state_dir.clone();
        let repo = self.options.repo.clone();
        let state = State::update(&dir, |s| {
            if let Some(r) = &repo {
                s.remember_board(r, &board);
            }
        })
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not save the remembered board");
            State::load(&dir)
        });

        let cached = load_cache(&cache_path(&dir, &board)).filter(|c| c.project.board == board);
        self.app.reset_for_new_board();
        self.app.store = Box::new(MemoryStore::new(cached.clone()));
        self.app.status.stale = cached.is_some();
        if let Some(view) = state.last_view(&board) {
            self.app.select_view(&view);
        }
        self.board = Some(board.clone());
        self.reconcile_layouts();
        self.scheduler = Scheduler::new(PollConfig::from_config(&self.options.config), now);
        self.scheduler.started(PollKind::Full, now);
        self.full_load_in_flight = true;
        let job = match cached {
            Some(snapshot) => SyncJob::Refresh(snapshot.project),
            None => SyncJob::Resolve(board),
        };
        let mut effects = vec![self.spawn(job)];
        let cmds: Vec<Command> = self.app.view_needs_ids().into_iter().collect();
        effects.extend(self.commands(cmds, now));
        effects
    }

    /// Unregisters this pane from the board it shows. Called on board switch and on exit.
    pub fn release(&mut self) {
        if let (Some(board), Some(own)) = (self.board.take(), &self.options.own_pane)
            && let Err(e) = PaneRegistry::new(&self.options.state_dir).unregister(&board, own)
        {
            tracing::warn!(error = %e, "could not remove this pane from the registry");
        }
    }

    fn poll(&mut self, kind: PollKind, now: Instant) -> Vec<Effect> {
        let Some(snapshot) = self.app.snapshot() else {
            // The first resolve failed and left nothing to refresh: try it again.
            let Some(board) = self.board.clone().filter(|_| !self.scheduler.in_flight()) else {
                return Vec::new();
            };
            self.full_load_in_flight = true;
            self.scheduler.started(PollKind::Full, now);
            return vec![self.spawn(SyncJob::Resolve(board))];
        };
        let project = snapshot.project.clone();
        let since = snapshot.fetched_at.clone();
        let job = match (kind, since) {
            (PollKind::Incremental, Some(since)) if self.mode != IncrementalMode::Unsupported => {
                SyncJob::Incremental {
                    project: project.id.clone(),
                    since: poll_since(&since),
                }
            }
            _ => {
                self.full_load_in_flight = true;
                SyncJob::Refresh(project)
            }
        };
        let started = if matches!(job, SyncJob::Refresh(_)) {
            PollKind::Full
        } else {
            PollKind::Incremental
        };
        self.scheduler.started(started, now);
        vec![self.spawn(job)]
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Vec<Effect> {
        match input {
            Input::Key(key) => {
                let mut effects = self
                    .scheduler
                    .on_input(now)
                    .map(|k| self.poll(k, now))
                    .unwrap_or_default();
                let cmds = self.app.handle_key(key);
                effects.extend(self.commands(cmds, now));
                effects
            }
            Input::Focus(focused) => self
                .scheduler
                .on_focus(focused, now)
                .map(|k| self.poll(k, now))
                .unwrap_or_default(),
            Input::Tick => self
                .scheduler
                .tick(now)
                .map(|k| self.poll(k, now))
                .unwrap_or_default(),
            Input::Sync { generation, event } => {
                if generation != self.generation {
                    return Vec::new();
                }
                self.on_sync(event, now)
            }
        }
    }

    fn on_sync(&mut self, event: SyncEvent, now: Instant) -> Vec<Effect> {
        match &event {
            SyncEvent::Rate(rate) => self.scheduler.set_low_budget(rate.is_low()),
            SyncEvent::Failed {
                error: GithubError::RateLimited { retry_after_secs },
                ..
            } => {
                self.scheduler
                    .pause_until(now + Duration::from_secs(*retry_after_secs));
            }
            _ => {}
        }
        if matches!(
            &event,
            SyncEvent::ItemsComplete { .. }
                | SyncEvent::Failed {
                    task: SyncTask::FullLoad | SyncTask::Resolve,
                    ..
                }
        ) {
            self.full_load_in_flight = false;
        }
        if matches!(
            &event,
            SyncEvent::ItemsComplete { .. }
                | SyncEvent::ItemsUpdated { .. }
                | SyncEvent::Failed {
                    task: SyncTask::FullLoad | SyncTask::Incremental | SyncTask::Resolve,
                    ..
                }
        ) {
            self.scheduler.finished();
        }
        let persist = match &event {
            SyncEvent::ItemsComplete { .. } | SyncEvent::Hydrated(_) => true,
            SyncEvent::ItemsUpdated { items, .. } => !items.is_empty(),
            _ => false,
        };

        if self.awaiting_repo_projects {
            match &event {
                SyncEvent::RepoProjects(list) => {
                    self.awaiting_repo_projects = false;
                    return match after_repo_projects(list.clone()) {
                        AfterRepo::Use(board) => self.open_board(board, now),
                        AfterRepo::Pick(list) => {
                            self.app.picker = Some(PickerState::with(list, true));
                            self.app.mode = Mode::Picker;
                            Vec::new()
                        }
                        AfterRepo::PickAll => self.show_picker(true),
                    };
                }
                SyncEvent::Failed {
                    task: SyncTask::Projects,
                    error,
                } if setup_message(error).is_none() => {
                    self.awaiting_repo_projects = false;
                    return self.show_picker(true);
                }
                _ => {}
            }
        }

        // A board list nobody waits for any more (its picker was cancelled). A picker is
        // required only while no board is chosen; reopening one now could trap the user in it.
        if matches!(event, SyncEvent::Projects(_))
            && self.app.picker.is_none()
            && self.board.is_some()
        {
            return Vec::new();
        }
        let projects_failed = matches!(
            &event,
            SyncEvent::Failed {
                task: SyncTask::Projects,
                ..
            }
        );
        let project_changed = matches!(event, SyncEvent::Project(_));
        let cmds = self.app.on_sync(event);
        if project_changed {
            self.reconcile_layouts();
        }
        if projects_failed {
            // The App records the error in the status line; do not leave "Loading boards…" up.
            if let Some(picker) = self.app.picker.as_mut() {
                picker.loading = false;
            }
        }
        if persist {
            self.persist_cache();
        }
        self.commands(cmds, now)
    }

    /// Gives the app this board's saved layout choices, dropping (and forgetting) any whose
    /// view has since changed layout on GitHub. Run whenever the views may have changed;
    /// running it twice changes nothing more. Choices for views not in the project yet are
    /// kept unchecked.
    fn reconcile_layouts(&mut self) {
        let Some(board) = self.board.clone() else {
            return;
        };
        let saved = State::load(&self.options.state_dir).layout_overrides_for(&board);
        let mut keep = std::collections::HashMap::new();
        let mut stale: Vec<(ViewId, String)> = Vec::new();
        for (id, o) in saved {
            let view = self
                .app
                .project()
                .and_then(|p| p.views.iter().find(|v| v.id == id));
            match view {
                Some(v) if v.layout != o.github_layout => stale.push((id, v.name.clone())),
                _ => {
                    keep.insert(id, o.layout);
                }
            }
        }
        self.app.set_layout_overrides(keep);
        if stale.is_empty() {
            return;
        }
        if let Err(e) = State::update(&self.options.state_dir, |s| {
            for (id, _) in &stale {
                s.clear_layout_override(&board, id);
            }
        }) {
            tracing::warn!(error = %e, "could not clear stale layout choices");
        }
        self.app.status.flash = Some(format!(
            "View '{}' layout changed on GitHub; local layout cleared",
            stale[0].1
        ));
    }

    fn persist_cache(&self) {
        if let (Some(snapshot), Some(board)) = (self.app.snapshot(), &self.board)
            && let Err(e) = save_cache(&cache_path(&self.options.state_dir, board), snapshot)
        {
            tracing::warn!(error = %e, "could not save the board cache");
        }
    }

    fn commands(&mut self, cmds: Vec<Command>, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        for cmd in cmds {
            match cmd {
                Command::FetchViewIds { view, filter } => {
                    if let Some(s) = self.app.snapshot() {
                        effects.push(self.spawn(SyncJob::ViewIds {
                            project: s.project.id.clone(),
                            view,
                            filter,
                            known: s.items.keys().cloned().collect(),
                            hydrate: !self.full_load_in_flight,
                        }));
                    }
                }
                Command::LoadDetail { item, before } => {
                    effects.push(self.spawn(SyncJob::Detail { item, before }))
                }
                Command::OpenUrl(url) if is_web_link(&url) => effects.push(Effect::OpenUrl(url)),
                Command::OpenUrl(_) => self.app.status.flash = Some(NOT_A_WEB_LINK.into()),
                Command::Refresh => {
                    if self.scheduler.in_flight() {
                        self.app.status.flash = Some(REFRESHING.into());
                    } else {
                        effects.extend(self.poll(PollKind::Full, now));
                    }
                }
                Command::SaveLastView(view) => {
                    if let Some(board) = self.board.clone()
                        && let Err(e) = State::update(&self.options.state_dir, |s| {
                            s.set_last_view(&board, &view)
                        })
                    {
                        tracing::warn!(error = %e, "could not save the last view");
                    }
                }
                Command::SaveLayout {
                    view,
                    layout,
                    github_layout,
                } => {
                    if let Some(board) = self.board.clone()
                        && let Err(e) = State::update(&self.options.state_dir, |s| {
                            s.set_layout_override(
                                &board,
                                &view,
                                LayoutOverride {
                                    layout,
                                    github_layout,
                                },
                            )
                        })
                    {
                        tracing::warn!(error = %e, "could not save the layout");
                    }
                }
                Command::ClearLayout { view } => {
                    if let Some(board) = self.board.clone()
                        && let Err(e) = State::update(&self.options.state_dir, |s| {
                            s.clear_layout_override(&board, &view)
                        })
                    {
                        tracing::warn!(error = %e, "could not clear the layout");
                    }
                }
                Command::PickBoard(board) => effects.extend(self.open_board(board, now)),
                Command::ShowPicker => {
                    let required = self.board.is_none();
                    effects.extend(self.show_picker(required));
                }
                Command::Retry => effects.push(Effect::ResolveToken),
                Command::Quit => effects.push(Effect::Exit),
            }
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::cli::FakeHerdr;
    use crate::store::cache::{cache_path, save_cache};
    use crate::ui::fixtures::{code, key, snapshot};
    use serde_json::json;
    use std::path::Path;

    fn options(dir: &Path, board: Option<&str>) -> PaneOptions {
        PaneOptions {
            state_dir: dir.to_path_buf(),
            own_pane: Some("me".into()),
            config: Config::default(),
            warnings: vec![],
            repo: Some("tviles/app".parse().unwrap()),
            board: board.map(|b| b.parse().unwrap()),
            picker: false,
        }
    }

    fn controller(dir: &Path, board: Option<&str>) -> (Controller, Arc<FakeHerdr>, Instant) {
        let fake = Arc::new(FakeHerdr::default());
        let t0 = Instant::now();
        (
            Controller::new(
                options(dir, board),
                fake.clone(),
                IncrementalMode::DateTime,
                t0,
            ),
            fake,
            t0,
        )
    }

    fn board() -> BoardRef {
        "tviles/3".parse().unwrap()
    }

    fn summary(b: &str) -> ProjectSummary {
        ProjectSummary {
            id: ProjectId::new(b),
            board: b.parse().unwrap(),
            title: b.into(),
            closed: false,
        }
    }

    fn complete(fetched_at: &str) -> SyncEvent {
        SyncEvent::ItemsComplete {
            items: crate::ui::fixtures::items(),
            fetched_at: fetched_at.into(),
            total: 5,
            truncated: false,
        }
    }

    #[test]
    fn token_errors_show_the_setup_screen() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        assert!(c.start(Err(GithubError::NoToken), t0).is_empty());
        assert_eq!(c.app.mode, Mode::Setup);
        assert_eq!(
            c.handle(Input::Key(key('r')), t0),
            vec![Effect::ResolveToken]
        );
    }

    #[test]
    fn a_board_without_cache_is_resolved_registered_and_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        assert_eq!(c.start(Ok(()), t0), vec![sp(&c, SyncJob::Resolve(board()))]);
        assert_eq!(
            PaneRegistry::new(dir.path()).lookup(&board()).as_deref(),
            Some("me")
        );
        assert_eq!(
            State::load(dir.path()).remembered_board(&"tviles/app".parse().unwrap()),
            Some(board())
        );
    }

    #[test]
    fn a_cached_board_paints_at_once_and_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        let effects = c.start(Ok(()), t0);
        assert!(
            matches!(&effects[0], Effect::Spawn { job: SyncJob::Refresh(p), .. } if p.board == board())
        );
        assert!(c.app.snapshot().is_some() && c.app.status.stale);
    }

    #[test]
    fn a_board_open_elsewhere_is_focused_and_this_pane_exits() {
        let dir = tempfile::tempdir().unwrap();
        PaneRegistry::new(dir.path())
            .register(&board(), "other")
            .unwrap();
        let (mut c, fake, t0) = controller(dir.path(), Some("tviles/3"));
        fake.respond(&["pane", "get", "other"], Ok(json!({})));
        fake.respond(&["plugin", "pane", "focus", "other"], Ok(json!({})));
        assert_eq!(c.start(Ok(()), t0), vec![Effect::Exit]);
        assert!(
            fake.calls()
                .iter()
                .any(|call| call.join(" ") == "plugin pane focus other")
        );
    }

    #[test]
    fn repo_projects_decide_the_board() {
        let dir = tempfile::tempdir().unwrap();
        let repo: RepoSlug = "tviles/app".parse().unwrap();

        let (mut one, _, t0) = controller(dir.path(), None);
        assert_eq!(
            one.start(Ok(()), t0),
            vec![sp(&one, SyncJob::RepoProjects(repo.clone()))]
        );
        let effects = one.handle(
            Input::Sync {
                generation: one.generation(),
                event: SyncEvent::RepoProjects(vec![summary("tviles/3")]),
            },
            t0,
        );
        assert_eq!(effects, vec![sp(&one, SyncJob::Resolve(board()))]);

        let dir2 = tempfile::tempdir().unwrap();
        let (mut two, _, t0) = controller(dir2.path(), None);
        two.start(Ok(()), t0);
        two.handle(
            Input::Sync {
                generation: two.generation(),
                event: SyncEvent::RepoProjects(vec![summary("tviles/3"), summary("tviles/4")]),
            },
            t0,
        );
        assert_eq!(two.app.mode, Mode::Picker);
        assert_eq!(two.app.picker.as_ref().unwrap().candidates.len(), 2);

        let dir3 = tempfile::tempdir().unwrap();
        let (mut failed, _, t0) = controller(dir3.path(), None);
        failed.start(Ok(()), t0);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: GithubError::Network("down".into()),
        };
        assert_eq!(
            failed.handle(
                Input::Sync {
                    generation: failed.generation(),
                    event: failure
                },
                t0
            ),
            vec![sp(&failed, SyncJob::Projects(Some(repo)))]
        );
        assert_eq!(failed.app.mode, Mode::Picker);
    }

    fn set_override(dir: &Path, board: &BoardRef, view: &str, layout: Layout, github: Layout) {
        State::update(dir, |s| {
            s.set_layout_override(
                board,
                &ViewId::new(view),
                LayoutOverride {
                    layout,
                    github_layout: github,
                },
            )
        })
        .unwrap();
    }

    #[test]
    fn opening_a_board_applies_its_saved_layouts_per_view_and_per_board() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        set_override(
            dir.path(),
            &board(),
            "V_table",
            Layout::Board,
            Layout::Table,
        );
        set_override(
            dir.path(),
            &"tviles/9".parse().unwrap(),
            "V_bugs",
            Layout::Board,
            Layout::Table,
        );
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert_eq!(c.app.layout().0, Layout::Board, "V_table is overridden");
        c.app.select_view(&ViewId::new("V_bugs"));
        assert_eq!(
            c.app.layout().0,
            Layout::Table,
            "another board's choice and unset views stay"
        );
        assert!(c.app.status.flash.is_none());
    }

    #[test]
    fn pressing_l_persists_the_layout_and_pressing_it_again_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        c.handle(Input::Key(key('L')), t0);
        let saved = State::load(dir.path()).layout_overrides_for(&board());
        assert_eq!(
            saved,
            vec![(
                ViewId::new("V_table"),
                LayoutOverride {
                    layout: Layout::Board,
                    github_layout: Layout::Table
                }
            )]
        );
        c.handle(Input::Key(key('L')), t0);
        assert!(State::load(dir.path()).layout_overrides.is_empty());
    }

    #[test]
    fn a_layout_changed_on_github_drops_the_saved_choice_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        // V_table is a table on GitHub; the choice was made when it was a board.
        set_override(
            dir.path(),
            &board(),
            "V_table",
            Layout::Table,
            Layout::Board,
        );
        set_override(
            dir.path(),
            &board(),
            "V_board",
            Layout::Table,
            Layout::Board,
        );
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert_eq!(
            c.app.status.flash.as_deref(),
            Some("View 'Table' layout changed on GitHub; local layout cleared")
        );
        let left = State::load(dir.path()).layout_overrides_for(&board());
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, ViewId::new("V_board"));
        assert_eq!(c.app.layout().0, Layout::Table);

        // The fresh project arriving re-checks, and changes nothing more.
        c.app.status.flash = None;
        let project = c.app.project().unwrap().clone();
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: SyncEvent::Project(project),
            },
            t0,
        );
        assert!(c.app.status.flash.is_none());
        assert_eq!(
            State::load(dir.path()).layout_overrides_for(&board()).len(),
            1
        );
    }

    #[test]
    fn a_choice_made_before_the_views_are_known_is_checked_when_they_arrive() {
        let dir = tempfile::tempdir().unwrap();
        set_override(
            dir.path(),
            &board(),
            "V_table",
            Layout::Board,
            Layout::Board,
        );
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert_eq!(
            State::load(dir.path()).layout_overrides.len(),
            1,
            "nothing to compare yet"
        );
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: SyncEvent::Project(snapshot().project),
            },
            t0,
        );
        assert!(State::load(dir.path()).layout_overrides.is_empty());
        assert!(c.app.status.flash.is_some());
    }

    /// Plan review Q6: no hydration while the full load is still fetching the same items.
    #[test]
    fn view_ids_hydrate_only_after_the_full_load() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        State::update(dir.path(), |s| {
            s.set_last_view(&board(), &ViewId::new("V_bugs"))
        })
        .unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        let effects = c.start(Ok(()), t0);
        assert!(effects.iter().any(|e| matches!(e, Effect::Spawn { job: SyncJob::ViewIds { hydrate: false, filter, .. }, .. } if filter == "label:bug")));
        let effects = c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::Spawn {
                    job: SyncJob::ViewIds { hydrate: true, .. },
                    ..
                }
            )),
            "revalidated after the load, now hydrating"
        );
        assert!(cache_path(dir.path(), &board()).exists());
    }

    #[test]
    fn polls_incrementally_with_a_two_minute_overlap() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        let effects = c.handle(Input::Tick, t0 + Duration::from_secs(31));
        let expected = SyncJob::Incremental {
            project: ProjectId::new("PVT_1"),
            since: "2026-10-01T11:58:00Z".into(),
        };
        assert_eq!(effects, vec![sp(&c, expected)]);
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(62))
                .is_empty(),
            "one poll in flight at a time"
        );
    }

    #[test]
    fn rate_limits_pause_polling() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        let limited = SyncEvent::Failed {
            task: SyncTask::Incremental,
            error: GithubError::RateLimited {
                retry_after_secs: 120,
            },
        };
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: limited,
            },
            t0 + Duration::from_secs(1),
        );
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(60))
                .is_empty()
        );
        assert_eq!(
            c.handle(Input::Tick, t0 + Duration::from_secs(122)).len(),
            1
        );
    }

    #[test]
    fn unsupported_incremental_mode_polls_in_full() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let fake = Arc::new(FakeHerdr::default());
        let t0 = Instant::now();
        let mut c = Controller::new(
            options(dir.path(), Some("tviles/3")),
            fake,
            IncrementalMode::Unsupported,
            t0,
        );
        c.start(Ok(()), t0);
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        assert!(matches!(
            c.handle(Input::Tick, t0 + Duration::from_secs(31))
                .as_slice(),
            [Effect::Spawn {
                job: SyncJob::Refresh(_),
                ..
            }]
        ));
    }

    #[test]
    fn refresh_key_starts_a_full_poll_unless_one_is_running() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert!(
            c.handle(Input::Key(key('r')), t0).is_empty(),
            "initial load still running"
        );
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        assert!(matches!(
            c.handle(Input::Key(key('r')), t0).as_slice(),
            [Effect::Spawn {
                job: SyncJob::Refresh(_),
                ..
            }]
        ));
    }

    #[test]
    fn a_cancelled_picker_stays_closed_while_the_board_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert!(
            c.app.snapshot().is_none(),
            "no cache: the board is still resolving"
        );
        c.handle(Input::Key(key('B')), t0);
        assert_eq!(c.app.mode, Mode::Picker);
        c.handle(Input::Key(code(crossterm::event::KeyCode::Esc)), t0);
        assert_eq!(c.app.mode, Mode::Normal);
        c.handle(at(&c, SyncEvent::Projects(vec![summary("tviles/4")])), t0);
        assert_eq!(c.app.mode, Mode::Normal);
        assert!(c.app.picker.is_none());
    }

    #[test]
    fn switching_boards_clears_the_old_boards_search_error_and_load_note() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let mut opts = options(dir.path(), Some("tviles/3"));
        opts.warnings = vec!["config: unknown key".into()];
        let t0 = Instant::now();
        let mut c = Controller::new(
            opts,
            Arc::new(FakeHerdr::default()),
            IncrementalMode::DateTime,
            t0,
        );
        c.start(Ok(()), t0);
        for k in ['/', 'b', 'u', 'g'] {
            c.handle(Input::Key(key(k)), t0);
        }
        c.handle(Input::Key(code(crossterm::event::KeyCode::Enter)), t0);
        assert_eq!(c.app.search_query(), "bug");
        c.handle(
            at(
                &c,
                SyncEvent::ItemsComplete {
                    items: crate::ui::fixtures::items(),
                    fetched_at: "2026-10-01T12:00:00Z".into(),
                    total: 9,
                    truncated: true,
                },
            ),
            t0,
        );
        c.handle(at(&c, fail(SyncTask::Incremental)), t0);
        assert!(c.app.status.error.is_some());
        assert!(c.app.status.notes.iter().any(|n| n.starts_with("loaded ")));

        c.commands(vec![Command::PickBoard("tviles/4".parse().unwrap())], t0);
        assert_eq!(
            c.board().map(|b| b.to_string()).as_deref(),
            Some("tviles/4")
        );
        assert_eq!(c.app.search_query(), "");
        assert_eq!(c.app.status.error, None);
        assert_eq!(c.app.status.notes, ["config: unknown key"]);
    }

    #[test]
    fn refresh_during_a_running_poll_says_it_is_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert!(c.handle(Input::Key(key('r')), t0).is_empty());
        assert_eq!(c.app.status.flash.as_deref(), Some(REFRESHING));
        c.handle(at(&c, complete("2026-10-01T12:00:00Z")), t0);
        assert!(!c.handle(Input::Key(key('r')), t0).is_empty());
        assert_eq!(
            c.app.status.flash, None,
            "a refresh that starts needs no note"
        );
    }

    #[test]
    fn job_kinds_name_the_job_without_its_payload() {
        let job = SyncJob::Refresh(snapshot().project);
        assert_eq!(job.kind(), "Refresh");
        assert_eq!(SyncJob::Resolve(board()).kind(), "Resolve");
    }

    #[test]
    fn only_http_and_https_urls_are_web_links() {
        for url in [
            "https://github.com/x",
            "http://example.com/a?b=c#d",
            "HTTPS://GITHUB.COM/x",
        ] {
            assert!(is_web_link(url), "{url}");
        }
        for url in [
            "docs/setup.md",
            "../etc/passwd",
            "file:///etc/passwd",
            "-a Calculator",
            "javascript:alert(1)",
            "mailto:a@b.c",
            "https://",
            " https://github.com/x",
            "https://github.com/x -a Calculator",
            "",
        ] {
            assert!(!is_web_link(url), "{url:?}");
        }
    }

    #[test]
    fn links_that_are_not_web_links_are_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        for url in [
            "file:///etc/passwd",
            "-a Calculator",
            "javascript:x",
            "notes.md",
        ] {
            assert!(
                c.commands(vec![Command::OpenUrl(url.into())], t0)
                    .is_empty(),
                "{url}"
            );
            assert_eq!(c.app.status.flash.as_deref(), Some(NOT_A_WEB_LINK));
        }
        assert_eq!(
            c.commands(vec![Command::OpenUrl("https://github.com/x".into())], t0),
            vec![Effect::OpenUrl("https://github.com/x".into())]
        );
        c.handle(Input::Key(key('j')), t0);
        assert_eq!(c.app.status.flash, None, "the next key clears it");
    }

    #[test]
    fn quit_exits_and_release_unregisters_the_pane() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        assert_eq!(c.handle(Input::Key(key('q')), t0), vec![Effect::Exit]);
        c.release();
        assert_eq!(PaneRegistry::new(dir.path()).lookup(&board()), None);
    }

    #[test]
    fn a_failed_board_list_ends_the_loading_picker() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = options(dir.path(), None);
        opts.picker = true;
        let fake = Arc::new(FakeHerdr::default());
        let t0 = Instant::now();
        let mut c = Controller::new(opts, fake, IncrementalMode::DateTime, t0);
        assert_eq!(
            c.start(Ok(()), t0),
            vec![sp(
                &c,
                SyncJob::Projects(Some("tviles/app".parse().unwrap()))
            )]
        );
        assert!(c.app.picker.as_ref().unwrap().loading);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: GithubError::Network("down".into()),
        };
        assert!(
            c.handle(
                Input::Sync {
                    generation: c.generation(),
                    event: failure
                },
                t0
            )
            .is_empty()
        );
        assert!(!c.app.picker.as_ref().unwrap().loading);
        assert!(
            c.app
                .status
                .error
                .as_deref()
                .is_some_and(|e| e.contains("could not list boards"))
        );
    }

    fn sp(c: &Controller, job: SyncJob) -> Effect {
        Effect::Spawn {
            job,
            generation: c.generation(),
        }
    }

    fn at(c: &Controller, event: SyncEvent) -> Input {
        Input::Sync {
            generation: c.generation(),
            event,
        }
    }

    fn fail(task: SyncTask) -> SyncEvent {
        SyncEvent::Failed {
            task,
            error: GithubError::Network("down".into()),
        }
    }

    #[test]
    fn a_failed_refresh_ends_the_load_and_polling_resumes() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let e = at(&c, fail(SyncTask::Resolve));
        c.handle(e, t0 + Duration::from_secs(1));
        let effects = c.handle(Input::Tick, t0 + Duration::from_secs(31));
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::Spawn {
                job: SyncJob::Refresh(_),
                ..
            }
        ));
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(32))
                .is_empty()
        );
    }

    #[test]
    fn a_failed_first_resolve_is_retried_after_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let e = at(&c, fail(SyncTask::Resolve));
        c.handle(e, t0 + Duration::from_secs(1));
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(2))
                .is_empty()
        );
        assert_eq!(
            c.handle(Input::Tick, t0 + Duration::from_secs(31)),
            vec![sp(&c, SyncJob::Resolve(board()))]
        );
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(32))
                .is_empty()
        );
    }

    #[test]
    fn events_of_the_previous_board_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let old = c.generation();
        let other: BoardRef = "tviles/4".parse().unwrap();
        let effects = c.commands(vec![Command::PickBoard(other.clone())], t0);
        assert_eq!(effects, vec![sp(&c, SyncJob::Resolve(other.clone()))]);
        assert!(c.generation() > old);
        let late = Input::Sync {
            generation: old,
            event: complete("2026-10-01T12:00:00Z"),
        };
        assert!(c.handle(late, t0).is_empty());
        assert!(c.app.snapshot().is_none(), "board B's store untouched");
        assert!(!cache_path(dir.path(), &other).exists());
        assert!(
            c.handle(Input::Key(key('r')), t0).is_empty(),
            "B's load is still in flight"
        );
        // A current-generation event applies.
        let e = at(&c, SyncEvent::Project(crate::ui::fixtures::project()));
        c.handle(e, t0);
        let e = at(&c, complete("2026-10-01T12:00:00Z"));
        c.handle(e, t0);
        assert!(c.app.snapshot().is_some());
        assert!(cache_path(dir.path(), &other).exists());
    }

    #[test]
    fn failing_to_focus_the_other_pane_opens_the_board_here() {
        let dir = tempfile::tempdir().unwrap();
        PaneRegistry::new(dir.path())
            .register(&board(), "other")
            .unwrap();
        let (mut c, fake, t0) = controller(dir.path(), Some("tviles/3"));
        fake.respond(&["pane", "get", "other"], Ok(json!({})));
        fake.respond(
            &["plugin", "pane", "focus", "other"],
            FakeHerdr::not_found(),
        );
        assert_eq!(c.start(Ok(()), t0), vec![sp(&c, SyncJob::Resolve(board()))]);
    }

    #[test]
    fn empty_updates_are_not_persisted_but_hydration_is() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let e = at(&c, SyncEvent::Project(crate::ui::fixtures::project()));
        c.handle(e, t0);
        let path = cache_path(dir.path(), &board());
        let e = at(
            &c,
            SyncEvent::ItemsUpdated {
                items: vec![],
                fetched_at: "2026-10-01T12:00:00Z".into(),
            },
        );
        c.handle(e, t0);
        assert!(!path.exists());
        let e = at(&c, SyncEvent::Hydrated(crate::ui::fixtures::items()));
        c.handle(e, t0);
        assert!(path.exists());
    }

    #[test]
    fn focus_regained_after_idle_polls() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let e = at(&c, complete("2026-10-01T12:00:00Z"));
        c.handle(e, t0);
        assert!(c.handle(Input::Focus(false), t0).is_empty());
        let effects = c.handle(Input::Focus(true), t0 + Duration::from_secs(1));
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            Effect::Spawn {
                job: SyncJob::Incremental { .. },
                ..
            }
        ));
    }

    #[test]
    fn an_auth_failure_on_the_repo_board_list_shows_setup() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        c.start(Ok(()), t0);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: GithubError::Unauthorized,
        };
        let e = at(&c, failure);
        assert!(c.handle(e, t0).is_empty());
        assert_eq!(c.app.mode, Mode::Setup);
    }

    #[test]
    fn effects_are_tagged_when_produced_not_when_executed() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(()), t0);
        let e = at(&c, complete("2026-10-01T12:00:00Z"));
        c.handle(e, t0);
        let old = c.generation();
        c.app.picker = Some(PickerState::with(vec![summary("tviles/4")], false));
        c.app.mode = Mode::Picker;
        // One Enter, after idling: the idle poll starts on the old board, then the pick switches.
        let enter = Input::Key(crate::ui::fixtures::code(crossterm::event::KeyCode::Enter));
        let effects = c.handle(enter, t0 + Duration::from_secs(400));
        let other: BoardRef = "tviles/4".parse().unwrap();
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert!(
            matches!(&effects[0], Effect::Spawn { job: SyncJob::Incremental { .. }, generation } if *generation == old)
        );
        assert_eq!(
            effects[1],
            Effect::Spawn {
                job: SyncJob::Resolve(other),
                generation: old + 1
            }
        );
    }

    #[test]
    fn opening_a_board_forgets_a_pending_repo_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        c.start(Ok(()), t0);
        assert!(c.awaiting_repo_projects);
        c.commands(vec![Command::PickBoard(board())], t0);
        assert!(!c.awaiting_repo_projects);
        let e = at(&c, fail(SyncTask::Projects));
        c.handle(e, t0);
        assert_ne!(
            c.app.mode,
            Mode::Picker,
            "no required picker reopened over the board"
        );
    }

    #[test]
    fn poll_since_subtracts_two_minutes() {
        assert_eq!(poll_since("2026-10-01T12:00:00Z"), "2026-10-01T11:58:00Z");
        assert_eq!(poll_since("not a time"), "not a time");
    }
}
