use crate::error::BackendError;
use crate::model::*;
use crate::store::{BoardSnapshot, Store, StoreUpdate, ViewList};
use crate::sync::{SyncEvent, SyncTask, store_update};
use crate::ui::board::{BoardSelection, Column, build_columns, constrain_columns, resolve_layout};
use crate::ui::detail::{DetailOutcome, DetailState, build_doc};
use crate::ui::keymap::{Action, Keymap};
use crate::ui::markdown::Target;
use crate::ui::picker::{PickerOutcome, PickerState};
use crate::ui::pills::card_fields;
use crate::ui::search;
use crate::ui::table::{Row, build_rows, collapse_key};
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};

mod edit;
pub use edit::ago as edit_ago;
mod popups;

use crate::ops::OpId;
pub use edit::{Confirm, ConfirmAction, FailedEdit, Lookup};
pub use popups::{CandidateSource, FieldTarget, LineKind, MenuEntry, NewKind, Popup, RepoPurpose};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Filter,
    Help,
    Detail,
    Picker,
    Setup,
    /// A yes/no question on the status line.
    Confirm,
    /// A popover editing a field of the item.
    Popup,
    /// The list of failed edits, drawn over the board (and the detail, if one is open).
    ErrorLog,
}

/// What a text form, or a `$EDITOR` session opened from one, is for.
#[derive(Debug, Clone, PartialEq)]
pub enum EditPurpose {
    Title(ItemId),
    Body(ItemId),
    Comment(ItemId),
    /// A new draft: its title and body, and the values it starts with.
    NewDraft {
        presets: Vec<(FieldId, FieldValue)>,
    },
    /// A new issue in `repo`.
    NewIssue {
        repo: RepoRef,
        presets: Vec<(FieldId, FieldValue)>,
    },
    /// A new Linear issue in `team`; `None` while the form's Team line chooses it. `presets`
    /// are the column's and lane's values as the board shows them, resolved for the team on
    /// submit (spec section 9).
    NewLinearIssue {
        team: Option<TeamMeta>,
        presets: Vec<(FieldId, FieldValue)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    FetchViewIds {
        view: ViewId,
        query: ListQuery,
    },
    LoadDetail {
        item: ItemId,
        before: Option<String>,
    },
    OpenUrl(String),
    LoadCandidates(RepoSlug),
    /// A Linear team's members and labels, by team id.
    LoadTeamCandidates(String),
    /// Look up a Linear issue by its key (`TBA-12`), for "add existing".
    LookupIssue(String),
    LoadRepos,
    LookupRepo(RepoSlug),
    Search {
        query: String,
        scope: Vec<RepoSlug>,
    },
    Refresh,
    SaveLastView(ViewId),
    /// The user chose `layout` for `view`, whose own layout on GitHub is `server_layout`.
    SaveLayout {
        view: ViewId,
        layout: Layout,
        server_layout: Layout,
    },
    /// The user is back on GitHub's layout for `view`: forget the choice.
    ClearLayout {
        view: ViewId,
    },
    PickBoard(BoardKey),
    ShowPicker,
    /// Re-run startup after the user fixed authentication.
    Retry,
    /// Send the pending edit with this id (the controller queues it).
    Send(OpId),
    /// Open `$EDITOR` on `initial` (a text form's `Open in editor`); the text comes back
    /// through `App::on_edited`, into the form.
    Edit {
        purpose: EditPurpose,
        initial: String,
    },
    Quit,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Showing the cache; a fresh full load has not finished yet.
    pub stale: bool,
    pub loading: Option<(usize, usize)>,
    pub error: Option<String>,
    /// Config warnings and notes such as "loaded N of M".
    pub notes: Vec<String>,
    pub rate_low: bool,
    /// A one-off reply to the last key press, such as "not opened: not a web link". The next
    /// key press clears it.
    pub flash: Option<String>,
    /// Writes are paused by a secondary rate limit until then.
    pub write_pause: Option<std::time::Instant>,
}

pub struct App {
    pub store: Box<dyn Store>,
    pub keymap: Keymap,
    pub theme: Theme,
    pub mode: Mode,
    /// Text being typed in Search or Filter mode.
    pub input: String,
    pub status: Status,
    pub detail: Option<DetailState>,
    pub picker: Option<PickerState>,
    pub setup: Option<String>,
    /// The setup screen is for a board whose credentials failed, so `B` may open another
    /// board; not when no provider has credentials, or the board list itself failed.
    pub setup_offers_boards: bool,
    pub table_selected: usize,
    pub board_sel: BoardSelection,
    pub collapsed: HashSet<String>,
    pub quit: bool,
    /// The current view, by id, so views reordered on GitHub never move you to another one.
    current: Option<ViewId>,
    /// The selected item, so polls, re-sorts and search never move the cursor to another one.
    selected_id: Option<ItemId>,
    /// A new item to highlight until the next key press.
    pub highlight: Option<ItemId>,
    /// Items edited during this visit of the current view: they stay where they are, even
    /// when the view's filter no longer matches them, until the view or its filter changes
    /// or `r` (spec section 5, "Items that stop matching the view").
    sticky: Vec<edit::Sticky>,
    search: String,
    layout_toggle: HashMap<ViewId, Layout>,
    /// Per view, a filter the user added on top of the view's own filter. Session only. On
    /// GitHub it is sent with the view's filter; on Linear it runs on the client.
    extra_filter: HashMap<ViewId, String>,
    /// Classic token scopes GitHub reported, and those it refused an edit for.
    scopes: Option<Vec<String>>,
    refused_scopes: Vec<String>,
    /// The kind each pending op was allowed as, for the error log and retries.
    pending_kinds: HashMap<OpId, OpKind>,
    /// Failed edits, oldest first.
    pub errors: Vec<FailedEdit>,
    /// The error log's selected row; 0 is the newest failure.
    pub error_sel: usize,
    /// The question `Mode::Confirm` asks.
    pub confirm: Option<Confirm>,
    /// The popover `Mode::Popup` shows.
    pub popup: Option<Popup>,
    /// Assignable users, labels and milestones per repository, loaded when first needed.
    pub(crate) candidates: HashMap<RepoSlug, Lookup<RepoCandidates>>,
    /// Members and labels per Linear team id, loaded when first needed.
    pub(crate) team_candidates: HashMap<String, Lookup<crate::model::TeamCandidates>>,
    /// Repositories the user can push to, for the repository picker.
    pub(crate) repos: Option<Lookup<PushableRepos>>,
    /// Numbers the placeholders of new items (`local:N`).
    temp_counter: u64,
    /// The team the last new Linear issue went to this session, which the next one's Team
    /// line starts with (T2).
    pub(crate) last_team: Option<TeamMeta>,
    pub(crate) repo_lookups: HashMap<RepoSlug, Lookup<Option<RepoRef>>>,
    /// The latest "add existing" search and its results (`search` is the quick-search text).
    pub(crate) content_search: Option<(String, Lookup<Vec<ContentHit>>)>,
    /// The focused pane's repository when the board opened.
    pub pane_repo: Option<RepoSlug>,
    /// The local UTC offset, for "today".
    pub utc_offset: time::UtcOffset,
    /// Detail width used to number targets; kept in sync by the renderer.
    pub detail_width: u16,
    /// The help overlay's first visible row, and how many rows it showed last draw.
    pub help_scroll: usize,
    pub help_visible: usize,
}

pub(crate) fn task_words(task: &SyncTask) -> &'static str {
    match task {
        SyncTask::Resolve => "could not load the board",
        SyncTask::FullLoad => "could not load items",
        SyncTask::Incremental => "refresh failed",
        SyncTask::ViewIds(_) => "could not load this view",
        SyncTask::Hydrate => "could not load some items",
        SyncTask::Detail(_) => "could not load the item",
        SyncTask::Projects => "could not list boards",
        SyncTask::Candidates(_) => "could not load the repository's users and labels",
        SyncTask::TeamCandidates(_) => "could not load the team's members and labels",
        SyncTask::IssueByKey(_) => "could not look up the issue",
        SyncTask::Repos | SyncTask::RepoLookup(_) => "could not list repositories",
        SyncTask::Search(_) => "search failed",
    }
}

/// Whether every option behind `bucket` (its key and aliases) has a kind in `excluded`. A
/// bucket with no option (No Status, an iteration) never does.
fn all_excluded(bucket: &Bucket, field: &Field, excluded: &[OptionKind]) -> bool {
    let FieldKind::SingleSelect { options } = &field.kind else {
        return false;
    };
    let mut ids = bucket.key.iter().chain(&bucket.aliases).peekable();
    ids.peek().is_some()
        && ids.all(|id| {
            options
                .iter()
                .find(|o| o.id.0 == *id)
                .and_then(|o| o.kind)
                .is_some_and(|k| excluded.contains(&k))
        })
}

/// The setup screen text for errors the user has to fix outside the app.
pub fn setup_message(error: &BackendError) -> Option<String> {
    use crate::model::Provider::{Github, Linear};
    match error {
        BackendError::NoCredentials { provider: Github } => Some(
            "No GitHub token found.\n\nRun:  gh auth login\nor set GH_TOKEN, then press r.".into(),
        ),
        BackendError::Unauthorized { provider: Github } => {
            Some("GitHub rejected the token.\n\nRun:  gh auth login\nthen press r.".into())
        }
        BackendError::MissingPermission {
            provider: Github,
            needed,
        } => Some(format!(
            "The token is missing the {} scope.\n\nRun:  gh auth refresh -s project\n(add -s repo for private repositories), then press r.",
            needed.join(", ")
        )),
        BackendError::NoCredentials { provider: Linear } => Some(
            "No Linear API key found.\n\nCreate a personal API key in Linear\n(Settings → Security & access → Personal API keys),\nthen set LINEAR_API_KEY or linear_api_key_command, and press r."
                .into(),
        ),
        BackendError::Unauthorized { provider: Linear } => Some(
            "Linear rejected the API key.\n\nCreate a new personal API key in Linear's settings, then press r."
                .into(),
        ),
        BackendError::MissingPermission {
            provider: Linear, ..
        } => Some(
            "Linear refused the request: the API key cannot see this board.\n\nUse a key from the board's workspace, then press r."
                .into(),
        ),
        _ => None,
    }
}

/// The setup screen when no provider has credentials: each error's setup text (or the error
/// itself), one after another.
pub fn setup_message_for_all(errors: &[BackendError]) -> String {
    errors
        .iter()
        .map(|e| setup_message(e).unwrap_or_else(|| e.to_string()))
        .collect::<Vec<_>>()
        .join("\n\n— or —\n\n")
}

