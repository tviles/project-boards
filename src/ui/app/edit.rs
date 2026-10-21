//! Editing: every op goes through the capability matrix, joins the store's pending list, and
//! settles when GitHub answers. Failed edits go to the error log.

use super::*;
use crate::model::teams::{StateGoal, TeamRefusal, issue_team, state_for};
use crate::model::{
    CloseReason, ContentId, Denial, EditContext, ItemContent, OpKind, check, check_new,
};
use crate::ops::{Confirmed, NewItem, Op, OpId};
use crate::store::BoardSnapshot;
use crate::ui::app::popups::{
    CandidateSource, GONE, assignee_options, fill_checklist, label_options, milestone_choices,
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
    Submit {
        kind: OpKind,
        op: Op,
    },
    /// A new item (`submit_new`): an existing Linear issue that moves here from another
    /// project (T5). `note` names the presets left out of it, said once it is sent
    /// (Decision 9).
    SubmitNew {
        op: Op,
        note: Option<String>,
    },
}

/// "Add existing" on a Linear board, for text that is neither a key nor an issue link.
pub(crate) const NOT_AN_ISSUE_REF: &str =
    "Type an issue key like TBA-12, or paste a linear.app issue link";

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
/// and every other edit sets a value, so those are safe to send again. A new Linear issue,
/// and a comment with a client id, name the same issue or comment on a resend.
fn not_safe_to_resend(op: &Op) -> bool {
    matches!(
        op,
        Op::Create {
            new: NewItem::Draft { .. } | NewItem::Issue { .. },
            ..
        } | Op::Comment {
            client_id: None,
            ..
        }
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

/// The heading of a new Linear issue's form, naming its team: `New issue in TBA`.
pub(super) fn new_issue_heading(team: &TeamMeta) -> String {
    format!("New issue in {}", team.key)
}

/// `presets` for a new issue of `team` (spec section 6), or the first that team cannot
/// take. A preset whose field has left the board goes as it is.
fn presets_for_team(
    project: &Project,
    team: &TeamMeta,
    presets: &[(FieldId, FieldValue)],
) -> Result<Vec<(FieldId, FieldValue)>, TeamRefusal> {
    presets
        .iter()
        .map(|(id, value)| match project.field(id) {
            Some(field) => crate::model::teams::for_team(project, team, field, value.clone())
                .map(|v| (id.clone(), v)),
            None => Ok((id.clone(), value.clone())),
        })
        .collect()
}

/// `presets` for adding the existing Linear `issue` (spec section 9, D9): each resolved for
/// the issue's own team (`for_issue`), and the reasons for those it cannot take, which are
/// left out. A preset whose field has left the board goes as it is.
fn presets_for_issue(
    project: &Project,
    issue: &Item,
    presets: &[(FieldId, FieldValue)],
) -> (Vec<(FieldId, FieldValue)>, Vec<TeamRefusal>) {
    let (mut kept, mut refused) = (Vec::new(), Vec::new());
    for (id, value) in presets {
        match project.field(id) {
            Some(field) => {
                match crate::model::teams::for_issue(project, issue, field, value.clone()) {
                    Ok(v) => kept.push((id.clone(), v)),
                    Err(r) => refused.push(r),
                }
            }
            None => kept.push((id.clone(), value.clone())),
        }
    }
    (kept, refused)
}

/// What the status line says when presets were left out of adding `key` (Decision 9):
/// `TBA has no "In Review" state; adding TBA-12 without it`.
fn left_out_note(refused: &[TeamRefusal], key: &str) -> Option<String> {
    if refused.is_empty() {
        return None;
    }
    let reasons: Vec<String> = refused.iter().map(TeamRefusal::message).collect();
    let them = if refused.len() == 1 { "it" } else { "them" };
    Some(format!(
        "{}; adding {key} without {them}",
        reasons.join("; ")
    ))
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

/// The provider's own words for a failure; other errors as their message.
pub fn error_message(error: &BackendError, provider: Provider) -> String {
    crate::ui::text::sanitize(&raw_error_message(error, provider))
}

fn raw_error_message(error: &BackendError, provider: Provider) -> String {
    match error {
        BackendError::Api(entries) => {
            let messages: Vec<&str> = entries
                .iter()
                .map(|e| e.message.trim())
                .filter(|m| !m.is_empty())
                .collect();
            if messages.is_empty() {
                format!("{provider} returned an error")
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

    /// Asks for the Linear team `team`'s candidates unless they are loaded or loading; after
    /// a failure, again.
    pub(crate) fn need_team_candidates(&mut self, team: &str) -> Vec<Command> {
        if matches!(
            self.team_candidates.get(team),
            Some(Lookup::Ready(_) | Lookup::Loading)
        ) {
            return Vec::new();
        }
        self.team_candidates
            .insert(team.to_string(), Lookup::Loading);
        vec![Command::LoadTeamCandidates(team.to_string())]
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
        self.team_candidates.clear();
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
            Some(r) => r.container.parse().ok(),
            None => self.draft_repo(),
        }
    }

    /// Records a failed lookup where its popover shows it. False for other failures.
    pub(super) fn lookup_failed(&mut self, task: &SyncTask, error: &BackendError) -> bool {
        let message = format!(
            "{}: {}",
            task_words(task),
            error_message(error, self.board_provider())
        );
        match task {
            SyncTask::Candidates(repo) => {
                self.candidates
                    .insert(repo.clone(), Lookup::Failed(message));
            }
            SyncTask::TeamCandidates(team) => {
                self.team_candidates
                    .insert(team.clone(), Lookup::Failed(message));
            }
            // An answer for a key the popup no longer waits on is dropped.
            SyncTask::IssueByKey(key) => {
                if let Some(Popup::IssueKey {
                    input,
                    loading,
                    error,
                    ..
                }) = &mut self.popup
                    && *loading
                    && crate::model::parse_linear_issue_ref(input).as_deref() == Some(key)
                {
                    *loading = false;
                    *error = Some(message);
                }
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
                item,
                source,
                select,
                ..
            } if select.loading => {
                let (it, name) = (self.item(item), |u: &UserRef| u.login.clone());
                match source {
                    CandidateSource::Repo(repo) => fill_checklist(
                        select,
                        self.candidates.get(repo),
                        it,
                        assignee_options,
                        name,
                    ),
                    CandidateSource::Team(team) => fill_checklist(
                        select,
                        self.team_candidates.get(team),
                        it,
                        assignee_options,
                        name,
                    ),
                }
            }
            Popup::Labels {
                item,
                source,
                select,
                ..
            } if select.loading => {
                let (it, name) = (self.item(item), |l: &LabelRef| l.name.clone());
                match source {
                    CandidateSource::Repo(repo) => {
                        fill_checklist(select, self.candidates.get(repo), it, label_options, name)
                    }
                    CandidateSource::Team(team) => fill_checklist(
                        select,
                        self.team_candidates.get(team),
                        it,
                        label_options,
                        name,
                    ),
                }
            }
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

    /// The provider of the open board; GitHub before one is open.
    pub(super) fn board_provider(&self) -> Provider {
        self.project()
            .map(|p| p.board.provider())
            .unwrap_or_default()
    }

    pub fn edit_context(&self) -> EditContext {
        EditContext {
            viewer_can_update: self.project().is_some_and(|p| p.viewer_can_update),
            scopes: self.scopes.clone(),
            refused: self.refused_scopes.clone(),
            provider: self.board_provider(),
            team_board: self
                .project()
                .is_some_and(|p| matches!(p.board, BoardKey::LinearTeam { .. })),
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
            status_field: self
                .project()
                .filter(|_| self.is_linear())
                .and_then(|p| p.field(&FieldId::new(crate::linear::ids::STATUS)))
                .cloned(),
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

    /// `submit_new`, then `note` on the status line once the add is sent.
    fn submit_new_noting(&mut self, op: Op, note: Option<String>) -> Vec<Command> {
        let cmds = self.submit_new(op, false);
        if !cmds.is_empty()
            && let Some(note) = note
        {
            self.status.flash = Some(note);
        }
        cmds
    }

    /// The answer to "add existing" on a Linear board: the issue `key` names, or `None`. An
    /// answer the open popup does not wait on is dropped. No issue says so in the popup; one
    /// on the board is selected; one in another project asks first (T5); any other is added
    /// at once, with the presets its team can take (spec section 9).
    pub(crate) fn issue_found(&mut self, key: &str, item: Option<Item>) -> Vec<Command> {
        let Some(Popup::IssueKey {
            presets,
            input,
            loading: true,
            error,
        }) = &mut self.popup
        else {
            return Vec::new();
        };
        if crate::model::parse_linear_issue_ref(input).as_deref() != Some(key) {
            return Vec::new();
        }
        let Some(issue) = item else {
            *error = Some(format!("No issue {key}"));
            if let Some(Popup::IssueKey { loading, .. }) = &mut self.popup {
                *loading = false;
            }
            return Vec::new();
        };
        let presets = std::mem::take(presets);
        self.close_popup();
        let label = issue.ref_label();
        if self.item(&issue.id).is_some() {
            self.status.flash = Some(format!("{label} is already on the board"));
            self.selected_id = Some(issue.id);
            self.restore_selection();
            return Vec::new();
        }
        let Some(project) = self.project() else {
            return Vec::new();
        };
        let (presets, refused) = presets_for_issue(project, &issue, &presets);
        let note = left_out_note(&refused, &label);
        // An issue has at most one project, so adding it here moves it from its own.
        let elsewhere = match (
            &project.board,
            issue.value(&FieldId::new(crate::linear::ids::PROJECT)),
        ) {
            (
                BoardKey::LinearProject { id },
                Some(FieldValue::SingleSelect { option_id, name }),
            ) if option_id.as_str() != id => Some(name.clone()),
            _ => None,
        };
        let temp = self.next_temp();
        let op = Op::Create {
            temp,
            new: NewItem::LinearExisting {
                issue: Box::new(issue),
            },
            presets,
        };
        match elsewhere {
            Some(other) => {
                self.confirm = Some(Confirm {
                    question: format!("{label} is in \"{other}\"; move it here? (y/n)"),
                    action: ConfirmAction::SubmitNew { op, note },
                });
                self.mode = Mode::Confirm;
                Vec::new()
            }
            None => self.submit_new_noting(op, note),
        }
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

    /// A sticky item the view no longer matches: the latest list for the view dropped it,
    /// or its values fail the filter: on a Linear board the `f` filter, evaluated here; on
    /// GitHub as far as `still_matches` can tell.
    pub fn unmatched(&self, item: &Item) -> bool {
        let Some(sticky) = self.sticky.iter().find(|s| s.id == item.id) else {
            return false;
        };
        let (Some(snap), Some(view)) = (self.snapshot(), self.current_view()) else {
            return false;
        };
        let dropped = self.list_query(view).is_some()
            && sticky.listed
            && snap
                .views
                .get(&view.id)
                .is_some_and(|l| !l.ids.contains(&item.id));
        if dropped {
            return true;
        }
        match self.client_filter(view) {
            Some(filter) => !filter.matches(item, &self.filter_context()),
            None => !still_matches(&self.effective_filter(), item, &snap.project.fields),
        }
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

    /// `n`, "New issue", on a Linear board (spec section 9, T2): the form for a new issue of
    /// the board's team, of the selected column's team, or of the team its Team line
    /// chooses (prefilled with the last team chosen, else the first). Only teams for which
    /// every preset resolves are offered; with none, the first refusal shows and nothing
    /// opens.
    pub(crate) fn new_linear_issue_form(
        &mut self,
        presets: Vec<(FieldId, FieldValue)>,
    ) -> Vec<Command> {
        let Some(project) = self.project() else {
            return Vec::new();
        };
        let all = project
            .linear
            .as_ref()
            .map_or(&[][..], |m| m.teams.as_slice());
        // A board cached by 0.4 does not know its teams: refresh rather than guess.
        if all.is_empty() {
            return self.say(&TeamRefusal::NeedsRefresh.message());
        }
        let fixed = if self.edit_context().team_board {
            all.first().cloned()
        } else {
            self.column_team()
        };
        let candidates = match &fixed {
            Some(team) => vec![team.clone()],
            None => all.to_vec(),
        };
        let mut refused = None;
        let offered: Vec<TeamMeta> = candidates
            .into_iter()
            .filter(|team| match presets_for_team(project, team, &presets) {
                Ok(_) => true,
                Err(r) => {
                    refused.get_or_insert(r);
                    false
                }
            })
            .collect();
        if offered.is_empty() {
            let reason = refused.unwrap_or(TeamRefusal::NeedsRefresh);
            return self.say(&reason.message());
        }
        let (team, form) = match fixed {
            Some(team) => {
                let form = TextForm::new(FormKind::NewItem, new_issue_heading(&team), "", "");
                (Some(team), form)
            }
            None => {
                let chosen = self
                    .last_team
                    .as_ref()
                    .and_then(|last| offered.iter().position(|t| t.id == last.id))
                    .unwrap_or(0);
                let heading = new_issue_heading(&offered[chosen]);
                let form =
                    TextForm::new(FormKind::NewItem, heading, "", "").with_team(offered, chosen);
                (None, form)
            }
        };
        self.open_text_form(EditPurpose::NewLinearIssue { team, presets }, form)
    }

    /// The one team owning the selected board column, on a board layout (`column_team`).
    fn column_team(&self) -> Option<TeamMeta> {
        if self.layout().0 != Layout::Board {
            return None;
        }
        let (project, field) = (self.project()?, self.column_field()?);
        let column = self
            .board_columns()
            .into_iter()
            .nth(self.board_sel.column)?;
        crate::model::teams::column_team(project, field, &column.bucket).cloned()
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
                // A Linear comment is made with an id of ours, so a resend names the same
                // comment (GitHub has no such id).
                let client_id = if self.board_provider() == Provider::Linear {
                    match crate::uuid::new_v4() {
                        Ok(id) => Some(id),
                        Err(e) => {
                            return self.say(&format!("Could not make an id for the comment: {e}"));
                        }
                    }
                } else {
                    None
                };
                self.submit_content(OpKind::Comment, id, |item, content| Op::Comment {
                    item,
                    content,
                    body,
                    client_id,
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
            EditPurpose::NewLinearIssue { team, presets } => match title_and_body(&text) {
                None => self.say(unchanged("Empty title; nothing was created")),
                Some((title, body)) => self.submit_linear_issue(team, presets, title, body),
            },
        }
    }

    /// Creates a new Linear issue in `team` with `presets` resolved for it, under a client
    /// id made now, so a resend names the same issue (spec section 9). The team is the next
    /// form's prefill.
    fn submit_linear_issue(
        &mut self,
        team: Option<TeamMeta>,
        presets: Vec<(FieldId, FieldValue)>,
        title: String,
        body: String,
    ) -> Vec<Command> {
        let Some(team) = team else {
            return self.say("Choose a team for the new issue");
        };
        // The board may have changed since the form opened.
        let presets = match self.project() {
            Some(project) => match presets_for_team(project, &team, &presets) {
                Ok(presets) => presets,
                Err(reason) => return self.say(&reason.message()),
            },
            None => presets,
        };
        let id = match crate::uuid::new_v4() {
            Ok(id) => id,
            Err(e) => return self.say(&format!("Could not make an id for the issue: {e}")),
        };
        let temp = self.next_temp();
        let new = NewItem::LinearIssue {
            id,
            team: team.clone(),
            title,
            body,
        };
        let cmds = self.submit_new(Op::Create { temp, new, presets }, false);
        if !cmds.is_empty() {
            self.last_team = Some(team);
        }
        cmds
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
    pub(super) fn say(&mut self, message: &str) -> Vec<Command> {
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
        // A merged column's canonical option may be another team's: send the issue's own.
        let op = self
            .team_value(&item, &field, value)
            .and_then(|value| self.field_op(&item, &field, Some(value)));
        let op = match op {
            Ok(op) => op,
            Err(reason) => return self.say(&reason),
        };
        self.selected_id = Some(item.id.clone());
        self.submit(OpKind::Move, op)
    }

    /// `value` for `item` in `field`: on a Linear board the issue's own team's value (spec
    /// section 6), else the reason it has none; on GitHub `value` itself.
    pub(crate) fn team_value(
        &self,
        item: &Item,
        field: &Field,
        value: FieldValue,
    ) -> Result<FieldValue, String> {
        match self.project() {
            Some(project) => {
                crate::model::teams::for_issue(project, item, field, value).map_err(|r| r.message())
            }
            None => Ok(value),
        }
    }

    /// The op that sets (`Some`) or clears (`None`) `field` of `item`. A Linear label group
    /// is `Op::GroupLabel`, naming the label it replaces (the item's current one in the
    /// group); every other field is `SetField` or `ClearField`. A clear the board does not
    /// allow (a Linear issue's status) is refused with its reason.
    pub(crate) fn field_op(
        &self,
        item: &Item,
        field: &Field,
        value: Option<FieldValue>,
    ) -> Result<Op, String> {
        let project = self.project();
        if value.is_none()
            && let Some(project) = project
        {
            crate::model::teams::may_clear(project, field).map_err(|r| r.message())?;
        }
        let (id, field_id) = (item.id.clone(), field.id.clone());
        let group = field
            .id
            .as_str()
            .starts_with(crate::model::teams::GROUP_FIELD_PREFIX);
        if self.is_linear() && group {
            // The label the group holds now: a group value's option id is its label's id
            // (`linear::convert`, and `Op::apply` for an edit still sending). Not looked up
            // by name, which a plain label of the same name would confuse.
            let replaces = match item.value(&field.id) {
                Some(FieldValue::SingleSelect { option_id, .. }) => {
                    Some(option_id.as_str().to_string())
                }
                _ => None,
            };
            return Ok(Op::GroupLabel {
                item: id,
                field: field_id,
                value,
                replaces,
            });
        }
        Ok(match value {
            Some(value) => Op::SetField {
                item: id,
                field: field_id,
                value,
            },
            None => Op::ClearField {
                item: id,
                field: field_id,
            },
        })
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
        if self.board_provider() == Provider::Linear {
            // The card lands between the neighbour and the item beyond it.
            let beyond = if down {
                scope.get(pos + 2)
            } else {
                pos.checked_sub(2).and_then(|p| scope.get(p))
            };
            return self.reorder_linear(&item.id, down, &neighbour, beyond.cloned());
        }
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
                sort_order: None,
            },
        )
    }

    /// Linear's reorder: the new `sortOrder` is the midpoint of the two issues the card lands
    /// between, `neighbour` (the one it passes) and `beyond` (the next one that way, if any).
    /// Equal or missing values refuse, and nothing is sent.
    fn reorder_linear(
        &mut self,
        id: &ItemId,
        down: bool,
        neighbour: &ItemId,
        beyond: Option<ItemId>,
    ) -> Vec<Command> {
        let order_of = |a: &Self, id: &ItemId| -> Option<f64> {
            a.snapshot()?.items.get(id)?.content_fields.sort_order
        };
        let sort_order = order_of(self, neighbour).and_then(|near| match &beyond {
            // The card lands at the end of the list: one past the neighbour.
            None if down => sort_order_between(Some(near), None),
            None => sort_order_between(None, Some(near)),
            Some(b) => {
                let far = order_of(self, b)?;
                if down {
                    sort_order_between(Some(near), Some(far))
                } else {
                    sort_order_between(Some(far), Some(near))
                }
            }
        });
        // Going down the card lands after the neighbour, going up after the one beyond it.
        let after = if down {
            Some(neighbour.clone())
        } else {
            beyond
        };
        let Some(sort_order) = sort_order else {
            self.status.flash = Some("Can't place this card here; refresh with r".into());
            return Vec::new();
        };
        self.selected_id = Some(id.clone());
        self.submit(
            OpKind::Reorder,
            Op::Reorder {
                item: id.clone(),
                after,
                sort_order: Some(sort_order),
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
                    error_message(&error, self.board_provider())
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
                    Some(op) => format!(
                        "{} ({})",
                        maybe_done(op),
                        error_message(&error, self.board_provider())
                    ),
                    None => error_message(&error, self.board_provider()),
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
                format!("{name} ({})", error_message(e, self.board_provider()))
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
            ConfirmAction::SubmitNew { op, note } => self.submit_new_noting(op, note),
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
        let question = match (&item.content, item.reference().and_then(ContentRef::label)) {
            (_, Some(label)) if self.board_provider() == Provider::Linear => {
                format!("Remove {label} '{title}' from the project? It stays in its team. (y/n)")
            }
            (ItemContent::Draft { .. }, _) => {
                format!("Remove the draft '{title}' from the project? The draft is deleted. (y/n)")
            }
            (_, Some(label)) => {
                format!(
                    "Remove {label} '{title}' from the project? It stays in its repository. (y/n)"
                )
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
            .is_some_and(|v| archived_token(v.filter.github_query()).is_some())
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
    /// The Status value that closes `item` for `reason`, or reopens it (`None`), as a
    /// `SetField` on `linear:status`: the state of its team's, or the refusal to flash.
    pub(crate) fn linear_close_op(&self, item: &Item, goal: StateGoal) -> Result<Op, String> {
        let refused = |r: TeamRefusal| r.message();
        let project = self
            .project()
            .ok_or_else(|| refused(TeamRefusal::NeedsRefresh))?;
        let team = issue_team(project, item).ok_or_else(|| refused(TeamRefusal::NeedsRefresh))?;
        let value = state_for(project, team, goal).map_err(refused)?;
        Ok(Op::SetField {
            item: item.id.clone(),
            field: FieldId::new(crate::model::teams::STATUS_FIELD),
            value,
        })
    }

    /// `close_or_reopen` on a Linear board: both are moves to a state of the issue's team.
    fn linear_close_or_reopen(
        &mut self,
        kind: OpKind,
        item: &Item,
        reference: &str,
    ) -> Vec<Command> {
        if kind == OpKind::Reopen {
            return match self.linear_close_op(item, StateGoal::Reopen) {
                Ok(op) => self.submit(kind, op),
                Err(reason) => {
                    self.status.flash = Some(reason);
                    Vec::new()
                }
            };
        }
        // The names the two choices lead to; a missing target reads `none` and says why
        // when chosen.
        let target = |a: &Self, goal| match a.linear_close_op(item, goal) {
            Ok(Op::SetField { value, .. }) => Ok(value.display()),
            Ok(_) => Err(String::new()),
            Err(reason) => Err(reason),
        };
        let (completed, canceled) = (
            target(self, StateGoal::Completed),
            target(self, StateGoal::Canceled),
        );
        // An issue whose team is not known has no states to name: say so, open nothing.
        if self.project().and_then(|p| issue_team(p, item)).is_none() {
            self.status.flash = Some(TeamRefusal::NeedsRefresh.message());
            return Vec::new();
        }
        let label = |what: &str, name: Result<String, String>| {
            format!(
                "Close as {what} ({})",
                name.unwrap_or_else(|_| "none".into())
            )
        };
        let picker = ListPicker::new(
            format!("Close {reference}?"),
            vec![
                Choice::new(label("completed", completed), CloseReason::Completed),
                Choice::new(label("canceled", canceled), CloseReason::NotPlanned),
            ],
        );
        self.open_popup(Popup::CloseReason {
            item: item.id.clone(),
            picker,
        });
        Vec::new()
    }

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
        let reference = match item.reference().and_then(ContentRef::label) {
            Some(label) => format!("{label} '{title}'"),
            None => format!("'{title}'"),
        };
        if self.is_linear() {
            return self.linear_close_or_reopen(kind, &item, &reference);
        }
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
pub(crate) mod tests {
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
        assert_eq!(
            error_message(&none, Provider::Github),
            "GitHub returned an error"
        );
        let blank = BackendError::Api(vec![GraphqlErrorEntry {
            message: " ".into(),
            kind: None,
        }]);
        assert_eq!(
            error_message(&blank, Provider::Github),
            "GitHub returned an error"
        );
    }

    #[test]
    fn an_empty_linear_error_names_linear() {
        let none = BackendError::Api(vec![]);
        assert_eq!(
            error_message(&none, Provider::Linear),
            "Linear returned an error"
        );
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
            query: ListQuery::github("-status:\"In Progress\""),
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
                after: Some(ItemId::new("b")),
                sort_order: None,
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
                after: Some(ItemId::new("a")),
                sort_order: None,
            }
        );
        let mut c = test_app();
        c.handle_key(key('j'));
        c.handle_key(key('K'));
        assert_eq!(
            c.store.pending()[0].op,
            Op::Reorder {
                item: ItemId::new("b"),
                after: None,
                sort_order: None,
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
                after: Some(ItemId::new("a")),
                sort_order: None,
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
                after: Some(ItemId::new("c")),
                sort_order: None,
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
            query: ListQuery::github("-status:\"In Progress\""),
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
                after: Some(ItemId::new("b")),
                sort_order: None,
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
            query: ListQuery::github("-status:\"In Progress\""),
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
            kind: None,
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
                after: Some(ItemId::new("b")),
                sort_order: None,
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
            query: ListQuery::github("-status:Done"),
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
            query: ListQuery::github("-status:Done"),
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
            query: ListQuery::github("label:bug"),
            list: list(&["a"]),
        });
        a.handle_key(key('>'));
        assert!(
            !a.unmatched(&a.snapshot().unwrap().items[&ItemId::new("a")].clone()),
            "still listed"
        );
        a.on_sync(SyncEvent::ViewIds {
            view: bugs.clone(),
            query: ListQuery::github("label:bug"),
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
            query: ListQuery::github("label:bug"),
            list: list(&["a"]),
        });
        a.handle_key(key('>'));
        a.on_sync(SyncEvent::ViewIds {
            view: bugs,
            query: ListQuery::github("label:bug"),
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
        let mut a = app_edited(|s| s.project.views[1].filter = f.into());
        let board = crate::model::ViewId::new("V_board");
        a.select_view(&board);
        a.on_sync(SyncEvent::ViewIds {
            view: board,
            query: ListQuery::github(filter),
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
            query: ListQuery::github("-status:Done"),
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
        assert_eq!(error_message(&error, Provider::Github), "bad thing here");
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
                let shown = error_message(&error, Provider::Github);
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
            let shown = error_message(&error, Provider::Github);
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

    /// A Linear create and comment carry a client id, so a resend names the same issue or
    /// comment (spec section 9, T6); Linear's answer, or the lookup after it, settles it.
    #[test]
    fn linear_creates_and_comments_with_ids_are_safe_to_resend() {
        let id = "0f8e7d6c-5b4a-4938-8271-605f4e3d2c1b";
        let linear_issue = Op::Create {
            temp: ItemId::new("local:1"),
            new: NewItem::LinearIssue {
                id: id.into(),
                team: crate::model::TeamMeta {
                    id: "team-a".into(),
                    key: "TBA".into(),
                },
                title: "Write the docs".into(),
                body: String::new(),
            },
            presets: vec![],
        };
        let comment = |client_id: Option<&str>| Op::Comment {
            item: ItemId::new("a"),
            content: ContentId::Issue("I_a".into()),
            body: "LGTM".into(),
            client_id: client_id.map(String::from),
        };
        assert!(!not_safe_to_resend(&linear_issue));
        assert!(!not_safe_to_resend(&comment(Some(id))));
        // GitHub's comments have none, and stay unsafe.
        assert!(not_safe_to_resend(&comment(None)));
        assert!(not_safe_to_resend(&new_issue("local:1")));
        assert!(not_safe_to_resend(&new_draft("local:1")));
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
                container: "tviles/t".into(),
                number: 2,
                key: None,
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
            query: ListQuery::github("is:archived"),
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
            matches!(&cmds[..], [Command::FetchViewIds { query, .. }] if query == &ListQuery::github("is:archived"))
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
            query: ListQuery::github("label:bug"),
            list: list(&["a"]),
        });
        a.on_edited(
            EditPurpose::NewDraft { presets: vec![] },
            Some("Idea".into()),
        );
        a.on_sync(SyncEvent::ViewIds {
            view: bugs,
            query: ListQuery::github("label:bug"),
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

    // Linear boards (plan 3, PR C).

    const TBA_ID: &str = "team-a";
    const TBB_ID: &str = "team-b";
    const TYPE_GROUP: &str = "lg-type";

    fn linear_option(
        id: &str,
        name: &str,
        kind: crate::model::OptionKind,
    ) -> crate::model::SelectOption {
        crate::model::SelectOption {
            id: OptionId::new(id),
            name: name.into(),
            color: crate::model::OptionColor::Gray,
            kind: Some(kind),
        }
    }

    /// A Linear project board of teams TBA and TBB, as in Task 6. Status options in rank,
    /// then position, order: `Todo` is both teams' (the merged column's canonical option is
    /// TBB's), `In Progress` only TBA's, `In Review` only TBB's, and `Done` both teams' (its
    /// canonical option is TBA's). Each team has its own cycle; the label group Type
    /// (Bug, Feature) is TBA's.
    pub(crate) fn linear_project() -> crate::model::Project {
        use crate::model::OptionKind::{Canceled, Completed, Started, Unstarted};
        use crate::model::{Field, FieldKind, Iteration, IterationId};
        let field = |id: &str, name: &str, kind: FieldKind| Field {
            id: FieldId::new(id),
            name: name.into(),
            kind,
        };
        let plain = |id: &str, name: &str| crate::model::SelectOption {
            id: OptionId::new(id),
            name: name.into(),
            color: crate::model::OptionColor::Gray,
            kind: None,
        };
        let cycle = |id: &str, title: &str| Iteration {
            id: IterationId::new(id),
            title: title.into(),
            start_date: "2026-10-12".into(),
            duration_days: 14,
        };
        let fields = vec![
            field(crate::linear::ids::TITLE, "Title", FieldKind::Title),
            field(
                crate::linear::ids::STATUS,
                "Status",
                FieldKind::SingleSelect {
                    options: vec![
                        linear_option("s-a-todo", "Todo", Unstarted),
                        linear_option("s-b-todo", "Todo", Unstarted),
                        linear_option("s-a-prog", "In Progress", Started),
                        linear_option("s-b-review", "In Review", Started),
                        linear_option("s-b-done", "Done", Completed),
                        linear_option("s-a-done", "Done", Completed),
                        // TBA only: TBB has no canceled state.
                        linear_option("s-a-canceled", "Canceled", Canceled),
                    ],
                },
            ),
            field(
                crate::linear::ids::PRIORITY,
                "Priority",
                FieldKind::SingleSelect {
                    options: [(1, "Urgent"), (2, "High"), (3, "Medium"), (4, "Low")]
                        .into_iter()
                        .map(|(n, name)| crate::model::SelectOption {
                            id: crate::linear::ids::priority_option(n),
                            ..plain("", name)
                        })
                        .collect(),
                },
            ),
            field(
                crate::linear::ids::ASSIGNEES,
                "Assignees",
                FieldKind::Assignees,
            ),
            field(crate::linear::ids::LABELS, "Labels", FieldKind::Labels),
            Field {
                id: crate::linear::ids::group(TYPE_GROUP),
                name: "Type".into(),
                kind: FieldKind::SingleSelect {
                    options: vec![plain("l-bug", "Bug"), plain("l-feature", "Feature")],
                },
            },
            field(
                crate::linear::ids::CYCLE,
                "Cycle",
                FieldKind::Iteration {
                    iterations: vec![cycle("c-a", "TBA Sprint"), cycle("c-b", "TBB Sprint")],
                    completed: vec![],
                },
            ),
            field(crate::linear::ids::ESTIMATE, "Estimate", FieldKind::Number),
            field(crate::linear::ids::DUE, "Due date", FieldKind::Date),
        ];
        let owners = [
            ("s-a-todo", TBA_ID),
            ("s-a-prog", TBA_ID),
            ("s-a-done", TBA_ID),
            ("s-a-canceled", TBA_ID),
            ("s-b-todo", TBB_ID),
            ("s-b-review", TBB_ID),
            ("s-b-done", TBB_ID),
            ("c-a", TBA_ID),
            ("c-b", TBB_ID),
            (TYPE_GROUP, TBA_ID),
        ]
        .into_iter()
        .map(|(id, team)| (id.to_string(), team.to_string()))
        .collect();
        let team = |id: &str, key: &str| crate::model::TeamMeta {
            id: id.into(),
            key: key.into(),
        };
        let mut board = crate::ui::fixtures::project().views[1].clone();
        board.id = crate::model::ViewId::new("V_board");
        board.name = "Board".into();
        board.filter = crate::model::ViewFilter::Linear(serde_json::json!({}));
        board.visible_fields = vec![
            FieldId::new(crate::linear::ids::TITLE),
            FieldId::new(crate::linear::ids::STATUS),
        ];
        board.vertical_group_by = vec![FieldId::new(crate::linear::ids::STATUS)];
        board.group_by = vec![];
        let mut all = crate::ui::fixtures::project().views[0].clone();
        all.id = crate::model::ViewId::new("V_all");
        all.name = "All issues".into();
        all.filter = crate::model::ViewFilter::Linear(serde_json::json!({}));
        all.visible_fields = board.visible_fields.clone();
        all.sort_by = vec![];
        let id = "linear:project:6f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
        crate::model::Project {
            id: crate::model::ProjectId::new(id),
            board: id.parse().unwrap(),
            owner_kind: crate::model::OwnerKind::Organization,
            title: "Launch".into(),
            url: "https://linear.app/acme/project/launch".into(),
            viewer_can_update: true,
            fields,
            views: vec![board, all],
            linked_repos: vec![],
            issue_repos: vec![],
            linear: Some(crate::model::LinearMeta {
                url_key: "acme".into(),
                viewer_id: "u-1".into(),
                team_keys: vec!["TBA".into(), "TBB".into()],
                teams: vec![team(TBA_ID, "TBA"), team(TBB_ID, "TBB")],
                owners,
            }),
        }
    }

    /// The Linear issue `<team>-<number>` (item id `i-<team>-<number>`) with its Status.
    fn linear_issue(team: &str, number: u32, state: (&str, &str)) -> crate::model::Item {
        let id = format!("i-{}-{number}", team.to_lowercase());
        let key = format!("{team}-{number}");
        crate::model::Item {
            id: ItemId::new(&id),
            content: crate::model::ItemContent::Issue {
                reference: crate::model::ContentRef {
                    container: team.into(),
                    number,
                    key: Some(key.clone()),
                    url: format!("https://linear.app/acme/issue/{key}"),
                },
                title: format!("Issue {key}"),
                state: crate::model::ContentState::Open,
            },
            archived: false,
            updated_at: "2026-10-01T00:00:00Z".into(),
            values: [(
                FieldId::new(crate::linear::ids::STATUS),
                select_value(state.0, state.1),
            )]
            .into(),
            content_fields: crate::model::ContentFields {
                content_id: Some(id),
                ..Default::default()
            },
        }
    }

    /// `linear_project`'s board with `TBA-1` Todo (labelled Bug, in Type), `TBA-2` In
    /// Progress, `TBB-1` Todo and `TBB-2` In Review.
    pub(crate) fn linear_snapshot() -> crate::store::BoardSnapshot {
        let mut tba1 = linear_issue("TBA", 1, ("s-a-todo", "Todo"));
        tba1.values.insert(
            crate::linear::ids::group(TYPE_GROUP),
            select_value("l-bug", "Bug"),
        );
        tba1.content_fields.label_ids = [("Bug".to_string(), "l-bug".to_string())].into();
        let mut snap = crate::store::BoardSnapshot::new(linear_project());
        snap.upsert_items(vec![
            tba1,
            linear_issue("TBA", 2, ("s-a-prog", "In Progress")),
            linear_issue("TBB", 1, ("s-b-todo", "Todo")),
            linear_issue("TBB", 2, ("s-b-review", "In Review")),
        ]);
        snap
    }

    /// The Linear test board for every PR C UI test: `linear_snapshot` on its Board tab (a
    /// board by Status); the All issues tab is a manually ordered table.
    pub(crate) fn linear_app() -> App {
        app_with(linear_snapshot())
    }

    /// Selects the card of the Linear issue `key` (`TBA-2`) on the board.
    pub(crate) fn pick(a: &mut App, key: &str) {
        let columns = a.board_columns();
        let (column, index) = columns
            .iter()
            .enumerate()
            .find_map(|(c, col)| {
                col.items()
                    .iter()
                    .position(|i| i.ref_label() == key)
                    .map(|i| (c, i))
            })
            .unwrap_or_else(|| panic!("{key} is not on the board"));
        a.board_sel = crate::ui::board::BoardSelection { column, index };
        a.sync_selected_id();
        assert_eq!(
            a.selected_item().map(|i| i.ref_label()),
            Some(key.to_string())
        );
    }

    fn linear_status(a: &App, key: &str) -> String {
        let item = a
            .snapshot()
            .unwrap()
            .all_items()
            .into_iter()
            .find(|i| i.ref_label() == key)
            .unwrap()
            .clone();
        item.value(&FieldId::new(crate::linear::ids::STATUS))
            .map(|v| v.display())
            .unwrap_or_default()
    }

    fn set_linear(item: &str, field: &str, value: FieldValue) -> Op {
        Op::SetField {
            item: ItemId::new(item),
            field: FieldId::new(field),
            value,
        }
    }

    fn pending_ops(a: &App) -> Vec<Op> {
        a.store.pending().iter().map(|p| p.op.clone()).collect()
    }

    #[test]
    fn the_linear_test_board_shows_merged_columns_by_status() {
        let a = linear_app();
        assert_eq!(a.layout().0, Layout::Board);
        assert_eq!(
            column_titles(&a),
            [
                "Todo",
                "In Progress",
                "In Review",
                "Done",
                "Canceled",
                "No Status"
            ]
        );
        let ids = |a: &App| -> Vec<Vec<String>> {
            a.board_columns()
                .iter()
                .map(|c| c.items().iter().map(|i| i.ref_label()).collect())
                .collect()
        };
        assert_eq!(
            ids(&a),
            [
                vec!["TBA-1".to_string(), "TBB-1".to_string()],
                vec!["TBA-2".to_string()],
                vec!["TBB-2".to_string()],
                vec![],
                vec![],
                vec![],
            ]
        );
    }

    #[test]
    fn linear_moves_take_the_issues_team_state() {
        let mut a = linear_app();
        // `<` on TBA-2 into the merged Todo column, whose canonical option is TBB's.
        pick(&mut a, "TBA-2");
        assert_eq!(a.handle_key(key('<')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tba-2",
                crate::linear::ids::STATUS,
                select_value("s-a-todo", "Todo")
            )]
        );
        assert_eq!(linear_status(&a, "TBA-2"), "Todo");
        // TBB-1 from Todo into In Review, the next column once In Progress is filtered out.
        let board = crate::model::ViewId::new("V_board");
        a.extra_filter
            .insert(board, "-status:\"In Progress\"".into());
        assert_eq!(column_titles(&a)[..2], ["Todo", "In Review"]);
        pick(&mut a, "TBB-1");
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(2)]);
        assert_eq!(
            pending_ops(&a)[1],
            set_linear(
                "i-tbb-1",
                crate::linear::ids::STATUS,
                select_value("s-b-review", "In Review")
            )
        );
        assert_eq!(linear_status(&a, "TBB-1"), "In Review");
    }

    #[test]
    fn a_move_into_a_column_the_team_lacks_is_refused() {
        let mut a = linear_app();
        pick(&mut a, "TBA-2");
        assert!(a.handle_key(key('>')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("TBA has no \"In Review\" state")
        );
        assert!(a.store.pending().is_empty());
        assert_eq!(linear_status(&a, "TBA-2"), "In Progress");
        pick(&mut a, "TBB-1");
        assert!(a.handle_key(key('>')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("TBB has no \"In Progress\" state")
        );
        assert!(a.store.pending().is_empty());
    }

    /// Plan Decision 5.
    #[test]
    fn a_move_into_done_shows_the_card_closed_at_once() {
        let mut a = linear_app();
        pick(&mut a, "TBB-2");
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tbb-2",
                crate::linear::ids::STATUS,
                select_value("s-b-done", "Done")
            )],
            "TBB's Done, not the column's canonical (TBA's) option"
        );
        let item = &a.snapshot().unwrap().items[&ItemId::new("i-tbb-2")];
        assert!(matches!(
            item.content,
            crate::model::ItemContent::Issue {
                state: crate::model::ContentState::Closed,
                ..
            }
        ));
        assert_eq!(
            a.store.confirmed().unwrap().items[&ItemId::new("i-tbb-2")].content,
            linear_snapshot().items[&ItemId::new("i-tbb-2")].content,
            "open until Linear answers"
        );
    }

    fn open_field(a: &mut App, field: &str) {
        assert!(a.handle_key(key('e')).is_empty());
        type_in(a, field);
        a.handle_key(code(KeyCode::Enter));
    }

    fn value_labels(a: &App) -> Vec<String> {
        match &a.popup {
            Some(crate::ui::app::Popup::Value { picker, .. }) => {
                picker.choices.iter().map(|c| c.label.clone()).collect()
            }
            other => panic!("expected a value picker, got {other:?}"),
        }
    }

    fn backspaces(a: &mut App, n: usize) {
        for _ in 0..n {
            a.handle_key(code(KeyCode::Backspace));
        }
    }

    #[test]
    fn e_sets_priority_estimate_due_and_cycle_on_linear() {
        use crate::linear::ids;
        let mut a = linear_app();
        pick(&mut a, "TBB-1");
        open_field(&mut a, "priority");
        type_in(&mut a, "high");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        open_field(&mut a, "estimate");
        type_in(&mut a, "3");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(2)]);
        open_field(&mut a, "due");
        type_in(&mut a, "2026-11-02");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(3)]);
        // The cycle picker offers only TBB's cycles.
        open_field(&mut a, "cycle");
        assert_eq!(value_labels(&a), ["TBB Sprint"]);
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(4)]);
        let item = "i-tbb-1";
        assert_eq!(
            pending_ops(&a),
            [
                set_linear(
                    item,
                    ids::PRIORITY,
                    select_value("linear:priority:2", "High")
                ),
                set_linear(item, ids::ESTIMATE, FieldValue::Number(3.0)),
                set_linear(item, ids::DUE, FieldValue::Date("2026-11-02".into())),
                set_linear(
                    item,
                    ids::CYCLE,
                    FieldValue::Iteration {
                        iteration_id: crate::model::IterationId::new("c-b"),
                        title: "TBB Sprint".into(),
                        start_date: "2026-10-12".into(),
                    }
                ),
            ]
        );
        // And TBA's issues see only TBA's.
        pick(&mut a, "TBA-1");
        open_field(&mut a, "cycle");
        assert_eq!(value_labels(&a), ["TBA Sprint"]);
    }

    #[test]
    fn a_fractional_estimate_is_refused() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        open_field(&mut a, "estimate");
        type_in(&mut a, "2.5");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let Some(crate::ui::app::Popup::Line { error, .. }) = &a.popup else {
            panic!("the estimate input stays open")
        };
        assert_eq!(error.as_deref(), Some("Linear estimates are whole numbers"));
        assert!(a.store.pending().is_empty());
        backspaces(&mut a, 3);
        type_in(&mut a, "2");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
    }

    #[test]
    fn clearing_the_status_is_refused() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        open_field(&mut a, "status");
        assert!(
            !value_labels(&a).iter().any(|l| l.starts_with("Clear")),
            "{:?}",
            value_labels(&a)
        );
        a.handle_key(code(KeyCode::Esc));
        a.handle_key(code(KeyCode::Esc));
        let field = FieldId::new(crate::linear::ids::STATUS);
        assert!(
            a.submit_value(ItemId::new("i-tba-1"), field, None)
                .is_empty()
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Every Linear issue has a status")
        );
        assert!(a.store.pending().is_empty());
        assert_eq!(linear_status(&a, "TBA-1"), "Todo");
    }

    #[test]
    fn a_group_field_swaps_its_label() {
        // A plain label named like the group's (a name map holds one of them): the label
        // replaced is still the group's own.
        let mut snap = linear_snapshot();
        snap.items
            .get_mut(&ItemId::new("i-tba-1"))
            .unwrap()
            .content_fields
            .label_ids
            .insert("Bug".into(), "l-plain-bug".into());
        let mut a = app_with(snap);
        pick(&mut a, "TBA-1");
        open_field(&mut a, "type");
        type_in(&mut a, "feature");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::GroupLabel {
                item: ItemId::new("i-tba-1"),
                field: crate::linear::ids::group(TYPE_GROUP),
                value: Some(select_value("l-feature", "Feature")),
                replaces: Some("l-bug".into()),
            }]
        );
        // Clearing it removes the label.
        open_field(&mut a, "type");
        type_in(&mut a, "clear");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(2)]);
        assert!(matches!(
            &pending_ops(&a)[1],
            Op::GroupLabel { value: None, replaces: Some(old), .. } if old == "l-feature"
        ));
        // On another team's issue the group is refused.
        pick(&mut a, "TBB-1");
        open_field(&mut a, "type");
        type_in(&mut a, "bug");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("\"Type\" belongs to team TBA")
        );
        assert_eq!(a.store.pending().len(), 2);
    }

    /// Review Focus 1.
    #[test]
    fn a_cached_04_board_refuses_team_owned_edits_until_resolved() {
        let mut snap = linear_snapshot();
        let meta = snap.project.linear.as_mut().unwrap();
        meta.teams.clear();
        meta.owners.clear();
        let mut a = app_with(snap);
        pick(&mut a, "TBA-2");
        assert!(a.handle_key(key('<')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This item was loaded by an older version; refresh with r to edit it")
        );
        assert!(a.store.pending().is_empty());
        // A field no team owns is fine.
        open_field(&mut a, "priority");
        type_in(&mut a, "urgent");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        // The board resolves: the teams are known now.
        a.on_sync(SyncEvent::Project(linear_project()));
        pick(&mut a, "TBA-2");
        assert_eq!(a.handle_key(key('<')), vec![Command::Send(2)]);
        assert_eq!(
            pending_ops(&a)[1],
            set_linear(
                "i-tba-2",
                crate::linear::ids::STATUS,
                select_value("s-a-todo", "Todo")
            )
        );
    }

    /// Review Focus 5.
    #[test]
    fn a_forbidden_edit_is_undone_and_later_edits_still_go() {
        let mut a = linear_app();
        // Both moves are queued before Linear answers the first (spec section 10).
        pick(&mut a, "TBB-2");
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(1)]);
        pick(&mut a, "TBA-2");
        assert_eq!(a.handle_key(key('<')), vec![Command::Send(2)]);
        let denial = BackendError::MissingPermission {
            provider: Provider::Linear,
            needed: vec![],
        };
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(denial.clone()),
        });
        assert_eq!(linear_status(&a, "TBB-2"), "In Review", "the card returns");
        assert_eq!(a.errors.len(), 1);
        assert_eq!(
            a.errors[0].message,
            error_message(&denial, Provider::Linear)
        );
        assert!(!a.errors[0].message.is_empty());
        assert!(a.refused_scopes.is_empty(), "Linear refuses no scope");
        // The TBA move is still on its way, and still shows.
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tba-2",
                crate::linear::ids::STATUS,
                select_value("s-a-todo", "Todo")
            )]
        );
        assert_eq!(a.store.pending()[0].id, 2);
        assert_eq!(linear_status(&a, "TBA-2"), "Todo");
        // A later move is still allowed.
        a.extra_filter.insert(
            crate::model::ViewId::new("V_board"),
            "-status:\"In Progress\"".into(),
        );
        pick(&mut a, "TBB-1");
        assert_eq!(a.handle_key(key('>')), vec![Command::Send(3)]);
    }

    /// `snap` with the Linear issue `key` closed in `status`.
    fn close_in_snapshot(a: &mut App, key: &str, status: (&str, &str)) {
        let mut snap = a.snapshot().unwrap().clone();
        let item = snap
            .items
            .values_mut()
            .find(|i| i.ref_label() == key)
            .unwrap();
        if let crate::model::ItemContent::Issue { state, .. } = &mut item.content {
            *state = crate::model::ContentState::Closed;
        }
        item.values.insert(
            FieldId::new(crate::linear::ids::STATUS),
            select_value(status.0, status.1),
        );
        a.store.apply(StoreUpdate::Replace(snap));
    }

    fn close_picker(a: &App) -> (String, Vec<String>) {
        let Some(crate::ui::app::Popup::CloseReason { picker, .. }) = &a.popup else {
            panic!("reason picker")
        };
        (
            picker.title.clone(),
            picker.choices.iter().map(|c| c.label.clone()).collect(),
        )
    }

    #[test]
    fn linear_close_offers_completed_and_canceled_states() {
        let mut a = linear_app();
        pick(&mut a, "TBA-2");
        assert!(a.handle_key(key('X')).is_empty());
        assert_eq!(
            close_picker(&a),
            (
                "Close TBA-2 'Issue TBA-2'?".to_string(),
                vec![
                    "Close as completed (Done)".to_string(),
                    "Close as canceled (Canceled)".to_string()
                ]
            )
        );
        // Choosing "completed" moves to the team's Done state.
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tba-2",
                crate::linear::ids::STATUS,
                select_value("s-a-done", "Done")
            )]
        );
        assert_eq!(linear_status(&a, "TBA-2"), "Done");
    }

    #[test]
    fn closing_as_canceled_moves_to_the_canceled_state() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        a.handle_key(key('X'));
        a.handle_key(code(KeyCode::Down));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tba-1",
                crate::linear::ids::STATUS,
                select_value("s-a-canceled", "Canceled")
            )]
        );
        assert!(a.popup.is_none());
        assert_eq!(linear_status(&a, "TBA-1"), "Canceled");
    }

    #[test]
    fn reopen_moves_to_the_first_unstarted_state() {
        let mut a = linear_app();
        close_in_snapshot(&mut a, "TBB-2", ("s-b-done", "Done"));
        pick(&mut a, "TBB-2");
        assert_eq!(a.handle_key(key('X')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [set_linear(
                "i-tbb-2",
                crate::linear::ids::STATUS,
                select_value("s-b-todo", "Todo")
            )]
        );
        assert!(a.popup.is_none());
        assert_eq!(linear_status(&a, "TBB-2"), "Todo");
    }

    #[test]
    fn a_team_without_canceled_says_so() {
        let mut a = linear_app();
        pick(&mut a, "TBB-1");
        a.handle_key(key('X'));
        assert_eq!(
            close_picker(&a).1,
            [
                "Close as completed (Done)".to_string(),
                "Close as canceled (none)".to_string()
            ]
        );
        a.handle_key(code(KeyCode::Down));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(a.status.flash.as_deref(), Some("TBB has no canceled state"));
        assert_eq!(a.pending_count(), 0);
        assert_eq!(linear_status(&a, "TBB-1"), "Todo");
        // The completed target of the same team still works.
        a.handle_key(key('X'));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(linear_status(&a, "TBB-1"), "Done");
    }

    #[test]
    fn linear_close_menu() {
        let mut a = linear_app();
        pick(&mut a, "TBB-1");
        a.handle_key(key('X'));
        let screen =
            crate::ui::fixtures::render_to_string(100, 20, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(screen.contains("Close as canceled (none)"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn linear_title_and_body_edits_send() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        a.edit_title();
        let (purpose, _) = text_form(&a);
        assert_eq!(purpose, &EditPurpose::Title(ItemId::new("i-tba-1")));
        a.on_text(
            EditPurpose::Title(ItemId::new("i-tba-1")),
            "A better title".into(),
            false,
        );
        let body = "Steps\n1. run\n".to_string();
        a.on_text(
            EditPurpose::Body(ItemId::new("i-tba-1")),
            body.clone(),
            false,
        );
        assert_eq!(
            pending_ops(&a),
            [
                Op::Title {
                    item: ItemId::new("i-tba-1"),
                    content: crate::model::ContentId::Issue("i-tba-1".into()),
                    title: "A better title".into(),
                },
                Op::Body {
                    item: ItemId::new("i-tba-1"),
                    content: crate::model::ContentId::Issue("i-tba-1".into()),
                    body: "Steps\n1. run".into(),
                },
            ]
        );
    }

    #[test]
    fn a_linear_comment_carries_a_client_id_and_r_resends_it() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        assert!(a.handle_key(key('c')).is_empty());
        type_in(&mut a, "looks good");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        let first = match &pending_ops(&a)[0] {
            Op::Comment {
                client_id, body, ..
            } => {
                assert_eq!(body, "looks good");
                client_id.clone().expect("a Linear comment has a client id")
            }
            other => panic!("expected a comment, got {other:?}"),
        };
        assert_eq!(first.len(), 36, "{first}");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(BackendError::Network("reset".into())),
        });
        assert_eq!(a.errors.len(), 1);
        assert!(!a.errors[0].may_exist, "safe to resend");
        assert!(!a.errors[0].message.contains(MAYBE_POSTED));
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        match &pending_ops(&a)[0] {
            Op::Comment { client_id, .. } => assert_eq!(client_id.as_deref(), Some(first.as_str())),
            other => panic!("expected a comment, got {other:?}"),
        }
    }

    #[test]
    fn archive_and_unarchive_on_linear() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        assert_eq!(a.handle_key(key('A')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::Archive {
                item: ItemId::new("i-tba-1")
            }]
        );
        // The archived card waits on the board; `A` on it unarchives.
        assert!(a.selected_item().unwrap().archived);
        assert_eq!(a.handle_key(key('A')), vec![Command::Send(2)]);
        assert_eq!(
            pending_ops(&a)[1],
            Op::Unarchive {
                item: ItemId::new("i-tba-1")
            }
        );
    }

    #[test]
    fn remove_asks_and_clears_the_project() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        assert!(a.confirm_remove().is_empty());
        assert_eq!(
            a.confirm.as_ref().unwrap().question,
            "Remove TBA-1 'Issue TBA-1' from the project? It stays in its team. (y/n)"
        );
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::Remove {
                item: ItemId::new("i-tba-1")
            }]
        );
    }

    /// The Linear table (All issues, manual) with `sortOrder`s 1, 2, 4, 8 for TBA-1, TBA-2,
    /// TBB-1, TBB-2, on its first row.
    fn linear_table(orders: [Option<f64>; 4]) -> App {
        let mut snap = linear_snapshot();
        for (id, order) in ["i-tba-1", "i-tba-2", "i-tbb-1", "i-tbb-2"]
            .iter()
            .zip(orders)
        {
            snap.items
                .get_mut(&ItemId::new(*id))
                .unwrap()
                .content_fields
                .sort_order = order;
        }
        let mut a = app_with(snap);
        a.select_view(&crate::model::ViewId::new("V_all"));
        a
    }

    fn reorder_op(item: &str, after: Option<&str>, sort_order: f64) -> Op {
        Op::Reorder {
            item: ItemId::new(item),
            after: after.map(ItemId::new),
            sort_order: Some(sort_order),
        }
    }

    #[test]
    fn j_and_k_on_linear_send_the_midpoint() {
        let mut a = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        assert_eq!(a.layout().0, Layout::Table);
        // J on TBA-1 lands between TBA-2 (2) and TBB-1 (4).
        assert_eq!(a.handle_key(key('J')), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [reorder_op("i-tba-1", Some("i-tba-2"), 3.0)]
        );
        // K on the last row, TBB-2, lands between TBA-2 (2) and TBB-1 (4).
        let mut b = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        for _ in 0..3 {
            b.handle_key(key('j'));
        }
        b.handle_key(key('K'));
        assert_eq!(
            pending_ops(&b),
            [reorder_op("i-tbb-2", Some("i-tba-2"), 3.0)]
        );
        // K on the second row goes to the top: the first issue's value minus 1.
        let mut c = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        c.handle_key(key('j'));
        c.handle_key(key('K'));
        assert_eq!(pending_ops(&c), [reorder_op("i-tba-2", None, 0.0)]);
        // J on the second to last goes to the bottom: the last value plus 1.
        let mut d = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        d.handle_key(key('j'));
        d.handle_key(key('j'));
        d.handle_key(key('J'));
        assert_eq!(
            pending_ops(&d),
            [reorder_op("i-tbb-1", Some("i-tbb-2"), 9.0)]
        );
        // The card moves at once.
        let order: Vec<String> = a
            .table_rows()
            .iter()
            .filter_map(|r| match r {
                Row::Item(i) => Some(i.ref_label()),
                Row::Group { .. } => None,
            })
            .collect();
        assert_eq!(order, ["TBA-2", "TBA-1", "TBB-1", "TBB-2"]);
        // The edges still say so.
        let mut e = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        assert!(e.handle_key(key('K')).is_empty());
        assert_eq!(e.status.flash.as_deref(), Some("Already first here"));
    }

    #[test]
    fn reorder_between_equal_neighbours_is_refused_and_sends_nothing() {
        let cant = Some("Can't place this card here; refresh with r");
        // Equal neighbours below TBA-1.
        let mut a = linear_table([Some(1.0), Some(5.0), Some(5.0), Some(8.0)]);
        assert!(a.handle_key(key('J')).is_empty());
        assert_eq!(a.status.flash.as_deref(), cant);
        assert!(a.store.pending().is_empty());
        // A neighbour without a sortOrder (those sort last): TBB-2 below TBB-1.
        let mut b = linear_table([Some(1.0), Some(2.0), Some(4.0), None]);
        b.handle_key(key('j'));
        b.handle_key(key('j'));
        assert!(b.handle_key(key('J')).is_empty());
        assert_eq!(b.status.flash.as_deref(), cant);
        assert!(b.store.pending().is_empty());
        // The item beyond the neighbour has none: TBA-2 passing TBB-1 would land before it.
        let mut d = linear_table([Some(1.0), Some(2.0), Some(4.0), None]);
        d.handle_key(key('j'));
        assert!(d.handle_key(key('J')).is_empty());
        assert_eq!(d.status.flash.as_deref(), cant);
        assert!(d.store.pending().is_empty());
        // Neighbours with nothing between them in f64.
        let near = 1.0_f64.next_up();
        let mut c = linear_table([Some(0.0), Some(1.0), Some(near), Some(8.0)]);
        assert!(c.handle_key(key('J')).is_empty());
        assert_eq!(c.status.flash.as_deref(), cant);
        assert!(c.store.pending().is_empty());
    }

    #[test]
    fn a_priority_sorted_linear_view_refuses_reorder() {
        let mut a = linear_table([Some(1.0), Some(2.0), Some(4.0), Some(8.0)]);
        let mut snap = linear_snapshot();
        snap.project.views[1].sort_by = vec![crate::model::SortSpec {
            field: FieldId::new(crate::linear::ids::PRIORITY),
            direction: crate::model::SortDirection::Desc,
        }];
        a.store = Box::new(crate::store::MemoryStore::new(Some(snap)));
        a.select_view(&crate::model::ViewId::new("V_all"));
        assert!(a.handle_key(key('J')).is_empty());
        let flash = a.status.flash.clone().unwrap_or_default();
        assert!(
            flash.starts_with("This view is sorted by ")
                && flash.ends_with("; reordering needs an unsorted view"),
            "{flash}"
        );
        assert!(a.store.pending().is_empty());
    }

    fn team_candidates(team: &str) -> crate::model::TeamCandidates {
        let user = |id: &str, name: &str| crate::model::UserRef {
            id: id.into(),
            login: name.into(),
        };
        let label = |id: &str, name: &str| crate::model::LabelRef {
            id: id.into(),
            name: name.into(),
            color: "5e6ad2".into(),
        };
        crate::model::TeamCandidates {
            team: team.into(),
            users: vec![user("u-1", "Ada Lovelace"), user("u-2", "Grace Hopper")],
            labels: vec![label("l-docs", "Docs"), label("l-ux", "UX")],
            truncated: false,
        }
    }

    /// `linear_app` with TBA-1 assigned to Ada Lovelace and labelled Docs (besides Bug, in
    /// the Type group).
    fn linear_app_assigned() -> App {
        let mut snap = linear_snapshot();
        let tba1 = snap.items.get_mut(&ItemId::new("i-tba-1")).unwrap();
        tba1.values.insert(
            FieldId::new(crate::linear::ids::ASSIGNEES),
            FieldValue::Users(vec!["Ada Lovelace".into()]),
        );
        tba1.content_fields
            .assignee_ids
            .insert("Ada Lovelace".into(), "u-1".into());
        tba1.values.insert(
            FieldId::new(crate::linear::ids::LABELS),
            FieldValue::Labels(vec![crate::model::Label {
                name: "Docs".into(),
                color: "5e6ad2".into(),
            }]),
        );
        tba1.content_fields
            .label_ids
            .insert("Docs".into(), "l-docs".into());
        app_with(snap)
    }

    fn team_loaded(a: &mut App, team: &str) {
        a.on_sync(SyncEvent::TeamCandidates {
            team: team.into(),
            candidates: team_candidates(team),
        });
    }

    fn assignee_select(a: &App) -> &crate::ui::multiselect::MultiSelect<crate::model::UserRef> {
        match &a.popup {
            Some(Popup::Assignees { select, .. }) => select,
            other => panic!("expected the assignee popover, got {other:?}"),
        }
    }

    fn checked_labels<T: Clone>(m: &crate::ui::multiselect::MultiSelect<T>) -> Vec<(String, bool)> {
        m.options
            .iter()
            .zip(&m.checked)
            .map(|(o, c)| (o.label.clone(), *c))
            .collect()
    }

    fn user(id: &str, name: &str) -> crate::model::UserRef {
        crate::model::UserRef {
            id: id.into(),
            login: name.into(),
        }
    }

    #[test]
    fn linear_assignee_popup_ticks_one_and_sends_assignee_id() {
        let mut a = linear_app_assigned();
        pick(&mut a, "TBA-2");
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadTeamCandidates(TBA_ID.into())]
        );
        assert!(matches!(
            &a.popup,
            Some(Popup::Assignees { source: CandidateSource::Team(t), select, .. })
                if t == TBA_ID && select.loading
        ));
        assert_eq!(assignee_select(&a).title, "Assignee · TBA-2");
        team_loaded(&mut a, TBA_ID);
        a.handle_key(key(' '));
        a.handle_key(code(KeyCode::Down));
        a.handle_key(key(' '));
        assert_eq!(
            checked_labels(assignee_select(&a)),
            [
                ("Ada Lovelace".to_string(), false),
                ("Grace Hopper".to_string(), true)
            ],
            "ticking Grace unticks Ada"
        );
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::Assignees {
                item: ItemId::new("i-tba-2"),
                content: crate::model::ContentId::Issue("i-tba-2".into()),
                add: vec![user("u-2", "Grace Hopper")],
                remove: vec![],
                all: vec![user("u-2", "Grace Hopper")],
            }]
        );
        // Switching TBA-1 from Ada to Grace adds one and removes the other.
        pick(&mut a, "TBA-1");
        assert!(
            a.handle_key(key('a')).is_empty(),
            "TBA's members are loaded"
        );
        assert_eq!(
            checked_labels(assignee_select(&a)),
            [
                ("Ada Lovelace".to_string(), true),
                ("Grace Hopper".to_string(), false)
            ]
        );
        a.handle_key(code(KeyCode::Down));
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(2)]);
        assert_eq!(
            pending_ops(&a)[1],
            Op::Assignees {
                item: ItemId::new("i-tba-1"),
                content: crate::model::ContentId::Issue("i-tba-1".into()),
                add: vec![user("u-2", "Grace Hopper")],
                remove: vec![user("u-1", "Ada Lovelace")],
                all: vec![user("u-2", "Grace Hopper")],
            }
        );
    }

    #[test]
    fn unassigning_sends_none() {
        let mut a = linear_app_assigned();
        team_loaded(&mut a, TBA_ID);
        pick(&mut a, "TBA-1");
        assert!(a.handle_key(key('a')).is_empty());
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::Assignees {
                item: ItemId::new("i-tba-1"),
                content: crate::model::ContentId::Issue("i-tba-1".into()),
                add: vec![],
                remove: vec![user("u-1", "Ada Lovelace")],
                all: vec![],
            }]
        );
    }

    #[test]
    fn team_candidates_load_once_per_team_and_r_forgets_them() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadTeamCandidates(TBA_ID.into())]
        );
        a.handle_key(code(KeyCode::Esc));
        pick(&mut a, "TBA-2");
        assert!(a.handle_key(key('t')).is_empty(), "TBA's are loading");
        team_loaded(&mut a, TBA_ID);
        assert!(matches!(&a.popup, Some(Popup::Labels { select, .. }) if !select.loading));
        a.handle_key(code(KeyCode::Esc));
        pick(&mut a, "TBB-1");
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadTeamCandidates(TBB_ID.into())],
            "another team, another list"
        );
        a.handle_key(code(KeyCode::Esc));
        a.handle_key(key('r'));
        pick(&mut a, "TBA-1");
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadTeamCandidates(TBA_ID.into())],
            "r forgot them"
        );
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::TeamCandidates(TBA_ID.into()),
            error: BackendError::Network("offline".into()),
        });
        let select = assignee_select(&a);
        assert!(!select.loading);
        let error = select.error.clone().unwrap_or_default();
        assert!(
            error.starts_with("could not load the team's members and labels")
                && error.contains("offline"),
            "{error}"
        );
        assert!(a.candidates.is_empty(), "no repository was asked");
    }

    #[test]
    fn linear_labels_leave_out_group_labels() {
        let mut a = linear_app_assigned();
        team_loaded(&mut a, TBA_ID);
        pick(&mut a, "TBA-1");
        assert!(a.handle_key(key('t')).is_empty());
        let Some(Popup::Labels { select, .. }) = &a.popup else {
            panic!("labels")
        };
        assert_eq!(
            checked_labels(select),
            [("Docs".to_string(), true), ("UX".to_string(), false)],
            "Bug is the Type field's, not a label to tick"
        );
        assert_eq!(select.options[0].color.as_deref(), Some("5e6ad2"));
        assert_eq!(select.page, 250, "a cut list says Linear's page size");
        a.handle_key(code(KeyCode::Down));
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            pending_ops(&a),
            [Op::Labels {
                item: ItemId::new("i-tba-1"),
                content: crate::model::ContentId::Issue("i-tba-1".into()),
                add: vec![crate::model::LabelRef {
                    id: "l-ux".into(),
                    name: "UX".into(),
                    color: "5e6ad2".into(),
                }],
                remove: vec![],
            }]
        );
    }

    #[test]
    fn a_linear_issue_of_an_unknown_team_needs_a_refresh() {
        let mut snap = linear_snapshot();
        snap.project.linear.as_mut().unwrap().teams.clear();
        let mut a = app_with(snap);
        pick(&mut a, "TBA-1");
        assert!(a.handle_key(key('a')).is_empty());
        assert_eq!(a.popup, None);
        assert_eq!(
            a.status.flash,
            Some(a.denial_text(crate::model::caps::Denial::NeedsRefresh))
        );
    }

    #[test]
    fn linear_assignee_popover() {
        let mut a = linear_app_assigned();
        team_loaded(&mut a, TBA_ID);
        pick(&mut a, "TBA-1");
        a.handle_key(key('a'));
        let screen =
            crate::ui::fixtures::render_to_string(100, 20, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(screen.contains("[x] Ada Lovelace"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    // New Linear issues (Task 14, spec section 9, T2).

    fn tba() -> crate::model::TeamMeta {
        crate::model::TeamMeta {
            id: TBA_ID.into(),
            key: "TBA".into(),
        }
    }

    fn tbb() -> crate::model::TeamMeta {
        crate::model::TeamMeta {
            id: TBB_ID.into(),
            key: "TBB".into(),
        }
    }

    fn new_kind_labels(a: &App) -> Vec<String> {
        match &a.popup {
            Some(Popup::NewKind { picker, .. }) => {
                picker.choices.iter().map(|c| c.label.clone()).collect()
            }
            other => panic!("expected the new-item menu, got {other:?}"),
        }
    }

    /// `n`, then "New issue" (the first entry).
    fn new_linear_issue(a: &mut App) -> Vec<Command> {
        assert!(a.handle_key(key('n')).is_empty());
        assert_eq!(new_kind_labels(a)[0], "New issue");
        a.handle_key(code(KeyCode::Enter))
    }

    /// `linear_snapshot` as the team board of TBA.
    fn linear_team_app() -> App {
        let mut snap = linear_snapshot();
        snap.project.board = format!("linear:team:{TBA_ID}").parse().unwrap();
        let meta = snap.project.linear.as_mut().unwrap();
        meta.teams = vec![tba()];
        meta.team_keys = vec!["TBA".into()];
        snap.items.retain(|_, i| i.ref_label().starts_with("TBA"));
        snap.order.retain(|id| id.as_str().starts_with("i-tba"));
        app_with(snap)
    }

    #[test]
    fn n_on_linear_offers_new_and_on_project_boards_add_existing() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        a.handle_key(key('n'));
        assert_eq!(
            new_kind_labels(&a),
            ["New issue", "Add existing issue (key or URL)"]
        );
        let mut t = linear_team_app();
        pick(&mut t, "TBA-1");
        t.handle_key(key('n'));
        assert_eq!(new_kind_labels(&t), ["New issue"], "never a draft");
        // A team board's issue is its team's, with no team line.
        assert!(t.handle_key(code(KeyCode::Enter)).is_empty());
        let (purpose, form) = text_form(&t);
        assert!(
            matches!(purpose, EditPurpose::NewLinearIssue { team: Some(team), .. } if *team == tba()),
            "{purpose:?}"
        );
        assert_eq!(form.heading(), "New issue in TBA");
        assert_eq!(form.team(), None, "no team line");
    }

    #[test]
    fn a_single_team_column_needs_no_team_line() {
        let mut a = linear_app();
        pick(&mut a, "TBA-2");
        assert!(new_linear_issue(&mut a).is_empty());
        let (purpose, form) = text_form(&a);
        assert_eq!(
            purpose,
            &EditPurpose::NewLinearIssue {
                team: Some(tba()),
                presets: vec![(
                    FieldId::new(crate::linear::ids::STATUS),
                    select_value("s-a-prog", "In Progress")
                )],
            }
        );
        assert_eq!(form.heading(), "New issue in TBA");
        assert_eq!(form.team(), None);
        // In Review is TBB's.
        a.handle_key(code(KeyCode::Esc));
        pick(&mut a, "TBB-2");
        new_linear_issue(&mut a);
        assert_eq!(text_form(&a).1.heading(), "New issue in TBB");
    }

    #[test]
    fn a_merged_column_asks_for_the_team_prefilled_with_the_last() {
        let mut a = linear_app();
        pick(&mut a, "TBB-1");
        new_linear_issue(&mut a);
        let (purpose, form) = text_form(&a);
        assert!(
            matches!(purpose, EditPurpose::NewLinearIssue { team: None, .. }),
            "{purpose:?}"
        );
        assert_eq!(form.teams(), [tba(), tbb()]);
        assert_eq!(
            form.team(),
            Some(&tba()),
            "the first team, before any choice"
        );
        assert_eq!(form.heading(), "New issue in TBA");
        // Shift+Tab reaches the team line; Enter opens the picker of the teams.
        a.handle_key(code(KeyCode::BackTab));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let Some(Popup::TeamPick { picker, .. }) = &a.popup else {
            panic!("expected the team picker, got {:?}", a.popup)
        };
        let keys: Vec<&str> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(keys, ["TBA", "TBB"]);
        // Esc goes back to the form unchanged.
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(text_form(&a).1.team(), Some(&tba()));
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(code(KeyCode::Down));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let form = text_form(&a).1;
        assert_eq!(form.team(), Some(&tbb()));
        assert_eq!(
            form.heading(),
            "New issue in TBB",
            "the heading follows the team"
        );
        a.handle_key(code(KeyCode::Tab));
        type_in(&mut a, "Write the docs");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert_eq!(a.last_team, Some(tbb()));
        // The next new issue in a merged column starts with TBB.
        pick(&mut a, "TBA-1");
        new_linear_issue(&mut a);
        let form = text_form(&a).1;
        assert_eq!(form.team(), Some(&tbb()));
        assert_eq!(form.heading(), "New issue in TBB");
    }

    #[test]
    fn teams_without_the_columns_state_are_not_offered() {
        // TBC has no states at all, so no Todo.
        let mut snap = linear_snapshot();
        let meta = snap.project.linear.as_mut().unwrap();
        meta.teams.push(crate::model::TeamMeta {
            id: "team-c".into(),
            key: "TBC".into(),
        });
        // Swimlanes by Type, TBA's label group; TBB-2 carries Bug too.
        let board = crate::model::ViewId::new("V_board");
        let view = snap
            .project
            .views
            .iter_mut()
            .find(|v| v.id == board)
            .unwrap();
        view.group_by = vec![crate::linear::ids::group(TYPE_GROUP)];
        snap.items
            .get_mut(&ItemId::new("i-tbb-2"))
            .unwrap()
            .values
            .insert(
                crate::linear::ids::group(TYPE_GROUP),
                select_value("l-bug", "Bug"),
            );
        let mut a = app_with(snap.clone());
        pick(&mut a, "TBB-1");
        new_linear_issue(&mut a);
        assert_eq!(text_form(&a).1.teams(), [tba(), tbb()], "not TBC");
        a.handle_key(code(KeyCode::Esc));
        // Todo in the Bug lane: only TBA owns the Type group.
        pick(&mut a, "TBA-1");
        new_linear_issue(&mut a);
        assert_eq!(text_form(&a).1.teams(), [tba()]);
        a.handle_key(code(KeyCode::Esc));
        // In Review is TBB's, and TBB cannot take Bug: nothing opens.
        pick(&mut a, "TBB-2");
        assert!(new_linear_issue(&mut a).is_empty());
        assert_eq!(a.popup, None);
        assert_eq!(
            a.status.flash.as_deref(),
            Some("\"Type\" belongs to team TBA")
        );
        // A board cached by 0.4 knows no teams: refresh rather than guess.
        let mut old = snap;
        old.project.linear.as_mut().unwrap().teams.clear();
        let mut a = app_with(old);
        pick(&mut a, "TBA-2");
        assert!(new_linear_issue(&mut a).is_empty());
        assert_eq!(a.popup, None);
        assert_eq!(
            a.status.flash,
            Some(a.denial_text(crate::model::caps::Denial::NeedsRefresh))
        );
    }

    #[test]
    fn a_new_linear_issue_carries_an_id_team_and_resolved_presets() {
        let mut a = linear_app();
        // Todo is merged; its canonical option is TBB's, and TBA is the prefilled team.
        pick(&mut a, "TBB-1");
        new_linear_issue(&mut a);
        type_in(&mut a, "Write the docs");
        a.handle_key(code(KeyCode::Enter));
        type_in(&mut a, "Steps");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert_eq!(a.popup, None);
        let ops = pending_ops(&a);
        let Op::Create {
            temp,
            new:
                NewItem::LinearIssue {
                    id,
                    team,
                    title,
                    body,
                },
            presets,
        } = &ops[0]
        else {
            panic!("expected a new Linear issue, got {ops:?}")
        };
        assert_eq!(temp, &ItemId::new("local:1"));
        assert_eq!(team, &tba());
        assert_eq!((title.as_str(), body.as_str()), ("Write the docs", "Steps"));
        assert_eq!(
            presets,
            &[(
                FieldId::new(crate::linear::ids::STATUS),
                select_value("s-a-todo", "Todo")
            )],
            "TBA's Todo, not the column's canonical (TBB's) option"
        );
        // A UUID v4.
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(id.chars().nth(14), Some('4'), "{id}");
        assert_eq!(a.last_team, Some(tba()));
        // The placeholder shows at once, selected, in Todo.
        let placeholder = a.selected_item().unwrap();
        assert_eq!(placeholder.id, ItemId::new("local:1"));
        assert_eq!(linear_status(&a, &placeholder.ref_label()), "Todo");
        // Each new issue has an id of its own.
        pick(&mut a, "TBA-2");
        new_linear_issue(&mut a);
        type_in(&mut a, "Another");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(2)]);
        let ids: Vec<String> = pending_ops(&a)
            .iter()
            .filter_map(|op| match op {
                Op::Create {
                    new: NewItem::LinearIssue { id, .. },
                    ..
                } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    /// Review Focus 2: a Linear create whose answer was lost is safe to resend, and `R`
    /// resends it with the same id, so Linear makes it at most once.
    #[test]
    fn r_resends_a_lost_linear_create() {
        let mut a = linear_app();
        pick(&mut a, "TBA-2");
        new_linear_issue(&mut a);
        type_in(&mut a, "Write the docs");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        let first = pending_ops(&a)[0].clone();
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Err(BackendError::Network("reset".into())),
        });
        assert_eq!(a.errors.len(), 1);
        assert!(!a.errors[0].may_exist, "safe to resend");
        assert!(!a.errors[0].message.contains(MAYBE_CREATED));
        a.handle_key(key('E'));
        assert_eq!(a.handle_key(key('R')), vec![Command::Send(2)]);
        assert_eq!(pending_ops(&a), [first], "the same op, the same id");
        assert!(a.errors.is_empty());
    }

    #[test]
    fn linear_new_issue_form_with_team() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        new_linear_issue(&mut a);
        type_in(&mut a, "Write the docs");
        let screen =
            crate::ui::fixtures::render_to_string(100, 24, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(screen.contains("New issue in TBA"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    // Task 15: adding an existing Linear issue by key or URL.

    /// `linear_project`'s Linear project id.
    const BOARD_PROJECT: &str = "6f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";

    /// The issue `TBA-12`, Todo, not on the board, in the project `project` (id, name).
    fn found_issue(project: Option<(&str, &str)>) -> crate::model::Item {
        let mut issue = linear_issue("TBA", 12, ("s-a-todo", "Todo"));
        if let Some((id, name)) = project {
            issue.values.insert(
                FieldId::new(crate::linear::ids::PROJECT),
                select_value(id, name),
            );
        }
        issue
    }

    /// `n`, "Add existing issue (key or URL)", then `text` and Enter.
    fn add_existing_linear(a: &mut App, text: &str) -> Vec<Command> {
        assert!(a.handle_key(key('n')).is_empty());
        assert_eq!(new_kind_labels(a)[1], "Add existing issue (key or URL)");
        a.handle_key(code(KeyCode::Down));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert!(
            matches!(&a.popup, Some(Popup::IssueKey { input, loading: false, error: None, .. }) if input.is_empty()),
            "{:?}",
            a.popup
        );
        assert!(type_in(a, text).is_empty());
        a.handle_key(code(KeyCode::Enter))
    }

    fn issue_key_error(a: &App) -> Option<String> {
        match &a.popup {
            Some(Popup::IssueKey { error, .. }) => error.clone(),
            other => panic!("expected the issue key popup, got {other:?}"),
        }
    }

    fn issue_key_loading(a: &App) -> bool {
        matches!(&a.popup, Some(Popup::IssueKey { loading: true, .. }))
    }

    #[test]
    fn an_unknown_key_says_so() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        // Nothing to look up in text that is no key or link.
        assert!(add_existing_linear(&mut a, "crash on start").is_empty());
        assert_eq!(issue_key_error(&a).as_deref(), Some(NOT_AN_ISSUE_REF));
        assert!(!issue_key_loading(&a));
        // Typing clears the error.
        for _ in 0.."crash on start".len() {
            a.handle_key(code(KeyCode::Backspace));
        }
        assert_eq!(issue_key_error(&a), None);
        type_in(&mut a, "tba-99");
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LookupIssue("TBA-99".into())]
        );
        assert!(issue_key_loading(&a));
        // Enter again while it looks up asks nothing more.
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let screen =
            crate::ui::fixtures::render_to_string(100, 24, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(screen.contains("Add existing issue"), "{screen}");
        assert!(screen.contains("Looking up…"), "{screen}");
        // An answer for another key is not this popup's.
        assert!(
            a.on_sync(SyncEvent::IssueFound {
                key: "TBA-98".into(),
                item: None,
            })
            .is_empty()
        );
        assert!(issue_key_loading(&a));
        assert!(
            a.on_sync(SyncEvent::IssueFound {
                key: "TBA-99".into(),
                item: None,
            })
            .is_empty()
        );
        assert_eq!(issue_key_error(&a).as_deref(), Some("No issue TBA-99"));
        assert!(!issue_key_loading(&a));
        assert!(pending_ops(&a).is_empty());
        let screen =
            crate::ui::fixtures::render_to_string(100, 24, |f| crate::ui::chrome::draw(f, &mut a));
        assert!(screen.contains("No issue TBA-99"), "{screen}");
        assert!(screen.contains("TBA-12 or a linear.app link"), "{screen}");
        // A lookup that fails says why, in the popup.
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LookupIssue("TBA-99".into())]
        );
        a.on_sync(SyncEvent::Failed {
            task: SyncTask::IssueByKey("TBA-99".into()),
            error: BackendError::Network("offline".into()),
        });
        let error = issue_key_error(&a).unwrap();
        assert!(
            error.starts_with("could not look up the issue: "),
            "{error}"
        );
        assert_eq!(
            a.status.error, None,
            "said in the popup, not the status line"
        );
        // Esc goes back to the new-item menu, with the same presets.
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(
            new_kind_labels(&a),
            ["New issue", "Add existing issue (key or URL)"]
        );
    }

    #[test]
    fn an_issue_on_the_board_is_selected_not_added() {
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        assert_eq!(
            add_existing_linear(&mut a, "https://linear.app/acme/issue/TBB-2/review-it"),
            vec![Command::LookupIssue("TBB-2".into())]
        );
        let on_board = a.item(&ItemId::new("i-tbb-2")).unwrap().clone();
        assert!(
            a.on_sync(SyncEvent::IssueFound {
                key: "TBB-2".into(),
                item: Some(on_board),
            })
            .is_empty()
        );
        assert_eq!(a.popup, None);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(
            a.status.flash.as_deref(),
            Some("TBB-2 is already on the board")
        );
        assert_eq!(
            a.selected_item().map(|i| i.ref_label()),
            Some("TBB-2".to_string())
        );
        assert!(pending_ops(&a).is_empty());
    }

    #[test]
    fn an_issue_in_another_project_asks_first() {
        let mut a = linear_app();
        // Todo: the merged column, whose canonical option is TBB's.
        pick(&mut a, "TBB-1");
        add_existing_linear(&mut a, "TBA-12");
        let found = found_issue(Some(("p-billing", "Billing")));
        assert!(
            a.on_sync(SyncEvent::IssueFound {
                key: "TBA-12".into(),
                item: Some(found.clone()),
            })
            .is_empty()
        );
        assert_eq!(a.popup, None);
        assert_eq!(a.mode, Mode::Confirm);
        assert_eq!(
            a.confirm.as_ref().map(|c| c.question.as_str()),
            Some("TBA-12 is in \"Billing\"; move it here? (y/n)")
        );
        assert!(pending_ops(&a).is_empty(), "nothing is sent before a yes");
        // `n` sends nothing.
        assert!(a.handle_key(key('n')).is_empty());
        assert_eq!(a.mode, Mode::Normal);
        assert!(pending_ops(&a).is_empty());
        // `y` adds it, with the column's status for TBA.
        let mut a = linear_app();
        pick(&mut a, "TBB-1");
        add_existing_linear(&mut a, "TBA-12");
        a.on_sync(SyncEvent::IssueFound {
            key: "TBA-12".into(),
            item: Some(found.clone()),
        });
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(
            pending_ops(&a),
            [Op::Create {
                temp: ItemId::new("local:1"),
                new: NewItem::LinearExisting {
                    issue: Box::new(found)
                },
                presets: vec![(
                    FieldId::new(crate::linear::ids::STATUS),
                    select_value("s-a-todo", "Todo")
                )],
            }]
        );
        // The placeholder shows at once, selected.
        assert_eq!(
            a.selected_item().map(|i| i.id.clone()),
            Some(ItemId::new("local:1"))
        );
        // An issue in this board's project, or in none, is added without a question.
        for project in [Some((BOARD_PROJECT, "Launch")), None] {
            let mut a = linear_app();
            pick(&mut a, "TBA-2");
            add_existing_linear(&mut a, "TBA-12");
            assert_eq!(
                a.on_sync(SyncEvent::IssueFound {
                    key: "TBA-12".into(),
                    item: Some(found_issue(project)),
                }),
                vec![Command::Send(1)],
                "{project:?}"
            );
            assert_eq!(a.mode, Mode::Normal);
            assert_eq!(a.popup, None);
            assert_eq!(a.status.flash, None, "no preset was left out");
        }
    }

    #[test]
    fn presets_the_team_lacks_are_left_out_and_named() {
        let mut a = linear_app();
        // In Review is TBB's only.
        pick(&mut a, "TBB-2");
        add_existing_linear(&mut a, "TBA-12");
        let found = found_issue(None);
        assert_eq!(
            a.on_sync(SyncEvent::IssueFound {
                key: "TBA-12".into(),
                item: Some(found.clone()),
            }),
            vec![Command::Send(1)],
            "the add still happens"
        );
        assert_eq!(
            pending_ops(&a),
            [Op::Create {
                temp: ItemId::new("local:1"),
                new: NewItem::LinearExisting {
                    issue: Box::new(found.clone())
                },
                presets: vec![],
            }]
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("TBA has no \"In Review\" state; adding TBA-12 without it")
        );
        // After a yes, too.
        let mut a = linear_app();
        pick(&mut a, "TBB-2");
        add_existing_linear(&mut a, "TBA-12");
        a.on_sync(SyncEvent::IssueFound {
            key: "TBA-12".into(),
            item: Some(found_issue(Some(("p-billing", "Billing")))),
        });
        assert_eq!(a.mode, Mode::Confirm);
        assert_eq!(a.handle_key(key('y')), vec![Command::Send(1)]);
        assert_eq!(
            a.status.flash.as_deref(),
            Some("TBA has no \"In Review\" state; adding TBA-12 without it")
        );
    }

    #[test]
    fn team_boards_have_no_add_existing() {
        let mut t = linear_team_app();
        pick(&mut t, "TBA-1");
        t.handle_key(key('n'));
        assert_eq!(new_kind_labels(&t), ["New issue"]);
        assert!(
            !new_kind_labels(&t).iter().any(|l| l.contains("existing")),
            "a team board holds every issue of its team already"
        );
        // A project board offers it.
        let mut a = linear_app();
        pick(&mut a, "TBA-1");
        a.handle_key(key('n'));
        assert!(new_kind_labels(&a).contains(&"Add existing issue (key or URL)".to_string()));
    }
}
