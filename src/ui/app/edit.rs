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
                    for scope in missing {
                        if !self.refused_scopes.contains(scope) {
                            self.refused_scopes.push(scope.clone());
                        }
                    }
                }
                let message = error_message(&error);
                self.status.error = Some(format!("Could not {what}: {message}"));
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
}