impl App {
    pub fn new(store: Box<dyn Store>, keymap: Keymap, theme: Theme) -> Self {
        Self {
            store,
            keymap,
            theme,
            mode: Mode::Normal,
            input: String::new(),
            status: Status::default(),
            detail: None,
            picker: None,
            setup: None,
            setup_offers_boards: false,
            table_selected: 0,
            board_sel: BoardSelection::default(),
            collapsed: HashSet::new(),
            quit: false,
            current: None,
            selected_id: None,
            highlight: None,
            sticky: Vec::new(),
            search: String::new(),
            layout_toggle: HashMap::new(),
            extra_filter: HashMap::new(),
            scopes: None,
            refused_scopes: Vec::new(),
            pending_kinds: HashMap::new(),
            errors: Vec::new(),
            error_sel: 0,
            confirm: None,
            popup: None,
            candidates: HashMap::new(),
            team_candidates: HashMap::new(),
            repos: None,
            temp_counter: 0,
            last_team: None,
            repo_lookups: HashMap::new(),
            content_search: None,
            pane_repo: None,
            utc_offset: time::UtcOffset::UTC,
            detail_width: 80,
            help_scroll: 0,
            help_visible: usize::MAX,
        }
    }

    pub fn snapshot(&self) -> Option<&BoardSnapshot> {
        self.store.snapshot()
    }

    pub fn project(&self) -> Option<&Project> {
        self.snapshot().map(|s| &s.project)
    }

    /// The selected view; the first view when none is selected or it was deleted on GitHub.
    pub fn current_view(&self) -> Option<&View> {
        let views = &self.project()?.views;
        self.current
            .as_ref()
            .and_then(|id| views.iter().find(|v| &v.id == id))
            .or_else(|| views.first())
    }

    /// Selects a view by id. Works before the project has loaded.
    pub fn select_view(&mut self, id: &ViewId) {
        self.current = Some(id.clone());
    }

    /// Replaces the remembered layout choices (one board's, loaded from state).
    pub fn set_layout_overrides(&mut self, overrides: HashMap<ViewId, Layout>) {
        self.layout_toggle = overrides;
    }

    /// The current view's GitHub filter plus the local one, if any.
    pub fn effective_filter(&self) -> String {
        self.current_view()
            .map(|v| self.filter_for(v))
            .unwrap_or_default()
    }

    /// The local filter of the current view, if one is set.
    pub fn extra_filter(&self) -> Option<&str> {
        let view = self.current_view()?;
        self.extra_filter.get(&view.id).map(String::as_str)
    }

    /// Views with a local filter; their id lists don't match the GitHub filter, so they
    /// are left out of the disk cache.
    pub fn views_with_extra_filter(&self) -> impl Iterator<Item = &ViewId> {
        self.extra_filter.keys()
    }

    fn filter_for(&self, view: &View) -> String {
        match self.extra_filter.get(&view.id) {
            Some(extra) => format!("{} {extra}", view.filter).trim().to_string(),
            None => view.filter.github_query().to_string(),
        }
    }

    fn is_linear(&self) -> bool {
        self.project()
            .is_some_and(|p| p.board.provider() == Provider::Linear)
    }

