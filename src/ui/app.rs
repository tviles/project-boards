use crate::github::GithubError;
use crate::model::*;
use crate::store::{BoardSnapshot, Store, StoreUpdate, ViewList};
use crate::sync::{SyncEvent, SyncTask, store_update};
use crate::ui::board::{BoardSelection, Column, build_columns, resolve_layout};
use crate::ui::detail::{DetailOutcome, DetailState, build_doc};
use crate::ui::keymap::{Action, Keymap};
use crate::ui::markdown::Target;
use crate::ui::picker::PickerState;
use crate::ui::search;
use crate::ui::table::{Row, build_rows, collapse_key};
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Filter,
    Help,
    Detail,
    Picker,
    Setup,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    FetchViewIds {
        view: ViewId,
        filter: String,
    },
    LoadDetail {
        item: ItemId,
        before: Option<String>,
    },
    OpenUrl(String),
    Refresh,
    SaveLastView(ViewId),
    PickBoard(BoardRef),
    ShowPicker,
    /// Re-run startup after the user fixed authentication.
    Retry,
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
    pub table_selected: usize,
    pub board_sel: BoardSelection,
    pub collapsed: HashSet<String>,
    pub quit: bool,
    /// The current view, by id, so views reordered on GitHub never move you to another one.
    current: Option<ViewId>,
    /// The selected item, so polls, re-sorts and search never move the cursor to another one.
    selected_id: Option<ItemId>,
    search: String,
    layout_toggle: HashMap<ViewId, Layout>,
    filter_override: HashMap<ViewId, String>,
    /// Detail width used to number targets; kept in sync by the renderer.
    pub detail_width: u16,
}

fn task_words(task: &SyncTask) -> &'static str {
    match task {
        SyncTask::Resolve => "could not load the board",
        SyncTask::FullLoad => "could not load items",
        SyncTask::Incremental => "refresh failed",
        SyncTask::ViewIds(_) => "could not load this view",
        SyncTask::Hydrate => "could not load some items",
        SyncTask::Detail(_) => "could not load the item",
        SyncTask::Projects => "could not list boards",
    }
}

