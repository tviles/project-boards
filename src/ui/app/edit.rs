//! Editing: every op goes through the capability matrix, joins the store's pending list, and
//! settles when GitHub answers. Failed edits go to the error log.

use super::*;
use crate::model::{Denial, EditContext, OpKind, check};
use crate::ops::{Confirmed, Op, OpId};
use crate::store::BoardSnapshot;
use crate::ui::marks::ItemMarks;
use crate::ui::table::Row;
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
pub fn error_message(error: &GithubError) -> String {
    crate::ui::text::sanitize(&raw_error_message(error))
}

fn raw_error_message(error: &GithubError) -> String {
    match error {
        GithubError::Graphql(entries) => {
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
    Action::EditField,
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

/// The single-action keys the hints list before move and reorder.
const HINTED: &[Action] = &[Action::EditField];

impl App {
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

    /// The edit keys the item allows, for the status line: single-key edits first (`e field`),
    /// then `< > move · J K reorder · space actions`. `None` with no item or nothing allowed.
    pub fn edit_hints(&self) -> Option<String> {
        let item = self.action_item()?;
        let ctx = self.edit_context();
        if crate::model::capabilities(item, &ctx).is_empty() {
            return None;
        }
        let key = |a: Action| self.keymap.keys_for(a).first().map(|k| k.label());
        let mut parts = Vec::new();
        for &action in HINTED {
            if let (Some(kind), Some(k)) = (self.allowed_kind(action, item), key(action)) {
                parts.push(format!("{k} {}", hint_word(action, kind)));
            }
        }
        if self.check_here(OpKind::Move, item).is_ok()
            && let (Some(p), Some(n)) = (key(Action::MovePrevColumn), key(Action::MoveNextColumn))
        {
            parts.push(format!("{p} {n} move"));
        }
        if self.check_here(OpKind::Reorder, item).is_ok()
            && let (Some(d), Some(u)) = (key(Action::ReorderDown), key(Action::ReorderUp))
        {
            parts.push(format!("{d} {u} reorder"));
        }
        if let Some(k) = key(Action::ActionMenu) {
            parts.push(format!("{k} actions"));
        }
        Some(parts.join(" · "))
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
            unmatched: self
                .view_items()
                .unwrap_or_default()
                .into_iter()
                .filter(|i| self.unmatched(i))
                .map(|i| i.id.clone())
                .collect(),
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
        if self.detail.is_some() {
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
            self.status.flash = Some("That item is no longer on the board".into());
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
        vec![Command::Send(id)]
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
            Action::EditField => self.open_field_menu(),
            Action::ActionMenu => self.open_action_menu(),
            Action::ErrorLog => self.open_error_log(),
            _ => return None,
        };
        Some(cmds)
    }

    /// `check`, plus what the current view rules out: moving needs a column field, and
    /// reordering needs an unsorted view (GitHub's rule). Every edit key, the action menu and
    /// the hints ask this, so the UI never offers what it would then refuse. The reason is
    /// the one-line message to show.
    pub(crate) fn check_here(&self, kind: OpKind, item: &Item) -> Result<(), String> {
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
    /// without it), shows GitHub's message and goes to the error log.
    pub(crate) fn settle(&mut self, id: OpId, result: Result<Confirmed, GithubError>) {
        let kind = self.pending_kinds.remove(&id);
        match result {
            Ok(confirmed) => {
                self.store.settle_ok(id, confirmed);
            }
            Err(error) => {
                let what = self
                    .store
                    .pending()
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.op.describe(self.snapshot()))
                    .unwrap_or_else(|| "edit".into());
                let Some(op) = self.store.settle_failed(id) else {
                    return;
                };
                if let GithubError::InsufficientScopes { missing } = &error {
                    for scope in missing {
                        if !self.refused_scopes.contains(scope) {
                            self.refused_scopes.push(scope.clone());
                        }
                    }
                }
                let message = error_message(&error);
                let hint = self
                    .keymap
                    .keys_for(Action::ErrorLog)
                    .first()
                    .map(|k| format!(" · {} error log", k.label()))
                    .unwrap_or_default();
                self.status.error = Some(format!("Could not {what}: {message}{hint}"));
                // The newest failure is row 0: while the log is open, keep the highlight on
                // the same edit.
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
                });
                let extra = self.errors.len().saturating_sub(MAX_ERRORS);
                self.errors.drain(..extra);
                self.error_sel = self.error_sel.min(self.errors.len() - 1);
            }
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
    /// open. When the matrix refuses, the entry stays and the reason shows.
    fn retry_selected(&mut self) -> Vec<Command> {
        let Some(index) = self.errors.len().checked_sub(1 + self.error_sel) else {
            return Vec::new();
        };
        let failed = self.errors.remove(index);
        let cmds = self.submit(failed.kind, failed.op.clone());
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
        let error = GithubError::Graphql(vec![GraphqlErrorEntry {
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
        let error = GithubError::InsufficientScopes {
            missing: vec!["project".into()],
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

    fn refused(scope: &str) -> GithubError {
        GithubError::InsufficientScopes {
            missing: vec![scope.into()],
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
            result: Err(GithubError::Unauthorized),
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
            result: Err(GithubError::Unauthorized),
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
        let error = GithubError::Graphql(vec![GraphqlErrorEntry {
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
                Err(GithubError::Unauthorized)
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
        let none = GithubError::Graphql(vec![]);
        assert_eq!(error_message(&none), "GitHub returned an error");
        let blank = GithubError::Graphql(vec![GraphqlErrorEntry {
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
        let error = GithubError::Graphql(vec![GraphqlErrorEntry {
            message: message.into(),
            kind: None,
        }]);
        a.on_sync(SyncEvent::OpDone {
            id,
            result: Err(error),
        });
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
        let error = GithubError::Graphql(vec![GraphqlErrorEntry {
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
}
