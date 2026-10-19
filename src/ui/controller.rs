//! Every decision the board pane makes, with no terminal and no network: inputs in,
//! effects out. The runtime shell (runtime.rs) feeds it and carries out its effects.

use crate::board_choice::{AfterRepo, FirstStep, after_repo_projects, first_step};
use crate::config::Config;
use crate::error::BackendError;
use crate::herdr::cli::{HerdrCli, focus_plugin_pane};
use crate::herdr::registry::PaneRegistry;
use crate::model::*;
use crate::ops::queue::OpQueue;
use crate::state::{LayoutOverride, State};
use crate::store::MemoryStore;
use crate::store::cache::{cache_path, load_cache, save_cache};
use crate::sync::scheduler::{PollConfig, PollKind, Scheduler};
use crate::sync::{IncrementalMode, SyncEvent, SyncTask};
use crate::ui::app::{App, Command, EditPurpose, Mode, setup_message, setup_message_for_all};
use crate::ui::editor::Edited;
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
    pub board: Option<BoardKey>,
    pub picker: bool,
    /// The local UTC offset, read once before any thread starts (the `time` crate refuses
    /// to read it afterwards); "today" in date input and iterations is the date at this offset.
    pub utc_offset: time::UtcOffset,
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
    /// `$EDITOR` closed (`Effect::Editor`).
    Edited {
        purpose: EditPurpose,
        edited: Edited,
    },
}

/// One unit of network work for the shell to spawn on the Syncer.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncJob {
    /// Resolve the board, then load every item.
    Resolve(BoardKey),
    /// Refresh the schema and views, then load every item.
    Refresh(Project),
    Incremental {
        project: ProjectId,
        since: String,
    },
    ViewIds {
        project: ProjectId,
        view: ViewId,
        query: ListQuery,
        known: HashSet<ItemId>,
        hydrate: bool,
    },
    Detail {
        item: ItemId,
        before: Option<String>,
    },
    RepoProjects(RepoSlug),
    Projects(Option<RepoSlug>),
    /// Lookups for the editors; they never touch the poll scheduler.
    Candidates(RepoSlug),
    Repos,
    LookupRepo(RepoSlug),
    Search {
        query: String,
        scope: Vec<RepoSlug>,
    },
    /// Send one pending edit.
    Mutate {
        id: crate::ops::OpId,
        project: ProjectId,
        op: crate::ops::Op,
    },
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
            Self::Candidates(_) => "Candidates",
            Self::Repos => "Repos",
            Self::LookupRepo(_) => "LookupRepo",
            Self::Search { .. } => "Search",
            Self::Mutate { .. } => "Mutate",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// `generation` is the controller's generation when the effect was produced; the shell
    /// tags the job's events with it. `provider` is the shown board's, whose backend runs
    /// the job; `None` when no board is open.
    Spawn {
        job: SyncJob,
        generation: u64,
        provider: Option<Provider>,
    },
    OpenUrl(String),
    /// Resolve the token again and call `start` (after Retry).
    ResolveToken,
    /// Run `$EDITOR` on `initial` with the TUI suspended, then answer with `Input::Edited`.
    /// Only ever produced by a key press (Decision 27).
    Editor {
        purpose: EditPurpose,
        initial: String,
    },
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
    /// A fixed incremental mode (tests); `None` follows the board's provider (Decision 18).
    incremental: Option<IncrementalMode>,
    scheduler: Scheduler,
    board: Option<BoardKey>,
    full_load_in_flight: bool,
    awaiting_repo_projects: bool,
    /// Bumped whenever the shown board changes, so late events of the old board are dropped.
    generation: u64,
    /// When each pending edit goes out.
    queue: OpQueue,
    /// Edit results not yet in the disk cache; saved at most once per tick and on exit.
    cache_dirty: bool,
    /// The opened board is remembered for the repository once it resolves (Decision 9).
    remember_on_resolve: bool,
}