    /// What a client-side filter is evaluated against: the board's fields, the viewer on a
    /// Linear board (for `@me`), and today in the pane's UTC offset.
    pub fn filter_context(&self) -> FilterContext<'_> {
        let project = self.project();
        FilterContext {
            fields: project.map_or(&[], |p| p.fields.as_slice()),
            me: project
                .and_then(|p| p.linear.as_ref())
                .map(|l| l.viewer_id.as_str()),
            today: Some(self.today()),
        }
    }

    /// The view's `f` text, parsed, on a Linear board; `None` on GitHub, which runs the text
    /// itself. Without `f` text it is the empty filter, which hides archived items.
    pub fn client_filter(&self, view: &View) -> Option<Filter> {
        let project = self.project().filter(|_| self.is_linear())?;
        let text = self.extra_filter.get(&view.id).map_or("", String::as_str);
        Some(Filter::parse(text, &project.fields))
    }

    /// The list request for `view`, or `None` when the view shows every item. GitHub: the
    /// effective filter text decides. Linear: the view's own filter, with archived items when
    /// the `f` text asks for them.
    pub fn list_query(&self, view: &View) -> Option<ListQuery> {
        match self.client_filter(view) {
            Some(client) => {
                let include_archived = client.wants_archived();
                (!view.filter.is_everything() || include_archived).then(|| ListQuery {
                    filter: view.filter.clone(),
                    include_archived,
                })
            }
            None => {
                let text = self.filter_for(view);
                (!text.trim().is_empty()).then(|| ListQuery::github(text))
            }
        }
    }

    /// The current view's list request; `None` without a view, as for one that lists every
    /// item.
    fn current_query(&self) -> Option<ListQuery> {
        self.current_view().and_then(|v| self.list_query(v))
    }

    /// Sets (or, when empty, clears) the current view's local filter, and refetches the
    /// view's ids when its list request changed. On a Linear board most filters change only
    /// what the client shows, so the list stays.
    fn set_extra_filter(&mut self, extra: &str) -> Vec<Command> {
        let Some(view) = self.current_view().cloned() else {
            return Vec::new();
        };
        let text_before = self.filter_for(&view);
        let before = self.list_query(&view);
        let extra = extra.trim();
        if extra.is_empty() {
            self.extra_filter.remove(&view.id);
        } else {
            self.extra_filter.insert(view.id.clone(), extra.to_string());
        }
        let query = self.list_query(&view);
        if self.filter_for(&view) == text_before {
            // Nothing changed: keep the cached list and the cursor.
            return self.revalidate_view().into_iter().collect();
        }
        let refetch = query != before;
        if refetch {
            // A changed request invalidates the cached list.
            self.store.apply(StoreUpdate::ClearViewIds(view.id.clone()));
        }
        // A changed filter ends the visit's sticky items and moves the cursor to the top.
        self.sticky.clear();
        self.table_selected = 0;
        self.board_sel = BoardSelection::default();
        self.sync_selected_id();
        match query {
            Some(query) if refetch => vec![Command::FetchViewIds {
                view: view.id,
                query,
            }],
            _ => Vec::new(),
        }
    }

    /// On a Linear board, the `f` text's keys that name no field, as a flash.
    fn flash_unknown_keys(&mut self) {
        let Some(filter) = self.current_view().and_then(|v| self.client_filter(v)) else {
            return;
        };
        let keys: Vec<&str> = filter
            .unknown_keys()
            .iter()
            .map(String::as_str)
            .filter(|k| !k.is_empty())
            .collect();
        if !keys.is_empty() {
            self.status.flash = Some(format!("unknown filter field: {}", keys.join(", ")));
        } else if !filter.unknown_values().is_empty() {
            self.status.flash = Some(format!(
                "unknown filter value: {}",
                filter.unknown_values().join(", ")
            ));
        }
    }

    /// Forgets what belonged to the previous board when another one opens (or the same one
    /// is loaded again): the quick search, the error, the error log, the highlight and the
    /// "loaded N of M" note. Config warnings stay. Edits still pending are never sent now;
    /// they are the new error log's entries, so none vanishes unrecorded.
    pub fn reset_for_new_board(&mut self) {
        self.setup = None;
        self.setup_offers_boards = false;
        if self.mode == Mode::Setup {
            self.mode = Mode::Normal;
        }
        self.sticky.clear();
        self.search.clear();
        self.input.clear();
        self.status.error = None;
        self.status.notes.retain(|n| !n.starts_with("loaded "));
        self.errors.clear();
        self.highlight = None;
        self.fail_all_pending(edit::NOT_SENT_RELOADED);
        self.pending_kinds.clear();
        self.error_sel = 0;
        self.candidates.clear();
        self.team_candidates.clear();
        self.repos = None;
        self.repo_lookups.clear();
        self.content_search = None;
    }

    pub fn search_query(&self) -> &str {
        if self.mode == Mode::Search {
            &self.input
        } else {
            &self.search
        }
    }

    /// Items of the current view: the whole board for an empty filter, else the view's id
    /// list (`None` until it arrives); on a Linear board, only those the `f` filter matches;
    /// then the visit's sticky items, quick search and the view's sort.
    pub fn view_items(&self) -> Option<Vec<&Item>> {
        let snap = self.snapshot()?;
        let view = self.current_view()?;
        let mut items: Vec<&Item> = if self.list_query(view).is_none() {
            snap.all_items()
                .into_iter()
                .filter(|i| !i.archived)
                .collect()
        } else {
            snap.view_items(&view.id)?
        };
        if let Some(filter) = self.client_filter(view) {
            let ctx = self.filter_context();
            items.retain(|i| filter.matches(i, &ctx));
        }
        self.add_sticky(snap, &mut items);
        let query = self.search_query();
        if !query.is_empty() {
            items.retain(|i| search::matches(i, query));
        }
        sort_items(&mut items, &view.sort_by, &snap.project.fields);
        Some(items)
    }

    pub fn column_field(&self) -> Option<&Field> {
        self.current_view()?.column_field(&self.project()?.fields)
    }

    pub fn layout(&self) -> (Layout, Option<&'static str>) {
        let Some(view) = self.current_view() else {
            return (Layout::Table, None);
        };
        let requested = self
            .layout_toggle
            .get(&view.id)
            .copied()
            .unwrap_or(view.layout);
        resolve_layout(requested, self.column_field())
    }

    pub fn table_rows(&self) -> Vec<Row<'_>> {
        let items = self.view_items().unwrap_or_default();
        let group = self
            .current_view()
            .zip(self.project())
            .and_then(|(v, p)| v.group_field(&p.fields));
        build_rows(&items, group, &self.collapsed)
    }

    pub fn board_columns(&self) -> Vec<Column<'_>> {
        let (Some(field), Some(view), Some(project)) =
            (self.column_field(), self.current_view(), self.project())
        else {
            return Vec::new();
        };
        let items = self.view_items().unwrap_or_default();
        let mut columns = build_columns(&items, field, view.group_field(&project.fields));
        // A Linear view that leaves out states (the Board tab: canceled, duplicate) shows no
        // empty column for them; one holding a card (a card just moved there) stays.
        let excluded = view.filter.excluded_state_kinds();
        if self.is_linear() && !excluded.is_empty() {
            columns.retain(|c| {
                !c.lanes.iter().all(|l| l.items.is_empty())
                    || !all_excluded(&c.bucket, field, &excluded)
            });
        }
        let constraint = match self.client_filter(view) {
            Some(filter) => filter.constraint(field, &self.filter_context()),
            None => column_constraint(&self.effective_filter(), field),
        };
        // A filter that allows no column at all was misread: `constrain_columns` shows every
        // column then, and none of them is "filtered".
        let reads = columns
            .iter()
            .any(|c| constraint.allows(&c.bucket.title, c.bucket.key.is_none()));
        constrain_columns(columns, &constraint)
            .into_iter()
            .map(|mut c| {
                // A column the filter rejects, kept for a card the user just edited into it
                // (spec: "Done (filtered)"). One kept for GitHub's own cards is not marked.
                if reads
                    && !constraint.allows(&c.bucket.title, c.bucket.key.is_none())
                    && c.items().iter().any(|i| self.is_sticky(&i.id))
                {
                    c.bucket.title = format!("{} (filtered)", c.bucket.title);
                }
                c
            })
            .collect()
    }

    /// The fields board cards show as pills in the current view.
    pub fn card_fields(&self) -> Vec<&Field> {
        let (Some(column), Some(view), Some(project)) =
            (self.column_field(), self.current_view(), self.project())
        else {
            return Vec::new();
        };
        card_fields(view, project, column, view.group_field(&project.fields))
    }

    pub fn selected_item(&self) -> Option<&Item> {
        match self.layout().0 {
            Layout::Board => {
                let cols = self.board_columns();
                cols.get(self.board_sel.column)?
                    .items()
                    .get(self.board_sel.index)
                    .copied()
            }
            _ => match self.table_rows().get(self.table_selected)? {
                Row::Item(i) => Some(*i),
                Row::Group { .. } => None,
            },
        }
    }

    pub fn detail_item(&self) -> Option<&Item> {
        let id = &self.detail.as_ref()?.item;
        self.snapshot()?.items.get(id)
    }

    /// A request for the current view's ids, when it has a filter and no list yet.
    pub fn view_needs_ids(&self) -> Option<Command> {
        let view = self.current_view()?;
        let query = self.list_query(view)?;
        let missing = !self.snapshot()?.views.contains_key(&view.id);
        missing.then(|| Command::FetchViewIds {
            view: view.id.clone(),
            query,
        })
    }

    /// A request for the current view's ids whenever it has a filter, cached list or not.
    pub fn revalidate_view(&self) -> Option<Command> {
        let view = self.current_view()?;
        let query = self.list_query(view)?;
        Some(Command::FetchViewIds {
            view: view.id.clone(),
            query,
        })
    }

    fn clamp_selection(&mut self) {
        let rows = self.table_rows().len();
        self.table_selected = self.table_selected.min(rows.saturating_sub(1));
        let cols = self.board_columns();
        let lens: Vec<usize> = cols.iter().map(|c| c.len()).collect();
        self.board_sel.column = self.board_sel.column.min(lens.len().saturating_sub(1));
        // Never rest on an empty column when a populated one exists: the nearest to the
        // right, else to the left.
        if lens.get(self.board_sel.column) == Some(&0) {
            let c = self.board_sel.column;
            if let Some(n) = (c + 1..lens.len())
                .find(|&n| lens[n] > 0)
                .or_else(|| (0..c).rev().find(|&n| lens[n] > 0))
            {
                self.board_sel.column = n;
            }
        }
        let len = lens.get(self.board_sel.column).copied().unwrap_or(0);
        self.board_sel.index = self.board_sel.index.min(len.saturating_sub(1));
    }

    /// Remembers the item under the cursor.
    fn sync_selected_id(&mut self) {
        self.selected_id = self.selected_item().map(|i| i.id.clone());
    }

    /// Re-finds the remembered item after the data or rows changed; clamps when it is gone.
    fn restore_selection(&mut self) {
        if let Some(id) = self.selected_id.clone() {
            let row = self
                .table_rows()
                .iter()
                .position(|r| matches!(r, Row::Item(i) if i.id == id));
            let cell = self
                .board_columns()
                .iter()
                .enumerate()
                .find_map(|(c, col)| col.items().iter().position(|i| i.id == id).map(|i| (c, i)));
            if let Some(row) = row {
                self.table_selected = row;
            }
            if let Some((column, index)) = cell {
                self.board_sel = BoardSelection { column, index };
            }
        }
        self.clamp_selection();
    }

    fn switch_view(&mut self, forward: bool) -> Vec<Command> {
        let Some(project) = self.project() else {
            return Vec::new();
        };
        let n = project.views.len();
        if n == 0 {
            return Vec::new();
        }
        let pos = self
            .current_view()
            .and_then(|cur| project.views.iter().position(|v| v.id == cur.id))
            .unwrap_or(0);
        let next = if forward {
            (pos + 1) % n
        } else {
            (pos + n - 1) % n
        };
        let id = project.views[next].id.clone();
        self.current = Some(id.clone());
        self.sticky.clear();
        self.table_selected = 0;
        self.board_sel = BoardSelection::default();
        self.sync_selected_id();
        let mut cmds = vec![Command::SaveLastView(id)];
        // The cached list shows at once; a fresh one replaces it (spec section 5, "Views").
        cmds.extend(self.revalidate_view());
        cmds
    }

    fn move_selection(&mut self, action: Action) {
        if self.layout().0 == Layout::Board {
            let lens: Vec<usize> = self.board_columns().iter().map(|c| c.len()).collect();
            if lens.is_empty() {
                return;
            }
            let s = &mut self.board_sel;
            match action {
                // Left and right skip empty columns; at the last populated one they stay put.
                Action::Left => {
                    if let Some(c) = (0..s.column).rev().find(|&c| lens[c] > 0) {
                        s.column = c;
                    }
                }
                Action::Right => {
                    if let Some(c) = (s.column + 1..lens.len()).find(|&c| lens[c] > 0) {
                        s.column = c;
                    }
                }
                Action::Up => s.index = s.index.saturating_sub(1),
                Action::Down => s.index += 1,
                Action::Top => s.index = 0,
                Action::Bottom => s.index = usize::MAX,
                _ => {}
            }
            s.index = s.index.min(lens[s.column].saturating_sub(1));
        } else {
            let rows = self.table_rows().len();
            let s = &mut self.table_selected;
            match action {
                Action::Up => *s = s.saturating_sub(1),
                Action::Down => *s += 1,
                Action::Top => *s = 0,
                Action::Bottom => *s = usize::MAX,
                _ => {}
            }
            *s = (*s).min(rows.saturating_sub(1));
        }
    }

    fn toggle_group(&mut self) {
        let mut header = None;
        for (i, row) in self
            .table_rows()
            .iter()
            .enumerate()
            .take(self.table_selected + 1)
            .rev()
        {
            if let Row::Group { key: k, .. } = row {
                header = Some((i, collapse_key(k)));
                break;
            }
        }
        if let Some((index, k)) = header
            && !self.collapsed.remove(&k)
        {
            self.collapsed.insert(k);
            // The rows under the header vanish: land on the header, not another group.
            self.table_selected = index;
        }
        self.sync_selected_id();
        self.restore_selection();
    }

    fn open_detail(&mut self, item: ItemId) -> Vec<Command> {
        self.detail = Some(DetailState::new(item.clone()));
        self.mode = Mode::Detail;
        // A new item has nothing on GitHub to load yet; it loads once created (`settle`).
        if is_placeholder(&item) {
            return Vec::new();
        }
        vec![Command::LoadDetail { item, before: None }]
    }

    fn follow(&mut self, target: Target) -> Vec<Command> {
        match target {
            Target::Link(url) => vec![Command::OpenUrl(url)],
            Target::IssueRef(n) => {
                let repo = self
                    .detail_item()
                    .and_then(|i| i.reference())
                    .map(|r| r.container.clone());
                let Some(repo) = repo else { return Vec::new() };
                let on_board = self.snapshot().and_then(|s| {
                    s.items
                        .values()
                        .find(|i| {
                            i.reference()
                                .is_some_and(|r| r.container == repo && r.number == n)
                        })
                        .map(|i| i.id.clone())
                });
                match on_board {
                    Some(id) => self.open_detail(id),
                    None => vec![Command::OpenUrl(format!(
                        "https://github.com/{repo}/issues/{n}"
                    ))],
                }
            }
            Target::Key(key) => {
                let on_board = self.snapshot().and_then(|s| {
                    s.items
                        .values()
                        .find(|i| i.reference().and_then(|r| r.key.as_ref()) == Some(&key))
                        .map(|i| i.id.clone())
                });
                match (on_board, self.project().and_then(|p| p.linear.as_ref())) {
                    (Some(id), _) => self.open_detail(id),
                    (None, Some(linear)) => vec![Command::OpenUrl(format!(
                        "https://linear.app/{}/issue/{key}",
                        linear.url_key
                    ))],
                    (None, None) => Vec::new(),
                }
            }
        }
    }

    fn detail_targets(&self) -> Vec<Target> {
        match (self.detail.as_ref(), self.detail_item(), self.project()) {
            (Some(state), Some(item), Some(project)) => {
                build_doc(item, project, state, self.detail_width, &self.theme).targets
            }
            _ => Vec::new(),
        }
    }

    fn handle_text_input(&mut self, key: &KeyEvent) -> Option<bool> {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
                Some(false)
            }
            KeyCode::Backspace => {
                self.input.pop();
                Some(false)
            }
            KeyCode::Enter => Some(true),
            KeyCode::Esc => None,
            _ => Some(false),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Command> {
        self.status.flash = None;
        self.highlight = None;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return vec![Command::Quit];
        }
        let action = self.keymap.action(&key);
        match self.mode {
            Mode::Search => {
                // The anchor stays on the item that was selected when typing began, so
                // clearing the search returns to it; Enter re-anchors on what is visible.
                let committed = match self.handle_text_input(&key) {
                    Some(true) => {
                        self.search = std::mem::take(&mut self.input);
                        self.mode = Mode::Normal;
                        true
                    }
                    None => {
                        self.input.clear();
                        self.search.clear();
                        self.mode = Mode::Normal;
                        false
                    }
                    Some(false) => false,
                };
                self.restore_selection();
                if committed {
                    self.sync_selected_id();
                }
                Vec::new()
            }
            Mode::Filter => match self.handle_text_input(&key) {
                Some(true) => {
                    self.mode = Mode::Normal;
                    let extra = std::mem::take(&mut self.input);
                    let cmds = self.set_extra_filter(&extra);
                    self.flash_unknown_keys();
                    cmds
                }
                None => {
                    self.input.clear();
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                Some(false) => Vec::new(),
            },
            Mode::Help => {
                // Too tall for the pane: the up and down keys scroll, anything else closes.
                let max = Action::ALL.len().saturating_sub(self.help_visible);
                match action {
                    Some(Action::Down) if max > 0 => {
                        self.help_scroll = (self.help_scroll + 1).min(max);
                        return Vec::new();
                    }
                    Some(Action::Up) if max > 0 => {
                        self.help_scroll = self.help_scroll.saturating_sub(1);
                        return Vec::new();
                    }
                    _ => {}
                }
                self.mode = Mode::Normal;
                if action == Some(Action::Quit) {
                    return self.request_quit();
                }
                Vec::new()
            }
            Mode::Setup => match key.code {
                KeyCode::Char('r') => vec![Command::Retry],
                // Esc never quits; the keymap's Quit does, asking first while edits are sending.
                _ if action == Some(Action::Quit) => self.request_quit(),
                _ if action == Some(Action::PickBoard) && self.setup_offers_boards => {
                    self.pick_board()
                }
                _ => Vec::new(),
            },
            Mode::Picker => self.handle_picker_key(key),
            Mode::Confirm => self.handle_confirm_key(&key),
            Mode::Popup => self.handle_popup_key(key),
            Mode::ErrorLog if action.is_none() && key.code == KeyCode::Char('d') => {
                self.dismiss_selected();
                Vec::new()
            }
            Mode::ErrorLog => self.handle_error_log_key(action),
            Mode::Detail => {
                if let Some(a) = action
                    && let Some(cmds) = self.edit_key(a)
                {
                    return cmds;
                }
                let targets = self.detail_targets();
                let Some(state) = self.detail.as_mut() else {
                    self.mode = Mode::Normal;
                    return Vec::new();
                };
                match state.handle(action, targets.len()) {
                    DetailOutcome::None => Vec::new(),
                    DetailOutcome::Close => {
                        self.detail = None;
                        self.mode = Mode::Normal;
                        Vec::new()
                    }
                    DetailOutcome::Quit => self.request_quit(),
                    DetailOutcome::Follow(i) => self.follow(targets[i].clone()),
                    DetailOutcome::LoadOlder(cursor) => vec![Command::LoadDetail {
                        item: state.item.clone(),
                        before: Some(cursor),
                    }],
                    DetailOutcome::OpenInBrowser => self
                        .detail_item()
                        .and_then(|i| i.url())
                        .map(|u| vec![Command::OpenUrl(u.to_string())])
                        .unwrap_or_default(),
                }
            }
            Mode::Normal => {
                if let Some(a) = action
                    && let Some(cmds) = self.edit_key(a)
                {
                    return cmds;
                }
                match action {
                    Some(
                        a @ (Action::Up
                        | Action::Down
                        | Action::Left
                        | Action::Right
                        | Action::Top
                        | Action::Bottom),
                    ) => {
                        self.move_selection(a);
                        self.sync_selected_id();
                        Vec::new()
                    }
                    Some(Action::NextView) => self.switch_view(true),
                    Some(Action::PrevView) => self.switch_view(false),
                    Some(Action::ToggleLayout) => {
                        if let Some(view) = self.current_view().cloned() {
                            let next = if self.layout().0 == Layout::Board {
                                Layout::Table
                            } else {
                                Layout::Board
                            };
                            self.sync_selected_id();
                            let cmd = if next == view.layout {
                                self.layout_toggle.remove(&view.id);
                                Command::ClearLayout { view: view.id }
                            } else {
                                self.layout_toggle.insert(view.id.clone(), next);
                                Command::SaveLayout {
                                    view: view.id,
                                    layout: next,
                                    server_layout: view.layout,
                                }
                            };
                            self.restore_selection();
                            return vec![cmd];
                        }
                        Vec::new()
                    }
                    Some(Action::Open) => {
                        if self.layout().0 == Layout::Table
                            && matches!(
                                self.table_rows().get(self.table_selected),
                                Some(Row::Group { .. })
                            )
                        {
                            self.toggle_group();
                            return Vec::new();
                        }
                        match self.selected_item().map(|i| i.id.clone()) {
                            Some(id) => self.open_detail(id),
                            None => Vec::new(),
                        }
                    }
                    Some(Action::ToggleGroup) => {
                        self.toggle_group();
                        Vec::new()
                    }
                    Some(Action::Search) => {
                        self.sync_selected_id();
                        self.input = self.search.clone();
                        self.mode = Mode::Search;
                        Vec::new()
                    }
                    Some(Action::Filter) => {
                        self.input = self.extra_filter().unwrap_or_default().to_string();
                        self.mode = Mode::Filter;
                        Vec::new()
                    }
                    Some(Action::ClearFilter) if self.extra_filter().is_some() => {
                        self.set_extra_filter("")
                    }
                    Some(Action::Refresh) => {
                        self.sticky.clear();
                        self.clear_lookups();
                        vec![Command::Refresh]
                    }
                    Some(Action::OpenBrowser) => self.open_in_browser(),
                    Some(Action::Help) => {
                        self.help_scroll = 0;
                        self.mode = Mode::Help;
                        Vec::new()
                    }
                    Some(Action::Back) => {
                        self.search.clear();
                        self.status.error = None;
                        self.sync_selected_id();
                        self.restore_selection();
                        Vec::new()
                    }
                    Some(Action::PickBoard) => self.pick_board(),
                    Some(Action::Quit) => self.request_quit(),
                    _ => Vec::new(),
                }
            }
        }
    }

    /// `B`: the board picker, unless edits are still sending.
    fn pick_board(&mut self) -> Vec<Command> {
        let n = self.pending_count();
        if n > 0 {
            self.status.flash = Some(format!(
                "{n} edit{} still sending; switch boards when {} done",
                if n == 1 { " is" } else { "s are" },
                if n == 1 { "it is" } else { "they are" }
            ));
            return Vec::new();
        }
        vec![Command::ShowPicker]
    }

    fn handle_picker_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let Some(picker) = self.picker.as_mut() else {
            self.mode = Mode::Normal;
            return Vec::new();
        };
        match picker.handle(&key) {
            PickerOutcome::None => Vec::new(),
            // Opened from the setup screen, the picker goes back to it: a chosen board leaves
            // it once it opens (`reset_for_new_board`).
            PickerOutcome::Chosen(board) => {
                self.picker = None;
                self.mode = self.after_picker();
                vec![Command::PickBoard(board)]
            }
            PickerOutcome::Cancel => {
                self.picker = None;
                self.mode = self.after_picker();
                Vec::new()
            }
        }
    }

    fn after_picker(&self) -> Mode {
        if self.setup.is_some() {
            Mode::Setup
        } else {
            Mode::Normal
        }
    }

    /// The list request a view currently makes, looked up by id: `None` for an unknown
    /// view, `Some(None)` for one that lists everything.
    fn query_of(&self, id: &ViewId) -> Option<Option<ListQuery>> {
        let view = self.project()?.views.iter().find(|v| &v.id == id)?;
        Some(self.list_query(view))
    }

    pub fn on_sync(&mut self, event: SyncEvent) -> Vec<Command> {
        // A late answer for a request that has since changed must not replace the list.
        if let SyncEvent::ViewIds { view, query, .. } = &event
            && self
                .query_of(view)
                .is_some_and(|current| current.as_ref() != Some(query))
        {
            return Vec::new();
        }
        self.sync_selected_id();
        let query_before = self.current_query();
        if let Some(update) = store_update(&event) {
            self.store.apply(update);
        }
        if matches!(event, SyncEvent::Project(_)) && self.current_query() != query_before {
            self.sticky.clear();
        }
        // The first snapshot has just arrived: anchor the cursor so an edit key pressed
        // before any navigation key still makes the cursor follow its item.
        if self.selected_id.is_none() {
            self.sync_selected_id();
        }
        let mut cmds = Vec::new();
        match event {
            SyncEvent::Project(_) => cmds.extend(self.view_needs_ids()),
            SyncEvent::ItemsPage { loaded, total, .. } => {
                self.status.loading = Some((loaded, total))
            }
            SyncEvent::ItemsComplete {
                fetched_at,
                total,
                truncated,
                ..
            } => {
                self.store.apply(StoreUpdate::FetchedAt(fetched_at));
                self.status.loading = None;
                self.status.stale = false;
                self.status.error = None;
                self.status.notes.retain(|n| !n.starts_with("loaded "));
                if truncated {
                    let loaded = self.snapshot().map(|s| s.items.len()).unwrap_or(0);
                    // Linear has no count: a total of 0 means unknown, shown as `N+`.
                    let note = if total > loaded {
                        format!("loaded {loaded} of {total} items (max_items)")
                    } else {
                        format!("loaded {loaded}+ items (max_items)")
                    };
                    self.status.notes.push(note);
                }
                cmds.extend(self.revalidate_view());
            }
            SyncEvent::ItemsUpdated {
                items,
                removed,
                fetched_at,
            } => {
                self.store.apply(StoreUpdate::FetchedAt(fetched_at));
                self.status.error = None;
                if !items.is_empty() || !removed.is_empty() {
                    cmds.extend(self.revalidate_view());
                }
            }
            SyncEvent::Detail {
                item,
                detail,
                older,
            } => {
                if let Some(state) = self.detail.as_mut().filter(|s| s.item == item) {
                    state.set_detail(detail, older);
                }
            }
            SyncEvent::Projects { boards, notes } => self.show_projects(boards, notes),
            SyncEvent::Rate(rate) => self.status.rate_low = rate.is_low(),
            SyncEvent::Failed { task, error } => {
                if self.lookup_failed(&task, &error) {
                    self.restore_selection();
                    return cmds;
                }
                if matches!(task, SyncTask::FullLoad | SyncTask::Resolve) {
                    self.status.loading = None;
                }
                if let Some(mut message) = setup_message(&error) {
                    // A failed board list has no board to offer; any other failure is the
                    // board's, and another provider's boards may still open.
                    self.setup_offers_boards = !matches!(task, SyncTask::Projects);
                    if self.setup_offers_boards
                        && let Some(k) = self.keymap.keys_for(Action::PickBoard).first()
                    {
                        message
                            .push_str(&format!("\nOr press {} to open another board.", k.label()));
                    }
                    self.setup = Some(message);
                    self.mode = Mode::Setup;
                } else if let (SyncTask::Detail(id), Some(state)) = (&task, self.detail.as_mut()) {
                    if &state.item == id {
                        state.loading_older = false;
                        state.error = Some(format!("{}: {error}", task_words(&task)));
                    }
                } else {
                    self.status.error = Some(format!("{}: {error}", task_words(&task)));
                }
            }
            SyncEvent::OpDone { id, result } => {
                cmds.extend(self.settle(id, result));
                cmds.extend(self.refresh_quit_question());
            }
            SyncEvent::Access { scopes } => self.set_scopes(scopes),
            SyncEvent::Candidates { repo, candidates } => {
                self.candidates.insert(repo, Lookup::Ready(candidates));
                self.refresh_popup();
            }
            SyncEvent::TeamCandidates { team, candidates } => {
                self.team_candidates.insert(team, Lookup::Ready(candidates));
                self.refresh_popup();
            }
            SyncEvent::IssueFound { key, item } => cmds.extend(self.issue_found(&key, item)),
            SyncEvent::Repos(list) => {
                self.repos = Some(Lookup::Ready(list));
                self.refresh_popup();
            }
            SyncEvent::RepoFound { slug, repo } => {
                self.repo_lookups.insert(slug, Lookup::Ready(repo));
                self.refresh_popup();
            }
            SyncEvent::SearchResults { query, hits } => {
                if self
                    .content_search
                    .as_ref()
                    .is_some_and(|(q, _)| q == &query)
                {
                    self.content_search = Some((query, Lookup::Ready(hits)));
                }
                self.refresh_popup();
            }
            SyncEvent::ViewIds { .. } | SyncEvent::Hydrated(_) | SyncEvent::RepoProjects(_) => {}
        }
        // A poll may have dropped the item the detail shows.
        self.close_gone_detail();
        self.restore_selection();
        cmds
    }

    fn show_projects(&mut self, list: Vec<ProjectSummary>, notes: Vec<String>) {
        // No picker is waiting: the user cancelled it, or is reading a board (maybe in
        // detail). Only with no board at all is a picker still required.
        if self.picker.is_none() && self.snapshot().is_some() {
            return;
        }
        let required = self
            .picker
            .as_ref()
            .map(|p| p.required)
            .unwrap_or(self.snapshot().is_none());
        let typed = self.picker.take().map(|p| p.input).unwrap_or_default();
        let mut picker = PickerState::with(list, notes, required);
        picker.input = typed;
        self.picker = Some(picker);
        self.mode = Mode::Picker;
    }

    pub(crate) fn view_list(&self) -> Option<&ViewList> {
        self.snapshot()?.views.get(&self.current_view()?.id)
    }
}

