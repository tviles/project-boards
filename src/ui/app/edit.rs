//! Editing: every op goes through the capability matrix, joins the store's pending list, and
//! settles when GitHub answers. Failed edits go to the error log.

use super::*;
use crate::model::{
    CloseReason, ContentId, Denial, EditContext, ItemContent, OpKind, check, check_new,
};
use crate::ops::{Confirmed, NewItem, Op, OpId};
use crate::store::BoardSnapshot;
use crate::ui::app::popups::{
    GONE, assignee_options, fill_checklist, label_options, milestone_choices,
};
use crate::ui::editor::{Edited, title_and_body, trim_blank_lines};
use crate::ui::marks::ItemMarks;
use crate::ui::popup::{Choice, ListPicker};
use crate::ui::table::Row;
use crate::ui::textform::{FormKind, TextForm};
use std::collections::HashMap;
use std::time::Instant;

/// A sticky item, and whether the view's list held it when it became sticky.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sticky {
    pub id: ItemId,
    pub listed: bool,
}

/// An edit GitHub refused, for the error log.
#[derive(Debug, Clone, PartialEq)]
pub struct FailedEdit {
    pub kind: OpKind,
    pub op: Op,
    /// What the edit was, e.g. "set Status to Done on 'Fix crash'".
    pub what: String,
    /// GitHub's own message.
    pub message: String,
    pub at: Instant,
    /// A new item or a comment that may exist on GitHub after all (the answer was lost, or
    /// the board was reloaded while it was sending): `R` refuses, because a retry could
    /// create it twice (`not_safe_to_resend`).
    pub may_exist: bool,
}

/// A yes/no question on the status line (`Mode::Confirm`).
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub question: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    Quit,
    Submit { kind: OpKind, op: Op },
}

/// The byte range of the `is:archived` word in `text`, if it is there as a word of its own.
pub(crate) fn archived_token(text: &str) -> Option<(usize, usize)> {
    const TOKEN: &str = "is:archived";
    let mut from = 0;
    while let Some(rel) = text[from..]
        .char_indices()
        .map(|(i, _)| from + i)
        .find(|&i| {
            text.get(i..i + TOKEN.len())
                .is_some_and(|w| w.eq_ignore_ascii_case(TOKEN))
        })
    {
        let end = rel + TOKEN.len();
        let before = text[..rel]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let after = text[end..].chars().next().is_none_or(char::is_whitespace);
        if before && after {
            return Some((rel, end));
        }
        from = rel + 1;
        while !text.is_char_boundary(from) {
            from += 1;
        }
    }
    None
}

/// The error log's message for an edit dropped unsent because its board was loaded again.
pub const NOT_SENT_RELOADED: &str = "not sent: the board was reloaded";

/// The same for a new draft or issue, or a comment: it may have been sending, and so may
/// exist on GitHub already.
pub const MAYBE_SENT_RELOADED: &str =
    "the board was reloaded while it was sending; check the board before retrying";

/// A new draft or issue whose answer was lost (a network or decode error): it may exist.
/// `R` refuses such an entry with this text; the user checks GitHub and adds it again
/// deliberately if it is not there.
pub const MAYBE_CREATED: &str = "may have been created; check GitHub before retrying";

/// A comment whose answer was lost: it may have been posted. `R` refuses such an entry with
/// this text (controller ruling E19).
pub const MAYBE_POSTED: &str = "may have been posted; check the issue before retrying";

/// An op whose retry could make a second copy on GitHub when its answer was lost: a new
/// draft, a new issue, or a comment. Adding an existing issue twice returns the same item,
/// and every other edit sets a value, so those are safe to send again.
fn not_safe_to_resend(op: &Op) -> bool {
    matches!(
        op,
        Op::Create {
            new: NewItem::Draft { .. } | NewItem::Issue { .. },
            ..
        } | Op::Comment { .. }
    )
}

/// What the log says, and `R` refuses with, for `op` whose answer was lost.
fn maybe_done(op: &Op) -> &'static str {
    match op {
        Op::Comment { .. } => MAYBE_POSTED,
        _ => MAYBE_CREATED,
    }
}

/// An edit, or the open detail, whose item has left the board.
pub const NO_LONGER_ON_BOARD: &str = "That item is no longer on the board";

/// Why a draft's content edits, and a second convert, wait while it converts (ruling E21).
pub const CONVERTING: &str = "Converting to an issue…";

/// An operation on the item's issue, pull request or draft (built with its content id), or
/// a convert: these wait for a pending convert. Project-field edits do not.
fn edits_content(kind: OpKind) -> bool {
    matches!(
        kind,
        OpKind::Title
            | OpKind::Body
            | OpKind::Assignees
            | OpKind::Labels
            | OpKind::Milestone
            | OpKind::Comment
            | OpKind::Close
            | OpKind::Reopen
            | OpKind::Convert
    )
}

/// "Add existing" with nothing to scope its search to (ruling E22).
pub const NO_REPOS_TO_SEARCH: &str = "No repositories to search — type repo:owner/name";

/// The editor came back at once with nothing changed: it probably opened a window and did
/// not wait for it.
pub const RETURNED_AT_ONCE: &str =
    "Nothing changed — if your editor opens a window, use a waiting flag such as `code -w`";

/// The editor saved bytes that aren't UTF-8.
pub const NOT_UTF8: &str = "The editor saved text that isn't UTF-8; nothing was sent";

/// The editor exited with an error.
pub const EDITOR_FAILED: &str = "The editor exited with an error; nothing was sent";

/// Puts what the editor returned into the form it was opened from, with a line saying what
/// went wrong, if anything. A failed editor leaves the form's text as it was.
fn refill(form: &mut TextForm, edited: Edited) {
    let message = match edited {
        Edited::Saved { text, quick } => {
            let before = form.editor_text();
            form.set_from_editor(&text);
            let same = trim_blank_lines(&form.editor_text()) == trim_blank_lines(&before);
            (quick && same).then(|| RETURNED_AT_ONCE.to_string())
        }
        Edited::Failed => Some(EDITOR_FAILED.into()),
        Edited::NotUtf8 { lossy } => {
            form.set_from_editor(&lossy);
            Some(NOT_UTF8.into())
        }
        Edited::Error(message) => Some(message),
    };
    form.set_message(message);
}

/// `text` with CRLF line endings as LF.
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Failed edits kept in the log; the oldest drop first.
const MAX_ERRORS: usize = 50;