/// The setup screen text for errors the user has to fix outside the app.
pub fn setup_message(error: &GithubError) -> Option<String> {
    match error {
        GithubError::NoToken => Some(
            "No GitHub token found.\n\nRun:  gh auth login\nor set GH_TOKEN, then press r.".into(),
        ),
        GithubError::Unauthorized => {
            Some("GitHub rejected the token.\n\nRun:  gh auth login\nthen press r.".into())
        }
        GithubError::InsufficientScopes { missing } => Some(format!(
            "The token is missing the {} scope.\n\nRun:  gh auth refresh -s project\n(add -s repo for private repositories), then press r.",
            missing.join(", ")
        )),
        _ => None,
    }
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
            table_selected: 0,
            board_sel: BoardSelection::default(),
            collapsed: HashSet::new(),
            quit: false,
            current: None,
            selected_id: None,
            search: String::new(),
            layout_toggle: HashMap::new(),
            filter_override: HashMap::new(),
            detail_width: 80,
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

    pub fn effective_filter(&self) -> String {
        let Some(view) = self.current_view() else {
            return String::new();
        };
        self.filter_override
            .get(&view.id)
            .cloned()
            .unwrap_or_else(|| view.filter.clone())
    }

    pub fn search_query(&self) -> &str {
        if self.mode == Mode::Search {
            &self.input
        } else {
            &self.search
        }
    }

    /// Items of the current view: the whole board for an empty filter, else the view's id
    /// list (`None` until it arrives); then quick search; then the view's sort.
    pub fn view_items(&self) -> Option<Vec<&Item>> {
        let snap = self.snapshot()?;
        let view = self.current_view()?;
        let filter = self.effective_filter();
        let mut items = if filter.trim().is_empty() {
            snap.all_items()
        } else {
            snap.view_items(&view.id)?
        };
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
        build_columns(&items, field, view.group_field(&project.fields))
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
        let filter = self.effective_filter();
        let missing = !self.snapshot()?.views.contains_key(&view.id);
        (!filter.trim().is_empty() && missing).then(|| Command::FetchViewIds {
            view: view.id.clone(),
            filter,
        })
    }

    /// A request for the current view's ids whenever it has a filter, cached list or not.
    pub fn revalidate_view(&self) -> Option<Command> {
        let view = self.current_view()?;
        let filter = self.effective_filter();
        (!filter.trim().is_empty()).then(|| Command::FetchViewIds {
            view: view.id.clone(),
            filter,
        })
    }

    fn clamp_selection(&mut self) {
        let rows = self.table_rows().len();
        self.table_selected = self.table_selected.min(rows.saturating_sub(1));
        let cols = self.board_columns();
        let lens: Vec<usize> = cols.iter().map(|c| c.len()).collect();
        self.board_sel.column = self.board_sel.column.min(lens.len().saturating_sub(1));
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
                Action::Left => s.column = s.column.saturating_sub(1),
                Action::Right => s.column = (s.column + 1).min(lens.len() - 1),
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
        if let Some((index, k)) = header {
            if !self.collapsed.remove(&k) {
                self.collapsed.insert(k);
                // The rows under the header vanish: land on the header, not another group.
                self.table_selected = index;
            }
        }
        self.sync_selected_id();
        self.restore_selection();
    }

    fn open_detail(&mut self, item: ItemId) -> Vec<Command> {
        self.detail = Some(DetailState::new(item.clone()));
        self.mode = Mode::Detail;
        vec![Command::LoadDetail { item, before: None }]
    }

    fn follow(&mut self, target: Target) -> Vec<Command> {
        match target {
            Target::Link(url) => vec![Command::OpenUrl(url)],
            Target::IssueRef(n) => {
                let repo = self
                    .detail_item()
                    .and_then(|i| i.reference())
                    .map(|r| r.repo.clone());
                let Some(repo) = repo else { return Vec::new() };
                let on_board = self.snapshot().and_then(|s| {
                    s.items
                        .values()
                        .find(|i| {
                            i.reference()
                                .is_some_and(|r| r.repo == repo && r.number == n)
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return vec![Command::Quit];
        }
        let action = self.keymap.action(&key);
        match self.mode {
            Mode::Search => {
                self.sync_selected_id();
                match self.handle_text_input(&key) {
                    Some(true) => {
                        self.search = std::mem::take(&mut self.input);
                        self.mode = Mode::Normal;
                    }
                    None => {
                        self.input.clear();
                        self.search.clear();
                        self.mode = Mode::Normal;
                    }
                    Some(false) => {}
                }
                self.restore_selection();
                Vec::new()
            }
            Mode::Filter => match self.handle_text_input(&key) {
                Some(true) => {
                    self.mode = Mode::Normal;
                    let filter = std::mem::take(&mut self.input).trim().to_string();
                    let Some(view) = self.current_view().cloned() else {
                        return Vec::new();
                    };
                    let before = self.effective_filter();
                    if filter == view.filter {
                        self.filter_override.remove(&view.id);
                    } else {
                        self.filter_override.insert(view.id.clone(), filter.clone());
                    }
                    if filter == before {
                        // Nothing changed: keep the cached list and the cursor.
                        return self.revalidate_view().into_iter().collect();
                    }
                    // A changed filter invalidates the cached list.
                    self.store.apply(StoreUpdate::ClearViewIds(view.id.clone()));
                    self.table_selected = 0;
                    self.board_sel = BoardSelection::default();
                    self.sync_selected_id();
                    if filter.is_empty() {
                        Vec::new()
                    } else {
                        vec![Command::FetchViewIds {
                            view: view.id,
                            filter,
                        }]
                    }
                }
                None => {
                    self.input.clear();
                    self.mode = Mode::Normal;
                    Vec::new()
                }
                Some(false) => Vec::new(),
            },
            Mode::Help => {
                self.mode = Mode::Normal;
                if action == Some(Action::Quit) {
                    self.quit = true;
                    return vec![Command::Quit];
                }
                Vec::new()
            }
            Mode::Setup => match key.code {
                KeyCode::Char('r') => vec![Command::Retry],
                // Esc never quits; the keymap's Quit does.
                _ if action == Some(Action::Quit) => {
                    self.quit = true;
                    vec![Command::Quit]
                }
                _ => Vec::new(),
            },
            Mode::Picker => self.handle_picker_key(key),
            Mode::Detail => {
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
                    DetailOutcome::Quit => {
                        self.quit = true;
                        vec![Command::Quit]
                    }
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
            Mode::Normal => match action {
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
                        self.layout_toggle.insert(view.id, next);
                        self.restore_selection();
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
                    self.input = self.search.clone();
                    self.mode = Mode::Search;
                    Vec::new()
                }
                Some(Action::Filter) => {
                    self.input = self.effective_filter();
                    self.mode = Mode::Filter;
                    Vec::new()
                }
                Some(Action::Refresh) => vec![Command::Refresh],
                Some(Action::OpenBrowser) => {
                    let url = self
                        .selected_item()
                        .and_then(|i| i.url())
                        .map(String::from)
                        .or_else(|| self.project().map(|p| p.url.clone()));
                    url.map(|u| vec![Command::OpenUrl(u)]).unwrap_or_default()
                }
                Some(Action::Help) => {
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
                Some(Action::PickBoard) => vec![Command::ShowPicker],
                Some(Action::Quit) => {
                    self.quit = true;
                    vec![Command::Quit]
                }
                _ => Vec::new(),
            },
        }
    }

    /// Replaced in Task 23, which defines the picker.
    fn handle_picker_key(&mut self, _key: KeyEvent) -> Vec<Command> {
        self.mode = Mode::Normal;
        Vec::new()
    }

    /// The filter a view currently applies, looked up by id.
    fn filter_of(&self, id: &ViewId) -> Option<String> {
        let view = self.project()?.views.iter().find(|v| &v.id == id)?;
        Some(
            self.filter_override
                .get(id)
                .cloned()
                .unwrap_or_else(|| view.filter.clone()),
        )
    }

    pub fn on_sync(&mut self, event: SyncEvent) -> Vec<Command> {
        // A late answer for a filter that has since changed must not replace the list.
        if let SyncEvent::ViewIds { view, filter, .. } = &event {
            if self
                .filter_of(view)
                .is_some_and(|current| &current != filter)
            {
                return Vec::new();
            }
        }
        self.sync_selected_id();
        if let Some(update) = store_update(&event) {
            self.store.apply(update);
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
                    self.status
                        .notes
                        .push(format!("loaded {loaded} of {total} items (max_items)"));
                }
                cmds.extend(self.revalidate_view());
            }
            SyncEvent::ItemsUpdated { items, fetched_at } => {
                self.store.apply(StoreUpdate::FetchedAt(fetched_at));
                self.status.error = None;
                if !items.is_empty() {
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
            SyncEvent::Projects(list) => self.show_projects(list),
            SyncEvent::Rate(rate) => self.status.rate_low = rate.is_low(),
            SyncEvent::Failed { task, error } => {
                if matches!(task, SyncTask::FullLoad | SyncTask::Resolve) {
                    self.status.loading = None;
                }
                if let Some(message) = setup_message(&error) {
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
            SyncEvent::ViewIds { .. } | SyncEvent::Hydrated(_) | SyncEvent::RepoProjects(_) => {}
        }
        self.restore_selection();
        cmds
    }

    /// Replaced in Task 23, which defines the picker.
    fn show_projects(&mut self, _list: Vec<ProjectSummary>) {}

    #[allow(dead_code)] // consumed by the chrome (Task 23)
    pub(crate) fn view_list(&self) -> Option<&ViewList> {
        self.snapshot()?.views.get(&self.current_view()?.id)
    }
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
            filter: "label:bug".into()
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
    fn layout_toggle_is_per_view() {
        let mut a = app();
        a.handle_key(key('L'));
        assert_eq!(a.layout().0, Layout::Board);
        a.handle_key(code(KeyCode::Tab));
        a.handle_key(code(KeyCode::BackTab));
        assert_eq!(a.layout().0, Layout::Board, "remembered for the session");
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
                filter: "label:bug".into()
            }]
        );
        assert_eq!(a.effective_filter(), "label:bug");
        a.on_sync(SyncEvent::ViewIds {
            view: ViewId::new("V_table"),
            filter: "label:bug".into(),
            list: ViewList {
                ids: vec![ItemId::new("a")],
                total: 1,
                truncated: false,
            },
        });
        assert_eq!(a.view_items().unwrap().len(), 1);
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
            error: GithubError::Network("offline".into()),
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
    fn auth_failures_switch_to_the_setup_screen() {
        let mut a = app();
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: GithubError::InsufficientScopes {
                missing: vec!["read:project".into()],
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
            filter: "label:bug".into(),
            list,
        });
        let fetch = Command::FetchViewIds {
            view: ViewId::new("V_bugs"),
            filter: "label:bug".into(),
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
                fetched_at: "t".into()
            })
            .is_empty()
        );
        let changed = SyncEvent::ItemsUpdated {
            items: vec![crate::ui::fixtures::items().remove(0)],
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
            error: GithubError::NoToken,
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
            error: GithubError::Network("offline".into()),
        });
        assert!(a.status.loading.is_none());
        assert!(a.status.error.is_some());
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
            filter: "label:x".into(),
            list: list("b"),
        });
        a.on_sync(SyncEvent::ViewIds {
            view: table,
            filter: "label:bug".into(),
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
            filter: "label:bug".into(),
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
            error: GithubError::Network("offline".into()),
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
        all.push(crate::model::item::tests::issue("0", 50, "Earlier"));
        a.on_sync(SyncEvent::ItemsComplete {
            items: all,
            fetched_at: "t".into(),
            total: 6,
            truncated: false,
        });
        assert_eq!(a.table_rows().len(), 6);
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
}