/// An App over the UI fixture board, for tests in this module's children.
#[cfg(test)]
pub(crate) fn test_app() -> App {
    App::new(
        Box::new(crate::store::MemoryStore::new(Some(
            crate::ui::fixtures::snapshot(),
        ))),
        Keymap::defaults(),
        Theme::plain(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use crate::ui::fixtures::{code, key, snapshot};
    use crossterm::event::KeyCode;

    fn app() -> App {
        App::new(
            Box::new(MemoryStore::new(Some(snapshot()))),
            Keymap::defaults(),
            Theme::plain(),
        )
    }

    fn selected_id(app: &App) -> String {
        app.selected_item()
            .map(|i| i.id.0.clone())
            .unwrap_or_default()
    }

    #[test]
    fn picker_choice_becomes_a_command_and_required_esc_stays_but_ctrl_c_quits() {
        let mut a = app();
        a.picker = Some(PickerState::loading(false)); // `B` was pressed
        a.on_sync(SyncEvent::Projects {
            boards: vec![ProjectSummary {
                id: ProjectId::new("x"),
                board: "acme/7".parse().unwrap(),
                title: "Sprint".into(),
                closed: false,
            }],
            notes: vec![],
        });
        assert_eq!(a.mode, Mode::Picker);
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::PickBoard("acme/7".parse().unwrap())]
        );
        let mut empty = App::new(
            Box::new(MemoryStore::new(None)),
            Keymap::defaults(),
            Theme::plain(),
        );
        empty.on_sync(SyncEvent::Projects {
            boards: vec![],
            notes: vec![],
        });
        assert_eq!(empty.handle_key(code(KeyCode::Esc)), Vec::<Command>::new());
        assert_eq!(empty.mode, Mode::Picker);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(empty.handle_key(ctrl_c), vec![Command::Quit]);
    }

    #[test]
    fn a_late_board_list_does_not_reopen_a_cancelled_picker_or_leave_detail() {
        let list = || SyncEvent::Projects {
            boards: vec![ProjectSummary {
                id: ProjectId::new("x"),
                board: "acme/7".parse().unwrap(),
                title: "Sprint".into(),
                closed: false,
            }],
            notes: vec![],
        };
        let mut a = app();
        a.picker = Some(PickerState::loading(false));
        a.mode = Mode::Picker;
        a.handle_key(code(KeyCode::Esc));
        assert_eq!((a.mode, a.picker.is_none()), (Mode::Normal, true));
        a.on_sync(list());
        assert_eq!(a.mode, Mode::Normal, "a cancelled picker stays closed");
        assert!(a.picker.is_none());

        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        a.on_sync(list());
        assert_eq!(a.mode, Mode::Detail, "the user stays in detail");

        a.picker = Some(PickerState::loading(false));
        a.mode = Mode::Picker;
        a.on_sync(list());
        assert_eq!(
            a.picker.as_ref().unwrap().visible().len(),
            1,
            "a waiting picker fills"
        );
    }

    #[test]
    fn filter_typed_while_loading_survives_the_list_arriving() {
        let mut a = App::new(
            Box::new(MemoryStore::new(None)),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.picker = Some(PickerState::loading(true));
        a.mode = Mode::Picker;
        a.handle_key(key('s'));
        a.on_sync(SyncEvent::Projects {
            boards: vec![],
            notes: vec![],
        });
        assert_eq!(a.picker.as_ref().unwrap().input, "s");
    }

    #[test]
    fn moves_through_table_rows() {
        let mut a = app();
        assert_eq!(selected_id(&a), "a");
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        assert_eq!(selected_id(&a), "c");
        a.handle_key(code(KeyCode::End));
        assert_eq!(selected_id(&a), "e");
        a.handle_key(key('j'));
        assert_eq!(selected_id(&a), "e", "stops at the end");
    }

    #[test]
    fn switching_views_saves_and_requests_filtered_ids() {
        let mut a = app();
        let cmds = a.handle_key(code(KeyCode::Tab));
        assert_eq!(a.current_view().unwrap().name, "Board");
        assert!(cmds.contains(&Command::SaveLastView(ViewId::new("V_board"))));
        let cmds = a.handle_key(code(KeyCode::Tab));
        assert!(cmds.contains(&Command::FetchViewIds {
            view: ViewId::new("V_bugs"),
            query: ListQuery::github("label:bug")
        }));
        assert!(a.view_items().is_none(), "filtered view has no ids yet");
    }

    #[test]
    fn board_navigation_moves_between_columns() {
        let mut a = app();
        a.handle_key(code(KeyCode::Tab));
        assert_eq!(a.layout().0, Layout::Board);
        assert_eq!(selected_id(&a), "a");
        a.handle_key(key('l'));
        assert_eq!(selected_id(&a), "b");
        a.handle_key(key('l'));
        a.handle_key(key('l'));
        assert_eq!(selected_id(&a), "e", "No Status column");
        a.handle_key(key('h'));
        assert_eq!(selected_id(&a), "d");
    }

    #[test]
    fn left_and_right_skip_empty_board_columns() {
        use crate::model::field::{FieldKind, OptionColor, SelectOption};
        use crate::model::ids::OptionId;
        // Two empty options between the first and second columns.
        let mut snap = snapshot();
        let status = snap
            .project
            .fields
            .iter_mut()
            .find(|f| f.name == "Status")
            .unwrap();
        let FieldKind::SingleSelect { options } = &mut status.kind else {
            panic!("Status is single-select")
        };
        for (i, name) in ["Empty one", "Empty two"].into_iter().enumerate() {
            options.insert(
                1 + i,
                SelectOption {
                    id: OptionId::new(format!("o_empty{i}")),
                    name: name.into(),
                    color: OptionColor::Gray,
                    kind: None,
                },
            );
        }
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.handle_key(code(KeyCode::Tab));
        assert_eq!(a.layout().0, Layout::Board);
        assert_eq!(selected_id(&a), "a");
        a.handle_key(key('l'));
        assert_eq!(selected_id(&a), "b", "skipped both empty columns");
        a.handle_key(key('h'));
        assert_eq!(selected_id(&a), "a", "and back over them");
        a.handle_key(key('h'));
        assert_eq!(
            selected_id(&a),
            "a",
            "nothing populated further left: stays"
        );
    }

    #[test]
    fn the_board_hides_status_columns_its_filter_excludes() {
        let mut snap = snapshot();
        snap.project.views[1].filter = "-status:Done,\"In Progress\"".into();
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.select_view(&ViewId::new("V_board"));
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_board"),
            query: ListQuery::github("-status:Done,\"In Progress\""),
            list: ViewList {
                ids: vec![ItemId::new("a"), ItemId::new("c"), ItemId::new("e")],
                total: 3,
                truncated: false,
            },
        });
        let titles: Vec<String> = a
            .board_columns()
            .iter()
            .map(|c| c.bucket.title.clone())
            .collect();
        assert_eq!(titles, ["Todo", "No Status"]);
        a.handle_key(key('l'));
        assert_eq!(selected_id(&a), "e", "moves straight to No Status");
    }

    #[test]
    fn board_cards_show_the_views_fields_but_not_its_column_or_lane_field() {
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snapshot()))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.select_view(&ViewId::new("V_board"));
        let names: Vec<&str> = a.card_fields().iter().map(|f| f.name.as_str()).collect();
        // Status is the column field and Priority the lane field.
        assert_eq!(names, ["Linked pull requests", "Created"]);
    }

    #[test]
    fn layout_toggle_is_per_view() {
        let mut a = app();
        a.handle_key(key('L'));
        assert_eq!(a.layout().0, Layout::Board);
        a.handle_key(code(KeyCode::Tab));
        a.handle_key(code(KeyCode::BackTab));
        assert_eq!(a.layout().0, Layout::Board, "remembered for the session");
    }

    #[test]
    fn toggling_the_layout_saves_it_and_toggling_back_clears_it() {
        let mut a = app();
        let cmds = a.handle_key(key('L'));
        assert_eq!(
            cmds,
            vec![Command::SaveLayout {
                view: ViewId::new("V_table"),
                layout: Layout::Board,
                server_layout: Layout::Table,
            }]
        );
        let cmds = a.handle_key(key('L'));
        assert_eq!(
            cmds,
            vec![Command::ClearLayout {
                view: ViewId::new("V_table")
            }]
        );
        assert_eq!(a.layout().0, Layout::Table);
    }

    #[test]
    fn quick_search_filters_live_and_esc_clears() {
        let mut a = app();
        a.handle_key(key('/'));
        assert_eq!(a.mode, Mode::Search);
        for c in "emoji".chars() {
            a.handle_key(key(c));
        }
        assert_eq!(a.view_items().unwrap().len(), 1);
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.view_items().unwrap().len(), 1, "committed");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(
            a.view_items().unwrap().len(),
            5,
            "esc clears a committed search"
        );
    }

    #[test]
    fn filter_edit_requests_ids_and_empty_filter_shows_all() {
        let mut a = app();
        a.handle_key(key('f'));
        assert_eq!(a.mode, Mode::Filter);
        for c in "label:bug".chars() {
            a.handle_key(key(c));
        }
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::FetchViewIds {
                view: ViewId::new("V_table"),
                query: ListQuery::github("label:bug")
            }]
        );
        assert_eq!(a.effective_filter(), "label:bug");
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_table"),
            query: ListQuery::github("label:bug"),
            list: ViewList {
                ids: vec![ItemId::new("a")],
                total: 1,
                truncated: false,
            },
        });
        assert_eq!(a.view_items().unwrap().len(), 1);
    }

    fn type_text(a: &mut App, text: &str) {
        for c in text.chars() {
            a.handle_key(key(c));
        }
    }

    #[test]
    fn f_edits_a_local_filter_added_to_the_github_one() {
        let mut a = app();
        let bugs = ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.handle_key(key('f'));
        assert_eq!(a.input, "", "the GitHub filter is not in the input");
        type_text(&mut a, " assignee:x ");
        let cmds = a.handle_key(code(KeyCode::Enter));
        let fetch = |filter: &str| {
            vec![Command::FetchViewIds {
                view: bugs.clone(),
                query: ListQuery::github(filter),
            }]
        };
        assert_eq!(cmds, fetch("label:bug assignee:x"));
        assert_eq!(a.effective_filter(), "label:bug assignee:x");
        assert_eq!(a.extra_filter(), Some("assignee:x"));

        a.handle_key(key('f'));
        assert_eq!(
            a.input, "assignee:x",
            "pre-filled with the local filter only"
        );
        a.handle_key(key('y'));
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(a.effective_filter(), "label:bug assignee:x", "esc cancels");

        a.handle_key(key('f'));
        for _ in 0.."assignee:x".len() {
            a.handle_key(code(KeyCode::Backspace));
        }
        assert_eq!(a.handle_key(code(KeyCode::Enter)), fetch("label:bug"));
        assert_eq!(a.effective_filter(), "label:bug");
        assert_eq!(a.extra_filter(), None, "an empty input clears it");
    }

    #[test]
    fn x_clears_the_local_filter_and_does_nothing_without_one() {
        let mut a = app();
        let bugs = ViewId::new("V_bugs");
        a.select_view(&bugs);
        assert!(a.handle_key(key('x')).is_empty());
        assert_eq!(a.effective_filter(), "label:bug");
        a.handle_key(key('f'));
        type_text(&mut a, "assignee:x");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            a.handle_key(key('x')),
            vec![Command::FetchViewIds {
                view: bugs,
                query: ListQuery::github("label:bug")
            }]
        );
        assert_eq!(a.extra_filter(), None);

        // On a view without a GitHub filter, clearing shows the whole board again.
        let mut b = app();
        b.handle_key(key('f'));
        type_text(&mut b, "label:bug");
        b.handle_key(code(KeyCode::Enter));
        assert!(b.view_items().is_none());
        assert!(b.handle_key(key('x')).is_empty());
        assert_eq!(b.view_items().unwrap().len(), 5);
    }

    #[test]
    fn a_late_list_for_the_github_filter_alone_is_dropped_once_a_local_one_is_added() {
        let mut a = app();
        let bugs = ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.handle_key(key('f'));
        type_text(&mut a, "assignee:x");
        a.handle_key(code(KeyCode::Enter));
        let ids = |filter: &str, id: &str| SyncEvent::ViewIds {
            view: bugs.clone(),
            query: ListQuery::github(filter),
            list: ViewList {
                ids: vec![ItemId::new(id)],
                total: 1,
                truncated: false,
            },
        };
        a.on_sync(ids("label:bug", "a"));
        assert!(a.view_items().is_none(), "stale list dropped");
        a.on_sync(ids("label:bug assignee:x", "c"));
        assert_eq!(selected_id(&a), "c");
    }

    fn linear_app() -> (App, ViewId, ViewId) {
        let mut snap = snapshot();
        let mut view = snap.project.views[0].clone();
        snap.project.board = "linear:team:t1".parse().unwrap();
        view.id = ViewId::new("V_all");
        view.name = "All issues".into();
        view.filter = ViewFilter::Linear(serde_json::json!({}));
        let mut bugs = view.clone();
        bugs.id = ViewId::new("V_bugs");
        bugs.name = "Bugs".into();
        bugs.filter = ViewFilter::LinearView {
            id: "cv1".into(),
            hide_closed: false,
        };
        snap.project.views = vec![view, bugs];
        let app = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        (app, ViewId::new("V_all"), ViewId::new("V_bugs"))
    }

    #[test]
    fn linear_views_ask_for_lists_by_their_view_filter() {
        let (mut a, all, bugs) = linear_app();
        a.select_view(&all);
        let cmds = a.switch_view(true);
        let bugs_filter = ViewFilter::LinearView {
            id: "cv1".into(),
            hide_closed: false,
        };
        assert!(cmds.contains(&Command::FetchViewIds {
            view: bugs.clone(),
            query: ListQuery {
                filter: bugs_filter,
                include_archived: false,
            },
        }));
        let cmds = a.switch_view(true);
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, Command::FetchViewIds { .. }))
        );
        let expected = a.snapshot().unwrap().all_items().len()
            - a.snapshot()
                .unwrap()
                .all_items()
                .iter()
                .filter(|i| i.archived)
                .count();
        assert_eq!(a.view_items().unwrap().len(), expected);
    }

    #[test]
    fn a_stale_list_answer_is_dropped() {
        let (mut a, _, bugs) = linear_app();
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            query: ListQuery {
                filter: ViewFilter::LinearView {
                    id: "other".into(),
                    hide_closed: false,
                },
                include_archived: false,
            },
            list: ViewList {
                ids: vec![ItemId::new("a")],
                total: 1,
                truncated: false,
            },
        });
        assert!(!a.snapshot().unwrap().views.contains_key(&bugs));
    }

    /// A Linear team board, hand-built from the UI fixture: the viewer is `U_tviles` (item
    /// `a`'s assignee), the Sprint field is the team's Cycle field with cycles around today
    /// (`a` and `c` in the current one, `b` in the next, `e` in the previous), and `z` is an
    /// archived bug. Views: All issues, Bugs (a custom view) and Cycles (a board by cycle).
    fn linear_board() -> App {
        let mut snap = snapshot();
        let today = time::OffsetDateTime::now_utc().date();
        let day = |offset: i64| (today + time::Duration::days(offset)).to_string();
        let cycle = |id: &str, title: &str, start: i64| Iteration {
            id: IterationId::new(id),
            title: title.into(),
            start_date: day(start),
            duration_days: 28,
        };
        let p = &mut snap.project;
        p.board = "linear:team:t1".parse().unwrap();
        p.linear = Some(LinearMeta {
            url_key: "acme".into(),
            viewer_id: "U_tviles".into(),
            team_keys: vec!["TBA".into()],
            ..Default::default()
        });
        let sprint = p
            .fields
            .iter_mut()
            .find(|f| f.id == FieldId::new("F_sprint"))
            .unwrap();
        sprint.name = "Cycle".into();
        sprint.kind = FieldKind::Iteration {
            iterations: vec![cycle("c5", "Cycle 5", -7), cycle("c6", "Cycle 6", 21)],
            completed: vec![cycle("c4", "Cycle 4", -35)],
        };
        let mut all = p.views[0].clone();
        all.id = ViewId::new("V_all");
        all.name = "All issues".into();
        all.filter = ViewFilter::Linear(serde_json::json!({}));
        let mut bugs = all.clone();
        bugs.id = ViewId::new("V_bugs");
        bugs.name = "Bugs".into();
        bugs.filter = ViewFilter::LinearView {
            id: "cv1".into(),
            hide_closed: false,
        };
        let mut cycles = p.views[1].clone();
        cycles.id = ViewId::new("V_cycles");
        cycles.name = "Cycles".into();
        cycles.filter = ViewFilter::Linear(serde_json::json!({}));
        cycles.vertical_group_by = vec![FieldId::new("F_sprint")];
        cycles.group_by = Vec::new();
        p.views = vec![all, bugs, cycles];
        let in_cycle = |id: &str, title: &str| FieldValue::Iteration {
            iteration_id: IterationId::new(id),
            title: title.into(),
            start_date: String::new(),
        };
        for (item, id, title) in [
            ("a", "c5", "Cycle 5"),
            ("c", "c5", "Cycle 5"),
            ("b", "c6", "Cycle 6"),
            ("e", "c4", "Cycle 4"),
        ] {
            snap.items
                .get_mut(&ItemId::new(item))
                .unwrap()
                .values
                .insert(FieldId::new("F_sprint"), in_cycle(id, title));
        }
        let mut z = snap.items[&ItemId::new("a")].clone();
        z.id = ItemId::new("z");
        z.archived = true;
        snap.upsert_items(vec![z]);
        App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        )
    }

    fn ids(items: Option<Vec<&Item>>) -> Vec<String> {
        items
            .unwrap_or_default()
            .iter()
            .map(|i| i.id.0.clone())
            .collect()
    }

    fn fetches(cmds: &[Command]) -> Vec<&Command> {
        cmds.iter()
            .filter(|c| matches!(c, Command::FetchViewIds { .. }))
            .collect()
    }

    #[test]
    fn linear_f_filters_on_the_client_without_a_request() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_all"));
        assert_eq!(ids(a.view_items()), ["a", "b", "c", "d", "e"]);
        a.handle_key(key('f'));
        type_text(&mut a, "label:bug");
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert!(fetches(&cmds).is_empty(), "{cmds:?}");
        assert_eq!(a.status.flash, None);
        assert_eq!(ids(a.view_items()), ["a"], "the archived bug stays hidden");
        assert_eq!(a.list_query(a.current_view().unwrap()), None);

        // `@me` is the board's viewer; a custom view's own list is filtered further.
        a.handle_key(key('x'));
        a.select_view(&ViewId::new("V_bugs"));
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_bugs"),
            query: ListQuery {
                filter: ViewFilter::LinearView {
                    id: "cv1".into(),
                    hide_closed: false,
                },
                include_archived: false,
            },
            list: ViewList {
                ids: vec![ItemId::new("a"), ItemId::new("b"), ItemId::new("c")],
                total: 0,
                truncated: false,
            },
        });
        a.handle_key(key('f'));
        type_text(&mut a, "assignee:@me");
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert!(
            fetches(&cmds).is_empty(),
            "the view's list is unchanged: {cmds:?}"
        );
        assert_eq!(ids(a.view_items()), ["a"]);
    }

    #[test]
    fn linear_unknown_filter_field_flashes_and_shows_nothing() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_all"));
        a.handle_key(key('f'));
        type_text(&mut a, "stauts:Todo label:bug prority:High no:");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            a.status.flash.as_deref(),
            Some("unknown filter field: stauts, prority")
        );
        assert_eq!(ids(a.view_items()), Vec::<String>::new());
        a.handle_key(key('j'));
        assert_eq!(a.status.flash, None, "the next key clears it");
    }

    #[test]
    fn linear_is_typo_flashes_unknown_value() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_all"));
        a.handle_key(key('f'));
        type_text(&mut a, "is:closd is:open");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            a.status.flash.as_deref(),
            Some("unknown filter value: is:closd")
        );
    }

    #[test]
    fn the_linear_board_tab_hides_empty_canceled_and_duplicate_columns() {
        use crate::model::field::{OptionKind, SelectOption};
        let mut a = linear_board();
        let mut snap = a.snapshot().unwrap().clone();
        let p = &mut snap.project;
        let status = p.fields.iter_mut().find(|f| f.name == "Status").unwrap();
        let FieldKind::SingleSelect { options } = &mut status.kind else {
            panic!("status is a single select")
        };
        for (o, kind) in options.iter_mut().zip([
            OptionKind::Unstarted,
            OptionKind::Started,
            OptionKind::Completed,
        ]) {
            o.kind = Some(kind);
        }
        for (id, name, kind) in [
            ("o_canceled", "Canceled", OptionKind::Canceled),
            ("o_dup", "Duplicate", OptionKind::Duplicate),
        ] {
            options.push(SelectOption {
                id: OptionId::new(id),
                name: name.into(),
                color: OptionColor::Gray,
                kind: Some(kind),
            });
        }
        let view = p.views.iter_mut().find(|v| v.id.0 == "V_cycles").unwrap();
        view.vertical_group_by = vec![FieldId::new("F_status")];
        view.filter = ViewFilter::Linear(crate::linear::views::board_tab_filter());
        a.store.apply(StoreUpdate::Replace(snap));
        a.select_view(&ViewId::new("V_cycles"));
        let query = a.list_query(a.current_view().unwrap()).unwrap();
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_cycles"),
            query,
            list: ViewList {
                ids: ["a", "b", "c", "e"].map(ItemId::new).to_vec(),
                total: 4,
                truncated: false,
            },
        });
        let titles = |a: &App| -> Vec<String> {
            a.board_columns()
                .iter()
                .map(|c| c.bucket.title.clone())
                .collect()
        };
        let shown = titles(&a);
        assert!(shown.contains(&"Todo".to_string()), "{shown:?}");
        assert!(!shown.contains(&"Canceled".to_string()), "{shown:?}");
        assert!(!shown.contains(&"Duplicate".to_string()), "{shown:?}");

        // A card moved into Canceled keeps the column for this visit.
        let id = ItemId::new("b");
        a.stick(&id);
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&id).unwrap().values.insert(
            FieldId::new("F_status"),
            FieldValue::SingleSelect {
                option_id: OptionId::new("o_canceled"),
                name: "Canceled".into(),
            },
        );
        a.store.apply(StoreUpdate::Replace(snap));
        let shown = titles(&a);
        assert!(shown.contains(&"Canceled".to_string()), "{shown:?}");
        assert!(!shown.contains(&"Duplicate".to_string()), "{shown:?}");
    }

    #[test]
    fn linear_columns_follow_the_filter_exactly() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_cycles"));
        assert_eq!(a.layout().0, Layout::Board);
        let titles = |a: &App| -> Vec<String> {
            a.board_columns()
                .iter()
                .map(|c| c.bucket.title.clone())
                .collect()
        };
        assert!(titles(&a).contains(&"Cycle 6".to_string()));
        a.handle_key(key('f'));
        type_text(&mut a, "cycle:@current");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(titles(&a), ["Cycle 5"]);
        assert_eq!(ids(a.view_items()), ["a", "c"]);
    }

    #[test]
    fn linear_show_archived_lists_with_archived_items() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_all"));
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        let at = picker
            .choices
            .iter()
            .position(|c| c.label == "Show archived items")
            .unwrap();
        for _ in 0..at {
            a.handle_key(key('j'));
        }
        let query = ListQuery {
            filter: ViewFilter::Linear(serde_json::json!({})),
            include_archived: true,
        };
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::FetchViewIds {
                view: ViewId::new("V_all"),
                query: query.clone(),
            }]
        );
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_all"),
            query,
            list: ViewList {
                ids: vec![ItemId::new("a"), ItemId::new("z"), ItemId::new("b")],
                total: 0,
                truncated: false,
            },
        });
        assert_eq!(ids(a.view_items()), ["z"]);
        assert!(a.handle_key(key('x')).is_empty());
        assert_eq!(ids(a.view_items()), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn linear_sticky_items_unmatched_by_the_evaluator() {
        let mut a = linear_board();
        a.select_view(&ViewId::new("V_all"));
        a.handle_key(key('f'));
        type_text(&mut a, "label:bug");
        a.handle_key(code(KeyCode::Enter));
        let id = ItemId::new("a");
        a.stick(&id);
        let mut snap = a.snapshot().unwrap().clone();
        snap.items
            .get_mut(&id)
            .unwrap()
            .values
            .remove(&FieldId::new("F_labels"));
        a.store.apply(StoreUpdate::Replace(snap));
        let items = a.view_items().unwrap();
        assert_eq!(ids(Some(items.clone())), ["a"], "it stays for this visit");
        assert!(a.unmatched(items[0]), "no longer labelled bug");

        let mut b = linear_board();
        b.handle_key(key('f'));
        type_text(&mut b, "cycle:\"Cycle 5\"");
        b.handle_key(code(KeyCode::Enter));
        b.stick(&id);
        let item = b.snapshot().unwrap().items[&id].clone();
        assert!(!b.unmatched(&item), "still in Cycle 5");
    }

    #[test]
    fn github_filters_still_go_to_the_server() {
        let mut a = app();
        a.handle_key(key('f'));
        type_text(&mut a, "label:bug stauts:x");
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::FetchViewIds {
                view: ViewId::new("V_table"),
                query: ListQuery::github("label:bug stauts:x"),
            }]
        );
        assert_eq!(a.status.flash, None, "GitHub reads its own filter");
        assert_eq!(
            a.list_query(a.current_view().unwrap()),
            Some(ListQuery::github("label:bug stauts:x"))
        );
    }

    #[test]
    fn enter_opens_detail_and_esc_closes_it() {
        let mut a = app();
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert!(a.detail.is_none());
    }

    #[test]
    fn following_an_issue_ref_on_the_board_opens_that_item() {
        let mut a = app();
        a.handle_key(code(KeyCode::Enter));
        let detail = ItemDetail {
            body: "see #2 and #99".into(),
            ..Default::default()
        };
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("a"),
            detail,
            older: false,
        });
        a.handle_key(code(KeyCode::Tab));
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("b"),
                before: None
            }]
        );
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("b"),
            detail: ItemDetail {
                body: "back to #99".into(),
                ..Default::default()
            },
            older: false,
        });
        a.handle_key(code(KeyCode::Tab));
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::OpenUrl(
                "https://github.com/tviles/t/issues/99".into()
            )]
        );
    }

    #[test]
    fn a_key_on_the_board_opens_its_detail_and_others_open_linear() {
        let mut a = linear_board();
        let mut snap = a.snapshot().unwrap().clone();
        for (id, number) in [("a", 1), ("b", 2)] {
            let item = snap.items.get_mut(&ItemId::new(id)).unwrap();
            if let ItemContent::Issue { reference, .. } = &mut item.content {
                reference.container = "TBA".into();
                reference.key = Some(format!("TBA-{number}"));
                reference.url = format!("https://linear.app/acme/issue/TBA-{number}");
            }
        }
        a.store.apply(StoreUpdate::Replace(snap));
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("a"),
            detail: ItemDetail {
                body: "see TBA-2, #2 and TBA-99".into(),
                ..Default::default()
            },
            older: false,
        });
        assert_eq!(
            a.detail_targets(),
            vec![Target::Key("TBA-2".into()), Target::Key("TBA-99".into())]
        );
        a.handle_key(code(KeyCode::Tab));
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("b"),
                before: None
            }]
        );
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("b"),
            detail: ItemDetail {
                body: "back to TBA-99".into(),
                ..Default::default()
            },
            older: false,
        });
        a.handle_key(code(KeyCode::Tab));
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            cmds,
            vec![Command::OpenUrl(
                "https://linear.app/acme/issue/TBA-99".into()
            )]
        );
    }

    #[test]
    fn a_linear_boards_marks_carry_its_status_field() {
        assert_eq!(app().marks().status_field, None, "GitHub");
        let mut a = linear_board();
        assert_eq!(a.marks().status_field, None, "no Status field yet");
        let mut snap = a.snapshot().unwrap().clone();
        let mut status = snap.project.fields[1].clone();
        status.id = FieldId::new(crate::linear::ids::STATUS);
        snap.project.fields.push(status.clone());
        a.store.apply(StoreUpdate::Replace(snap));
        assert_eq!(a.marks().status_field, Some(status));
    }

    #[test]
    fn group_rows_collapse() {
        let mut a = app();
        let p = a.project().unwrap().clone();
        let mut snap = snapshot();
        snap.project.views[0].group_by = vec![p.fields[1].id.clone()];
        a.store.apply(StoreUpdate::Replace(snap));
        assert!(matches!(a.table_rows()[0], Row::Group { .. }));
        let before = a.table_rows().len();
        a.handle_key(key('z'));
        assert_eq!(a.table_rows().len(), before - 2, "Todo's two items hidden");
    }

    #[test]
    fn sync_events_update_status() {
        let mut a = app();
        a.status.stale = true;
        a.on_sync(SyncEvent::ItemsPage {
            items: vec![],
            loaded: 100,
            total: 300,
        });
        assert_eq!(a.status.loading, Some((100, 300)));
        a.on_sync(SyncEvent::ItemsComplete {
            items: crate::ui::fixtures::items(),
            fetched_at: "t".into(),
            total: 3000,
            truncated: true,
        });
        assert!(!a.status.stale && a.status.loading.is_none());
        assert!(
            a.status
                .notes
                .iter()
                .any(|n| n.contains("loaded 5 of 3000"))
        );
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::Incremental,
            error: BackendError::Network("offline".into()),
        });
        assert!(
            a.status
                .error
                .as_deref()
                .unwrap()
                .contains("refresh failed")
        );
    }

    #[test]
    fn github_setup_texts_are_unchanged() {
        let gh = Provider::Github;
        assert_eq!(
            setup_message(&BackendError::NoCredentials { provider: gh }).unwrap(),
            "No GitHub token found.\n\nRun:  gh auth login\nor set GH_TOKEN, then press r."
        );
        assert_eq!(
            setup_message(&BackendError::Unauthorized { provider: gh }).unwrap(),
            "GitHub rejected the token.\n\nRun:  gh auth login\nthen press r."
        );
        assert!(
            setup_message(&BackendError::MissingPermission {
                provider: gh,
                needed: vec!["project".into()],
            })
            .unwrap()
            .contains("gh auth refresh -s project")
        );
    }

    #[test]
    fn linear_setup_texts_say_where_a_key_comes_from() {
        let ln = Provider::Linear;
        let missing = setup_message(&BackendError::NoCredentials { provider: ln }).unwrap();
        assert!(missing.contains("No Linear API key found"), "{missing}");
        assert!(
            missing.contains("Settings → Security & access"),
            "{missing}"
        );
        assert!(missing.contains("LINEAR_API_KEY"), "{missing}");
        for e in [
            BackendError::NoCredentials { provider: ln },
            BackendError::Unauthorized { provider: ln },
            BackendError::MissingPermission {
                provider: ln,
                needed: vec![],
            },
        ] {
            let text = setup_message(&e).unwrap();
            assert!(!text.contains("gh "), "{text}");
            assert!(text.ends_with("press r."), "{text}");
        }
        assert_eq!(setup_message(&BackendError::Network("x".into())), None);
    }

    #[test]
    fn the_setup_screen_lists_every_providers_fix() {
        let gh = BackendError::NoCredentials {
            provider: Provider::Github,
        };
        let ln = BackendError::NoCredentials {
            provider: Provider::Linear,
        };
        assert_eq!(
            setup_message_for_all(&[gh.clone(), ln.clone()]),
            format!(
                "{}\n\n— or —\n\n{}",
                setup_message(&gh).unwrap(),
                setup_message(&ln).unwrap()
            )
        );
        assert_eq!(
            setup_message_for_all(std::slice::from_ref(&gh)),
            setup_message(&gh).unwrap()
        );
        let odd = BackendError::Network("offline".into());
        assert_eq!(
            setup_message_for_all(std::slice::from_ref(&odd)),
            odd.to_string(),
            "an error with no setup text shows its own"
        );
    }

    #[test]
    fn auth_failures_switch_to_the_setup_screen() {
        let mut a = app();
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: BackendError::MissingPermission {
                provider: Provider::Github,
                needed: vec!["read:project".into()],
            },
        });
        assert_eq!(a.mode, Mode::Setup);
        assert!(
            a.setup
                .as_deref()
                .unwrap()
                .contains("gh auth refresh -s project")
        );
        assert_eq!(a.handle_key(key('r')), vec![Command::Retry]);
    }

    #[test]
    fn filtered_views_revalidate_on_switch_and_after_loads() {
        let mut a = app();
        a.select_view(&ViewId::new("V_bugs"));
        let list = ViewList {
            ids: vec![ItemId::new("a")],
            total: 1,
            truncated: false,
        };
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_bugs"),
            query: ListQuery::github("label:bug"),
            list,
        });
        let fetch = Command::FetchViewIds {
            view: ViewId::new("V_bugs"),
            query: ListQuery::github("label:bug"),
        };
        a.handle_key(code(KeyCode::Tab));
        assert!(
            a.handle_key(code(KeyCode::BackTab)).contains(&fetch),
            "a cached list is still revalidated"
        );
        assert_eq!(
            a.view_items().unwrap().len(),
            1,
            "the cached list shows meanwhile"
        );
        let done = SyncEvent::ItemsComplete {
            items: crate::ui::fixtures::items(),
            fetched_at: "t".into(),
            total: 5,
            truncated: false,
        };
        assert_eq!(a.on_sync(done), vec![fetch.clone()]);
        assert!(
            a.on_sync(SyncEvent::ItemsUpdated {
                items: vec![],
                removed: vec![],
                fetched_at: "t".into()
            })
            .is_empty()
        );
        let changed = SyncEvent::ItemsUpdated {
            items: vec![crate::ui::fixtures::items().remove(0)],
            removed: vec![],
            fetched_at: "t".into(),
        };
        assert_eq!(a.on_sync(changed), vec![fetch]);
    }

    #[test]
    fn the_current_view_survives_views_reordered_on_github() {
        let mut a = app();
        a.select_view(&ViewId::new("V_bugs"));
        let mut project = a.project().unwrap().clone();
        project.views.reverse();
        a.on_sync(SyncEvent::Project(project));
        assert_eq!(a.current_view().unwrap().id, ViewId::new("V_bugs"));
    }

    #[test]
    fn q_quits_from_anywhere_but_input_modes() {
        let mut a = app();
        a.handle_key(key('/'));
        a.handle_key(key('q'));
        assert!(!a.quit, "q is text while searching");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.handle_key(key('q')), vec![Command::Quit]);
        assert!(a.quit);
    }

    #[test]
    fn esc_on_the_setup_screen_does_not_quit() {
        let mut a = app();
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: BackendError::NoCredentials {
                provider: Provider::Github,
            },
        });
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert!(!a.quit);
        assert_eq!(a.handle_key(key('q')), vec![Command::Quit]);
        assert!(a.quit);
    }

    #[test]
    fn failed_full_loads_clear_the_loading_indicator() {
        let mut a = app();
        a.on_sync(SyncEvent::ItemsPage {
            items: vec![],
            loaded: 1,
            total: 9,
        });
        assert!(a.status.loading.is_some());
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: BackendError::Network("offline".into()),
        });
        assert!(a.status.loading.is_none());
        assert!(a.status.error.is_some());
    }

    #[test]
    fn a_linear_resolve_failure_shows_the_linear_setup_text() {
        let mut a = app();
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::Resolve,
            error: BackendError::NoCredentials {
                provider: Provider::Linear,
            },
        });
        assert_eq!(a.mode, Mode::Setup);
        assert!(
            a.setup
                .as_deref()
                .unwrap()
                .contains("No Linear API key found")
        );
    }

    /// A board whose credentials fail: its setup text, then the line offering another board.
    fn failing_board_setup(error: &BackendError) -> String {
        format!(
            "{}\nOr press B to open another board.",
            setup_message(error).unwrap()
        )
    }

    #[test]
    fn cancelling_the_picker_returns_to_setup() {
        let mut a = app();
        let error = BackendError::NoCredentials {
            provider: Provider::Linear,
        };
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::Resolve,
            error: error.clone(),
        });
        assert_eq!(a.mode, Mode::Setup);
        assert_eq!(a.setup, Some(failing_board_setup(&error)));
        assert_eq!(a.handle_key(key('B')), vec![Command::ShowPicker]);
        a.picker = Some(PickerState::loading(false)); // as the controller opens it
        a.mode = Mode::Picker;
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(a.mode, Mode::Setup);
        assert_eq!(a.setup, Some(failing_board_setup(&error)));
        assert!(a.picker.is_none());
        assert_eq!(
            a.handle_key(key('r')),
            vec![Command::Retry],
            "r still retries"
        );
    }

    #[test]
    fn setup_with_pending_edits_refuses_to_switch() {
        let mut a = app();
        a.submit(OpKind::Move, edit::tests::set_status("a", "o_done", "Done"));
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: BackendError::Unauthorized {
                provider: Provider::Github,
            },
        });
        assert_eq!(a.mode, Mode::Setup);
        assert!(a.handle_key(key('B')).is_empty());
        assert_eq!(a.mode, Mode::Setup);
        assert_eq!(
            a.status.flash.as_deref(),
            Some("1 edit is still sending; switch boards when it is done")
        );
    }

    #[test]
    fn the_start_time_setup_text_has_no_board_line() {
        let all = setup_message_for_all(&[
            BackendError::NoCredentials {
                provider: Provider::Github,
            },
            BackendError::NoCredentials {
                provider: Provider::Linear,
            },
        ]);
        assert!(!all.contains("another board"), "{all}");
        // A failed board list has no other board to offer either.
        let mut a = app();
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::Projects,
            error: BackendError::Unauthorized {
                provider: Provider::Github,
            },
        });
        assert_eq!(a.mode, Mode::Setup);
        assert!(!a.setup.as_deref().unwrap().contains("another board"));
        assert!(a.handle_key(key('B')).is_empty());
        assert_eq!(a.mode, Mode::Setup);
    }

    #[test]
    fn late_view_ids_for_an_old_filter_are_dropped() {
        let mut a = app();
        let list = |id: &str| ViewList {
            ids: vec![ItemId::new(id)],
            total: 1,
            truncated: false,
        };
        let table = ViewId::new("V_table");
        a.handle_key(key('f'));
        for c in "label:bug".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('f'));
        for _ in 0.."label:bug".len() {
            a.handle_key(code(KeyCode::Backspace));
        }
        for c in "label:x".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::ViewIds {
            view: table.clone(),
            query: ListQuery::github("label:x"),
            list: list("b"),
        });
        a.on_sync(SyncEvent::ViewIds {
            view: table,
            query: ListQuery::github("label:bug"),
            list: list("a"),
        });
        let ids: Vec<_> = a
            .view_items()
            .unwrap()
            .iter()
            .map(|i| i.id.0.clone())
            .collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn an_unchanged_filter_keeps_the_cached_list() {
        let mut a = app();
        a.select_view(&ViewId::new("V_bugs"));
        let list = ViewList {
            ids: vec![ItemId::new("a")],
            total: 1,
            truncated: false,
        };
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_bugs"),
            query: ListQuery::github("label:bug"),
            list,
        });
        a.handle_key(key('f'));
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.view_items().unwrap().len(), 1);
    }

    #[test]
    fn a_detail_failure_allows_loading_older_comments_again() {
        let mut a = app();
        a.handle_key(code(KeyCode::Enter));
        let detail = ItemDetail {
            older_cursor: Some("c".into()),
            ..Default::default()
        };
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("a"),
            detail,
            older: false,
        });
        let older = Command::LoadDetail {
            item: ItemId::new("a"),
            before: Some("c".into()),
        };
        assert_eq!(a.handle_key(key('P')), vec![older.clone()]);
        assert!(a.handle_key(key('P')).is_empty(), "already loading");
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::Detail(ItemId::new("a")),
            error: BackendError::Network("offline".into()),
        });
        assert_eq!(a.handle_key(key('P')), vec![older]);
    }

    #[test]
    fn background_loads_keep_the_same_item_selected() {
        let mut a = app();
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        assert_eq!(selected_id(&a), "c");
        let mut all = crate::ui::fixtures::items();
        all.insert(0, crate::model::item::tests::issue("0", 50, "Earlier"));
        a.on_sync(SyncEvent::ItemsComplete {
            items: all,
            fetched_at: "t".into(),
            total: 6,
            truncated: false,
        });
        assert_eq!(a.table_rows().len(), 6);
        assert_eq!(a.table_selected, 3, "the index moved with the insert");
        assert_eq!(
            selected_id(&a),
            "c",
            "the cursor follows the item, not the index"
        );
    }

    #[test]
    fn collapsing_a_group_selects_its_header() {
        let mut a = app();
        let p = a.project().unwrap().clone();
        let mut snap = snapshot();
        snap.project.views[0].group_by = vec![p.fields[1].id.clone()];
        a.store.apply(StoreUpdate::Replace(snap));
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        assert!(matches!(a.table_rows()[2], Row::Item(_)));
        a.handle_key(key('z'));
        assert_eq!(a.table_selected, 0);
        assert!(matches!(
            a.table_rows()[a.table_selected],
            Row::Group { .. }
        ));
    }

    #[test]
    fn clearing_a_search_returns_to_the_original_selection() {
        let mut a = app();
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        assert_eq!(selected_id(&a), "c");
        a.handle_key(key('/'));
        for c in "crash".chars() {
            a.handle_key(key(c));
        }
        assert_ne!(selected_id(&a), "c");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(selected_id(&a), "c");
    }
}
