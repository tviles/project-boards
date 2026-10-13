//! Editing: every op goes through the capability matrix, joins the store's pending list, and
//! settles when GitHub answers. Failed edits go to the error log.

use super::*;
use crate::model::{Denial, EditContext, OpKind, check};
use crate::ops::{Confirmed, Op, OpId};
use std::time::Instant;

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

/// GitHub's own words for a failure; other errors as their message.
pub fn error_message(error: &GithubError) -> String {
    match error {
        GithubError::Graphql(entries) if !entries.is_empty() => entries
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

impl App {
    pub fn edit_context(&self) -> EditContext {
        EditContext {
            viewer_can_update: self.project().is_some_and(|p| p.viewer_can_update),
            scopes: self.scopes.clone(),
            refused: self.refused_scopes.clone(),
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
    // Edit keys (later tasks) call this; until then only the tests do.
    #[allow(dead_code)]
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
        let id = self.store.begin(op);
        self.pending_kinds.insert(id, kind);
        self.restore_selection();
        vec![Command::Send(id)]
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
                    self.refused_scopes.extend(missing.iter().cloned());
                }
                let message = error_message(&error);
                self.status.error = Some(format!("Could not {what}: {message}"));
                self.errors.push(FailedEdit {
                    kind: kind.unwrap_or(OpKind::SetField),
                    op,
                    what,
                    message,
                    at: Instant::now(),
                });
            }
        }
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
            Some("Could not set Status to Done on 'Fix crash': The field is archived")
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
}