/// How long ago, roughly: `5s ago`, `2m ago`, `2h ago`.
pub fn ago(elapsed: std::time::Duration) -> String {
    let s = elapsed.as_secs();
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 48 * 3600 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

/// GitHub's own words for a failure; other errors as their message.
pub fn error_message(error: &BackendError) -> String {
    crate::ui::text::sanitize(&raw_error_message(error))
}

fn raw_error_message(error: &BackendError) -> String {
    match error {
        BackendError::Api(entries) => {
            let messages: Vec<&str> = entries
                .iter()
                .map(|e| e.message.trim())
                .filter(|m| !m.is_empty())
                .collect();
            if messages.is_empty() {
                "GitHub returned an error".into()
            } else {
                messages.join("; ")
            }
        }
        other => other.to_string(),
    }
}

/// The edit keys in the order the action menu and the hints list them.
pub(crate) const EDIT_ACTIONS: &[Action] = &[
    Action::MovePrevColumn,
    Action::MoveNextColumn,
    Action::ReorderUp,
    Action::ReorderDown,
    Action::Assignees,
    Action::Labels,
    Action::EditField,
    Action::Comment,
    Action::CloseReopen,
    Action::Archive,
    Action::Convert,
];

/// The menu's words for an action.
pub(crate) fn menu_label(action: Action, kind: OpKind) -> String {
    match action {
        Action::MovePrevColumn => "Move to the previous column".into(),
        Action::MoveNextColumn => "Move to the next column".into(),
        Action::ReorderUp => "Move up".into(),
        Action::ReorderDown => "Move down".into(),
        Action::EditField => "Edit a field".into(),
        _ => {
            let label = kind.label();
            let mut chars = label.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// The hints' word for an action's key, e.g. `e field`.
fn hint_word(action: Action, kind: OpKind) -> &'static str {
    match action {
        Action::EditField => "field",
        _ => kind.label(),
    }
}

/// The single-action keys the hints list before `< > move`, most used first.
const HINTED: &[Action] = &[Action::Assignees, Action::Labels, Action::EditField];

/// The single-action keys listed after `< > move`.
const HINTED_AFTER_MOVE: &[Action] = &[Action::CloseReopen, Action::Comment];

/// The key listed last, after `J K reorder`: the rarest edits drop first under E17.
const HINTED_LAST: &[Action] = &[Action::Archive, Action::Convert];

/// A lookup an editor waits on.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl App {
    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.snapshot()?.items.get(id)
    }

    /// Asks for `repo`'s candidates unless they are loaded or loading; after a failure, again.
    pub(crate) fn need_candidates(&mut self, repo: &RepoSlug) -> Vec<Command> {
        if matches!(
            self.candidates.get(repo),
            Some(Lookup::Ready(_) | Lookup::Loading)
        ) {
            return Vec::new();
        }
        self.candidates.insert(repo.clone(), Lookup::Loading);
        vec![Command::LoadCandidates(repo.clone())]
    }

    pub(crate) fn need_repos(&mut self) -> Vec<Command> {
        if matches!(self.repos, Some(Lookup::Ready(_) | Lookup::Loading)) {
            return Vec::new();
        }
        self.repos = Some(Lookup::Loading);
        vec![Command::LoadRepos]
    }

    pub(crate) fn need_repo_lookup(&mut self, slug: &RepoSlug) -> Vec<Command> {
        if matches!(
            self.repo_lookups.get(slug),
            Some(Lookup::Ready(_) | Lookup::Loading)
        ) {
            return Vec::new();
        }
        self.repo_lookups.insert(slug.clone(), Lookup::Loading);
        vec![Command::LookupRepo(slug.clone())]
    }

    /// Searches for "add existing", unless the search can't be sent: the text is too long,
    /// or nothing scopes it (no default scope, no `repo:` token, not a pasted URL), which
    /// would search all of GitHub. Then the popover shows why.
    pub(crate) fn start_search(&mut self, query: &str, scope: Vec<RepoSlug>) -> Vec<Command> {
        let plan = crate::sync::search_plan(query, &scope);
        let refused = if !plan.fits {
            Some("search text too long")
        } else if !plan.scoped && crate::model::parse_content_url(query).is_none() {
            Some(NO_REPOS_TO_SEARCH)
        } else {
            None
        };
        if let Some(why) = refused {
            self.content_search = Some((query.to_string(), Lookup::Failed(why.into())));
            self.refresh_popup();
            return Vec::new();
        }
        if plan.repos_dropped > 0 {
            self.status.flash = Some(format!(
                "searching the first {} repositories",
                scope.len() - plan.repos_dropped
            ));
        }
        self.content_search = Some((query.to_string(), Lookup::Loading));
        vec![Command::Search {
            query: query.to_string(),
            scope,
        }]
    }

    /// Forgets the candidate and repository lists so the next popover loads them afresh (`r`,
    /// ruling E23). The "add existing" search stays while its popover is open.
    pub(crate) fn clear_lookups(&mut self) {
        self.candidates.clear();
        self.repos = None;
        self.repo_lookups.clear();
        if !matches!(self.popup, Some(Popup::Search { .. })) {
            self.content_search = None;
        }
    }

    /// Where a draft's assignees come from: the pane's repository, else the project's first
    /// linked repository.
    pub fn draft_repo(&self) -> Option<RepoSlug> {
        self.pane_repo
            .clone()
            .or_else(|| self.project()?.linked_repos.first().map(|r| r.slug.clone()))
    }

    /// The repository whose users, labels and milestones `item` can take.
    pub fn candidate_repo(&self, item: &Item) -> Option<RepoSlug> {
        match item.reference() {
            Some(r) => r.repo.parse().ok(),
            None => self.draft_repo(),
        }
    }

    /// Records a failed lookup where its popover shows it. False for other failures.
    pub(super) fn lookup_failed(&mut self, task: &SyncTask, error: &BackendError) -> bool {
        let message = format!("{}: {}", task_words(task), error_message(error));
        match task {
            SyncTask::Candidates(repo) => {
                self.candidates
                    .insert(repo.clone(), Lookup::Failed(message));
            }
            SyncTask::Repos => self.repos = Some(Lookup::Failed(message)),
            SyncTask::RepoLookup(slug) => {
                self.repo_lookups
                    .insert(slug.clone(), Lookup::Failed(message));
            }
            SyncTask::Search(query) => {
                // An answer to a search the user has since replaced is dropped.
                if self.content_search.as_ref().is_none_or(|(q, _)| q != query) {
                    return true;
                }
                self.content_search = Some((query.clone(), Lookup::Failed(message)));
            }
            _ => return false,
        }
        self.refresh_popup();
        true
    }

    /// Fills a popover that waits on a lookup which has landed or failed.
    pub(crate) fn refresh_popup(&mut self) {
        let Some(mut popup) = self.popup.take() else {
            return;
        };
        match &mut popup {
            Popup::Search {
                searched: Some(q),
                hits,
                loading,
                error,
                selected,
                ..
            } if *loading => match &self.content_search {
                Some((query, Lookup::Ready(found))) if query == q => {
                    *hits = found.clone();
                    *loading = false;
                    *selected = 0;
                }
                Some((query, Lookup::Failed(e))) if query == q => {
                    *error = Some(e.clone());
                    *loading = false;
                }
                _ => {}
            },
            Popup::Assignees {
                item, repo, select, ..
            } if select.loading => fill_checklist(
                select,
                self.candidates.get(repo),
                self.item(item),
                assignee_options,
                |u: &UserRef| u.login.clone(),
            ),
            Popup::Labels {
                item, repo, select, ..
            } if select.loading => fill_checklist(
                select,
                self.candidates.get(repo),
                self.item(item),
                label_options,
                |l: &LabelRef| l.name.clone(),
            ),
            Popup::Milestone {
                item, repo, picker, ..
            } if picker.loading => match self.candidates.get(repo) {
                Some(Lookup::Ready(c)) => match self.item(item) {
                    Some(it) => picker.set_choices(milestone_choices(c, it)),
                    None => {
                        picker.loading = false;
                        picker.error = Some(GONE.into());
                    }
                },
                Some(Lookup::Failed(e)) => {
                    picker.loading = false;
                    picker.error = Some(e.clone());
                }
                _ => {}
            },
            Popup::Repo { picker, wanted, .. } => {
                let (choices, loading) = self.repo_choices();
                picker.set_choices(choices);
                picker.loading = loading && picker.choices.is_empty();
                picker.error = match wanted.as_ref().map(|w| (w, self.repo_lookups.get(w))) {
                    Some((w, Some(Lookup::Ready(None)))) => {
                        Some(format!("{w}: not found, or you can't create issues there"))
                    }
                    Some((_, Some(Lookup::Failed(e)))) => Some(e.clone()),
                    _ => match &self.repos {
                        Some(Lookup::Failed(e)) => Some(e.clone()),
                        _ => None,
                    },
                };
                picker.note = match &self.repos {
                    Some(Lookup::Ready(list)) if list.truncated => Some(
                        "showing the first 500 repositories; type owner/name for another".into(),
                    ),
                    _ => None,
                };
            }
            _ => {}
        }
        self.popup = Some(popup);
    }

    /// Whether `kind` is allowed on `item` here.
    pub(crate) fn allowed(&self, kind: OpKind, item: &Item) -> bool {
        self.check_here(kind, item).is_ok()
    }

    /// The first operation of `action` allowed on `item` here (`check_here`: the matrix and
    /// the view's rules).
    pub(crate) fn allowed_kind(&self, action: Action, item: &Item) -> Option<OpKind> {
        action
            .kinds()
            .iter()
            .copied()
            .find(|k| self.check_here(*k, item).is_ok())
    }

    /// Runs `action` as its key would (the action menu uses this).
    pub(crate) fn run_action(&mut self, action: Action) -> Vec<Command> {
        if let Some(cmds) = self.edit_key(action) {
            return cmds;
        }
        match action {
            Action::OpenBrowser => self.open_in_browser(),
            _ => Vec::new(),
        }
    }

    /// The item's page, else the board's.
    pub(crate) fn open_in_browser(&self) -> Vec<Command> {
        self.action_item()
            .and_then(|i| i.url())
            .map(String::from)
            .or_else(|| self.project().map(|p| p.url.clone()))
            .map(|u| vec![Command::OpenUrl(u)])
            .unwrap_or_default()
    }

    /// The edit keys the item allows, for the status line, most useful first so the rarest
    /// drop first under E17: `a assignees · t labels · e field · < > move · X close ·
    /// c comment · J K reorder · A archive`. `space actions` is separate (`actions_hint`): it never
    /// drops. `None` with no item or nothing allowed.
    pub fn edit_hints(&self) -> Option<Vec<String>> {
        let item = self.action_item()?;
        let ctx = self.edit_context();
        if crate::model::capabilities(item, &ctx).is_empty() {
            return None;
        }
        let key = |a: Action| self.keymap.keys_for(a).first().map(|k| k.label());
        let mut parts = Vec::new();
        let single = |parts: &mut Vec<String>, actions: &[Action]| {
            for &action in actions {
                if let (Some(kind), Some(k)) = (self.allowed_kind(action, item), key(action)) {
                    parts.push(format!("{k} {}", hint_word(action, kind)));
                }
            }
        };
        single(&mut parts, HINTED);
        if self.check_here(OpKind::Move, item).is_ok()
            && let (Some(p), Some(n)) = (key(Action::MovePrevColumn), key(Action::MoveNextColumn))
        {
            parts.push(format!("{p} {n} move"));
        }
        single(&mut parts, HINTED_AFTER_MOVE);
        if self.check_here(OpKind::Reorder, item).is_ok()
            && let (Some(d), Some(u)) = (key(Action::ReorderDown), key(Action::ReorderUp))
        {
            parts.push(format!("{d} {u} reorder"));
        }
        single(&mut parts, HINTED_LAST);
        Some(parts)
    }

    /// `space actions`, when the item allows any action (Decision E17: always kept).
    pub fn actions_hint(&self) -> Option<String> {
        let item = self.action_item()?;
        if crate::model::capabilities(item, &self.edit_context()).is_empty() {
            return None;
        }
        let k = self.keymap.keys_for(Action::ActionMenu).first()?.label();
        Some(format!("{k} actions"))
    }

    /// `n new`, offered in the status line only when no item is selected (an empty view, where
    /// no edit hint shows); with an item, `n` stays out of the hints.
    pub fn new_hint(&self) -> Option<String> {
        if self.action_item().is_some() || check_new(false, &self.edit_context()).is_err() {
            return None;
        }
        let k = self.keymap.keys_for(Action::New).first()?.label();
        Some(format!("{k} new"))
    }

    pub fn edit_context(&self) -> EditContext {
        EditContext {
            viewer_can_update: self.project().is_some_and(|p| p.viewer_can_update),
            scopes: self.scopes.clone(),
            refused: self.refused_scopes.clone(),
        }
    }

    /// What the table and the board mark: items with edits sending, sticky items the view no
    /// longer matches, and the new item to highlight.
    pub fn marks(&self) -> ItemMarks {
        ItemMarks {
            pending: self
                .store
                .pending()
                .iter()
                .map(|p| p.op.item().clone())
                .collect(),
            // Only sticky items can be unmatched; skip building the view without any.
            unmatched: if self.sticky.is_empty() {
                Default::default()
            } else {
                self.view_items()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|i| self.unmatched(i))
                    .map(|i| i.id.clone())
                    .collect()
            },
            highlight: self.highlight.clone(),
        }
    }

    pub fn pending_count(&self) -> usize {
        self.store.pending().len()
    }

    /// The item edit keys act on: the open detail's item, else the selected one.
    pub fn action_item(&self) -> Option<&Item> {
        if self.detail.is_some() {
            self.detail_item()
        } else {
            self.selected_item()
        }
    }

    /// The mode a popover or question returns to.
    pub fn base_mode(&self) -> Mode {
        if self.setup.is_some() {
            Mode::Setup
        } else if self.detail.is_some() {
            Mode::Detail
        } else {
            Mode::Normal
        }
    }

    pub fn denial_text(&self, denial: Denial) -> String {
        denial.message(&|kind| {
            Action::for_kind(kind).and_then(|a| self.keymap.keys_for(a).first().map(|k| k.label()))
        })
    }

    /// Runs `op` when the capability matrix allows `kind` on its item: the edit shows at once
    /// and `Command::Send` asks the controller to send it. Otherwise shows the reason.
    pub(crate) fn submit(&mut self, kind: OpKind, op: Op) -> Vec<Command> {
        let Some(item) = self
            .snapshot()
            .and_then(|s| s.items.get(op.item()))
            .cloned()
        else {
            self.status.flash = Some(NO_LONGER_ON_BOARD.into());
            return Vec::new();
        };
        if let Err(denial) = check(kind, &item, &self.edit_context()) {
            self.status.flash = Some(self.denial_text(denial));
            return Vec::new();
        }
        self.stick(&item.id);
        let id = self.store.begin(op);
        self.pending_kinds.insert(id, kind);
        self.restore_selection();
        // Removing the item the detail shows.
        self.close_gone_detail();
        vec![Command::Send(id)]
    }

    /// Closes a detail whose item has left the board (removed, a new item that failed, or
    /// dropped by a poll) and returns to the board, rather than showing a blank detail. The
    /// status line says why; when nothing on it does yet, `NO_LONGER_ON_BOARD`.
    pub(super) fn close_gone_detail(&mut self) {
        if self.detail.is_none() || self.detail_item().is_some() {
            return;
        }
        self.detail = None;
        if self.mode == Mode::Detail {
            self.mode = Mode::Normal;
        }
        if self.status.flash.is_none() && self.status.error.is_none() {
            self.status.flash = Some(NO_LONGER_ON_BOARD.into());
        }
    }

    /// Adds a new item (`Op::Create`): the placeholder shows at once, selected and
    /// highlighted, and stays in this view until it is created. `needs_repo`: a new issue,
    /// which needs the repo scope as well as the board.
    pub(crate) fn submit_new(&mut self, op: Op, needs_repo: bool) -> Vec<Command> {
        if let Err(denial) = check_new(needs_repo, &self.edit_context()) {
            self.status.flash = Some(self.denial_text(denial));
            return Vec::new();
        }
        let temp = op.item().clone();
        let id = self.store.begin(op);
        self.pending_kinds.insert(id, OpKind::Create);
        self.stick(&temp);
        self.selected_id = Some(temp.clone());
        self.highlight = Some(temp);
        self.restore_selection();
        vec![Command::Send(id)]
    }

    /// A placeholder became a real item: follow it, in the detail too (the caller loads the
    /// real item's detail).
    pub(crate) fn renamed(&mut self, temp: &ItemId, real: &ItemId) {
        if let Some(d) = self.detail.as_mut().filter(|d| &d.item == temp) {
            d.item = real.clone();
        }
        for s in &mut self.sticky {
            if &s.id == temp {
                s.id = real.clone();
            }
        }
        if self.selected_id.as_ref() == Some(temp) {
            self.selected_id = Some(real.clone());
        }
        if self.highlight.as_ref() == Some(temp) {
            self.highlight = Some(real.clone());
        }
    }

    /// Keeps `id` in the current view for this visit, if it is shown there (or new).
    pub(crate) fn stick(&mut self, id: &ItemId) {
        if self.sticky.iter().any(|s| &s.id == id) {
            return;
        }
        let shown = self
            .view_items()
            .is_some_and(|items| items.iter().any(|i| &i.id == id));
        if !shown && !is_placeholder(id) {
            return;
        }
        let listed = match (self.snapshot(), self.current_view()) {
            (Some(s), Some(v)) => s.views.get(&v.id).is_some_and(|l| l.ids.contains(id)),
            _ => false,
        };
        self.sticky.push(Sticky {
            id: id.clone(),
            listed,
        });
    }

    /// Puts each sticky item missing from `items` where its project rank places it.
    pub(super) fn add_sticky<'a>(&'a self, snap: &'a BoardSnapshot, items: &mut Vec<&'a Item>) {
        if self.sticky.is_empty() {
            return;
        }
        let rank: HashMap<&ItemId, usize> = snap
            .order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let rank_of = |id: &ItemId| rank.get(id).copied().unwrap_or(usize::MAX);
        for sticky in &self.sticky {
            if items.iter().any(|i| i.id == sticky.id) {
                continue;
            }
            let Some(item) = snap.items.get(&sticky.id) else {
                continue;
            };
            let mine = rank_of(&sticky.id);
            let at = items
                .iter()
                .position(|i| rank_of(&i.id) > mine)
                .unwrap_or(items.len());
            items.insert(at, item);
        }
    }

    pub(super) fn is_sticky(&self, id: &ItemId) -> bool {
        self.sticky.iter().any(|s| &s.id == id)
    }

    /// A sticky item the view no longer matches: GitHub's latest list for the view dropped
    /// it, or its values fail the filter as far as `still_matches` can tell.
    pub fn unmatched(&self, item: &Item) -> bool {
        let Some(sticky) = self.sticky.iter().find(|s| s.id == item.id) else {
            return false;
        };
        let (Some(snap), Some(view)) = (self.snapshot(), self.current_view()) else {
            return false;
        };
        let filter = self.effective_filter();
        let dropped = !filter.trim().is_empty()
            && sticky.listed
            && snap
                .views
                .get(&view.id)
                .is_some_and(|l| !l.ids.contains(&item.id));
        dropped || !still_matches(&filter, item, &snap.project.fields)
    }

    /// Runs an edit key on `action_item`. `None` when `action` is not an edit key.
    pub(crate) fn edit_key(&mut self, action: Action) -> Option<Vec<Command>> {
        let cmds = match action {
            Action::MovePrevColumn => self.move_card(-1),
            Action::MoveNextColumn => self.move_card(1),
            Action::ReorderUp => self.reorder(false),
            Action::ReorderDown => self.reorder(true),
            Action::Assignees => self.open_candidates(OpKind::Assignees, false),
            Action::Labels => self.open_candidates(OpKind::Labels, false),
            Action::EditField => self.open_field_menu(),
            Action::Comment => self.comment(),
            Action::CloseReopen => self.close_or_reopen(),
            Action::Archive => self.archive_or_unarchive(),
            Action::Convert => self.convert(),
            Action::New => self.open_new(),
            Action::ActionMenu => self.open_action_menu(),
            Action::ErrorLog => self.open_error_log(),
            _ => return None,
        };
        Some(cmds)
    }

    /// `C`: pick the repository, then convert.
    pub(crate) fn convert(&mut self) -> Vec<Command> {
        let Some(item) = self.guarded(OpKind::Convert) else {
            return Vec::new();
        };
        self.open_repo_picker(RepoPurpose::Convert { item: item.id })
    }

    /// The item `kind` would act on, when `check_here` allows it; otherwise shows why.
    fn guarded(&mut self, kind: OpKind) -> Option<Item> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return None;
        };
        match self.check_here(kind, &item) {
            Ok(()) => Some(item),
            Err(reason) => {
                self.status.flash = Some(reason);
                None
            }
        }
    }

    /// Opens the text form for `purpose` (Part C): what it submits goes where the editor's
    /// text for that purpose went.
    fn open_text_form(&mut self, purpose: EditPurpose, form: TextForm) -> Vec<Command> {
        self.open_popup(Popup::Text { purpose, form });
        Vec::new()
    }

    /// Opens the form on the item's title.
    pub(crate) fn edit_title(&mut self) -> Vec<Command> {
        let Some(item) = self.guarded(OpKind::Title) else {
            return Vec::new();
        };
        let heading = format!("Edit title of {}", item.ref_label());
        let form = TextForm::new(FormKind::Title, heading, item.title(), "");
        self.open_text_form(EditPurpose::Title(item.id), form)
    }

    /// Opens the form on the item's body. The body is only known once the item detail has
    /// loaded it (Decision 27): until then this opens the detail (or loads it again after a
    /// failure) and asks to try again.
    pub(crate) fn edit_body(&mut self) -> Vec<Command> {
        let Some(item) = self.guarded(OpKind::Body) else {
            return Vec::new();
        };
        if let Some(body) = self.loaded_body(&item.id) {
            let heading = format!("Edit body of {}", item.ref_label());
            let form = TextForm::new(FormKind::Body, heading, "", &lf(&body));
            return self.open_text_form(EditPurpose::Body(item.id), form);
        }
        let cmds = match self.detail.as_ref().filter(|d| d.item == item.id) {
            // Still loading.
            Some(d) if d.error.is_none() => Vec::new(),
            Some(_) => vec![Command::LoadDetail {
                item: item.id.clone(),
                before: None,
            }],
            None => self.open_detail(item.id.clone()),
        };
        self.status.flash = Some("Loading the body; choose Edit body again once it shows".into());
        cmds
    }

    /// `c`: opens the form on an empty comment.
    pub(crate) fn comment(&mut self) -> Vec<Command> {
        let Some(item) = self.guarded(OpKind::Comment) else {
            return Vec::new();
        };
        let heading = format!("Comment on {}", item.ref_label());
        let form = TextForm::new(FormKind::Comment, heading, "", "");
        self.open_text_form(EditPurpose::Comment(item.id), form)
    }

    /// `n`, "New draft": the form for a new draft that starts with `presets`.
    pub(crate) fn new_draft_form(&mut self, presets: Vec<(FieldId, FieldValue)>) -> Vec<Command> {
        let form = TextForm::new(FormKind::NewItem, "New draft", "", "");
        self.open_text_form(EditPurpose::NewDraft { presets }, form)
    }

    /// `n`, "New issue", once the repository is chosen: the form for the new issue.
    pub(crate) fn new_issue_form(
        &mut self,
        repo: RepoRef,
        presets: Vec<(FieldId, FieldValue)>,
    ) -> Vec<Command> {
        let heading = format!("New issue in {}", repo.slug);
        let form = TextForm::new(FormKind::NewItem, heading, "", "");
        self.open_text_form(EditPurpose::NewIssue { repo, presets }, form)
    }

    /// The body the open detail has loaded for `item`.
    fn loaded_body(&self, item: &ItemId) -> Option<String> {
        let d = self.detail.as_ref().filter(|d| &d.item == item)?;
        d.detail.as_ref().map(|x| x.body.clone())
    }

    /// The editor closed. When the text form it was opened from is still open, its text goes
    /// back into the form (decision C6): nothing is sent, and a failure or text that isn't
    /// UTF-8 is said in the form, which keeps what it had. With no such form open, the text
    /// goes to `on_text`, and a failure is said on the status line. `Option<String>`
    /// converts: `Some` saved, `None` failed.
    pub(crate) fn on_edited(
        &mut self,
        purpose: EditPurpose,
        edited: impl Into<Edited>,
    ) -> Vec<Command> {
        let edited = edited.into();
        if let Some(Popup::Text {
            purpose: open,
            form,
        }) = &mut self.popup
            && *open == purpose
        {
            refill(form, edited);
            return Vec::new();
        }
        // Defensive: the runtime handles an editor's result before any other input, so the
        // form it was opened from is always still open. If it somehow is not, the result
        // goes through the shared submit path as it did before the form existed.
        match edited {
            Edited::Saved { text, quick } => self.on_text(purpose, text, quick),
            Edited::Failed => self.say(EDITOR_FAILED),
            Edited::NotUtf8 { .. } => self.say(NOT_UTF8),
            Edited::Error(message) => self.say(&message),
        }
    }

    /// Sends `text` for `purpose` (decision C5), shared by the text form (`quick` false) and
    /// an editor whose form is gone. Nothing is sent when the text is empty or unchanged; the
    /// status line says which. Nothing changed after an editor that returned at once
    /// (`quick`) suggests a waiting flag.
    pub(crate) fn on_text(
        &mut self,
        purpose: EditPurpose,
        text: String,
        quick: bool,
    ) -> Vec<Command> {
        let unchanged = |what: &'static str| if quick { RETURNED_AT_ONCE } else { what };
        match purpose {
            EditPurpose::Title(id) => {
                let title = text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_string();
                if title.is_empty() {
                    return self.say("Empty title; nothing was sent");
                }
                if self.item(&id).is_some_and(|i| i.title() == title) {
                    return self.say(unchanged("Title unchanged"));
                }
                self.submit_content(OpKind::Title, id, |item, content| Op::Title {
                    item,
                    content,
                    title,
                })
            }
            EditPurpose::Body(id) => {
                let body = text.trim_end().to_string();
                let current = self.loaded_body(&id).unwrap_or_default();
                // An editor that rewrites line endings has not changed the body.
                if lf(&body).trim_end() == lf(&current).trim_end() {
                    return self.say(unchanged("Body unchanged"));
                }
                self.submit_content(OpKind::Body, id, |item, content| Op::Body {
                    item,
                    content,
                    body,
                })
            }
            EditPurpose::Comment(id) => {
                let body = trim_blank_lines(&text);
                if body.is_empty() {
                    return self.say(unchanged("Empty comment; nothing was sent"));
                }
                self.submit_content(OpKind::Comment, id, |item, content| Op::Comment {
                    item,
                    content,
                    body,
                })
            }
            EditPurpose::NewDraft { presets } => match title_and_body(&text) {
                None => self.say(unchanged("Empty title; nothing was created")),
                Some((title, body)) => {
                    let temp = self.next_temp();
                    self.submit_new(
                        Op::Create {
                            temp,
                            new: NewItem::Draft { title, body },
                            presets,
                        },
                        false,
                    )
                }
            },
            EditPurpose::NewIssue { repo, presets } => match title_and_body(&text) {
                None => self.say(unchanged("Empty title; nothing was created")),
                Some((title, body)) => {
                    let temp = self.next_temp();
                    self.submit_new(
                        Op::Create {
                            temp,
                            new: NewItem::Issue { repo, title, body },
                            presets,
                        },
                        true,
                    )
                }
            },
        }
    }

    /// Submits an edit of the item's issue, pull request or draft, which needs the item still
    /// on the board and its content node id.
    fn submit_content(
        &mut self,
        kind: OpKind,
        id: ItemId,
        op: impl FnOnce(ItemId, ContentId) -> Op,
    ) -> Vec<Command> {
        let Some(item) = self.item(&id) else {
            return self.say(NO_LONGER_ON_BOARD);
        };
        let Some(content) = item.content_id() else {
            return self.say(&self.denial_text(Denial::NeedsRefresh));
        };
        self.submit(kind, op(id, content))
    }

    /// Shows `message` on the status line; nothing to do.
    fn say(&mut self, message: &str) -> Vec<Command> {
        self.status.flash = Some(message.to_string());
        Vec::new()
    }

    /// `check`, plus what the current view rules out: moving needs a column field, and
    /// reordering needs an unsorted view (GitHub's rule); and while the item is converting
    /// to an issue, its content waits (`CONVERTING`). Every edit key, the action menu and
    /// the hints ask this, so the UI never offers what it would then refuse. The reason is
    /// the one-line message to show.
    pub(crate) fn check_here(&self, kind: OpKind, item: &Item) -> Result<(), String> {
        if edits_content(kind) && self.converting(&item.id) {
            return Err(CONVERTING.into());
        }
        check(kind, item, &self.edit_context()).map_err(|d| self.denial_text(d))?;
        match kind {
            OpKind::Move if self.column_field().is_none() => {
                Err("This view has no single-select or iteration field to move along".into())
            }
            OpKind::Reorder => match self.current_view().and_then(|v| v.sort_by.first()) {
                Some(sort) => {
                    let name = self
                        .project()
                        .and_then(|p| p.field(&sort.field))
                        .map_or_else(|| "a field".to_string(), |f| f.name.clone());
                    Err(format!(
                        "This view is sorted by {name}; reordering needs an unsorted view"
                    ))
                }
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// A `Convert` of `item` is still sending: here it is a draft until GitHub answers.
    fn converting(&self, item: &ItemId) -> bool {
        self.store
            .pending()
            .iter()
            .any(|p| matches!(&p.op, Op::Convert { item: i, .. } if i == item))
    }

    /// Moves the item to the neighbouring column among those the board shows (a column the
    /// filter hides is not shown, so it is skipped), purely by position (Decision 12, user-
    /// confirmed): never wraps, never clears. It stops at the first and the last option;
    /// "No <field>", drawn last, is never a destination, and from it `<` goes to the column
    /// before it. Clearing a field is `e`'s job. The table uses the same columns.
    fn move_card(&mut self, step: isize) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        if let Err(reason) = self.check_here(OpKind::Move, &item) {
            self.status.flash = Some(reason);
            return Vec::new();
        }
        let Some(field) = self.column_field().cloned() else {
            return Vec::new();
        };
        let columns = self.board_columns();
        let Some(pos) = columns
            .iter()
            .position(|c| c.items().iter().any(|i| i.id == item.id))
        else {
            self.status.flash = Some("This item is not shown in this view".into());
            return Vec::new();
        };
        // `None`: past either edge, or the "No <field>" column (its key is `None`).
        let target: Option<String> = pos
            .checked_add_signed(step)
            .and_then(|i| columns.get(i))
            .and_then(|c| c.bucket.key.clone());
        let Some(key) = target else {
            self.status.flash = Some(
                if step < 0 {
                    "Already in the first column"
                } else {
                    "Already in the last column"
                }
                .into(),
            );
            return Vec::new();
        };
        let Some(value) = field.value_for_bucket(&key) else {
            self.status.flash = Some(format!("Can't move to that {} column", field.name));
            return Vec::new();
        };
        self.selected_id = Some(item.id.clone());
        self.submit(
            OpKind::Move,
            Op::SetField {
                item: item.id.clone(),
                field: field.id.clone(),
                value,
            },
        )
    }

    /// Moves the item one place up or down among its neighbours: `K` puts it after its upper
    /// neighbour's predecessor in project order (first when there is none), `J` after its
    /// lower neighbour. Needs an unsorted view; stays inside the swimlane or group.
    fn reorder(&mut self, down: bool) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        if let Err(reason) = self.check_here(OpKind::Reorder, &item) {
            self.status.flash = Some(reason);
            return Vec::new();
        }
        let Some(scope) = self.reorder_scope(&item.id) else {
            return Vec::new();
        };
        let Some(pos) = scope.iter().position(|i| *i == item.id) else {
            return Vec::new();
        };
        let neighbour = if down {
            scope.get(pos + 1)
        } else {
            pos.checked_sub(1).and_then(|p| scope.get(p))
        };
        let Some(neighbour) = neighbour.cloned() else {
            self.status.flash = Some(
                if down {
                    "Already last here"
                } else {
                    "Already first here"
                }
                .into(),
            );
            return Vec::new();
        };
        let after = if down {
            Some(neighbour)
        } else {
            match self.predecessor(&neighbour, &item.id) {
                Some(p) => p,
                None => {
                    self.status.flash = Some("Can't place this card here; refresh with r".into());
                    return Vec::new();
                }
            }
        };
        self.selected_id = Some(item.id.clone());
        self.submit(
            OpKind::Reorder,
            Op::Reorder {
                item: item.id,
                after,
            },
        )
    }

    /// The ids `id` can be reordered among, in display order: its swimlane on a board; its
    /// group, or the whole table, otherwise.
    fn reorder_scope(&self, id: &ItemId) -> Option<Vec<ItemId>> {
        if self.layout().0 == Layout::Board {
            return self
                .board_columns()
                .iter()
                .flat_map(|c| c.lanes.iter())
                .find(|l| l.items.iter().any(|i| &i.id == id))
                .map(|l| l.items.iter().map(|i| i.id.clone()).collect());
        }
        let rows = self.table_rows();
        let at = rows
            .iter()
            .position(|r| matches!(r, Row::Item(i) if &i.id == id))?;
        let start = rows[..at]
            .iter()
            .rposition(|r| matches!(r, Row::Group { .. }))
            .map_or(0, |g| g + 1);
        let end = rows[at..]
            .iter()
            .position(|r| matches!(r, Row::Group { .. }))
            .map_or(rows.len(), |g| at + g);
        Some(
            rows[start..end]
                .iter()
                .filter_map(|r| match r {
                    Row::Item(i) => Some(i.id.clone()),
                    Row::Group { .. } => None,
                })
                .collect(),
        )
    }

    /// The item before `of` in project order, skipping `skip` (the item being moved).
    /// `None` when `of` is not in project order; `Some(None)` when nothing precedes it.
    fn predecessor(&self, of: &ItemId, skip: &ItemId) -> Option<Option<ItemId>> {
        let order = &self.snapshot()?.order;
        let at = order.iter().position(|i| i == of)?;
        Some(order[..at].iter().rev().find(|i| *i != skip).cloned())
    }

    /// GitHub answered edit `id`. A failure leaves the pending list (the view rebuilds
    /// without it), shows GitHub's message and goes to the error log. A new item replaces
    /// its placeholder (an open detail of it loads the real item), and presets GitHub
    /// refused are named.
    pub(crate) fn settle(
        &mut self,
        id: OpId,
        result: Result<Confirmed, BackendError>,
    ) -> Vec<Command> {
        let kind = self.pending_kinds.remove(&id);
        let pending = self
            .store
            .pending()
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.op.clone());
        match result {
            Ok(Confirmed::NotAdded { issue, error }) => {
                // Controller ruling E15: the issue exists. The log keeps its id, and a retry
                // only adds it.
                let add = pending.as_ref().and_then(|op| op.add_created(&issue));
                debug_assert!(
                    add.is_some() || pending.is_none(),
                    "only a new issue is created but not added"
                );
                let message = format!(
                    "the issue was created but not added to the project: {}",
                    error_message(&error)
                );
                if self.fail_pending(id, kind, message, add, false) {
                    self.record_refused_scopes(&error);
                }
                Vec::new()
            }
            Ok(confirmed) => {
                if let Confirmed::Created { item, refused, .. } = &confirmed
                    && !refused.is_empty()
                {
                    self.status.error = Some(self.refused_presets(item, refused));
                    for (_, e) in refused {
                        self.record_refused_scopes(e);
                    }
                }
                // A new body or comment shows in the open detail once it is reloaded.
                let reload = pending
                    .as_ref()
                    .and_then(|op| match op {
                        Op::Body { item, .. } | Op::Comment { item, .. } => Some(item.clone()),
                        _ => None,
                    })
                    .filter(|item| self.detail.as_ref().is_some_and(|d| &d.item == item));
                // A new item replaces its placeholder; an open detail of it loads the real one.
                let load = match self.store.settle_ok(id, confirmed) {
                    Some((temp, real)) => {
                        self.renamed(&temp, &real);
                        Some(real)
                            .filter(|real| self.detail.as_ref().is_some_and(|d| &d.item == real))
                    }
                    None => reload,
                };
                load.map(|item| vec![Command::LoadDetail { item, before: None }])
                    .unwrap_or_default()
            }
            Err(error) => {
                // The answer was lost, not refused (a caught panic reports as `Network`): the
                // draft, issue or comment may exist.
                let lost = pending.as_ref().filter(|op| {
                    not_safe_to_resend(op)
                        && matches!(error, BackendError::Network(_) | BackendError::Decode(_))
                });
                let may_exist = lost.is_some();
                let message = match lost {
                    Some(op) => format!("{} ({})", maybe_done(op), error_message(&error)),
                    None => error_message(&error),
                };
                if self.fail_pending(id, kind, message, None, may_exist) {
                    self.record_refused_scopes(&error);
                }
                Vec::new()
            }
        }
    }

    /// "Added 'T', but a field was not set: Status (reason)", naming every refused preset.
    fn refused_presets(&self, item: &Item, refused: &[(FieldId, BackendError)]) -> String {
        let named: Vec<String> = refused
            .iter()
            .map(|(field, e)| {
                let name = self
                    .project()
                    .and_then(|p| p.field(field))
                    .map_or_else(|| field.to_string(), |f| f.name.clone());
                format!("{name} ({})", error_message(e))
            })
            .collect();
        let what = match refused.len() {
            1 => "a field was".to_string(),
            n => format!("{n} fields were"),
        };
        format!(
            "Added '{}', but {what} not set: {}",
            item.title(),
            named.join("; ")
        )
    }

    /// GitHub refused an edit for a missing scope: later edits that need it are blocked with
    /// the fix instead of failing one by one.
    fn record_refused_scopes(&mut self, error: &BackendError) {
        if let BackendError::MissingPermission {
            provider: Provider::Github,
            needed: missing,
        } = error
        {
            // GitHub names one of `repo` / `public_repo`, but either satisfies the write,
            // and `caps::check` blocks only when all of them are refused, so a refused repo
            // scope refuses both.
            let mut refused: Vec<&str> = missing.iter().map(String::as_str).collect();
            if refused.iter().any(|s| matches!(*s, "repo" | "public_repo")) {
                refused.extend(["repo", "public_repo"]);
            }
            for scope in refused {
                if !self.refused_scopes.iter().any(|r| r == scope) {
                    self.refused_scopes.push(scope.to_string());
                }
            }
        }
    }

    /// Takes pending op `id` off the pending list as failed: the status line says so and it
    /// goes to the error log with `message`, logging `instead` (what a retry should send) in
    /// place of the op when given, and whether a new item may exist after all
    /// (`FailedEdit::may_exist`). `false` when no such op is pending.
    fn fail_pending(
        &mut self,
        id: OpId,
        kind: Option<OpKind>,
        message: String,
        instead: Option<Op>,
        may_exist: bool,
    ) -> bool {
        let what = match &instead {
            Some(op) => op.describe(self.snapshot()),
            None => self
                .store
                .pending()
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.op.describe(self.snapshot()))
                .unwrap_or_else(|| "edit".into()),
        };
        let Some(op) = self.store.settle_failed(id) else {
            return false;
        };
        let op = instead.unwrap_or(op);
        let hint = self
            .keymap
            .keys_for(Action::ErrorLog)
            .first()
            .map(|k| format!(" · {} error log", k.label()))
            .unwrap_or_default();
        self.status.error = Some(format!("Could not {what}: {message}{hint}"));
        // The newest failure is row 0: while the log is open, keep the highlight on the
        // same edit.
        if self.mode == Mode::ErrorLog {
            self.error_sel += 1;
        }
        self.errors.push(FailedEdit {
            kind: kind.unwrap_or_else(|| {
                debug_assert!(false, "pending op {id} has no recorded kind");
                OpKind::SetField
            }),
            op,
            what,
            message,
            at: Instant::now(),
            may_exist,
        });
        let extra = self.errors.len().saturating_sub(MAX_ERRORS);
        self.errors.drain(..extra);
        self.error_sel = self.error_sel.min(self.errors.len() - 1);
        // A new item that failed while its placeholder's detail was open.
        self.close_gone_detail();
        true
    }

    /// Every edit still pending goes to the error log as not sent (its board is being
    /// replaced, so it never will be). A new draft or issue, or a comment, says to check
    /// first instead, and `R` refuses it: it may have been sending, and its result is dropped
    /// with the old board.
    pub(super) fn fail_all_pending(&mut self, message: &str) {
        let ids: Vec<(OpId, bool)> = self
            .store
            .pending()
            .iter()
            .map(|p| (p.id, not_safe_to_resend(&p.op)))
            .collect();
        for (id, may_exist) in ids {
            let kind = self.pending_kinds.remove(&id);
            let message = if may_exist {
                MAYBE_SENT_RELOADED
            } else {
                message
            };
            self.fail_pending(id, kind, message.to_string(), None, may_exist);
        }
    }

    /// A reported scope list that now includes a refused scope lifts that refusal, so a
    /// refreshed token unblocks edits without a restart.
    pub(crate) fn set_scopes(&mut self, scopes: Option<Vec<String>>) {
        if let Some(granted) = &scopes {
            self.refused_scopes.retain(|r| !granted.contains(r));
        }
        self.scopes = scopes;
    }

    pub(crate) fn open_error_log(&mut self) -> Vec<Command> {
        self.error_sel = 0;
        // The failure the status line announced is now being read.
        self.status.error = None;
        self.mode = Mode::ErrorLog;
        Vec::new()
    }

    /// The error log lists failures newest first; `error_sel` counts from the newest. Esc
    /// closes it (never quits); the movement keys move the selection.
    pub(crate) fn handle_error_log_key(&mut self, action: Option<Action>) -> Vec<Command> {
        let last = self.errors.len().saturating_sub(1);
        match action {
            Some(Action::Down) => self.error_sel = (self.error_sel + 1).min(last),
            Some(Action::Up) => self.error_sel = self.error_sel.saturating_sub(1),
            Some(Action::Top) => self.error_sel = 0,
            Some(Action::Bottom) => self.error_sel = last,
            Some(Action::Retry) => return self.retry_selected(),
            Some(Action::Back | Action::ErrorLog) => self.mode = self.base_mode(),
            Some(Action::Quit) => {
                self.mode = self.base_mode();
                return self.request_quit();
            }
            _ => {}
        }
        Vec::new()
    }

    /// Drops the selected entry from the log (a stale one for an item that is gone).
    pub(crate) fn dismiss_selected(&mut self) {
        if let Some(index) = self.errors.len().checked_sub(1 + self.error_sel) {
            self.errors.remove(index);
        }
        self.error_sel = self.error_sel.min(self.errors.len().saturating_sub(1));
    }

    /// Sends the selected failed edit again as a fresh op, after the capability matrix has
    /// had its say (the item may have become read-only, archived or gone). The log stays
    /// open. When the matrix refuses, the entry stays and the reason shows. A new item or a
    /// comment that may exist after all (`FailedEdit::may_exist`) is never sent again: the
    /// entry stays and `MAYBE_CREATED` or `MAYBE_POSTED` shows.
    fn retry_selected(&mut self) -> Vec<Command> {
        let Some(index) = self.errors.len().checked_sub(1 + self.error_sel) else {
            return Vec::new();
        };
        if self.errors[index].may_exist {
            self.status.flash = Some(maybe_done(&self.errors[index].op).into());
            return Vec::new();
        }
        let failed = self.errors.remove(index);
        let cmds = match &failed.op {
            Op::Create { new, .. } => {
                self.submit_new(failed.op.clone(), matches!(new, NewItem::Issue { .. }))
            }
            _ => self.submit(failed.kind, failed.op.clone()),
        };
        if cmds.is_empty() {
            self.errors.insert(index, failed);
        }
        self.error_sel = self.error_sel.min(self.errors.len().saturating_sub(1));
        cmds
    }

    /// After an edit settles: an open quit question is recounted, or the app quits plainly
    /// when nothing is sending any more.
    pub(crate) fn refresh_quit_question(&mut self) -> Vec<Command> {
        if self.mode != Mode::Confirm
            || !self
                .confirm
                .as_ref()
                .is_some_and(|c| c.action == ConfirmAction::Quit)
        {
            return Vec::new();
        }
        self.confirm = None;
        self.mode = self.base_mode();
        self.request_quit()
    }

    /// Quits, or asks first while edits are still sending.
    pub(crate) fn request_quit(&mut self) -> Vec<Command> {
        let n = self.pending_count();
        if n == 0 {
            self.quit = true;
            return vec![Command::Quit];
        }
        let (s, verb) = if n == 1 { ("", "is") } else { ("s", "are") };
        self.confirm = Some(Confirm {
            question: format!("{n} edit{s} {verb} still sending; quit anyway? (y/n)"),
            action: ConfirmAction::Quit,
        });
        self.mode = Mode::Confirm;
        Vec::new()
    }

    pub(crate) fn handle_confirm_key(&mut self, key: &KeyEvent) -> Vec<Command> {
        let Some(confirm) = self.confirm.take() else {
            self.mode = self.base_mode();
            return Vec::new();
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.mode = self.base_mode();
                self.run_confirmed(confirm.action)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = self.base_mode();
                Vec::new()
            }
            _ => {
                self.confirm = Some(confirm);
                Vec::new()
            }
        }
    }

    fn run_confirmed(&mut self, action: ConfirmAction) -> Vec<Command> {
        match action {
            ConfirmAction::Quit => {
                self.quit = true;
                vec![Command::Quit]
            }
            ConfirmAction::Submit { kind, op } => self.submit(kind, op),
        }
    }

    /// `A`: archive the item, or unarchive an archived one. No confirmation: unarchiving
    /// undoes it.
    pub(crate) fn archive_or_unarchive(&mut self) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        let (kind, op) = if item.archived {
            (OpKind::Unarchive, Op::Unarchive { item: item.id })
        } else {
            (OpKind::Archive, Op::Archive { item: item.id })
        };
        self.submit(kind, op)
    }

    /// Asks before removing the item from the project.
    pub(crate) fn confirm_remove(&mut self) -> Vec<Command> {
        let Some(item) = self.guarded(OpKind::Remove) else {
            return Vec::new();
        };
        let title = crate::ui::text::truncate_to_width(item.title(), 40);
        let question = match (&item.content, item.number()) {
            (ItemContent::Draft { .. }, _) => {
                format!("Remove the draft '{title}' from the project? The draft is deleted. (y/n)")
            }
            (_, Some(n)) => {
                format!("Remove #{n} '{title}' from the project? It stays in its repository. (y/n)")
            }
            (_, None) => format!("Remove '{title}' from the project? (y/n)"),
        };
        self.confirm = Some(Confirm {
            question,
            action: ConfirmAction::Submit {
                kind: OpKind::Remove,
                op: Op::Remove { item: item.id },
            },
        });
        self.mode = Mode::Confirm;
        Vec::new()
    }

    /// Whether the view's own GitHub filter already shows archived items.
    pub(crate) fn view_shows_archived(&self) -> bool {
        self.current_view()
            .is_some_and(|v| archived_token(&v.filter).is_some())
    }

    /// "Show archived items" in the action menu: toggles `is:archived` in the session filter,
    /// leaving the rest of the user's filter text exactly as typed. A view whose own filter
    /// already has it needs nothing added.
    pub(crate) fn toggle_archived(&mut self) -> Vec<Command> {
        if self.view_shows_archived() {
            self.status.flash = Some("This view already shows archived items".into());
            return Vec::new();
        }
        let current = self.extra_filter().unwrap_or_default().to_string();
        let next = match archived_token(&current) {
            Some((start, end)) => {
                let mut text = current.clone();
                // The token and one adjacent space.
                if start > 0 {
                    text.replace_range(start - 1..end, "");
                } else if text[end..].starts_with(char::is_whitespace) {
                    let n = text[end..].chars().next().map_or(0, char::len_utf8);
                    text.replace_range(start..end + n, "");
                } else {
                    text.replace_range(start..end, "");
                }
                text
            }
            None if current.trim().is_empty() => "is:archived".to_string(),
            None => format!("{current} is:archived"),
        };
        self.set_extra_filter(&next)
    }

    /// `X`: reopen a closed item at once; close an open one after asking (a reason for an
    /// issue, yes or no for a pull request).
    pub(crate) fn close_or_reopen(&mut self) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        let ctx = self.edit_context();
        let kind = match (
            check(OpKind::Close, &item, &ctx),
            check(OpKind::Reopen, &item, &ctx),
        ) {
            (Ok(()), _) => OpKind::Close,
            (_, Ok(())) => OpKind::Reopen,
            (Err(close), Err(reopen)) => {
                // The reason that explains the item's state: reopen's for closed items.
                let shown = if matches!(close, Denial::AlreadyClosed) {
                    reopen
                } else {
                    close
                };
                self.status.flash = Some(self.denial_text(shown));
                return Vec::new();
            }
        };
        let Some(content) = item.content_id() else {
            self.status.flash = Some(self.denial_text(Denial::NeedsRefresh));
            return Vec::new();
        };
        let title = crate::ui::text::truncate_to_width(item.title(), 40);
        let reference = match item.number() {
            Some(n) => format!("#{n} '{title}'"),
            None => format!("'{title}'"),
        };
        match (kind, &content) {
            (OpKind::Reopen, _) => self.submit(
                kind,
                Op::Reopen {
                    item: item.id,
                    content,
                },
            ),
            (_, ContentId::Issue(_)) => {
                let picker = ListPicker::new(
                    format!("Close {reference}?"),
                    vec![
                        Choice::new("Close as completed", CloseReason::Completed),
                        Choice::new("Close as not planned", CloseReason::NotPlanned),
                    ],
                );
                self.open_popup(Popup::CloseReason {
                    item: item.id,
                    picker,
                });
                Vec::new()
            }
            _ => {
                self.confirm = Some(Confirm {
                    question: format!("Close pull request {reference}? (y/n)"),
                    action: ConfirmAction::Submit {
                        kind,
                        op: Op::Close {
                            item: item.id,
                            content,
                            reason: None,
                        },
                    },
                });
                self.mode = Mode::Confirm;
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::GraphqlErrorEntry;
    use crate::model::{FieldId, FieldValue, OptionId};
    use crate::ui::app::test_app;
    use crate::ui::fixtures::{code, key, snapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    pub(crate) fn set_status(item: &str, option: &str, name: &str) -> Op {
        Op::SetField {
            item: ItemId::new(item),
            field: FieldId::new("F_status"),
            value: FieldValue::SingleSelect {
                option_id: OptionId::new(option),
                name: name.into(),
            },
        }
    }

    pub(crate) fn status(s: Option<&crate::store::BoardSnapshot>, item: &str) -> String {
        s.unwrap().items[&ItemId::new(item)]
            .value(&FieldId::new("F_status"))
            .map(|v| v.display())
            .unwrap_or_default()
    }

    #[test]
    fn candidates_load_once_per_repository_and_a_failure_can_be_retried() {
        let mut a = test_app();
        let repo: RepoSlug = "tviles/t".parse().unwrap();
        assert_eq!(
            a.need_candidates(&repo),
            vec![Command::LoadCandidates(repo.clone())]
        );
        assert!(a.need_candidates(&repo).is_empty(), "already loading");
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Candidates(repo.clone()),
            error: BackendError::Network("offline".into()),
        });
        assert!(
            matches!(a.candidates.get(&repo), Some(Lookup::Failed(m)) if m.contains("offline"))
        );
        assert_eq!(
            a.status.error, None,
            "the popover shows it, not the status line"
        );
        assert_eq!(
            a.need_candidates(&repo),
            vec![Command::LoadCandidates(repo.clone())]
        );
        let candidates = crate::model::RepoCandidates {
            repo: crate::model::RepoRef {
                id: "R_1".into(),
                slug: repo.clone(),
            },
            users: vec![],
            labels: vec![],
            milestones: vec![],
            truncated: false,
        };
        a.on_sync(SyncEvent::Candidates {
            repo: repo.clone(),
            candidates,
        });
        assert!(matches!(a.candidates.get(&repo), Some(Lookup::Ready(_))));
        assert!(a.need_candidates(&repo).is_empty());
    }

    #[test]
    fn drafts_take_candidates_from_the_pane_repository_then_the_first_linked_one() {
        let mut a = test_app();
        let draft = a.item(&ItemId::new("d")).unwrap().clone();
        assert_eq!(a.candidate_repo(&draft), None);
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.linked_repos = vec![crate::model::RepoRef {
            id: "R_2".into(),
            slug: "acme/api".parse().unwrap(),
        }];
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert_eq!(
            a.candidate_repo(&draft).map(|r| r.to_string()).as_deref(),
            Some("acme/api")
        );
        a.pane_repo = Some("acme/app".parse().unwrap());
        assert_eq!(
            a.candidate_repo(&draft).map(|r| r.to_string()).as_deref(),
            Some("acme/app")
        );
        let issue = a.item(&ItemId::new("a")).unwrap().clone();
        assert_eq!(
            a.candidate_repo(&issue).map(|r| r.to_string()).as_deref(),
            Some("tviles/t"),
            "its own repository"
        );
    }

    #[test]
    fn a_failed_search_that_was_replaced_is_ignored() {
        let mut a = test_app();
        let scope: Vec<RepoSlug> = vec!["acme/app".parse().unwrap()];
        a.start_search("one", scope.clone());
        a.start_search("two", scope);
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Search("one".into()),
            error: BackendError::Network("offline".into()),
        });
        assert!(matches!(&a.content_search, Some((q, Lookup::Loading)) if q == "two"));
        assert_eq!(a.status.error, None);
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Search("two".into()),
            error: BackendError::Network("offline".into()),
        });
        assert!(matches!(&a.content_search, Some((_, Lookup::Failed(_)))));
    }

    #[test]
    fn a_new_board_forgets_lookups_and_unanswered_jobs_can_be_retried() {
        let mut a = test_app();
        let repo: RepoSlug = "acme/app".parse().unwrap();
        a.need_candidates(&repo);
        a.need_repos();
        a.need_repo_lookup(&repo);
        a.start_search("q", vec!["acme/app".parse().unwrap()]);
        a.reset_for_new_board();
        assert!(a.candidates.is_empty() && a.repos.is_none() && a.repo_lookups.is_empty());
        assert!(a.content_search.is_none());
        // The runtime answers a job it could not run with a Failed; the entry is retryable.
        a.need_candidates(&repo);
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Candidates(repo.clone()),
            error: BackendError::NoCredentials {
                provider: Provider::Github,
            },
        });
        assert_eq!(
            a.need_candidates(&repo),
            vec![Command::LoadCandidates(repo.clone())]
        );
        assert_eq!(a.mode, Mode::Normal, "no setup screen for a lookup");
    }

    #[test]
    fn an_over_long_search_fails_early_and_a_long_scope_is_cut() {
        let mut a = test_app();
        assert!(a.start_search(&"x".repeat(300), vec![]).is_empty());
        assert!(
            matches!(&a.content_search, Some((_, Lookup::Failed(m))) if m.contains("too long"))
        );
        let scope: Vec<RepoSlug> = (0..40)
            .map(|i| format!("acme/repository-{i:02}").parse().unwrap())
            .collect();
        assert_eq!(a.start_search("crash", scope).len(), 1);
        assert!(
            a.status
                .flash
                .as_deref()
                .is_some_and(|f| f.starts_with("searching the first "))
        );
    }

    #[test]
    fn only_the_latest_search_answer_is_kept() {
        let mut a = test_app();
        let scope: Vec<RepoSlug> = vec!["acme/app".parse().unwrap()];
        a.start_search("one", scope.clone());
        a.start_search("two", scope);
        a.on_sync(SyncEvent::SearchResults {
            query: "one".into(),
            hits: vec![],
        });
        assert!(matches!(&a.content_search, Some((q, Lookup::Loading)) if q == "two"));
        a.on_sync(SyncEvent::SearchResults {
            query: "two".into(),
            hits: vec![],
        });
        assert!(matches!(&a.content_search, Some((q, Lookup::Ready(_))) if q == "two"));
    }

    #[test]
    fn an_allowed_edit_shows_at_once_and_is_sent() {
        let mut a = test_app();
        let cmds = a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        assert_eq!(cmds, vec![Command::Send(1)]);
        assert_eq!(status(a.snapshot(), "a"), "Done");
        assert_eq!(status(a.store.confirmed(), "a"), "Todo");
        assert_eq!(a.pending_count(), 1);
    }

    #[test]
    fn a_read_only_board_says_why_and_sends_nothing() {
        let mut snap = snapshot();
        snap.project.viewer_can_update = false;
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        assert!(
            a.submit(OpKind::Move, set_status("a", "o_done", "Done"))
                .is_empty()
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn a_confirmed_edit_lands_in_the_confirmed_board() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(status(a.store.confirmed(), "a"), "Done");
        assert_eq!(a.pending_count(), 0);
        assert!(a.errors.is_empty());
    }

    #[test]
    fn a_failed_edit_rolls_back_and_shows_githubs_message() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        let error = BackendError::Api(vec![GraphqlErrorEntry {
            message: "The field is archived".into(),
            kind: None,
        }]);
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(error),
        });
        assert_eq!(status(a.snapshot(), "a"), "Todo");
        assert_eq!(
            a.status.error.as_deref(),
            Some(
                "Could not set Status to Done on 'Fix crash': The field is archived · E error log"
            )
        );
        assert_eq!(a.errors.len(), 1);
        assert_eq!(
            (a.errors[0].kind, a.errors[0].message.as_str()),
            (OpKind::Move, "The field is archived")
        );
    }

    #[test]
    fn a_refused_scope_blocks_later_edits_with_the_fix() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        let error = BackendError::MissingPermission {
            provider: Provider::Github,
            needed: vec!["project".into()],
        };
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(error),
        });
        assert!(
            a.submit(OpKind::Move, set_status("b", "o_done", "Done"))
                .is_empty()
        );
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .contains("gh auth refresh -s project")
        );
    }

    #[test]
    fn reported_scopes_without_project_block_edits() {
        let mut a = test_app();
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        assert!(
            a.submit(OpKind::Move, set_status("a", "o_done", "Done"))
                .is_empty()
        );
        assert!(a.status.flash.as_deref().unwrap().contains("project scope"));
    }

    /// Review Focus 3.
    #[test]
    fn quitting_with_edits_sending_asks_first() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        assert!(a.handle_key(key('q')).is_empty());
        assert_eq!(a.mode, Mode::Confirm);
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "1 edit is still sending; quit anyway? (y/n)"
        );
        a.handle_key(key('n'));
        assert_eq!((a.mode, a.quit), (Mode::Normal, false));
        assert!(
            a.handle_key(key('B')).is_empty(),
            "no board switch while sending"
        );
        assert!(a.status.flash.as_deref().unwrap().contains("still sending"));
        a.handle_key(key('q'));
        assert_eq!(a.handle_key(key('y')), vec![Command::Quit]);
        assert!(a.quit);
        let mut b = test_app();
        b.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            b.handle_key(ctrl_c),
            vec![Command::Quit],
            "Ctrl+C never asks"
        );
        let mut c = test_app();
        assert_eq!(
            c.handle_key(code(KeyCode::Char('q'))),
            vec![Command::Quit],
            "nothing pending: quits at once"
        );
    }

    fn refused(scope: &str) -> BackendError {
        BackendError::MissingPermission {
            provider: Provider::Github,
            needed: vec![scope.into()],
        }
    }

    #[test]
    fn a_refused_scope_is_recorded_once_and_lifted_when_the_token_reports_it() {
        let mut a = test_app();
        for _ in 0..2 {
            let cmds = a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
            assert_eq!(cmds.len(), 1);
            let id = a.store.pending().last().unwrap().id;
            a.on_sync(SyncEvent::OpDone {
                id,
                result: Err(refused("project")),
            });
            if a.refused_scopes.len() == 1
                && a.submit(OpKind::Move, set_status("a", "o_done", "Done"))
                    .is_empty()
            {
                break;
            }
        }
        assert_eq!(a.refused_scopes, vec!["project".to_string()]);
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        assert_eq!(
            a.refused_scopes,
            vec!["project".to_string()],
            "unrelated scopes keep it"
        );
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["project".into(), "repo".into()]),
        });
        assert!(a.refused_scopes.is_empty());
        assert_eq!(
            a.submit(OpKind::Move, set_status("b", "o_done", "Done"))
                .len(),
            1
        );
    }

    #[test]
    fn a_refused_repo_scope_refuses_both_repo_and_public_repo() {
        for named in ["repo", "public_repo"] {
            let mut a = test_app();
            a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
            a.on_sync(SyncEvent::OpDone {
                id: 1,
                result: Err(refused(named)),
            });
            let mut got = a.refused_scopes.clone();
            got.sort();
            assert_eq!(got, ["public_repo", "repo"], "named {named}");
        }
    }

    #[test]
    fn a_repeated_refusal_does_not_duplicate_the_scope() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(refused("project")),
        });
        a.on_sync(SyncEvent::OpDone {
            id: 2,
            result: Err(refused("project")),
        });
        assert_eq!(a.refused_scopes, vec!["project".to_string()]);
    }

    #[test]
    fn esc_cancels_the_quit_question() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.handle_key(key('q'));
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert_eq!((a.mode, a.quit), (Mode::Normal, false));
        assert!(a.confirm.is_none());
    }

    #[test]
    fn the_quit_question_returns_to_the_detail() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        a.open_detail(ItemId::new("a"));
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(key('q'));
        assert_eq!(a.mode, Mode::Confirm);
        a.handle_key(key('n'));
        assert_eq!(a.mode, Mode::Detail);
        assert!(a.detail.is_some());
    }

    #[test]
    fn help_then_q_asks_when_edits_are_sending() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.handle_key(key('?'));
        assert_eq!(a.mode, Mode::Help);
        assert!(a.handle_key(key('q')).is_empty());
        assert_eq!(a.mode, Mode::Confirm);
    }

    #[test]
    fn answers_for_unknown_or_repeated_ids_change_nothing() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.on_sync(SyncEvent::OpDone {
            id: 99,
            result: Err(BackendError::Unauthorized {
                provider: Provider::Github,
            }),
        });
        assert_eq!(
            (a.pending_count(), a.errors.len(), a.status.error.clone()),
            (1, 0, None)
        );
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        let before = status(a.store.confirmed(), "a");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(BackendError::Unauthorized {
                provider: Provider::Github,
            }),
        });
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(
            (a.pending_count(), a.errors.len(), a.status.error.clone()),
            (0, 0, None)
        );
        assert_eq!(status(a.store.confirmed(), "a"), before);
    }

    #[test]
    fn a_failure_after_the_item_vanished_still_goes_to_the_log() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.store
            .apply(crate::store::StoreUpdate::ReplaceItems(Vec::new()));
        let error = BackendError::Api(vec![GraphqlErrorEntry {
            message: "Gone".into(),
            kind: None,
        }]);
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(error),
        });
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.errors[0].what, "set Status to Done on 'an item'");
    }

    #[test]
    fn selection_stays_on_the_same_item_through_settles() {
        for ok in [true, false] {
            let mut a = test_app();
            a.handle_key(key('j'));
            let sel = a.selected_item().unwrap().id.clone();
            a.submit(OpKind::Move, set_status(sel.as_str(), "o_done", "Done"));
            assert_eq!(a.selected_item().unwrap().id, sel);
            let result = if ok {
                Ok(Confirmed::Applied)
            } else {
                Err(BackendError::Unauthorized {
                    provider: Provider::Github,
                })
            };
            a.on_sync(SyncEvent::OpDone { id: 1, result });
            assert_eq!(a.selected_item().unwrap().id, sel, "ok={ok}");
        }
    }

    #[test]
    fn the_quit_question_closes_itself_when_the_last_edit_settles() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        a.handle_key(key('q'));
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "2 edits are still sending; quit anyway? (y/n)"
        );
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert!(cmds.is_empty());
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "1 edit is still sending; quit anyway? (y/n)"
        );
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 2,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(cmds, vec![Command::Quit]);
        assert!(a.quit);
    }

    #[test]
    fn a_graphql_error_without_messages_has_no_dangling_colon() {
        let none = BackendError::Api(vec![]);
        assert_eq!(error_message(&none), "GitHub returned an error");
        let blank = BackendError::Api(vec![GraphqlErrorEntry {
            message: " ".into(),
            kind: None,
        }]);
        assert_eq!(error_message(&blank), "GitHub returned an error");
    }

    fn on_board(a: &mut App) {
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.handle_key(code(KeyCode::Home));
    }

    fn selected(a: &App) -> String {
        a.selected_item()
            .map(|i| i.id.0.clone())
            .unwrap_or_default()
    }

    #[test]
    fn moving_follows_the_columns_and_the_cursor_follows_the_card() {
        let mut a = test_app();
        on_board(&mut a);
        assert_eq!(selected(&a), "a");
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        assert_eq!(status(a.snapshot(), "a"), "In Progress");
        assert_eq!(selected(&a), "a", "the cursor moved with the card");
        assert_eq!(a.board_sel.column, 1);
        assert_eq!(a.handle_key(key('<')), vec![Command::Send(2)]);
        assert_eq!(status(a.snapshot(), "a"), "Todo");
    }

    /// Review Focus 2.
    #[test]
    fn quick_moves_follow_the_shown_columns_and_stop_at_the_last_option() {
        let mut a = test_app();
        on_board(&mut a);
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(2)]);
        assert_eq!(
            status(a.snapshot(), "a"),
            "Done",
            "Todo → In Progress → Done"
        );
        assert_eq!(
            a.handle_key(key('>')),
            Vec::<Command>::new(),
            "never into No Status: clearing is e's job"
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Already in the last column")
        );
        let names: Vec<&str> = a.store.pending().iter().map(|p| p.op.name()).collect();
        assert_eq!(names, ["SetField", "SetField"], "no ClearField ever");
    }

    #[test]
    fn the_first_column_stops_a_left_move() {
        let mut a = test_app();
        on_board(&mut a);
        assert!(a.handle_key(key('<')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Already in the first column")
        );
    }

    #[test]
    fn a_card_on_a_deleted_option_moves_from_no_status() {
        let mut a = test_app();
        on_board(&mut a);
        for _ in 0..3 {
            a.handle_key(key('l'));
        }
        assert_eq!(selected(&a), "e");
        assert!(
            a.handle_key(key('>')).is_empty(),
            "No Status is last: nothing to the right"
        );
        a.handle_key(key('<'));
        assert_eq!(status(a.snapshot(), "e"), "Done");
    }

    #[test]
    fn moves_skip_columns_the_filter_hides_and_stop_before_no_status() {
        let mut snap = snapshot();
        snap.project.views[1].filter = "-status:\"In Progress\"".into();
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board,
            filter: "-status:\"In Progress\"".into(),
            list: crate::store::ViewList {
                ids: ["a", "c", "d", "e"]
                    .iter()
                    .map(|i| ItemId::new(*i))
                    .collect(),
                total: 4,
                truncated: false,
            },
        });
        a.handle_key(code(KeyCode::Home));
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        assert_eq!(
            status(a.snapshot(), "a"),
            "Done",
            "the hidden In Progress column is skipped"
        );
        assert!(a.handle_key(key('>')).is_empty());
    }

    #[test]
    fn table_moves_use_the_views_column_field() {
        let mut a = test_app();
        assert_eq!(selected(&a), "a");
        a.handle_key(key('>'));
        assert_eq!(status(a.snapshot(), "a"), "In Progress");
    }

    #[test]
    fn reorder_moves_past_the_neighbour_in_the_table() {
        let mut a = test_app();
        a.handle_key(key('J'));
        assert_eq!(
            a.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("a"),
                after: Some(ItemId::new("b"))
            }
        );
        let mut b = test_app();
        b.handle_key(key('j'));
        b.handle_key(key('j'));
        b.handle_key(key('K'));
        assert_eq!(
            b.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("c"),
                after: Some(ItemId::new("a"))
            }
        );
        let mut c = test_app();
        c.handle_key(key('j'));
        c.handle_key(key('K'));
        assert_eq!(
            c.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("b"),
                after: None
            }
        );
        assert_eq!(selected(&c), "b", "the cursor stays on the moved item");
        let mut d = test_app();
        assert!(d.handle_key(key('K')).is_empty());
        assert_eq!(d.status.flash.as_deref(), Some("Already first here"));
    }

    #[test]
    fn reorder_stays_inside_a_swimlane() {
        let mut a = test_app();
        on_board(&mut a);
        // Todo holds a (lane P0) and c (lane P2).
        assert!(a.handle_key(key('J')).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some("Already last here"));
    }

    #[test]
    fn a_sorted_view_refuses_reorder() {
        let mut snap = snapshot();
        snap.project.views[0].sort_by = vec![crate::model::SortSpec {
            field: FieldId::new("F_prio"),
            direction: crate::model::SortDirection::Desc,
        }];
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        assert!(a.handle_key(key('J')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This view is sorted by Priority; reordering needs an unsorted view")
        );
    }

    #[test]
    fn a_private_item_moves_but_does_not_reorder() {
        let mut snap = snapshot();
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        assert!(a.handle_key(key('J')).is_empty());
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .starts_with("You can't see this item's content")
        );
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
    }

    #[test]
    fn edit_keys_act_on_the_open_detail_item() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        assert_eq!(status(a.snapshot(), "a"), "In Progress");
        assert_eq!(a.mode, Mode::Detail);
    }

    fn app_with(snap: crate::store::BoardSnapshot) -> App {
        App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        )
    }

    fn set_value(snap: &mut crate::store::BoardSnapshot, item: &str, field: &str, v: FieldValue) {
        snap.items
            .get_mut(&ItemId::new(item))
            .unwrap()
            .values
            .insert(FieldId::new(field), v);
    }

    fn select_value(id: &str, name: &str) -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new(id),
            name: name.into(),
        }
    }

    /// Todo / P0 holds a, b, c (project order); the board view has no filter.
    fn one_lane() -> crate::store::BoardSnapshot {
        let mut snap = snapshot();
        set_value(&mut snap, "b", "F_status", select_value("o_todo", "Todo"));
        set_value(&mut snap, "b", "F_prio", select_value("p0", "P0"));
        set_value(&mut snap, "c", "F_prio", select_value("p0", "P0"));
        snap
    }

    #[test]
    fn the_cursor_follows_the_card_on_the_very_first_keypress() {
        let mut a = test_app();
        a.select_view(&crate::model::ViewId::new("V_board"));
        assert_eq!(a.selected_id, None, "no navigation key yet");
        a.handle_key(key('>'));
        assert_eq!(selected(&a), "a");
        assert_eq!(a.board_sel.column, 1);

        let mut b = app_with(one_lane());
        b.select_view(&crate::model::ViewId::new("V_board"));
        b.board_sel = crate::ui::board::BoardSelection {
            column: 0,
            index: 1,
        };
        assert_eq!(selected(&b), "b");
        b.handle_key(key('K'));
        assert_eq!(selected(&b), "b", "the cursor stays on the reordered card");
    }

    #[test]
    fn the_first_snapshot_anchors_the_cursor() {
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(None)),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        a.on_sync(SyncEvent::Project(snapshot().project));
        a.on_sync(SyncEvent::ItemsPage {
            items: crate::ui::fixtures::items(),
            loaded: 5,
            total: 5,
        });
        assert_eq!(a.selected_id, Some(ItemId::new("a")));
    }

    #[test]
    fn a_board_reorder_inside_a_lane_uses_project_order() {
        let mut a = app_with(one_lane());
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.board_sel = crate::ui::board::BoardSelection {
            column: 0,
            index: 2,
        };
        assert_eq!(selected(&a), "c");
        assert_eq!(a.handle_key(key('K')), vec![Command::Send(1)]);
        assert_eq!(
            a.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("c"),
                after: Some(ItemId::new("a"))
            }
        );
        a.board_sel = crate::ui::board::BoardSelection {
            column: 0,
            index: 0,
        };
        a.sync_selected_id();
        a.handle_key(key('J'));
        assert_eq!(
            a.store.pending()[1].op,
            Op::Reorder {
                item: ItemId::new("a"),
                after: Some(ItemId::new("c"))
            }
        );
    }

    #[test]
    fn k_after_a_hidden_predecessor_still_follows_project_order() {
        let mut snap = one_lane();
        snap.order = ["b", "a", "c", "d", "e"]
            .iter()
            .map(|i| ItemId::new(*i))
            .collect();
        snap.project.views[1].filter = "-status:\"In Progress\"".into();
        // b is In Progress again, so the filter hides it.
        set_value(
            &mut snap,
            "b",
            "F_status",
            select_value("o_prog", "In Progress"),
        );
        let mut a = app_with(snap);
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board,
            filter: "-status:\"In Progress\"".into(),
            list: crate::store::ViewList {
                ids: ["a", "c", "d", "e"]
                    .iter()
                    .map(|i| ItemId::new(*i))
                    .collect(),
                total: 4,
                truncated: false,
            },
        });
        a.board_sel = crate::ui::board::BoardSelection {
            column: 0,
            index: 1,
        };
        assert_eq!(selected(&a), "c");
        a.handle_key(key('K'));
        assert_eq!(
            a.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("c"),
                after: Some(ItemId::new("b"))
            }
        );
    }

    #[test]
    fn a_missing_predecessor_refuses_instead_of_sending_first() {
        // b is in the view's list but not in project order (say, hydrated late).
        let mut snap = one_lane();
        snap.order = ["a", "c", "d", "e"]
            .iter()
            .map(|i| ItemId::new(*i))
            .collect();
        snap.project.views[1].filter = "-status:\"In Progress\"".into();
        let mut a = app_with(snap);
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board,
            filter: "-status:\"In Progress\"".into(),
            list: crate::store::ViewList {
                ids: ["a", "b", "c", "d", "e"]
                    .iter()
                    .map(|i| ItemId::new(*i))
                    .collect(),
                total: 5,
                truncated: false,
            },
        });
        a.board_sel = crate::ui::board::BoardSelection {
            column: 0,
            index: 2,
        };
        assert_eq!(selected(&a), "c");
        assert!(a.handle_key(key('K')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Can't place this card here; refresh with r")
        );
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn moves_into_and_out_of_a_merged_column_use_the_canonical_option() {
        let mut snap = snapshot();
        let col_field = snap
            .project
            .fields
            .iter_mut()
            .find(|f| f.name == "Status")
            .unwrap();
        let crate::model::FieldKind::SingleSelect { options } = &mut col_field.kind else {
            panic!("Status is single-select")
        };
        options.push(crate::model::SelectOption {
            id: OptionId::new("o_dup"),
            name: "Todo".into(),
            color: crate::model::OptionColor::Blue,
        });
        let mut a = app_with(snap);
        a.select_view(&crate::model::ViewId::new("V_board"));
        let names: Vec<String> = a
            .board_columns()
            .iter()
            .map(|c| c.bucket.title.clone())
            .collect();
        assert_eq!(names, ["In Progress", "Done", "Todo", "No Status"]);
        // d (Done) moves into the merged column: the canonical id is the last option's.
        a.board_sel = crate::ui::board::BoardSelection {
            column: 1,
            index: 0,
        };
        assert_eq!(selected(&a), "d");
        a.handle_key(key('>'));
        assert_eq!(a.store.pending()[0].op, set_status("d", "o_dup", "Todo"));
        // a sits on the alias id o_todo and moves out of the same column.
        let col = a
            .board_columns()
            .iter()
            .position(|c| c.items().iter().any(|i| i.id.0 == "a"))
            .unwrap();
        assert_eq!(col, 2);
        a.board_sel = crate::ui::board::BoardSelection {
            column: 2,
            index: 0,
        };
        a.sync_selected_id();
        assert_eq!(selected(&a), "a");
        a.handle_key(key('<'));
        assert_eq!(status(a.snapshot(), "a"), "Done");
    }

    #[test]
    fn a_group_header_asks_for_an_item() {
        let mut snap = snapshot();
        snap.project.views[0].group_by = vec![FieldId::new("F_status")];
        let mut a = app_with(snap);
        assert!(
            a.selected_item().is_none(),
            "the cursor is on a group header"
        );
        assert!(a.handle_key(key('>')).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some("Select an item first"));
    }

    #[test]
    fn edit_keys_reorder_the_open_detail_item() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(key('J'));
        assert_eq!(
            a.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("a"),
                after: Some(ItemId::new("b"))
            }
        );
        assert_eq!(a.mode, Mode::Detail);
    }

    use crate::store::ViewList;

    fn app_edited(edit: impl FnOnce(&mut crate::store::BoardSnapshot)) -> App {
        let mut snap = snapshot();
        edit(&mut snap);
        App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        )
    }

    fn list(ids: &[&str]) -> ViewList {
        ViewList {
            ids: ids.iter().map(|i| ItemId::new(*i)).collect(),
            total: ids.len(),
            truncated: false,
        }
    }

    fn column_titles(a: &App) -> Vec<String> {
        a.board_columns()
            .iter()
            .map(|c| c.bucket.title.clone())
            .collect()
    }

    #[test]
    fn a_card_moved_out_of_a_filtered_board_waits_in_a_filtered_column() {
        let mut a = app_edited(|s| s.project.views[1].filter = "-status:Done".into());
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board.clone(),
            filter: "-status:Done".into(),
            list: list(&["a", "b", "c", "e"]),
        });
        a.handle_key(code(KeyCode::Home));
        assert_eq!(column_titles(&a), ["Todo", "In Progress", "No Status"]);
        assert_eq!(a.handle_key(key('>')).len(), 1, "to In Progress");
        assert!(
            a.handle_key(key('>')).is_empty(),
            "Done is not shown and No Status is no destination"
        );
        // Setting Done as `e` would (Task 17 adds the key).
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        assert_eq!(
            column_titles(&a),
            ["Todo", "In Progress", "Done (filtered)", "No Status"]
        );
        let moved = a.snapshot().unwrap().items[&ItemId::new("a")].clone();
        assert!(a.unmatched(&moved));
        assert_eq!(selected(&a), "a");
        assert!(
            !a.unmatched(&a.snapshot().unwrap().items[&ItemId::new("c")].clone()),
            "untouched items are not marked"
        );
    }

    #[test]
    fn marks_name_sending_and_unmatched_items_and_a_key_press_clears_the_highlight() {
        let mut a = app_edited(|s| s.project.views[1].filter = "-status:Done".into());
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board.clone(),
            filter: "-status:Done".into(),
            list: list(&["a", "b", "c", "e"]),
        });
        assert_eq!(a.marks(), ItemMarks::default());
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        let marks = a.marks();
        let id = ItemId::new("a");
        assert!(marks.pending.contains(&id) && marks.unmatched.contains(&id));
        assert_eq!(marks.pending.len() + marks.unmatched.len(), 2);
        let screen =
            crate::ui::fixtures::render_to_string(120, 12, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(
            screen.lines().next().unwrap().contains("saving 1…"),
            "{screen}"
        );
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .contains("⊘ no longer matches filter · ? help"),
            "{screen}"
        );
        a.highlight = Some(id.clone());
        assert_eq!(a.marks().highlight, Some(id));
        a.handle_key(key('j'));
        assert_eq!(a.marks().highlight, None);
    }

    #[test]
    fn with_no_sticky_items_marks_name_only_pending_items() {
        let mut a = test_app();
        a.store.begin(set_status("a", "o_done", "Done"));
        assert!(a.sticky.is_empty());
        let marks = a.marks();
        assert_eq!(marks.pending, [ItemId::new("a")].into_iter().collect());
        assert!(marks.unmatched.is_empty());
        assert_eq!(marks.highlight, None);
    }

    #[test]
    fn an_item_the_next_list_leaves_out_stays_marked_until_the_view_is_left() {
        let mut a = test_app();
        let bugs = crate::model::ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            filter: "label:bug".into(),
            list: list(&["a"]),
        });
        a.handle_key(key('>'));
        assert!(
            !a.unmatched(&a.snapshot().unwrap().items[&ItemId::new("a")].clone()),
            "still listed"
        );
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            filter: "label:bug".into(),
            list: list(&[]),
        });
        let ids: Vec<String> = a
            .view_items()
            .unwrap()
            .iter()
            .map(|i| i.id.0.clone())
            .collect();
        assert_eq!(ids, ["a"], "sticky");
        assert!(a.unmatched(&a.snapshot().unwrap().items[&ItemId::new("a")].clone()));
        a.handle_key(code(KeyCode::Tab));
        a.handle_key(code(KeyCode::BackTab));
        assert!(
            a.view_items().unwrap().is_empty(),
            "leaving the view lets go"
        );
    }

    #[test]
    fn r_lets_sticky_items_go() {
        let mut a = test_app();
        let bugs = crate::model::ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            filter: "label:bug".into(),
            list: list(&["a"]),
        });
        a.handle_key(key('>'));
        a.on_sync(SyncEvent::ViewIds {
            view: bugs,
            filter: "label:bug".into(),
            list: list(&[]),
        });
        assert_eq!(a.handle_key(key('r')), vec![Command::Refresh]);
        assert!(a.view_items().unwrap().is_empty());
    }

    /// Controller ruling E23: `r` also forgets the candidate lists, so labels or milestones
    /// added on GitHub show after a refresh.
    #[test]
    fn r_forgets_candidates_and_the_next_popover_loads_them_again() {
        let mut a = test_app();
        let repo: RepoSlug = "tviles/t".parse().unwrap();
        let load = vec![Command::LoadCandidates(repo.clone())];
        assert_eq!(a.handle_key(key('t')), load);
        a.on_sync(SyncEvent::Candidates {
            repo: repo.clone(),
            candidates: crate::model::RepoCandidates {
                repo: RepoRef {
                    id: "R_1".into(),
                    slug: repo.clone(),
                },
                users: vec![],
                labels: vec![],
                milestones: vec![],
                truncated: false,
            },
        });
        a.handle_key(code(KeyCode::Esc));
        assert!(a.handle_key(key('t')).is_empty(), "cached");
        a.handle_key(code(KeyCode::Esc));
        a.need_repos();
        a.need_repo_lookup(&"acme/app".parse().unwrap());
        a.start_search("crash", vec![repo.clone()]);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.handle_key(key('r')), vec![Command::Refresh]);
        assert!(a.candidates.is_empty() && a.repos.is_none() && a.repo_lookups.is_empty());
        assert!(a.content_search.is_none());
        assert_eq!(a.handle_key(key('t')), load, "loaded again");
    }

    #[test]
    fn r_keeps_the_search_an_open_add_existing_popover_waits_on() {
        let mut a = test_app();
        a.pane_repo = Some("acme/app".parse().unwrap());
        a.handle_key(key('n'));
        for text in ["existing", "crash"] {
            for c in text.chars() {
                a.handle_key(key(c));
            }
            a.handle_key(code(KeyCode::Enter));
        }
        assert!(matches!(a.popup, Some(Popup::Search { .. })));
        assert!(matches!(&a.content_search, Some((q, Lookup::Loading)) if q == "crash"));
        a.clear_lookups();
        assert!(
            a.content_search.is_some(),
            "the open popover still waits on it"
        );
    }

    #[test]
    fn views_without_a_filter_hide_archived_items() {
        let a = app_edited(|s| s.items.get_mut(&ItemId::new("b")).unwrap().archived = true);
        let ids: Vec<String> = a
            .view_items()
            .unwrap()
            .iter()
            .map(|i| i.id.0.clone())
            .collect();
        assert_eq!(ids, ["a", "c", "d", "e"]);
    }

    fn board_app(filter: &str, ids: &[&str]) -> App {
        let f = filter.to_string();
        let mut a = app_edited(|s| s.project.views[1].filter = f);
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board,
            filter: filter.into(),
            list: list(ids),
        });
        a
    }

    #[test]
    fn a_column_kept_for_githubs_own_cards_is_not_marked_filtered() {
        // The list holds `e` (Blocked, an option the filter does not name): a misread filter.
        let a = board_app("status:Todo", &["a", "b", "e"]);
        assert!(
            column_titles(&a).iter().all(|t| !t.contains("(filtered)")),
            "{:?}",
            column_titles(&a)
        );
    }

    #[test]
    fn a_filter_that_allows_no_column_marks_nothing() {
        let mut a = board_app("status:Doen", &["a", "b", "c", "e"]);
        a.handle_key(key('>'));
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        assert!(
            column_titles(&a).iter().all(|t| !t.contains("(filtered)")),
            "{:?}",
            column_titles(&a)
        );
    }

    #[test]
    fn a_board_card_the_list_omits_is_added_back_into_its_filtered_column() {
        let mut a = board_app("-status:Done", &["a", "b", "c", "e"]);
        a.handle_key(code(KeyCode::Home));
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        a.on_sync(SyncEvent::ViewIds {
            view: crate::model::ViewId::new("V_board"),
            filter: "-status:Done".into(),
            list: list(&["b", "c", "e"]),
        });
        assert!(column_titles(&a).contains(&"Done (filtered)".to_string()));
        assert_eq!(selected(&a), "a");
    }

    #[test]
    fn a_changed_project_filter_clears_sticky_items() {
        let mut a = board_app("-status:Done", &["a", "b", "c", "e"]);
        a.handle_key(code(KeyCode::Home));
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        assert!(a.is_sticky(&ItemId::new("a")));
        let mut project = a.snapshot().unwrap().project.clone();
        a.on_sync(SyncEvent::Project(project.clone()));
        assert!(a.is_sticky(&ItemId::new("a")), "same filter keeps them");
        project.views[1].filter = "-status:Todo".into();
        a.on_sync(SyncEvent::Project(project));
        assert!(!a.is_sticky(&ItemId::new("a")));
    }

    #[test]
    fn a_changed_local_filter_and_a_board_switch_clear_sticky_items() {
        let mut a = board_app("-status:Done", &["a", "b", "c", "e"]);
        a.handle_key(code(KeyCode::Home));
        a.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        a.set_extra_filter("label:bug");
        assert!(!a.is_sticky(&ItemId::new("a")));
        let mut b = board_app("-status:Done", &["a", "b", "c", "e"]);
        b.handle_key(code(KeyCode::Home));
        b.submit(OpKind::SetField, set_status("a", "o_done", "Done"));
        b.reset_for_new_board();
        assert!(!b.is_sticky(&ItemId::new("a")));
    }

    #[test]
    fn an_item_made_sticky_before_its_list_arrived_is_not_marked_dropped() {
        let mut a = test_app();
        let bugs = crate::model::ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.stick(&ItemId::new("a"));
        assert!(a.sticky.is_empty(), "not shown, so not sticky");
        // No filter on the first view: it is shown at once, with no list behind it.
        let mut b = test_app();
        b.stick(&ItemId::new("a"));
        assert_eq!(b.sticky.len(), 1);
        assert!(!b.sticky[0].listed);
        let a_item = b.snapshot().unwrap().items[&ItemId::new("a")].clone();
        assert!(!b.unmatched(&a_item));
    }

    fn fail(a: &mut App, id: OpId, message: &str) {
        let error = BackendError::Api(vec![GraphqlErrorEntry {
            message: message.into(),
            kind: None,
        }]);
        a.on_sync(SyncEvent::OpDone {
            id,
            result: Err(error),
        });
    }

    #[test]
    fn a_new_board_forgets_the_old_error_log_highlight_and_kinds() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        fail(&mut a, 1, "boom a");
        fail(&mut a, 2, "boom b");
        a.error_sel = 1;
        a.highlight = Some(ItemId::new("a"));
        a.pending_kinds.insert(99, OpKind::Move);
        a.reset_for_new_board();
        assert!(a.errors.is_empty());
        assert_eq!(a.error_sel, 0);
        assert_eq!(a.highlight, None);
        assert!(a.pending_kinds.is_empty());
    }

    #[test]
    fn edits_still_pending_when_the_board_reloads_go_to_the_error_log() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::SetField, set_status("b", "o_done", "Done"));
        a.reset_for_new_board();
        assert_eq!(a.pending_count(), 0);
        let logged: Vec<_> = a
            .errors
            .iter()
            .map(|e| (e.kind, e.op.item().as_str(), e.message.as_str()))
            .collect();
        assert_eq!(
            logged,
            [
                (OpKind::Move, "a", NOT_SENT_RELOADED),
                (OpKind::SetField, "b", NOT_SENT_RELOADED)
            ]
        );
        assert!(
            a.status
                .error
                .as_deref()
                .is_some_and(|e| e.contains(NOT_SENT_RELOADED))
        );
    }

    #[test]
    fn q_on_the_setup_screen_asks_while_edits_are_sending() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: BackendError::Unauthorized {
                provider: Provider::Github,
            },
        });
        assert_eq!(a.mode, Mode::Setup);
        assert!(a.handle_key(key('q')).is_empty());
        assert_eq!(a.mode, Mode::Confirm);
        assert!(!a.quit);
        assert!(a.handle_key(key('n')).is_empty());
        assert_eq!(a.mode, Mode::Setup, "back on the setup screen");
        a.handle_key(key('q'));
        assert_eq!(a.handle_key(key('y')), vec![Command::Quit]);
        assert!(a.quit);
    }

    #[test]
    fn failed_edits_are_listed_newest_first_and_r_retries_the_selected_one() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        fail(&mut a, 1, "boom a");
        fail(&mut a, 2, "boom b");
        assert!(
            a.status
                .error
                .as_deref()
                .unwrap()
                .ends_with("boom b · E error log")
        );
        a.handle_key(key('E'));
        assert_eq!(a.mode, Mode::ErrorLog);
        assert_eq!(a.error_sel, 0);
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(3)]);
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.errors[0].message, "boom a", "b was retried");
        assert_eq!(status(a.snapshot(), "b"), "Done");
        assert_eq!(a.mode, Mode::ErrorLog);
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn a_retry_the_board_no_longer_allows_keeps_the_entry_and_says_why() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        fail(&mut a, 1, "boom");
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        a.handle_key(key('E'));
        assert!(a.handle_key(key('R')).is_empty());
        assert_eq!(a.errors.len(), 1);
        assert!(a.status.flash.as_deref().unwrap().contains("project scope"));
    }

    #[test]
    fn a_retry_for_an_item_that_is_gone_keeps_the_entry_and_says_so() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("gone", "o_done", "Done"));
        a.handle_key(key('E'));
        assert!(a.errors.is_empty());
        a.errors.push(FailedEdit {
            kind: OpKind::Move,
            op: set_status("gone", "o_done", "Done"),
            what: "set Status to Done on 'x'".into(),
            message: "boom".into(),
            at: Instant::now(),
            may_exist: false,
        });
        assert!(a.handle_key(key('R')).is_empty());
        assert_eq!(a.errors.len(), 1);
        assert_eq!(
            a.status.flash.as_deref(),
            Some("That item is no longer on the board")
        );
    }

    #[test]
    fn the_log_navigates_and_esc_closes_it_without_quitting() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        fail(&mut a, 1, "boom a");
        fail(&mut a, 2, "boom b");
        a.handle_key(key('E'));
        a.handle_key(key('j'));
        assert_eq!(a.error_sel, 1);
        a.handle_key(key('j'));
        assert_eq!(a.error_sel, 1, "stops at the oldest");
        a.handle_key(key('k'));
        assert_eq!(a.error_sel, 0);
        // R retries the older one when it is selected.
        a.handle_key(key('j'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(3)]);
        assert_eq!(a.errors[0].message, "boom b");
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert!(!a.quit);
        assert_eq!(a.mode, Mode::Normal);
        // E toggles it too, and q quits from inside it.
        a.handle_key(key('E'));
        a.handle_key(key('E'));
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key('E'));
        // The retried edit is still sending, so q asks before quitting.
        a.handle_key(key('q'));
        assert_eq!(a.mode, Mode::Confirm);
        assert!(!a.quit);
    }

    #[test]
    fn closing_the_log_returns_to_the_open_detail() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(key('E'));
        assert_eq!(a.mode, Mode::ErrorLog);
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Detail);
        assert!(a.detail.is_some());
    }

    #[test]
    fn ages_read_naturally() {
        use std::time::Duration;
        assert_eq!(ago(Duration::from_secs(5)), "5s ago");
        assert_eq!(ago(Duration::from_secs(125)), "2m ago");
        assert_eq!(ago(Duration::from_secs(7300)), "2h ago");
    }

    #[test]
    fn a_failure_arriving_while_the_log_is_open_keeps_the_highlight_on_its_edit() {
        let mut a = test_app();
        for item in ["a", "b", "c"] {
            a.submit(OpKind::Move, set_status(item, "o_done", "Done"));
        }
        fail(&mut a, 1, "boom a");
        fail(&mut a, 2, "boom b");
        a.handle_key(key('E'));
        a.handle_key(key('j'));
        assert_eq!(a.errors[a.errors.len() - 1 - a.error_sel].message, "boom a");
        fail(&mut a, 3, "boom c");
        assert_eq!(a.errors[a.errors.len() - 1 - a.error_sel].message, "boom a");
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(4)]);
        assert_eq!(status(a.snapshot(), "a"), "Done", "a was retried");
        // The log's hint line is not overwritten; the error waits for the log to close.
        assert!(a.status.error.is_none() || a.mode == Mode::ErrorLog);
    }

    #[test]
    fn the_selection_clamps_after_retrying_the_last_entry() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        fail(&mut a, 1, "boom a");
        fail(&mut a, 2, "boom b");
        a.handle_key(key('E'));
        a.handle_key(key('j'));
        assert_eq!(a.error_sel, 1);
        a.handle_key(key('R'));
        assert_eq!(a.errors.len(), 1);
        assert_eq!(a.error_sel, 0);
        a.handle_key(key('R'));
        assert!(a.errors.is_empty());
        assert_eq!(a.error_sel, 0);
        assert!(a.handle_key(key('R')).is_empty(), "nothing to retry");
    }

    #[test]
    fn d_dismisses_the_selected_entry_and_the_log_keeps_the_newest_fifty() {
        let mut a = test_app();
        for n in 0..52 {
            a.errors.push(FailedEdit {
                kind: OpKind::Move,
                op: set_status("a", "o_done", "Done"),
                what: "x".into(),
                message: format!("boom {n}"),
                at: Instant::now(),
                may_exist: false,
            });
        }
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        fail(&mut a, 1, "boom last");
        assert_eq!(a.errors.len(), 50, "capped");
        assert_eq!(a.errors[0].message, "boom 3", "the oldest dropped");
        a.handle_key(key('E'));
        a.handle_key(key('d'));
        assert_eq!(a.errors.len(), 49);
        assert_eq!(a.errors.last().unwrap().message, "boom 51");
        assert_eq!(a.mode, Mode::ErrorLog);
        for _ in 0..60 {
            a.handle_key(key('d'));
        }
        assert!(a.errors.is_empty());
        assert_eq!(a.error_sel, 0);
    }

    #[test]
    fn messages_lose_newlines_and_control_characters() {
        let error = BackendError::Api(vec![GraphqlErrorEntry {
            message: "bad\nthing\u{7}here".into(),
            kind: None,
        }]);
        assert_eq!(error_message(&error), "bad thing here");
    }

    #[test]
    fn ages_switch_to_days_after_forty_eight_hours() {
        use std::time::Duration;
        assert_eq!(ago(Duration::from_secs(47 * 3600)), "47h ago");
        assert_eq!(ago(Duration::from_secs(3 * 86400)), "3d ago");
    }

    use crate::model::{ContentHit, RepoRef};
    use crate::ops::NewItem;

    fn new_draft(temp: &str) -> Op {
        Op::Create {
            temp: ItemId::new(temp),
            new: NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![(
                FieldId::new("F_status"),
                FieldValue::SingleSelect {
                    option_id: OptionId::new("o_done"),
                    name: "Done".into(),
                },
            )],
        }
    }

    fn new_issue(temp: &str) -> Op {
        Op::Create {
            temp: ItemId::new(temp),
            new: NewItem::Issue {
                repo: RepoRef {
                    id: "R_1".into(),
                    slug: "tviles/t".parse().unwrap(),
                },
                title: "New bug".into(),
                body: "Steps".into(),
            },
            presets: vec![],
        }
    }

    fn created(a: &App, temp: &str, real: &str) -> crate::model::Item {
        let mut item = a.snapshot().unwrap().items[&ItemId::new(temp)].clone();
        item.id = ItemId::new(real);
        item
    }

    #[test]
    fn a_new_item_shows_selected_and_highlighted_then_follows_its_real_id() {
        let mut a = test_app();
        assert_eq!(
            a.submit_new(new_draft("local:1"), false),
            vec![Command::Send(1)]
        );
        let temp = ItemId::new("local:1");
        assert_eq!(a.selected_item().unwrap().id, temp);
        assert_eq!(a.highlight, Some(temp.clone()));
        assert_eq!(status(a.snapshot(), "local:1"), "Done");
        assert!(a.marks().pending.contains(&temp));
        let item = created(&a, "local:1", "PVTI_new");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Created {
                temp: temp.clone(),
                item,
                refused: vec![],
            }),
        });
        let real = ItemId::new("PVTI_new");
        assert_eq!(a.selected_item().unwrap().id, real, "the cursor follows");
        assert_eq!(a.highlight, Some(real.clone()));
        assert!(a.is_sticky(&real) && !a.is_sticky(&temp));
        assert_eq!(a.pending_count(), 0);
        assert!(a.status.error.is_none() && a.errors.is_empty());
    }

    #[test]
    fn a_refused_preset_is_reported_after_the_item_is_created() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        let item = created(&a, "local:1", "PVTI_new");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Created {
                temp: ItemId::new("local:1"),
                item,
                refused: vec![(
                    FieldId::new("F_status"),
                    BackendError::Network("reset".into()),
                )],
            }),
        });
        assert_eq!(
            a.status.error.as_deref(),
            Some("Added 'Idea', but a field was not set: Status (network error: reset)")
        );
        assert!(
            a.store
                .confirmed()
                .unwrap()
                .items
                .contains_key(&ItemId::new("PVTI_new"))
        );
    }

    #[test]
    fn a_new_issue_needs_the_repo_scope_and_a_draft_only_the_board() {
        let mut a = test_app();
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["project".into()]),
        });
        assert!(a.submit_new(new_issue("local:1"), true).is_empty());
        assert!(a.status.flash.as_deref().unwrap().contains("repo"));
        assert_eq!(a.pending_count(), 0);
        assert!(
            !a.snapshot()
                .unwrap()
                .items
                .contains_key(&ItemId::new("local:1"))
        );
        assert_eq!(
            a.submit_new(new_draft("local:2"), false),
            vec![Command::Send(1)]
        );
    }

    /// Controller ruling E15: the issue exists, so the log says so and `R` only adds it.
    #[test]
    fn an_issue_created_but_not_added_is_logged_and_r_only_adds_it() {
        let mut a = test_app();
        a.submit_new(new_issue("local:1"), true);
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::NotAdded {
                issue: "I_new".into(),
                error: BackendError::Network("reset".into()),
            }),
        });
        assert_eq!(a.pending_count(), 0);
        assert!(
            !a.snapshot()
                .unwrap()
                .items
                .contains_key(&ItemId::new("local:1"))
        );
        assert_eq!(a.errors.len(), 1);
        let entry = &a.errors[0];
        assert_eq!(
            entry.what,
            "add the new issue 'New bug' (tviles/t) to the project"
        );
        assert_eq!(
            entry.message,
            "the issue was created but not added to the project: network error: reset"
        );
        assert!(matches!(
            &entry.op,
            Op::Create { new: NewItem::Existing { hit: ContentHit { id, .. } }, .. } if id == "I_new"
        ));
        assert!(
            a.status
                .error
                .as_deref()
                .unwrap()
                .contains("was created but not added")
        );
        // Even without the repo scope a new issue would need: adding needs only the board.
        a.on_sync(SyncEvent::Access {
            scopes: Some(vec!["project".into()]),
        });
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        assert!(a.errors.is_empty());
        assert!(matches!(
            &a.store.pending()[0].op,
            Op::Create { new: NewItem::Existing { hit }, .. } if hit.id == "I_new"
        ));
        assert_eq!(
            a.snapshot().unwrap().items[&ItemId::new("local:1")].title(),
            "New bug",
            "the placeholder is back"
        );
    }

    #[test]
    fn a_failed_new_item_is_retried_as_a_new_item() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        fail(&mut a, 1, "boom");
        assert_eq!(a.errors[0].what, "add the draft 'Idea'");
        assert!(
            !a.snapshot()
                .unwrap()
                .items
                .contains_key(&ItemId::new("local:1"))
        );
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        assert!(
            a.snapshot()
                .unwrap()
                .items
                .contains_key(&ItemId::new("local:1"))
        );
        assert_eq!(a.highlight, Some(ItemId::new("local:1")));
    }

    /// A new item may already exist on GitHub when the board reloads mid-send; its log entry
    /// must not invite a blind retry.
    #[test]
    fn a_new_item_pending_when_the_board_reloads_says_to_check_first() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("a", "o_done", "Done"));
        a.submit_new(new_draft("local:1"), false);
        a.reset_for_new_board();
        let messages: Vec<&str> = a.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, [NOT_SENT_RELOADED, MAYBE_SENT_RELOADED]);
    }

    #[test]
    fn item_ops_settle_into_the_confirmed_board() {
        let mut a = test_app();
        a.submit(
            OpKind::Archive,
            Op::Archive {
                item: ItemId::new("a"),
            },
        );
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert!(a.store.confirmed().unwrap().items[&ItemId::new("a")].archived);
        a.submit(
            OpKind::Remove,
            Op::Remove {
                item: ItemId::new("b"),
            },
        );
        assert!(!a.snapshot().unwrap().items.contains_key(&ItemId::new("b")));
        a.on_sync(SyncEvent::OpDone {
            id: 2,
            result: Ok(Confirmed::Applied),
        });
        assert!(
            !a.store
                .confirmed()
                .unwrap()
                .items
                .contains_key(&ItemId::new("b"))
        );
    }

    #[test]
    fn every_refused_preset_is_named_and_a_refused_scope_is_recorded() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        let item = created(&a, "local:1", "PVTI_new");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Created {
                temp: ItemId::new("local:1"),
                item,
                refused: vec![
                    (FieldId::new("F_status"), refused("project")),
                    (
                        FieldId::new("F_notes"),
                        BackendError::Network("reset".into()),
                    ),
                ],
            }),
        });
        let error = a.status.error.clone().unwrap();
        assert!(
            error.starts_with("Added 'Idea', but 2 fields were not set: Status ("),
            "{error}"
        );
        assert!(error.ends_with("; Notes (network error: reset)"), "{error}");
        assert_eq!(a.refused_scopes, vec!["project".to_string()]);
        assert_eq!(a.selected_item().unwrap().id.as_str(), "PVTI_new");
    }

    /// A lost answer (network, decode) may hide a draft or issue GitHub made: the log says
    /// so, and `R` refuses rather than risk a second one.
    #[test]
    fn a_new_item_whose_answer_was_lost_may_exist_and_r_refuses() {
        for (op, needs_repo) in [(new_issue("local:1"), true), (new_draft("local:1"), false)] {
            for error in [
                BackendError::Network("reset".into()),
                BackendError::Decode("createIssue returned no issue id".into()),
            ] {
                let mut a = test_app();
                a.submit_new(op.clone(), needs_repo);
                let shown = error_message(&error);
                a.on_sync(SyncEvent::OpDone {
                    id: 1,
                    result: Err(error),
                });
                assert_eq!(a.errors.len(), 1);
                assert_eq!(a.errors[0].message, format!("{MAYBE_CREATED} ({shown})"));
                assert!(a.errors[0].may_exist);
                a.handle_key(key('E'));
                assert!(a.handle_key(key('R')).is_empty(), "never sent again");
                assert_eq!(a.status.flash.as_deref(), Some(MAYBE_CREATED));
                assert_eq!(a.errors.len(), 1, "the entry stays");
                assert_eq!(a.pending_count(), 0);
            }
        }
    }

    /// Controller ruling E19: a comment whose answer was lost may have been posted; `R`
    /// would post it twice.
    #[test]
    fn a_comment_whose_answer_was_lost_may_be_posted_and_r_refuses() {
        for error in [
            BackendError::Network("reset".into()),
            BackendError::Decode("addComment returned no comment".into()),
        ] {
            let mut a = test_app();
            assert_eq!(
                a.on_edited(
                    EditPurpose::Comment(ItemId::new("a")),
                    Some("LGTM\n".into())
                ),
                vec![Command::Send(1)]
            );
            let shown = error_message(&error);
            a.on_sync(SyncEvent::OpDone {
                id: 1,
                result: Err(error),
            });
            assert_eq!(a.errors.len(), 1);
            assert_eq!(a.errors[0].message, format!("{MAYBE_POSTED} ({shown})"));
            assert!(a.errors[0].may_exist);
            a.handle_key(key('E'));
            assert!(a.handle_key(key('R')).is_empty(), "never posted again");
            assert_eq!(a.status.flash.as_deref(), Some(MAYBE_POSTED));
            assert_eq!(a.errors.len(), 1, "the entry stays");
            assert_eq!(a.pending_count(), 0);
        }
        // A comment GitHub refused was not posted: `R` sends it again.
        let mut b = test_app();
        b.on_edited(
            EditPurpose::Comment(ItemId::new("a")),
            Some("LGTM\n".into()),
        );
        fail(&mut b, 1, "Issue is locked");
        assert!(!b.errors[0].may_exist);
        b.handle_key(key('E'));
        assert_eq!(b.handle_key(key('R')), vec![Command::Send(2)]);
    }

    #[test]
    fn a_comment_pending_when_the_board_reloads_may_be_posted_and_is_not_retried() {
        let mut a = test_app();
        a.submit(OpKind::Move, set_status("b", "o_done", "Done"));
        a.on_edited(
            EditPurpose::Comment(ItemId::new("a")),
            Some("LGTM\n".into()),
        );
        a.reset_for_new_board();
        let logged: Vec<(&str, bool)> = a
            .errors
            .iter()
            .map(|e| (e.message.as_str(), e.may_exist))
            .collect();
        assert_eq!(
            logged,
            [(NOT_SENT_RELOADED, false), (MAYBE_SENT_RELOADED, true)]
        );
        a.handle_key(key('E'));
        assert!(a.handle_key(key('R')).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some(MAYBE_POSTED));
        assert_eq!(a.errors.len(), 2);
    }

    #[test]
    fn a_refused_new_item_or_a_lost_add_of_an_existing_one_can_be_retried() {
        let mut a = test_app();
        a.submit_new(new_issue("local:1"), true);
        fail(&mut a, 1, "Repository is archived");
        assert!(!a.errors[0].may_exist);
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        let hit = ContentHit {
            id: "I_9".into(),
            pull: false,
            repo: "acme/app".into(),
            number: 9,
            title: "Fix".into(),
            open: true,
        };
        let mut b = test_app();
        b.submit_new(
            Op::Create {
                temp: ItemId::new("local:1"),
                new: NewItem::Existing { hit },
                presets: vec![],
            },
            false,
        );
        b.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(BackendError::Network("reset".into())),
        });
        assert!(
            !b.errors[0].may_exist,
            "adding it twice returns the same item"
        );
        b.handle_key(key('E'));
        assert_eq!(b.handle_key(key('R')), vec![Command::Send(2)]);
    }

    #[test]
    fn a_new_item_pending_when_the_board_reloads_is_not_retried() {
        let mut a = test_app();
        a.submit_new(new_issue("local:1"), true);
        a.reset_for_new_board();
        assert!(a.errors[0].may_exist);
        a.handle_key(key('E'));
        assert!(a.handle_key(key('R')).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some(MAYBE_CREATED));
    }

    /// The kind a retry is checked as is the op's own, never a stand-in.
    #[test]
    fn new_and_item_ops_record_their_own_kind_through_a_retry() {
        let mut a = test_app();
        a.submit_new(new_issue("local:1"), true);
        assert_eq!(a.pending_kinds.get(&1), Some(&OpKind::Create));
        fail(&mut a, 1, "Repository is archived");
        assert_eq!(a.errors[0].kind, OpKind::Create);
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        assert_eq!(a.pending_kinds.get(&2), Some(&OpKind::Create));
        a.handle_key(code(KeyCode::Esc));
        for (n, (kind, op)) in [
            (
                OpKind::Archive,
                Op::Archive {
                    item: ItemId::new("a"),
                },
            ),
            (
                OpKind::Remove,
                Op::Remove {
                    item: ItemId::new("b"),
                },
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let id = 3 + 2 * n as u64;
            assert_eq!(a.submit(kind, op), vec![Command::Send(id)]);
            fail(&mut a, id, "boom");
            assert_eq!(a.errors.last().unwrap().kind, kind);
            a.handle_key(key('E'));
            assert_eq!(a.handle_key(key('R')), vec![Command::Send(id + 1)]);
            assert_eq!(a.pending_kinds.get(&(id + 1)), Some(&kind));
            a.handle_key(code(KeyCode::Esc));
        }
    }

    #[test]
    fn a_placeholders_detail_waits_then_loads_the_real_item() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        assert!(
            a.handle_key(code(KeyCode::Enter)).is_empty(),
            "nothing to load from GitHub yet"
        );
        assert_eq!(a.mode, Mode::Detail);
        assert_eq!(a.detail.as_ref().unwrap().item.as_str(), "local:1");
        let item = created(&a, "local:1", "PVTI_new");
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Created {
                temp: ItemId::new("local:1"),
                item,
                refused: vec![],
            }),
        });
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("PVTI_new"),
                before: None
            }]
        );
        assert_eq!(a.detail.as_ref().unwrap().item.as_str(), "PVTI_new");
        assert_eq!(a.detail_item().unwrap().id.as_str(), "PVTI_new");
        // Created while the detail shows another item: nothing to load.
        let mut b = test_app();
        b.submit_new(new_draft("local:1"), false);
        b.open_detail(ItemId::new("a"));
        let item = created(&b, "local:1", "PVTI_new");
        let cmds = b.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Created {
                temp: ItemId::new("local:1"),
                item,
                refused: vec![],
            }),
        });
        assert!(cmds.is_empty());
        assert_eq!(b.detail.as_ref().unwrap().item.as_str(), "a");
    }

    use crate::ui::app::EditPurpose;

    fn open_detail_with_body(a: &mut App, body: &str) {
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("a"),
            detail: crate::model::ItemDetail {
                body: body.into(),
                ..Default::default()
            },
            older: false,
        });
    }

    #[test]
    fn a_title_edit_opens_the_form_with_the_title_and_sends_the_new_one() {
        let mut a = test_app();
        assert!(a.edit_title().is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Title(ItemId::new("a")));
        assert_eq!(form.title_text(), "Fix crash");
        // The editor's text, arriving with no form open, is sent as it always was.
        a.handle_key(code(KeyCode::Esc));
        let cmds = a.on_edited(
            EditPurpose::Title(ItemId::new("a")),
            Some("Fix the crash on start\n".into()),
        );
        assert_eq!(cmds, vec![Command::Send(1)]);
        assert_eq!(
            a.item(&ItemId::new("a")).unwrap().title(),
            "Fix the crash on start"
        );
    }

    /// Review Focus 5.
    #[test]
    fn an_aborted_empty_or_unchanged_edit_sends_nothing() {
        let mut a = test_app();
        let title = || EditPurpose::Title(ItemId::new("a"));
        assert!(a.on_edited(title(), None).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("The editor exited with an error; nothing was sent")
        );
        assert!(a.on_edited(title(), Some("\n  \n".into())).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Empty title; nothing was sent")
        );
        assert!(a.on_edited(title(), Some("Fix crash\n".into())).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some("Title unchanged"));
        assert!(
            a.on_edited(EditPurpose::Comment(ItemId::new("a")), Some(" \n".into()))
                .is_empty()
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Empty comment; nothing was sent")
        );
        open_detail_with_body(&mut a, "Steps\n");
        assert!(
            a.on_edited(
                EditPurpose::Body(ItemId::new("a")),
                Some("Steps\n\n".into())
            )
            .is_empty()
        );
        assert_eq!(a.status.flash.as_deref(), Some("Body unchanged"));
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn a_body_edit_needs_the_body_loaded_first() {
        let mut a = test_app();
        let cmds = a.edit_body();
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
        assert_eq!(a.mode, Mode::Detail);
        assert!(a.status.flash.as_deref().unwrap().contains("again"));
        a.on_sync(SyncEvent::Detail {
            item: ItemId::new("a"),
            detail: crate::model::ItemDetail {
                body: "Steps".into(),
                ..Default::default()
            },
            older: false,
        });
        assert!(a.edit_body().is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Body(ItemId::new("a")));
        assert_eq!(form.body_text(), "Steps");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(
            a.on_edited(
                EditPurpose::Body(ItemId::new("a")),
                Some("Steps\n\nMore".into())
            ),
            vec![Command::Send(1)]
        );
    }

    #[test]
    fn a_comment_is_sent_and_the_open_detail_reloads_once_it_lands() {
        let mut a = test_app();
        open_detail_with_body(&mut a, "Steps");
        assert!(a.handle_key(key('c')).is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Comment(ItemId::new("a")));
        assert_eq!(form.body_text(), "");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(
            a.on_edited(
                EditPurpose::Comment(ItemId::new("a")),
                Some("LGTM\n".into())
            ),
            vec![Command::Send(1)]
        );
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
    }

    #[test]
    fn edit_body_loads_again_after_a_failed_load_and_waits_while_loading() {
        let mut a = test_app();
        a.edit_body();
        assert!(
            a.edit_body().is_empty(),
            "still loading: nothing more to ask"
        );
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Detail(ItemId::new("a")),
            error: BackendError::Network("offline".into()),
        });
        assert_eq!(
            a.edit_body(),
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
    }

    #[test]
    fn an_edit_for_an_item_that_left_the_board_sends_nothing() {
        let mut a = test_app();
        let comment = EditPurpose::Comment(ItemId::new("a"));
        // The comment key opens the form for that item; closing it leaves a stray editor
        // result with no form to go back into.
        a.comment();
        assert!(
            matches!(&a.popup, Some(Popup::Text { purpose, .. }) if *purpose == comment),
            "the comment key opens the form for item a"
        );
        a.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.popup.is_none());
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.remove(&ItemId::new("a"));
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.on_edited(comment, Some("LGTM".into())).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("That item is no longer on the board")
        );
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn a_title_edit_takes_the_first_line_and_a_body_edit_reloads_the_detail() {
        let mut a = test_app();
        let title = EditPurpose::Title(ItemId::new("a"));
        assert_eq!(
            a.on_edited(title, Some("\n  New title  \nignored\n".into())),
            vec![Command::Send(1)]
        );
        assert_eq!(a.item(&ItemId::new("a")).unwrap().title(), "New title");
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert!(cmds.is_empty(), "a title needs no detail reload");
        open_detail_with_body(&mut a, "Steps");
        let body = EditPurpose::Body(ItemId::new("a"));
        assert_eq!(
            a.on_edited(body, Some("".into())),
            vec![Command::Send(2)],
            "a body can be emptied"
        );
        let cmds = a.on_sync(SyncEvent::OpDone {
            id: 2,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(
            cmds,
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }]
        );
    }

    #[test]
    fn an_editor_that_could_not_start_says_why() {
        let mut a = test_app();
        let error = Edited::Error("Could not start vi: No such file or directory".into());
        assert!(
            a.on_edited(EditPurpose::Title(ItemId::new("a")), error)
                .is_empty()
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Could not start vi: No such file or directory")
        );
    }

    #[test]
    fn nothing_changed_at_once_suggests_a_waiting_flag() {
        let mut a = test_app();
        let at_once = |text: &str| Edited::Saved {
            text: text.into(),
            quick: true,
        };
        assert!(
            a.on_edited(EditPurpose::Title(ItemId::new("a")), at_once("Fix crash"))
                .is_empty()
        );
        assert_eq!(a.status.flash.as_deref(), Some(RETURNED_AT_ONCE));
        assert!(
            a.on_edited(EditPurpose::Comment(ItemId::new("a")), at_once(""))
                .is_empty()
        );
        assert_eq!(a.status.flash.as_deref(), Some(RETURNED_AT_ONCE));
        assert_eq!(
            a.on_edited(EditPurpose::Title(ItemId::new("a")), at_once("Quick fix")),
            vec![Command::Send(1)],
            "a quick change is still sent"
        );
    }

    #[test]
    fn a_body_whose_line_endings_changed_is_unchanged() {
        let mut a = test_app();
        open_detail_with_body(&mut a, "Steps\r\n1. run\r\n");
        let body = || EditPurpose::Body(ItemId::new("a"));
        assert!(
            a.on_edited(body(), Some("Steps\n1. run\n".into()))
                .is_empty()
        );
        assert_eq!(a.status.flash.as_deref(), Some("Body unchanged"));
        assert_eq!(
            a.on_edited(body(), Some("Steps\n1. run\n2. crash\n".into())),
            vec![Command::Send(1)]
        );
    }

    #[test]
    fn a_comment_keeps_its_indentation() {
        let mut a = test_app();
        let text = "\n\n    let x = 1;\n  - item\n\n";
        assert_eq!(
            a.on_edited(EditPurpose::Comment(ItemId::new("a")), Some(text.into())),
            vec![Command::Send(1)]
        );
        match &a.store.pending()[0].op {
            Op::Comment { body, .. } => assert_eq!(body, "    let x = 1;\n  - item"),
            other => panic!("expected a comment, got {other:?}"),
        }
    }

    /// The open text form, and what it is for.
    fn text_form(a: &App) -> (&EditPurpose, &crate::ui::textform::TextForm) {
        match &a.popup {
            Some(crate::ui::app::Popup::Text { purpose, form }) => (purpose, form),
            other => panic!("expected the text form, got {other:?}"),
        }
    }

    fn type_in(a: &mut App, text: &str) -> Vec<Command> {
        text.chars().flat_map(|c| a.handle_key(key(c))).collect()
    }

    fn alt_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
    }

    #[test]
    fn c_opens_a_comment_form_and_alt_enter_sends_it() {
        use crate::ui::textform::FormKind;
        let mut a = test_app();
        assert!(a.handle_key(key('c')).is_empty());
        assert_eq!(a.mode, Mode::Popup);
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Comment(ItemId::new("a")));
        assert_eq!(form.kind(), FormKind::Comment);
        assert_eq!(form.heading(), "Comment on #1");
        // Printable keys are text, even board keys such as q, j and e.
        assert!(type_in(&mut a, "quick je").is_empty());
        assert!(!a.quit);
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert!(a.popup.is_none());
        assert_eq!(a.mode, Mode::Normal);
        match &a.store.pending()[0].op {
            Op::Comment { body, .. } => assert_eq!(body, "quick je"),
            other => panic!("expected a comment, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_comment_keeps_the_form_open_with_the_reason() {
        let mut a = test_app();
        a.handle_key(key('c'));
        assert!(a.handle_key(alt_enter()).is_empty());
        let (_, form) = text_form(&a);
        assert_eq!(form.message(), Some("Empty comment; nothing was sent"));
        assert_eq!(a.mode, Mode::Popup);
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn the_title_form_is_prefilled_and_enter_saves_it() {
        use crate::ui::textform::FormKind;
        let mut a = test_app();
        assert!(a.edit_title().is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Title(ItemId::new("a")));
        assert_eq!(form.kind(), FormKind::Title);
        assert_eq!(form.heading(), "Edit title of #1");
        assert_eq!(form.title_text(), "Fix crash");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(text_form(&a).1.message(), Some("Title unchanged"));
        type_in(&mut a, " now");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert!(a.popup.is_none());
        assert_eq!(a.item(&ItemId::new("a")).unwrap().title(), "Fix crash now");
    }

    #[test]
    fn the_body_form_is_prefilled_from_the_loaded_body() {
        use crate::ui::textform::FormKind;
        let mut a = test_app();
        open_detail_with_body(&mut a, "Steps\r\n1. run");
        assert!(a.edit_body().is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Body(ItemId::new("a")));
        assert_eq!(form.kind(), FormKind::Body);
        assert_eq!(form.heading(), "Edit body of #1");
        assert_eq!(form.body_text(), "Steps\n1. run");
        assert!(a.handle_key(alt_enter()).is_empty());
        assert_eq!(text_form(&a).1.message(), Some("Body unchanged"));
        a.handle_key(code(KeyCode::Enter));
        type_in(&mut a, "2. crash");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert_eq!(a.mode, Mode::Detail, "back to the detail");
        match &a.store.pending()[0].op {
            Op::Body { body, .. } => assert_eq!(body, "Steps\n1. run\n2. crash"),
            other => panic!("expected a body, got {other:?}"),
        }
    }

    #[test]
    fn open_in_editor_round_trips_through_the_form() {
        let mut a = test_app();
        let purpose = || EditPurpose::Comment(ItemId::new("a"));
        a.handle_key(key('c'));
        type_in(&mut a, "hi");
        a.handle_key(code(KeyCode::Tab));
        a.handle_key(code(KeyCode::Tab));
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Edit {
                purpose: purpose(),
                initial: "hi".into()
            }]
        );
        assert_eq!(a.mode, Mode::Popup, "the form stays open underneath");
        // The editor's text goes back into the form, never straight to GitHub.
        assert!(
            a.on_edited(purpose(), Some("hello\nworld\n".into()))
                .is_empty()
        );
        assert_eq!(text_form(&a).1.body_text(), "hello\nworld");
        assert_eq!(a.pending_count(), 0);
        // A failed editor keeps the form's text and says so in the form.
        assert!(a.on_edited(purpose(), None).is_empty());
        let (_, form) = text_form(&a);
        assert_eq!(form.body_text(), "hello\nworld");
        assert_eq!(
            form.message(),
            Some("The editor exited with an error; nothing was sent")
        );
        let error = Edited::Error("Could not start vi: No such file or directory".into());
        assert!(a.on_edited(purpose(), error).is_empty());
        assert_eq!(
            text_form(&a).1.message(),
            Some("Could not start vi: No such file or directory")
        );
        // An editor that came back at once with nothing changed hints at a waiting flag.
        let at_once = Edited::Saved {
            text: "hello\nworld".into(),
            quick: true,
        };
        assert!(a.on_edited(purpose(), at_once).is_empty());
        assert_eq!(text_form(&a).1.message(), Some(RETURNED_AT_ONCE));
        assert_eq!(a.pending_count(), 0);
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        match &a.store.pending()[0].op {
            Op::Comment { body, .. } => assert_eq!(body, "hello\nworld"),
            other => panic!("expected a comment, got {other:?}"),
        }
    }

    #[test]
    fn text_that_isnt_utf8_goes_into_the_form_and_is_not_sent() {
        let mut a = test_app();
        let comment = || EditPurpose::Comment(ItemId::new("a"));
        a.handle_key(key('c'));
        let lossy = Edited::NotUtf8 {
            lossy: "caf\u{FFFD}".into(),
        };
        assert!(a.on_edited(comment(), lossy).is_empty());
        let (_, form) = text_form(&a);
        assert_eq!(form.body_text(), "caf\u{FFFD}");
        assert_eq!(form.message(), Some(NOT_UTF8));
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn esc_with_typed_text_asks_and_y_discards() {
        let mut a = test_app();
        a.handle_key(key('c'));
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert!(a.popup.is_none(), "nothing typed: Esc closes at once");
        a.handle_key(key('c'));
        type_in(&mut a, "draft");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Popup, "asks first");
        a.handle_key(key('n'));
        assert_eq!(
            text_form(&a).1.body_text(),
            "draft",
            "n returns to the form"
        );
        a.handle_key(code(KeyCode::Esc));
        assert!(a.handle_key(key('y')).is_empty());
        assert!(a.popup.is_none());
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn ctrl_c_in_the_form_still_quits() {
        let mut a = test_app();
        a.handle_key(key('c'));
        type_in(&mut a, "draft");
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(a.handle_key(ctrl_c), vec![Command::Quit]);
        assert!(a.quit);
    }

    #[test]
    fn a_form_whose_item_left_the_board_says_so_and_stays_open() {
        let mut a = test_app();
        a.handle_key(key('c'));
        type_in(&mut a, "LGTM");
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.remove(&ItemId::new("a"));
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(alt_enter()).is_empty());
        let (_, form) = text_form(&a);
        assert_eq!(form.message(), Some("That item is no longer on the board"));
        assert_eq!(form.body_text(), "LGTM");
    }

    fn with_pull_request(a: &mut App, state: crate::model::ContentState) {
        let mut snap = a.snapshot().unwrap().clone();
        let b = snap.items.get_mut(&ItemId::new("b")).unwrap();
        b.content = crate::model::ItemContent::PullRequest {
            reference: crate::model::ContentRef {
                repo: "tviles/t".into(),
                number: 2,
                url: "u".into(),
            },
            title: "Add iteration columns".into(),
            state,
            is_draft: false,
        };
        a.store.apply(StoreUpdate::Replace(snap));
    }

    #[test]
    fn closing_an_issue_asks_for_a_reason() {
        let mut a = test_app();
        assert!(a.handle_key(key('X')).is_empty());
        let Some(crate::ui::app::Popup::CloseReason { picker, .. }) = &a.popup else {
            panic!("reason picker")
        };
        assert_eq!(picker.title, "Close #1 'Fix crash'?");
        let labels: Vec<&str> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Close as completed", "Close as not planned"]);
        a.handle_key(code(KeyCode::Down));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert!(matches!(
            &a.store.pending()[0].op,
            Op::Close {
                reason: Some(crate::model::CloseReason::NotPlanned),
                ..
            }
        ));
        a.handle_key(key('j'));
        a.handle_key(key('X'));
        assert!(
            matches!(a.popup, Some(crate::ui::app::Popup::CloseReason { .. })),
            "b is open: it asks again"
        );
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty(), "esc cancels");
        assert_eq!(a.pending_count(), 1);
    }

    #[test]
    fn closing_a_pull_request_asks_yes_or_no_and_reopening_asks_nothing() {
        let mut a = test_app();
        with_pull_request(&mut a, crate::model::ContentState::Open);
        a.handle_key(key('j'));
        a.handle_key(key('X'));
        assert_eq!(a.mode, Mode::Confirm);
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "Close pull request #2 'Add iteration columns'? (y/n)"
        );
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(Confirmed::Applied),
        });
        assert_eq!(
            a.handle_key(key('X')),
            vec![Command::Send(2)],
            "now closed: X reopens at once"
        );
        assert!(matches!(&a.store.pending()[0].op, Op::Reopen { .. }));
    }

    #[test]
    fn a_merged_pull_request_says_why() {
        let mut a = test_app();
        with_pull_request(&mut a, crate::model::ContentState::Merged);
        a.handle_key(key('j'));
        assert!(a.handle_key(key('X')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Merged pull requests can't be reopened")
        );
    }

    #[test]
    fn close_refusals_say_why_and_send_nothing() {
        // A draft.
        let mut a = test_app();
        for _ in 0..3 {
            a.handle_key(key('j'));
        }
        assert!(a.handle_key(key('X')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Drafts can't be closed — archive (`A`) or remove it instead")
        );
        assert!(a.popup.is_none());
        assert_eq!(a.pending_count(), 0);

        // A read-only board.
        let mut snap = snapshot();
        snap.project.viewer_can_update = false;
        let mut a = App::new(
            Box::new(crate::store::MemoryStore::new(Some(snap))),
            crate::ui::keymap::Keymap::defaults(),
            crate::ui::theme::Theme::plain(),
        );
        assert!(a.handle_key(key('X')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
        assert!(a.popup.is_none() && a.confirm.is_none());
        assert_eq!(a.pending_count(), 0);

        // An archived item (open in the detail, then archived).
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().archived = true;
        a.store.apply(StoreUpdate::Replace(snap));
        assert!(a.handle_key(key('X')).is_empty());
        assert!(
            a.status
                .flash
                .as_deref()
                .is_some_and(|f| f.contains("archived")),
            "{:?}",
            a.status.flash
        );
        assert!(a.popup.is_none() && a.confirm.is_none());
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn declining_the_pull_request_question_sends_nothing() {
        for decline in [key('n'), code(KeyCode::Esc)] {
            let mut a = test_app();
            with_pull_request(&mut a, crate::model::ContentState::Open);
            a.handle_key(key('j'));
            a.handle_key(key('X'));
            assert_eq!(a.mode, Mode::Confirm);
            assert!(a.handle_key(decline).is_empty());
            assert_eq!(a.mode, Mode::Normal);
            assert!(a.confirm.is_none());
            assert_eq!(a.pending_count(), 0);
        }
    }

    #[test]
    fn reopening_a_closed_issue_asks_nothing() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        let item = snap.items.get_mut(&ItemId::new("a")).unwrap();
        if let crate::model::ItemContent::Issue { state, .. } = &mut item.content {
            *state = crate::model::ContentState::Closed;
        }
        a.store.apply(StoreUpdate::Replace(snap));
        assert_eq!(a.handle_key(key('X')), vec![Command::Send(1)]);
        assert!(a.popup.is_none() && a.confirm.is_none());
        assert!(matches!(&a.store.pending()[0].op, Op::Reopen { .. }));
    }

    #[test]
    fn x_in_the_detail_returns_to_the_detail() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(key('X'));
        assert_eq!(a.mode, Mode::Popup);
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(a.mode, Mode::Detail, "esc returns to the detail");
        a.handle_key(key('X'));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            a.mode,
            Mode::Detail,
            "choosing a reason returns to the detail"
        );
    }

    #[test]
    fn a_archives_and_the_item_waits_dimmed_until_the_view_is_left() {
        let mut a = test_app();
        assert_eq!(a.handle_key(key('A')), vec![Command::Send(1)]);
        let item = a.item(&ItemId::new("a")).unwrap().clone();
        assert!(item.archived);
        assert!(
            a.view_items().unwrap().iter().any(|i| i.id == item.id),
            "sticky"
        );
        assert!(a.unmatched(&item));
    }

    #[test]
    fn archived_items_unarchive_with_a() {
        let mut a = app_edited(|s| s.items.get_mut(&ItemId::new("a")).unwrap().archived = true);
        a.handle_key(key('f'));
        for c in "is:archived".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::ViewIds {
            view: crate::model::ViewId::new("V_table"),
            filter: "is:archived".into(),
            list: list(&["a"]),
        });
        assert_eq!(selected(&a), "a");
        assert!(a.handle_key(key('e')).is_empty());
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .contains("(`A` unarchives)"),
            "the reason names the key"
        );
        assert_eq!(a.handle_key(key('A')), vec![Command::Send(1)]);
        assert!(matches!(&a.store.pending()[0].op, Op::Unarchive { .. }));
    }

    #[test]
    fn remove_asks_first_and_says_what_happens_to_the_content() {
        let mut a = test_app();
        assert!(a.confirm_remove().is_empty());
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "Remove #1 'Fix crash' from the project? It stays in its repository. (y/n)"
        );
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        assert!(a.item(&ItemId::new("a")).is_none());
        assert_eq!(selected(&a), "b", "the cursor moves on");
        for _ in 0..2 {
            a.handle_key(key('j'));
        }
        assert_eq!(selected(&a), "d");
        a.confirm_remove();
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "Remove the draft 'Draft idea' from the project? The draft is deleted. (y/n)"
        );
        assert!(a.handle_key(key('n')).is_empty());
    }

    /// Controller ruling E21: while a draft is converting it is still a draft here, so an
    /// edit of its content would be built with the draft's id. Project-field edits go ahead.
    #[test]
    fn content_edits_wait_for_a_pending_convert() {
        let mut a = test_app();
        let draft = ItemId::new("d");
        assert_eq!(
            a.submit(
                OpKind::Convert,
                Op::Convert {
                    item: draft.clone(),
                    repo: RepoRef {
                        id: "R_1".into(),
                        slug: "tviles/t".parse().unwrap(),
                    },
                },
            ),
            vec![Command::Send(1)]
        );
        a.open_detail(draft.clone());
        for k in ['t', 'c', 'a', 'C'] {
            assert!(a.handle_key(key(k)).is_empty(), "{k}");
            assert_eq!(
                a.status.flash.as_deref(),
                Some(CONVERTING),
                "{k} waits for the convert"
            );
            assert_eq!(a.mode, Mode::Detail, "{k} opens nothing");
        }
        let item = a.item(&draft).unwrap().clone();
        for kind in [OpKind::Title, OpKind::Body, OpKind::Milestone] {
            assert_eq!(
                a.check_here(kind, &item),
                Err(CONVERTING.into()),
                "{kind:?}"
            );
        }
        for kind in [
            OpKind::Move,
            OpKind::SetField,
            OpKind::Reorder,
            OpKind::Archive,
        ] {
            assert_eq!(a.check_here(kind, &item), Ok(()), "{kind:?}");
        }
        assert_eq!(
            a.handle_key(key('<')),
            vec![Command::Send(2)],
            "a move goes"
        );
        // Once the convert settles, content edits are offered again.
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(BackendError::Api(vec![])),
        });
        let item = a.item(&draft).unwrap().clone();
        assert_eq!(a.check_here(OpKind::Title, &item), Ok(()));
    }

    /// Controller ruling E20: a detail whose item left the board closes instead of going
    /// blank.
    #[test]
    fn removing_the_open_item_closes_its_detail() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Detail);
        a.handle_key(key(' '));
        assert_eq!(a.mode, Mode::Popup);
        let Some(Popup::Actions { picker, .. }) = a.popup.as_mut() else {
            panic!("menu")
        };
        let remove = picker
            .choices
            .iter()
            .position(|c| c.label == "Remove from project")
            .expect("Remove is offered");
        picker.selected = remove;
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.mode, Mode::Confirm);
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        assert!(a.detail.is_none());
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.status.flash.as_deref(), Some(NO_LONGER_ON_BOARD));
    }

    #[test]
    fn a_failed_new_item_closes_its_placeholders_detail() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.detail.as_ref().unwrap().item.as_str(), "local:1");
        fail(&mut a, 1, "boom");
        assert!(a.detail.is_none());
        assert_eq!(a.mode, Mode::Normal);
        assert!(
            a.status.error.as_deref().unwrap().contains("boom"),
            "the failure says why"
        );
        assert_eq!(a.status.flash, None);
    }

    #[test]
    fn a_poll_that_drops_the_open_item_closes_its_detail() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.detail.as_ref().unwrap().item.as_str(), "a");
        let rest: Vec<_> = crate::ui::fixtures::items()
            .into_iter()
            .filter(|i| i.id.as_str() != "a")
            .collect();
        a.on_sync(SyncEvent::ItemsComplete {
            items: rest,
            fetched_at: "2026-10-14T10:00:00Z".into(),
            total: 5,
            truncated: false,
        });
        assert!(a.detail.is_none());
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.status.flash.as_deref(), Some(NO_LONGER_ON_BOARD));
        // A poll that keeps the open item leaves the detail alone.
        let mut b = test_app();
        b.handle_key(code(KeyCode::Enter));
        b.on_sync(SyncEvent::ItemsComplete {
            items: crate::ui::fixtures::items(),
            fetched_at: "2026-10-14T10:00:00Z".into(),
            total: 6,
            truncated: false,
        });
        assert_eq!(b.mode, Mode::Detail);
        assert_eq!(b.status.flash, None);
    }

    #[test]
    fn show_archived_toggles_is_archived_in_the_session_filter() {
        let mut a = test_app();
        let cmds = a.toggle_archived();
        assert_eq!(a.effective_filter(), "is:archived");
        assert!(
            matches!(&cmds[..], [Command::FetchViewIds { filter, .. }] if filter == "is:archived")
        );
        a.toggle_archived();
        assert_eq!(a.effective_filter(), "");
        a.handle_key(key('f'));
        for c in "label:bug".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        a.toggle_archived();
        assert_eq!(a.effective_filter(), "label:bug is:archived");
    }

    #[test]
    fn the_action_menu_lists_remove_and_show_archived() {
        let mut a = test_app();
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        let labels: Vec<&str> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"Archive") && labels.contains(&"Remove from project"),
            "{labels:?}"
        );
        assert!(labels.contains(&"Show archived items"), "{labels:?}");
    }

    #[test]
    fn toggling_archived_twice_restores_the_filter_text_exactly() {
        let mut a = test_app();
        a.handle_key(key('f'));
        for c in "title:\"a  b\"".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.effective_filter(), "title:\"a  b\"");
        a.toggle_archived();
        assert_eq!(a.effective_filter(), "title:\"a  b\" is:archived");
        a.toggle_archived();
        assert_eq!(a.effective_filter(), "title:\"a  b\"");
    }

    #[test]
    fn a_view_that_already_shows_archived_adds_nothing() {
        let mut a = app_edited(|s| s.project.views[0].filter = "is:archived".into());
        assert!(a.view_shows_archived());
        assert!(a.toggle_archived().is_empty());
        assert_eq!(a.effective_filter(), "is:archived", "no duplicate");
        assert!(a.status.flash.as_deref().unwrap().contains("already shows"));
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        assert!(
            picker
                .choices
                .iter()
                .any(|c| c.label == "Archived items are shown by this view")
        );
    }

    #[test]
    fn esc_cancels_the_remove_confirmation() {
        let mut a = test_app();
        a.confirm_remove();
        assert!(a.handle_key(code(KeyCode::Esc)).is_empty());
        assert!(a.confirm.is_none());
        assert_eq!(a.pending_count(), 0);
        assert!(a.item(&ItemId::new("a")).is_some());
    }

    #[test]
    fn remove_follows_the_matrix_for_redacted_items() {
        let mut a = app_edited(|s| {
            s.items.get_mut(&ItemId::new("a")).unwrap().content =
                crate::model::ItemContent::Redacted;
        });
        // Redacted items allow remove (only move, archive and remove work).
        assert!(a.confirm_remove().is_empty());
        assert!(a.confirm.is_some());
        a.handle_key(key('n'));
        let mut ro = app_edited(|s| s.project.viewer_can_update = false);
        ro.confirm_remove();
        assert!(ro.confirm.is_none());
        assert_eq!(
            ro.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
    }

    #[test]
    fn a_placeholder_item_cannot_be_removed_yet() {
        let mut a = test_app();
        a.submit_new(new_draft("local:1"), false);
        assert_eq!(a.selected_item().unwrap().id, ItemId::new("local:1"));
        assert!(a.confirm_remove().is_empty());
        assert!(a.confirm.is_none());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Still being created on GitHub; try again in a moment")
        );
    }

    #[test]
    fn an_archived_items_menu_lists_only_what_still_applies() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().archived = true;
        a.store.apply(StoreUpdate::Replace(snap));
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        let labels: Vec<&str> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Unarchive",
                "Remove from project",
                "Open in the browser",
                "New item",
                "Show archived items"
            ]
        );
    }

    #[test]
    fn a_new_item_the_filter_would_hide_stays_visible() {
        let mut a = test_app();
        let bugs = crate::model::ViewId::new("V_bugs");
        a.select_view(&bugs);
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            filter: "label:bug".into(),
            list: list(&["a"]),
        });
        a.on_edited(
            EditPurpose::NewDraft { presets: vec![] },
            Some("Idea".into()),
        );
        a.on_sync(SyncEvent::ViewIds {
            view: bugs,
            filter: "label:bug".into(),
            list: list(&["a"]),
        });
        assert_eq!(a.selected_item().unwrap().id.as_str(), "local:1");
        assert!(
            a.view_items()
                .unwrap()
                .iter()
                .any(|i| i.id.as_str() == "local:1"),
            "a draft has no label:bug, yet it stays"
        );
    }
}