impl Controller {
    pub fn new(
        options: PaneOptions,
        herdr: Arc<dyn HerdrCli>,
        incremental: Option<IncrementalMode>,
        now: Instant,
    ) -> Self {
        let (keymap, key_warnings) = Keymap::with_overrides(&options.config.keys);
        let mut app = App::new(
            Box::new(MemoryStore::new(None)),
            keymap,
            Theme::from_env(|k| std::env::var(k).ok()),
        );
        app.utc_offset = options.utc_offset;
        app.pane_repo = options.repo.clone();
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
            incremental,
            scheduler,
            board: None,
            full_load_in_flight: false,
            awaiting_repo_projects: false,
            generation: 0,
            queue: OpQueue::new(),
            cache_dirty: false,
            remember_on_resolve: false,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn spawn(&self, job: SyncJob) -> Effect {
        Effect::Spawn {
            job,
            generation: self.generation,
            provider: self.board.as_ref().map(BoardKey::provider),
        }
    }

    /// How the shown board is polled: the fixed mode, else its provider's.
    fn incremental_mode(&self) -> IncrementalMode {
        self.incremental.unwrap_or_else(|| {
            let provider = self
                .board
                .as_ref()
                .map_or(Provider::Github, BoardKey::provider);
            crate::backend::profile_for(provider).incremental
        })
    }

    pub fn board(&self) -> Option<&BoardKey> {
        self.board.as_ref()
    }

    /// Called once the shell has tried to resolve each provider's credentials, and again after
    /// Retry: `Ok` with the providers that have them, `Err` with every provider's error when
    /// none has.
    pub fn start(
        &mut self,
        providers: Result<Vec<Provider>, Vec<BackendError>>,
        now: Instant,
    ) -> Vec<Effect> {
        let providers = match providers {
            Ok(providers) => providers,
            Err(errors) => {
                self.app.setup = Some(setup_message_for_all(&errors));
                self.app.mode = Mode::Setup;
                return Vec::new();
            }
        };
        self.app.setup = None;
        if self.app.mode == Mode::Setup {
            self.app.mode = Mode::Normal;
        }
        // A retry after the token failed mid-session: the board is open and loaded, so keep
        // its store, pending edits, queue and generation, and only refresh it with the new
        // client. Reopening it would drop the pending edits.
        if self.board.is_some() && self.app.snapshot().is_some() {
            return self.poll(PollKind::Full, now);
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
            &providers,
        ) {
            FirstStep::Use(board) => self.open_board(board, now),
            FirstStep::FetchRepoProjects(repo) => {
                self.awaiting_repo_projects = true;
                vec![self.spawn(SyncJob::RepoProjects(repo))]
            }
            FirstStep::PickAll => self.show_picker(true),
        }
    }

    fn show_picker(&mut self, required: bool) -> Vec<Effect> {
        self.app.picker = Some(PickerState::loading(required));
        self.app.mode = Mode::Picker;
        vec![self.spawn(SyncJob::Projects(self.options.repo.clone()))]
    }

    /// Shows `board` in this pane, unless another live pane already shows it.
    fn open_board(&mut self, board: BoardKey, now: Instant) -> Vec<Effect> {
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
        // Edits for the previous board are never sent now; its store goes away below.
        self.queue.clear();
        self.app.status.write_pause = None;
        self.awaiting_repo_projects = false;
        if let Some(own) = &own
            && let Err(e) = registry.register(&board, own)
        {
            tracing::warn!(error = %e, "could not record this pane in the registry");
        }
        let dir = self.options.state_dir.clone();
        // Remembered only once it resolves: a board that never loads is never reopened.
        self.remember_on_resolve = true;
        let state = State::load(&dir);

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

    /// The shown board resolved as `resolved`. A Linear board opened by its team key or
    /// project slug resolves to its canonical UUID key (`linear:team:TBA` →
    /// `linear:team:<uuid>`); from then on it is shown, registered and cached under that key.
    /// GitHub keys are canonical already. Once per opening, the board is remembered for the
    /// repository (Decision 9).
    fn on_resolved(&mut self, resolved: &BoardKey) {
        let Some(requested) = self.board.clone() else {
            return;
        };
        if requested.provider() == Provider::Linear && &requested != resolved {
            self.board = Some(resolved.clone());
            if let Some(own) = &self.options.own_pane
                && let Err(e) =
                    PaneRegistry::new(&self.options.state_dir).rekey(&requested, resolved, own)
            {
                tracing::warn!(error = %e, "could not record this pane in the registry");
            }
        }
        if std::mem::take(&mut self.remember_on_resolve)
            && let (Some(repo), Some(board)) = (&self.options.repo, &self.board)
            && let Err(e) =
                State::update(&self.options.state_dir, |s| s.remember_board(repo, board))
        {
            tracing::warn!(error = %e, "could not save the remembered board");
        }
    }

    /// The shown board does not exist or cannot be seen (Decision 9): forget it for the
    /// repository if it is the one remembered there, stop showing and polling it, and ask for
    /// another board.
    fn board_not_found(&mut self, board: BoardKey) -> Vec<Effect> {
        if let Some(repo) = &self.options.repo
            && let Err(e) = State::update(&self.options.state_dir, |s| s.forget_board(repo, &board))
        {
            tracing::warn!(error = %e, "could not forget the missing board");
        }
        self.release();
        self.generation += 1;
        self.queue.clear();
        self.remember_on_resolve = false;
        self.app.reset_for_new_board();
        self.app.store = Box::new(MemoryStore::new(None));
        self.app.status.write_pause = None;
        self.app.status.stale = false;
        self.app.status.loading = None;
        let effects = self.show_picker(true);
        self.app.status.flash = Some(format!("board not found: {board}"));
        effects
    }

    /// Unregisters this pane from the board it shows. Called on board switch and on exit.
    pub fn release(&mut self) {
        if self.cache_dirty {
            self.persist_cache();
        }
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
            (PollKind::Incremental, Some(since))
                if self.incremental_mode() != IncrementalMode::Unsupported =>
            {
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
            Input::Tick => {
                if self.cache_dirty {
                    self.persist_cache();
                }
                let mut effects = self
                    .scheduler
                    .tick(now)
                    .map(|k| self.poll(k, now))
                    .unwrap_or_default();
                effects.extend(self.dispatch(now));
                effects
            }
            Input::Sync { generation, event } => {
                if generation != self.generation {
                    return Vec::new();
                }
                self.on_sync(event, now)
            }
            Input::Edited { purpose, edited } => {
                let cmds = self.app.on_edited(purpose, edited);
                self.commands(cmds, now)
            }
        }
    }

    fn on_sync(&mut self, event: SyncEvent, now: Instant) -> Vec<Effect> {
        if let SyncEvent::OpDone { id, result } = &event {
            if let Err(BackendError::RateLimited { retry_after_secs }) = result {
                // Not a failure: the edit goes back to the front and writes pause.
                self.queue
                    .retry_after(*id, now + Duration::from_secs(*retry_after_secs));
                self.app.status.write_pause = self.queue.paused_until(now);
                return Vec::new();
            }
            self.queue.finished(*id);
        }
        let op_done = matches!(event, SyncEvent::OpDone { .. });
        let op_applied = matches!(event, SyncEvent::OpDone { result: Ok(_), .. });
        match &event {
            SyncEvent::Rate(rate) => self.scheduler.set_low_budget(rate.is_low()),
            SyncEvent::Failed {
                error: BackendError::RateLimited { retry_after_secs },
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
            SyncEvent::ItemsUpdated { items, removed, .. } => {
                !items.is_empty() || !removed.is_empty()
            }
            _ => false,
        };

        match &event {
            SyncEvent::Failed {
                task: SyncTask::Resolve,
                error,
            } if error.is_not_found() => {
                if let Some(board) = self.board.clone() {
                    return self.board_not_found(board);
                }
            }
            SyncEvent::Project(project) => self.on_resolved(&project.board),
            _ => {}
        }

        if self.awaiting_repo_projects {
            match &event {
                SyncEvent::RepoProjects(list) => {
                    self.awaiting_repo_projects = false;
                    return match after_repo_projects(list.clone()) {
                        AfterRepo::Use(board) => self.open_board(board, now),
                        AfterRepo::Pick(list) => {
                            self.app.picker = Some(PickerState::with(list, Vec::new(), true));
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
        if matches!(event, SyncEvent::Projects { .. })
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
        } else if op_applied {
            // Edits settle in bursts; the next tick saves them all at once.
            self.cache_dirty = true;
        }
        let mut effects = self.commands(cmds, now);
        if op_done {
            effects.extend(self.dispatch(now));
        }
        effects
    }

    /// Spawns every edit the queue lets go now, and mirrors the write pause into the header.
    fn dispatch(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        let project = self.app.project().map(|p| p.id.clone());
        for id in self.queue.ready(now) {
            let op = self
                .app
                .store
                .pending()
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.op.clone());
            match (op, &project) {
                (Some(op), Some(project)) => effects.push(self.spawn(SyncJob::Mutate {
                    id,
                    project: project.clone(),
                    op,
                })),
                // Settled or gone meanwhile: nothing to send.
                _ => self.queue.finished(id),
            }
        }
        self.app.status.write_pause = self.queue.paused_until(now);
        effects
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
                Some(v) if v.layout != o.server_layout => stale.push((id, v.name.clone())),
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
            "View '{}' layout changed on {}; local layout cleared",
            stale[0].1,
            board.provider()
        ));
    }

    fn persist_cache(&mut self) {
        self.cache_dirty = false;
        let Some(snapshot) = self.app.store.confirmed() else {
            return;
        };
        // A list fetched with a session-only local filter would reopen next time without it.
        let mut filtered = self.app.views_with_extra_filter().peekable();
        let trimmed;
        let snapshot = if filtered.peek().is_some() {
            let mut copy = snapshot.clone();
            for view in filtered {
                copy.views.remove(view);
            }
            trimmed = copy;
            &trimmed
        } else {
            snapshot
        };
        if let Some(board) = &self.board
            && let Err(e) = save_cache(&cache_path(&self.options.state_dir, board), snapshot)
        {
            tracing::warn!(error = %e, "could not save the board cache");
        }
    }

    pub(crate) fn commands(&mut self, cmds: Vec<Command>, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        for cmd in cmds {
            match cmd {
                Command::FetchViewIds { view, query } => {
                    if let Some(s) = self.app.snapshot() {
                        effects.push(self.spawn(SyncJob::ViewIds {
                            project: s.project.id.clone(),
                            view,
                            query,
                            known: s.items.keys().cloned().collect(),
                            hydrate: !self.full_load_in_flight,
                        }));
                    }
                }
                Command::Send(id) => {
                    if let Some(item) = self
                        .app
                        .store
                        .pending()
                        .iter()
                        .find(|p| p.id == id)
                        .map(|p| p.op.item().clone())
                    {
                        self.queue.push(id, item);
                    }
                    effects.extend(self.dispatch(now));
                }
                Command::LoadDetail { item, before } => {
                    effects.push(self.spawn(SyncJob::Detail { item, before }))
                }
                Command::LoadCandidates(repo) => {
                    effects.push(self.spawn(SyncJob::Candidates(repo)))
                }
                Command::LoadRepos => effects.push(self.spawn(SyncJob::Repos)),
                Command::LookupRepo(slug) => effects.push(self.spawn(SyncJob::LookupRepo(slug))),
                Command::Search { query, scope } => {
                    effects.push(self.spawn(SyncJob::Search { query, scope }))
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
                    server_layout,
                } => {
                    if let Some(board) = self.board.clone()
                        && let Err(e) = State::update(&self.options.state_dir, |s| {
                            s.set_layout_override(
                                &board,
                                &view,
                                LayoutOverride {
                                    layout,
                                    server_layout,
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
                Command::Edit { purpose, initial } => {
                    effects.push(Effect::Editor { purpose, initial })
                }
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
            utc_offset: time::UtcOffset::UTC,
        }
    }

    fn controller(dir: &Path, board: Option<&str>) -> (Controller, Arc<FakeHerdr>, Instant) {
        let fake = Arc::new(FakeHerdr::default());
        let t0 = Instant::now();
        (
            Controller::new(
                options(dir, board),
                fake.clone(),
                Some(IncrementalMode::DateTime),
                t0,
            ),
            fake,
            t0,
        )
    }

    fn board() -> BoardKey {
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
        assert!(
            c.start(
                Err(vec![BackendError::NoCredentials {
                    provider: Provider::Github
                }]),
                t0
            )
            .is_empty()
        );
        assert_eq!(c.app.mode, Mode::Setup);
        assert_eq!(
            c.handle(Input::Key(key('r')), t0),
            vec![Effect::ResolveToken]
        );
    }

    #[test]
    fn a_board_without_cache_is_resolved_and_registered() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        assert_eq!(
            c.start(Ok(vec![Provider::Github]), t0),
            vec![sp(&c, SyncJob::Resolve(board()))]
        );
        assert_eq!(
            PaneRegistry::new(dir.path()).lookup(&board()).as_deref(),
            Some("me")
        );
    }

    fn repo() -> RepoSlug {
        "tviles/app".parse().unwrap()
    }

    fn remembered(dir: &Path) -> Option<BoardKey> {
        State::load(dir).remembered_board(&repo())
    }

    #[test]
    fn a_board_is_remembered_only_after_it_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        assert_eq!(remembered(dir.path()), None, "not before it resolves");
        c.handle(at(&c, SyncEvent::Project(snapshot().project)), t0);
        assert_eq!(remembered(dir.path()), Some(board()));

        // A cached board is remembered once its refresh resolves it too.
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        assert_eq!(remembered(dir.path()), None);
        c.handle(at(&c, SyncEvent::Project(snapshot().project)), t0);
        assert_eq!(remembered(dir.path()), Some(board()));
    }

    fn not_found() -> SyncEvent {
        SyncEvent::Failed {
            task: SyncTask::Resolve,
            error: BackendError::Api(vec![crate::error::GraphqlErrorEntry {
                message: "Could not resolve to a ProjectV2 with the number 99.".into(),
                kind: Some("NOT_FOUND".into()),
            }]),
        }
    }

    #[test]
    fn a_missing_board_is_forgotten_and_the_picker_opens() {
        for b in ["tviles/99", LINEAR_TEAM] {
            let dir = tempfile::tempdir().unwrap();
            let gone: BoardKey = b.parse().unwrap();
            State::update(dir.path(), |s| s.remember_board(&repo(), &gone)).unwrap();
            let (mut c, _, t0) = controller(dir.path(), None);
            assert_eq!(
                c.start(Ok(vec![Provider::Github, Provider::Linear]), t0),
                vec![sp(&c, SyncJob::Resolve(gone.clone()))],
                "{b}"
            );
            let effects = c.handle(at(&c, not_found()), t0 + Duration::from_secs(1));
            assert_eq!(remembered(dir.path()), None, "{b}");
            assert_eq!(c.app.mode, Mode::Picker, "{b}");
            assert!(c.app.picker.as_ref().unwrap().required, "{b}");
            assert_eq!(
                c.app.status.flash,
                Some(format!("board not found: {b}")),
                "{b}"
            );
            assert_eq!(c.app.status.error, None, "{b}");
            assert_eq!(effects, vec![sp(&c, SyncJob::Projects(Some(repo())))]);
            assert_eq!(c.board(), None, "{b}");
            assert_eq!(PaneRegistry::new(dir.path()).lookup(&gone), None, "{b}");
            // It is not retried.
            assert!(
                c.handle(Input::Tick, t0 + Duration::from_secs(61))
                    .is_empty(),
                "{b}"
            );
        }
    }

    #[test]
    fn a_missing_cached_board_stops_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        State::update(dir.path(), |s| s.remember_board(&repo(), &board())).unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        c.start(Ok(vec![Provider::Github]), t0);
        c.handle(at(&c, not_found()), t0 + Duration::from_secs(1));
        assert_eq!(remembered(dir.path()), None);
        assert_eq!(c.app.mode, Mode::Picker);
        assert!(c.app.snapshot().is_none(), "the gone board is not shown");
        assert!(
            c.handle(Input::Tick, t0 + Duration::from_secs(61))
                .is_empty()
        );
    }

    #[test]
    fn only_a_missing_board_is_forgotten() {
        // Another failure keeps the remembered board and retries it.
        let dir = tempfile::tempdir().unwrap();
        State::update(dir.path(), |s| s.remember_board(&repo(), &board())).unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        c.start(Ok(vec![Provider::Github]), t0);
        c.handle(at(&c, fail(SyncTask::Resolve)), t0);
        assert_eq!(remembered(dir.path()), Some(board()));
        assert_eq!(c.app.mode, Mode::Normal);

        // `--project` naming a missing board leaves the repository's own board remembered.
        let dir = tempfile::tempdir().unwrap();
        State::update(dir.path(), |s| s.remember_board(&repo(), &board())).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/99"));
        c.start(Ok(vec![Provider::Github]), t0);
        c.handle(at(&c, not_found()), t0);
        assert_eq!(remembered(dir.path()), Some(board()));
        assert_eq!(c.app.mode, Mode::Picker);
        assert_eq!(
            c.app.status.flash.as_deref(),
            Some("board not found: tviles/99")
        );
    }

    #[test]
    fn a_team_key_becomes_its_uuid_once_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let key: BoardKey = "linear:team:TBA".parse().unwrap();
        let uuid: BoardKey = LINEAR_TEAM.parse().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("linear:team:TBA"));
        assert_eq!(
            c.start(Ok(vec![Provider::Linear]), t0),
            vec![sp(&c, SyncJob::Resolve(key.clone()))]
        );
        let mut project = snapshot().project;
        project.board = uuid.clone();
        project.id = ProjectId::new(LINEAR_TEAM);
        c.handle(at(&c, SyncEvent::Project(project)), t0);
        assert_eq!(c.board(), Some(&uuid));
        assert_eq!(remembered(dir.path()), Some(uuid.clone()));
        let registry = PaneRegistry::new(dir.path());
        assert_eq!(registry.lookup(&uuid).as_deref(), Some("me"));
        assert_eq!(registry.lookup(&key), None);
        for file in ["state.json", "panes.json"] {
            let text = std::fs::read_to_string(dir.path().join(file)).unwrap();
            assert!(
                text.contains(LINEAR_TEAM) && !text.contains("TBA"),
                "{file}: {text}"
            );
        }
        // The loaded board is cached under its canonical key.
        c.handle(at(&c, complete("2026-10-01T12:00:00Z")), t0);
        assert!(cache_path(dir.path(), &uuid).exists());
        assert!(!cache_path(dir.path(), &key).exists());
    }

    #[test]
    fn a_remembered_board_without_credentials_opens_the_picker() {
        let dir = tempfile::tempdir().unwrap();
        State::update(dir.path(), |s| s.remember_board(&repo(), &board())).unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        assert_eq!(
            c.start(Ok(vec![Provider::Linear]), t0),
            vec![sp(&c, SyncJob::Projects(Some(repo())))]
        );
        assert_eq!(c.app.mode, Mode::Picker);
        assert_eq!(c.board(), None);
    }

    #[test]
    fn a_cached_board_paints_at_once_and_refreshes() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        let effects = c.start(Ok(vec![Provider::Github]), t0);
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
        assert_eq!(c.start(Ok(vec![Provider::Github]), t0), vec![Effect::Exit]);
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
            one.start(Ok(vec![Provider::Github]), t0),
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
        two.start(Ok(vec![Provider::Github]), t0);
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
        failed.start(Ok(vec![Provider::Github]), t0);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: BackendError::Network("down".into()),
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

    fn set_override(dir: &Path, board: &BoardKey, view: &str, layout: Layout, github: Layout) {
        State::update(dir, |s| {
            s.set_layout_override(
                board,
                &ViewId::new(view),
                LayoutOverride {
                    layout,
                    server_layout: github,
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
        c.handle(Input::Key(key('L')), t0);
        let saved = State::load(dir.path()).layout_overrides_for(&board());
        assert_eq!(
            saved,
            vec![(
                ViewId::new("V_table"),
                LayoutOverride {
                    layout: Layout::Board,
                    server_layout: Layout::Table
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        let effects = c.start(Ok(vec![Provider::Github]), t0);
        assert!(effects.iter().any(|e| matches!(e, Effect::Spawn { job: SyncJob::ViewIds { hydrate: false, query, .. }, .. } if query == &ListQuery::github("label:bug"))));
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
    fn a_pending_edit_never_reaches_the_disk_cache() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        let item = crate::model::ItemId::new("a");
        let field = crate::model::FieldId::new("F_status");
        let before = c.app.store.confirmed().unwrap().items[&item]
            .value(&field)
            .cloned();
        c.app.store.begin(crate::ops::Op::ClearField {
            item: item.clone(),
            field: field.clone(),
        });
        assert_ne!(
            c.app.store.snapshot().unwrap().items[&item].value(&field),
            before.as_ref(),
            "the view shows the pending edit"
        );
        c.persist_cache();
        let saved = crate::store::cache::load_cache(&cache_path(dir.path(), &board())).unwrap();
        assert_eq!(saved.items[&item].value(&field), before.as_ref());
    }

    #[test]
    fn polls_incrementally_with_a_two_minute_overlap() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
        c.handle(
            Input::Sync {
                generation: c.generation(),
                event: complete("2026-10-01T12:00:00Z"),
            },
            t0,
        );
        let limited = SyncEvent::Failed {
            task: SyncTask::Incremental,
            error: BackendError::RateLimited {
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
            Some(IncrementalMode::Unsupported),
            t0,
        );
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
        assert!(
            c.app.snapshot().is_none(),
            "no cache: the board is still resolving"
        );
        c.handle(Input::Key(key('B')), t0);
        assert_eq!(c.app.mode, Mode::Picker);
        c.handle(Input::Key(code(crossterm::event::KeyCode::Esc)), t0);
        assert_eq!(c.app.mode, Mode::Normal);
        c.handle(
            at(
                &c,
                SyncEvent::Projects {
                    boards: vec![summary("tviles/4")],
                    notes: vec![],
                },
            ),
            t0,
        );
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
            Some(IncrementalMode::DateTime),
            t0,
        );
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        let mut c = Controller::new(opts, fake, Some(IncrementalMode::DateTime), t0);
        assert_eq!(
            c.start(Ok(vec![Provider::Github]), t0),
            vec![sp(
                &c,
                SyncJob::Projects(Some("tviles/app".parse().unwrap()))
            )]
        );
        assert!(c.app.picker.as_ref().unwrap().loading);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: BackendError::Network("down".into()),
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
            provider: c.board().map(BoardKey::provider),
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
            error: BackendError::Network("down".into()),
        }
    }

    #[test]
    fn a_failed_refresh_ends_the_load_and_polling_resumes() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
        let old = c.generation();
        let other: BoardKey = "tviles/4".parse().unwrap();
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
        assert_eq!(
            c.start(Ok(vec![Provider::Github]), t0),
            vec![sp(&c, SyncJob::Resolve(board()))]
        );
    }

    #[test]
    fn empty_updates_are_not_persisted_but_hydration_is() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        let e = at(&c, SyncEvent::Project(crate::ui::fixtures::project()));
        c.handle(e, t0);
        let path = cache_path(dir.path(), &board());
        let e = at(
            &c,
            SyncEvent::ItemsUpdated {
                items: vec![],
                removed: vec![],
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
    fn lists_fetched_with_a_local_filter_stay_out_of_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        let e = at(&c, SyncEvent::Project(crate::ui::fixtures::project()));
        c.handle(e, t0);
        let e = at(&c, complete("2026-10-01T12:00:00Z"));
        c.handle(e, t0);
        let view = c.app.current_view().unwrap().id.clone();
        c.handle(Input::Key(key('f')), t0);
        for ch in "label:bug".chars() {
            c.handle(Input::Key(key(ch)), t0);
        }
        c.handle(
            Input::Key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Enter,
            )),
            t0,
        );
        let filter = format!("{} label:bug", c.app.current_view().unwrap().filter)
            .trim()
            .to_string();
        let e = at(
            &c,
            SyncEvent::ViewIds {
                view: view.clone(),
                query: ListQuery::github(filter),
                list: crate::store::snapshot::ViewList {
                    ids: vec![],
                    total: 0,
                    truncated: false,
                },
            },
        );
        c.handle(e, t0);
        let e = at(&c, SyncEvent::Hydrated(crate::ui::fixtures::items()));
        c.handle(e, t0);
        let saved = load_cache(&cache_path(dir.path(), &board())).unwrap();
        assert!(!saved.views.contains_key(&view));
        assert!(
            c.app.snapshot().unwrap().views.contains_key(&view),
            "kept in memory"
        );
    }

    #[test]
    fn focus_regained_after_idle_polls() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(&cache_path(dir.path(), &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
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
        c.start(Ok(vec![Provider::Github]), t0);
        let failure = SyncEvent::Failed {
            task: SyncTask::Projects,
            error: BackendError::Unauthorized {
                provider: Provider::Github,
            },
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
        c.start(Ok(vec![Provider::Github]), t0);
        let e = at(&c, complete("2026-10-01T12:00:00Z"));
        c.handle(e, t0);
        let old = c.generation();
        c.app.picker = Some(PickerState::with(vec![summary("tviles/4")], vec![], false));
        c.app.mode = Mode::Picker;
        // One Enter, after idling: the idle poll starts on the old board, then the pick switches.
        let enter = Input::Key(crate::ui::fixtures::code(crossterm::event::KeyCode::Enter));
        let effects = c.handle(enter, t0 + Duration::from_secs(400));
        let other: BoardKey = "tviles/4".parse().unwrap();
        assert_eq!(effects.len(), 2, "{effects:?}");
        assert!(
            matches!(&effects[0], Effect::Spawn { job: SyncJob::Incremental { .. }, generation, .. } if *generation == old)
        );
        assert_eq!(
            effects[1],
            Effect::Spawn {
                job: SyncJob::Resolve(other),
                generation: old + 1,
                provider: Some(Provider::Github),
            }
        );
    }

    #[test]
    fn opening_a_board_forgets_a_pending_repo_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        c.start(Ok(vec![Provider::Github]), t0);
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

    use crate::model::OpKind;
    use crate::ops::{Confirmed, Op, OpId};

    fn opened(dir: &Path) -> (Controller, Instant) {
        save_cache(&cache_path(dir, &board()), &snapshot()).unwrap();
        let (mut c, _, t0) = controller(dir, Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        (c, t0)
    }

    fn set_status(item: &str, option: &str) -> Op {
        Op::SetField {
            item: ItemId::new(item),
            field: FieldId::new("F_status"),
            value: FieldValue::SingleSelect {
                option_id: OptionId::new(option),
                name: option.into(),
            },
        }
    }

    #[test]
    fn lookup_commands_become_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let repo: RepoSlug = "acme/app".parse().unwrap();
        let polling = c.scheduler.in_flight();
        let effects = c.commands(
            vec![
                Command::LoadCandidates(repo.clone()),
                Command::LoadRepos,
                Command::LookupRepo(repo.clone()),
                Command::Search {
                    query: "crash".into(),
                    scope: vec![repo.clone()],
                },
            ],
            t0,
        );
        let jobs: Vec<&str> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Spawn { job, .. } => Some(job.kind()),
                _ => None,
            })
            .collect();
        assert_eq!(jobs, ["Candidates", "Repos", "LookupRepo", "Search"]);
        assert_eq!(c.app.pane_repo, Some("tviles/app".parse().unwrap()));
        assert_eq!(c.scheduler.in_flight(), polling, "lookups are not polls");
    }

    #[test]
    fn a_stale_lookup_answer_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        c.app.need_repos();
        let stale = c.generation().wrapping_add(1);
        let effects = c.handle(
            Input::Sync {
                generation: stale,
                event: SyncEvent::Repos(PushableRepos {
                    repos: vec![],
                    truncated: false,
                }),
            },
            t0,
        );
        assert!(effects.is_empty());
        assert!(matches!(c.app.repos, Some(crate::ui::app::Lookup::Loading)));
    }

    fn submit(c: &mut Controller, op: Op, now: Instant) -> Vec<Effect> {
        let cmds = c.app.submit(OpKind::Move, op);
        c.commands(cmds, now)
    }

    fn mutates(effects: &[Effect]) -> Vec<OpId> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Spawn {
                    job: SyncJob::Mutate { id, .. },
                    ..
                } => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn done(
        c: &mut Controller,
        id: OpId,
        result: Result<Confirmed, BackendError>,
        now: Instant,
    ) -> Vec<Effect> {
        let generation = c.generation();
        c.handle(
            Input::Sync {
                generation,
                event: SyncEvent::OpDone { id, result },
            },
            now,
        )
    }

    #[test]
    fn edits_to_different_items_run_four_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let mut sent = Vec::new();
        for item in ["a", "b", "c", "d", "e"] {
            sent.extend(mutates(&submit(&mut c, set_status(item, "o_done"), t0)));
        }
        assert_eq!(sent, [1, 2, 3, 4]);
        assert_eq!(mutates(&done(&mut c, 2, Ok(Confirmed::Applied), t0)), [5]);
    }

    #[test]
    fn a_mutate_job_carries_the_generation_it_was_produced_under() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let effects = submit(&mut c, set_status("a", "o_done"), t0);
        assert!(matches!(
            &effects[..],
            [Effect::Spawn { generation, .. }] if *generation == c.generation()
        ));
    }

    #[test]
    fn a_second_edit_to_the_same_item_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        assert_eq!(mutates(&submit(&mut c, set_status("a", "o_prog"), t0)), [1]);
        assert!(mutates(&submit(&mut c, set_status("a", "o_done"), t0)).is_empty());
        let error = BackendError::Network("offline".into());
        assert_eq!(
            mutates(&done(&mut c, 1, Err(error), t0)),
            [2],
            "sent even though 1 failed"
        );
    }

    #[test]
    fn a_rate_limited_edit_is_retried_after_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        let limited = BackendError::RateLimited {
            retry_after_secs: 30,
        };
        assert!(done(&mut c, 1, Err(limited), t0).is_empty());
        assert_eq!(c.app.pending_count(), 1, "still pending, not failed");
        assert!(c.app.errors.is_empty());
        assert_eq!(c.app.status.write_pause, Some(t0 + Duration::from_secs(30)));
        assert!(mutates(&c.handle(Input::Tick, t0 + Duration::from_secs(29))).is_empty());
        assert_eq!(
            mutates(&c.handle(Input::Tick, t0 + Duration::from_secs(30))),
            [1]
        );
        assert_eq!(c.app.status.write_pause, None);
    }

    #[test]
    fn the_cache_holds_confirmed_edits_but_not_pending_ones() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        submit(&mut c, set_status("b", "o_done"), t0);
        done(&mut c, 1, Ok(Confirmed::Applied), t0);
        c.handle(Input::Tick, t0);
        let cached = load_cache(&cache_path(dir.path(), &board())).unwrap();
        let status = |id: &str| {
            cached.items[&ItemId::new(id)]
                .value(&FieldId::new("F_status"))
                .map(|v| v.display())
        };
        assert_eq!(status("a").as_deref(), Some("o_done"));
        assert_eq!(status("b").as_deref(), Some("In Progress"));
    }

    fn cached_status(dir: &Path, item: &str) -> Option<String> {
        load_cache(&cache_path(dir, &board()))?.items[&ItemId::new(item)]
            .value(&FieldId::new("F_status"))
            .map(|v| v.display())
    }

    #[test]
    fn quick_edit_results_save_the_cache_once_on_the_next_tick() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let before = cached_status(dir.path(), "a");
        for item in ["a", "b", "c", "d"] {
            submit(&mut c, set_status(item, "o_done"), t0);
        }
        for id in 1..=4 {
            done(&mut c, id, Ok(Confirmed::Applied), t0);
        }
        assert_eq!(
            cached_status(dir.path(), "a"),
            before,
            "not saved per result"
        );
        c.handle(Input::Tick, t0);
        assert_eq!(cached_status(dir.path(), "a").as_deref(), Some("o_done"));
        assert_eq!(cached_status(dir.path(), "d").as_deref(), Some("o_done"));
        std::fs::remove_file(cache_path(dir.path(), &board())).unwrap();
        c.handle(Input::Tick, t0 + Duration::from_millis(1));
        assert!(
            !cache_path(dir.path(), &board()).exists(),
            "one save: the next tick has nothing to write"
        );
    }

    #[test]
    fn exit_saves_edit_results_not_yet_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        done(&mut c, 1, Ok(Confirmed::Applied), t0);
        c.release();
        assert_eq!(cached_status(dir.path(), "a").as_deref(), Some("o_done"));
    }

    /// Review Focus 1, end to end.
    #[test]
    fn a_stale_poll_after_a_confirmed_move_keeps_the_card_moved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        done(&mut c, 1, Ok(Confirmed::Applied), t0);
        let stale = crate::ui::fixtures::items().remove(0);
        let generation = c.generation();
        c.handle(
            Input::Sync {
                generation,
                event: SyncEvent::ItemsUpdated {
                    items: vec![stale],
                    removed: vec![],
                    fetched_at: "2026-10-12T00:00:00Z".into(),
                },
            },
            t0,
        );
        let a = &c.app.snapshot().unwrap().items[&ItemId::new("a")];
        assert_eq!(
            a.value(&FieldId::new("F_status"))
                .map(|v| v.display())
                .as_deref(),
            Some("o_done")
        );
    }

    /// A token that stops working mid-session, then `r` on the setup screen: the board stays
    /// open, so pending edits, their queue slots and the generation are kept.
    #[test]
    fn a_token_failure_then_retry_keeps_pending_edits_and_their_slots() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let mut sent = Vec::new();
        for item in ["a", "b", "c", "d", "e"] {
            sent.extend(mutates(&submit(&mut c, set_status(item, "o_done"), t0)));
        }
        assert_eq!(sent, [1, 2, 3, 4], "four in flight, the fifth queued");
        let generation = c.generation();
        let unauthorized = at(
            &c,
            SyncEvent::Failed {
                task: SyncTask::FullLoad,
                error: BackendError::Unauthorized {
                    provider: Provider::Github,
                },
            },
        );
        c.handle(unauthorized, t0);
        assert_eq!(c.app.mode, Mode::Setup);
        let effects = c.handle(Input::Key(key('r')), t0);
        assert_eq!(effects, vec![Effect::ResolveToken]);
        let project = c.app.project().unwrap().clone();
        assert_eq!(
            c.start(Ok(vec![Provider::Github]), t0),
            vec![sp(&c, SyncJob::Refresh(project))]
        );
        assert_eq!(c.generation(), generation, "late answers still count");
        assert_eq!(c.app.mode, Mode::Normal);
        assert_eq!(c.app.pending_count(), 5);
        assert!(c.app.errors.is_empty(), "nothing was dropped");
        assert_eq!(
            mutates(&done(&mut c, 1, Ok(Confirmed::Applied), t0)),
            [5],
            "the queued edit takes the freed slot"
        );
    }

    #[test]
    fn q_on_the_setup_screen_asks_while_edits_are_sending() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        let no_token = at(
            &c,
            SyncEvent::Failed {
                task: SyncTask::FullLoad,
                error: BackendError::NoCredentials {
                    provider: Provider::Github,
                },
            },
        );
        c.handle(no_token, t0);
        assert_eq!(c.app.mode, Mode::Setup);
        assert!(c.handle(Input::Key(key('q')), t0).is_empty());
        assert_eq!(c.app.mode, Mode::Confirm);
        assert_eq!(c.handle(Input::Key(key('y')), t0), vec![Effect::Exit]);
    }

    #[test]
    fn edits_pending_when_another_board_opens_are_logged_as_not_sent() {
        let dir = tempfile::tempdir().unwrap();
        let other: BoardKey = "tviles/9".parse().unwrap();
        let (mut c, t0) = opened(dir.path());
        submit(&mut c, set_status("a", "o_done"), t0);
        c.commands(vec![Command::PickBoard(other)], t0);
        assert_eq!(c.app.pending_count(), 0);
        let messages: Vec<_> = c.app.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["not sent: the board was reloaded"]);
    }

    /// Edits still in flight for the old board must not hold the new board's write slots,
    /// and the old board's queued edit is never sent.
    #[test]
    fn opening_another_board_forgets_queued_edits() {
        let dir = tempfile::tempdir().unwrap();
        let other: BoardKey = "tviles/9".parse().unwrap();
        let mut other_snap = snapshot();
        other_snap.project.board = other.clone();
        save_cache(&cache_path(dir.path(), &other), &other_snap).unwrap();
        let (mut c, t0) = opened(dir.path());
        let mut sent = Vec::new();
        for item in ["a", "b", "c", "d", "e"] {
            sent.extend(mutates(&submit(&mut c, set_status(item, "o_done"), t0)));
        }
        assert_eq!(sent, [1, 2, 3, 4], "four in flight, the fifth queued");
        let old = c.generation();
        c.commands(vec![Command::PickBoard(other)], t0);
        assert!(
            mutates(&c.handle(Input::Tick, t0)).is_empty(),
            "the old board's fifth edit is never sent"
        );
        let late = Input::Sync {
            generation: old,
            event: SyncEvent::OpDone {
                id: 1,
                result: Ok(Confirmed::Applied),
            },
        };
        assert!(
            c.handle(late, t0).is_empty(),
            "late results of the old board are dropped"
        );
        assert_eq!(
            mutates(&submit(&mut c, set_status("a", "o_done"), t0)),
            [1],
            "the new board writes at once"
        );
    }

    fn mutate_op(effects: &[Effect]) -> Option<&Op> {
        effects.iter().find_map(|e| match e {
            Effect::Spawn {
                job: SyncJob::Mutate { op, .. },
                ..
            } => Some(op),
            _ => None,
        })
    }

    /// A new item is queued under its placeholder. Controller ruling E15: an issue GitHub
    /// created but did not add is never sent again by the queue, even when the add was rate
    /// limited; `R` sends only the add. Once created, edits to the real item go out at once.
    #[test]
    fn a_new_issue_is_created_once_and_its_real_item_takes_edits() {
        use crate::ops::NewItem;
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        let temp = ItemId::new("local:1");
        let create = Op::Create {
            temp: temp.clone(),
            new: NewItem::Issue {
                repo: crate::model::RepoRef {
                    id: "R_1".into(),
                    slug: "tviles/t".parse().unwrap(),
                },
                title: "New bug".into(),
                body: String::new(),
            },
            presets: vec![],
        };
        let cmds = c.app.submit_new(create, true);
        let effects = c.commands(cmds, t0);
        assert_eq!(mutates(&effects), [1]);
        let not_added = Confirmed::NotAdded {
            issue: "I_new".into(),
            error: BackendError::RateLimited {
                retry_after_secs: 30,
            },
        };
        assert!(mutates(&done(&mut c, 1, Ok(not_added), t0)).is_empty());
        assert_eq!(c.app.status.write_pause, None, "not a rate-limit pause");
        assert!(
            mutates(&c.handle(Input::Tick, t0 + Duration::from_secs(60))).is_empty(),
            "never sent again by itself"
        );
        assert_eq!(c.app.errors.len(), 1);
        c.handle(Input::Key(key('E')), t0);
        let effects = c.handle(Input::Key(key('R')), t0);
        assert_eq!(mutates(&effects), [2]);
        assert!(matches!(
            mutate_op(&effects),
            Some(Op::Create { new: NewItem::Existing { hit }, .. }) if hit.id == "I_new"
        ));
        let mut item = c.app.snapshot().unwrap().items[&temp].clone();
        item.id = ItemId::new("PVTI_new");
        done(
            &mut c,
            2,
            Ok(Confirmed::Created {
                temp,
                item,
                refused: vec![],
            }),
            t0,
        );
        assert_eq!(c.app.selected_item().unwrap().id.as_str(), "PVTI_new");
        assert_eq!(
            mutates(&submit(&mut c, set_status("PVTI_new", "o_done"), t0)),
            [3]
        );
    }

    #[test]
    fn the_editor_runs_as_an_effect_and_its_text_comes_back_as_input() {
        use crate::ui::app::EditPurpose;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = opened(dir.path());
        // `c` opens the comment form; its `Open in editor` button runs the editor.
        assert!(c.handle(Input::Key(key('c')), t0).is_empty());
        c.handle(Input::Key(code(KeyCode::Tab)), t0);
        c.handle(Input::Key(code(KeyCode::Tab)), t0);
        let effects = c.handle(Input::Key(code(KeyCode::Enter)), t0);
        assert_eq!(
            effects,
            vec![Effect::Editor {
                purpose: EditPurpose::Comment(ItemId::new("a")),
                initial: String::new()
            }]
        );
        let effects = c.handle(
            Input::Edited {
                purpose: EditPurpose::Comment(ItemId::new("a")),
                edited: Some("LGTM".to_string()).into(),
            },
            t0,
        );
        assert!(
            mutates(&effects).is_empty(),
            "the text goes back into the form"
        );
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let effects = c.handle(Input::Key(alt_enter), t0);
        assert_eq!(mutates(&effects), [1]);
    }

    const LINEAR_TEAM: &str = "linear:team:1b4e28ba-2fa1-4d3b-a3f5-ef19b5a7633b";

    /// A controller with no fixed incremental mode, as the runtime builds it, on `board`
    /// with a cached snapshot whose load finished at noon.
    fn polling(dir: &Path, board: &str) -> (Controller, Instant) {
        let key: BoardKey = board.parse().unwrap();
        let mut cached = snapshot();
        cached.project.board = key.clone();
        cached.project.id = ProjectId::new(board);
        save_cache(&cache_path(dir, &key), &cached).unwrap();
        let t0 = Instant::now();
        let mut c = Controller::new(
            options(dir, Some(board)),
            Arc::new(FakeHerdr::default()),
            None,
            t0,
        );
        c.start(Ok(vec![Provider::Github, Provider::Linear]), t0);
        c.handle(at(&c, complete("2026-10-01T12:00:00Z")), t0);
        (c, t0)
    }

    #[test]
    fn linear_boards_poll_by_datetime_and_github_by_date() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = polling(dir.path(), LINEAR_TEAM);
        assert_eq!(c.incremental_mode(), IncrementalMode::DateTime);
        assert_eq!(
            c.handle(Input::Tick, t0 + Duration::from_secs(31)),
            vec![Effect::Spawn {
                job: SyncJob::Incremental {
                    project: ProjectId::new(LINEAR_TEAM),
                    since: poll_since("2026-10-01T12:00:00Z"),
                },
                generation: c.generation(),
                provider: Some(Provider::Linear),
            }]
        );

        let dir = tempfile::tempdir().unwrap();
        let (mut c, t0) = polling(dir.path(), "tviles/3");
        assert_eq!(c.incremental_mode(), IncrementalMode::Date);
        assert_eq!(
            c.handle(Input::Tick, t0 + Duration::from_secs(31)),
            vec![Effect::Spawn {
                job: SyncJob::Incremental {
                    project: ProjectId::new("tviles/3"),
                    since: poll_since("2026-10-01T12:00:00Z"),
                },
                generation: c.generation(),
                provider: Some(Provider::Github),
            }]
        );
    }

    #[test]
    fn setup_shows_only_when_no_provider_has_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        let none = vec![
            BackendError::NoCredentials {
                provider: Provider::Github,
            },
            BackendError::NoCredentials {
                provider: Provider::Linear,
            },
        ];
        assert!(c.start(Err(none), t0).is_empty());
        assert_eq!(c.app.mode, Mode::Setup);
        let text = c.app.setup.clone().unwrap();
        assert!(text.contains("gh auth login"), "{text}");
        assert!(text.contains("LINEAR_API_KEY"), "{text}");

        assert_eq!(
            c.start(Ok(vec![Provider::Linear]), t0),
            vec![sp(&c, SyncJob::Resolve(board()))],
            "one provider with credentials is enough"
        );
        assert_eq!(c.app.mode, Mode::Normal);
        assert_eq!(c.app.setup, None);
    }

    #[test]
    fn a_github_only_user_sends_nothing_to_linear() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        let repo: RepoSlug = "tviles/app".parse().unwrap();
        assert_eq!(
            c.start(Ok(vec![Provider::Github]), t0),
            vec![Effect::Spawn {
                job: SyncJob::RepoProjects(repo),
                generation: c.generation(),
                provider: None,
            }]
        );
        let effects = c.handle(
            at(&c, SyncEvent::RepoProjects(vec![summary("tviles/3")])),
            t0,
        );
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                job: SyncJob::Resolve(board()),
                generation: c.generation(),
                provider: Some(Provider::Github),
            }]
        );
    }

    /// Runs the picker's `Projects` job as the runtime would, on a syncer over `backends`,
    /// and hands every event it sends to the controller.
    async fn run_picker_job(
        c: &mut Controller,
        effects: &[Effect],
        backends: crate::backend::Backends,
    ) {
        let [
            Effect::Spawn {
                job: SyncJob::Projects(repo),
                provider,
                ..
            },
        ] = effects
        else {
            panic!("expected one Projects job, got {effects:?}");
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        crate::sync::syncer::Syncer::new(backends, *provider, tx, 2000)
            .projects(repo.as_ref())
            .await;
        let t0 = Instant::now();
        while let Ok(event) = rx.try_recv() {
            c.handle(at(c, event), t0);
        }
    }

    fn picker_screen(c: &mut Controller) -> String {
        crate::ui::fixtures::render_to_string(90, 16, |f| crate::ui::chrome::draw(f, &mut c.app))
    }

    #[tokio::test]
    async fn a_linear_only_user_never_reaches_setup_through_the_picker() {
        use crate::backend::{Backend, Backends};
        use crate::github::fixture::FixtureTransport;
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        let effects = c.start(Ok(vec![Provider::Linear]), t0);
        assert_eq!(c.app.mode, Mode::Picker);
        let lin = Arc::new(FixtureTransport::new());
        lin.push(
            "ListBoards",
            json!({"projects": {"nodes": [{"id": "p1", "name": "Launch"}]},
                   "teams": {"nodes": [{"id": "t1", "key": "TBA", "name": "Testbed A"}]}}),
        );
        let backends = Backends {
            github: None,
            linear: Some(Arc::new(Backend::Linear(crate::linear::Linear::new(
                lin.clone(),
            )))),
        };
        run_picker_job(&mut c, &effects, backends).await;
        assert_eq!(c.app.mode, Mode::Picker, "never the GitHub setup screen");
        assert_eq!(c.app.setup, None);
        let picker = c.app.picker.as_ref().unwrap();
        assert!(!picker.loading);
        assert_eq!(picker.visible().len(), 2);
        let screen = picker_screen(&mut c);
        assert!(
            screen.contains("GitHub boards: run `gh auth login` or set GH_TOKEN"),
            "{screen}"
        );
        assert!(
            screen.contains("Linear projects") && screen.contains("Launch"),
            "{screen}"
        );
    }

    #[tokio::test]
    async fn a_github_only_users_picker_gains_one_linear_line() {
        use crate::backend::{Backend, Backends};
        use crate::github::fixture::FixtureTransport;
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), Some("tviles/3"));
        c.start(Ok(vec![Provider::Github]), t0);
        let effects = c.handle(Input::Key(key('B')), t0);
        let gh = Arc::new(FixtureTransport::new());
        gh.push(
            "RepoProjects",
            json!({"repository": {"projectsV2": {"nodes": [
                {"id": "PVT_3", "number": 3, "title": "App board", "closed": false,
                 "owner": {"__typename": "User", "login": "tviles"}}]}}}),
        );
        gh.push(
            "ViewerProjects",
            json!({"viewer": {"login": "tviles", "projectsV2": {"nodes": [
                {"id": "PVT_1", "number": 1, "title": "Roadmap", "closed": false,
                 "owner": {"__typename": "User", "login": "tviles"}}]},
                "organizations": {"nodes": []}}}),
        );
        let backends = Backends {
            github: Some(Arc::new(Backend::Github(crate::github::Github::new(
                gh.clone(),
            )))),
            linear: None,
        };
        run_picker_job(&mut c, &effects, backends).await;
        assert_eq!(c.app.mode, Mode::Picker);
        let picker = c.app.picker.as_ref().unwrap();
        let boards: Vec<String> = picker
            .visible()
            .iter()
            .map(|p| p.board.to_string())
            .collect();
        assert_eq!(boards, ["tviles/3", "tviles/1"], "linked first, as today");
        assert_eq!(
            picker.notes,
            ["Linear boards: set LINEAR_API_KEY or linear_api_key_command"]
        );
        let screen = picker_screen(&mut c);
        assert!(
            screen.contains("tviles/3  App board") && screen.contains("tviles/1  Roadmap"),
            "{screen}"
        );
        assert!(
            !screen.contains("│GitHub "),
            "a GitHub-only list draws no header: {screen}"
        );
        assert!(
            screen.contains("Linear boards: set LINEAR_API_KEY or linear_api_key_command"),
            "{screen}"
        );
        assert_eq!(
            c.handle(Input::Key(code(crossterm::event::KeyCode::Down)), t0),
            Vec::new()
        );
        assert!(matches!(
            c.app.handle_key(code(crossterm::event::KeyCode::Enter)).as_slice(),
            [Command::PickBoard(b)] if b.to_string() == "tviles/1"
        ));
    }

    #[test]
    fn a_linear_only_user_in_a_repository_gets_the_picker() {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, t0) = controller(dir.path(), None);
        assert_eq!(
            c.start(Ok(vec![Provider::Linear]), t0),
            vec![sp(
                &c,
                SyncJob::Projects(Some("tviles/app".parse().unwrap()))
            )]
        );
        assert_eq!(c.app.mode, Mode::Picker);
    }
}
