//! The popovers: what each one edits and what its keys do.

use super::*;
use crate::model::{
    FieldValue, Iteration, OptionId, format_iso_date, parse_date_input, parse_iso_date,
    parse_number_input,
};
use crate::ops::Op;
use crate::ui::multiselect::{MultiOption, MultiOutcome, MultiSelect};
use crate::ui::popup::{Choice, LineOutcome, ListOutcome, ListPicker, edit_line};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Text,
    Number,
    Date,
}

/// What the `e` menu can open.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldTarget {
    Field(FieldId),
}

/// An entry of the action menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    /// Runs the action, as its key would.
    Key(Action),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    /// Space: every operation the item allows, with its key.
    Actions {
        item: ItemId,
        picker: ListPicker<MenuEntry>,
    },
    /// `e`: which field of `item` to edit.
    Fields {
        item: ItemId,
        picker: ListPicker<FieldTarget>,
    },
    /// A single-select or iteration value; `None` clears it.
    Value {
        item: ItemId,
        field: FieldId,
        picker: ListPicker<Option<FieldValue>>,
    },
    /// A multi-select field.
    Options {
        item: ItemId,
        field: FieldId,
        select: MultiSelect<OptionId>,
    },
    /// A text, number or date field.
    Line {
        item: ItemId,
        field: FieldId,
        kind: LineKind,
        title: String,
        input: String,
        error: Option<String>,
    },
}

fn editable(kind: &FieldKind) -> bool {
    matches!(
        kind,
        FieldKind::Text
            | FieldKind::Number
            | FieldKind::Date
            | FieldKind::SingleSelect { .. }
            | FieldKind::MultiSelect { .. }
            | FieldKind::Iteration { .. }
    )
}

/// The iterations to pick from: the current one, then the others by start date, then the
/// completed ones newest first; the item's own value is marked `✓`, and "Clear" comes last
/// when there is a value to clear.
pub(crate) fn iteration_choices(
    field: &Field,
    current: Option<&FieldValue>,
    today: time::Date,
) -> Vec<Choice<Option<FieldValue>>> {
    let FieldKind::Iteration {
        iterations,
        completed,
    } = &field.kind
    else {
        return Vec::new();
    };
    let set = current.and_then(|v| v.bucket_key());
    let is_now = |i: &Iteration| {
        parse_iso_date(&i.start_date).is_some_and(|start| {
            start <= today && today < start + time::Duration::days(i.duration_days.into())
        })
    };
    let mut open: Vec<&Iteration> = iterations.iter().collect();
    open.sort_by(|a, b| a.start_date.cmp(&b.start_date));
    let (now, later): (Vec<&Iteration>, Vec<&Iteration>) =
        open.into_iter().partition(|i| is_now(i));
    let mut done: Vec<&Iteration> = completed.iter().collect();
    done.sort_by(|a, b| b.start_date.cmp(&a.start_date));
    let choice = |i: &Iteration, detail: &str| {
        let detail = match (set.as_deref() == Some(i.id.as_str()), detail) {
            (true, "") => "✓".to_string(),
            (true, d) => format!("✓ {d}"),
            (false, d) => d.to_string(),
        };
        Choice::new(
            i.title.clone(),
            Some(FieldValue::Iteration {
                iteration_id: i.id.clone(),
                title: i.title.clone(),
                start_date: i.start_date.clone(),
            }),
        )
        .with_detail(detail)
    };
    let mut out: Vec<_> = now.into_iter().map(|i| choice(i, "current")).collect();
    out.extend(later.into_iter().map(|i| choice(i, "")));
    out.extend(done.into_iter().map(|i| choice(i, "completed")));
    if current.is_some() {
        out.push(Choice::new(format!("Clear {}", field.name), None));
    }
    out
}

/// The date at `offset` when it is `now`.
fn local_date(now: time::OffsetDateTime, offset: time::UtcOffset) -> time::Date {
    now.to_offset(offset).date()
}

/// A value picker with the cursor on the row that marks the current value.
fn value_picker(
    title: String,
    choices: Vec<Choice<Option<FieldValue>>>,
) -> ListPicker<Option<FieldValue>> {
    let at = choices.iter().position(|c| c.detail.starts_with('✓'));
    let mut picker = ListPicker::new(title, choices);
    picker.selected = at.unwrap_or(0);
    picker
}

/// Whether `new` leaves the field as it is: the same value, or (for single-selects and
/// iterations) an option in the same bucket as the current one.
fn is_unchanged(field: &Field, current: Option<&FieldValue>, new: &FieldValue) -> bool {
    let Some(current) = current else { return false };
    match (current.bucket_key(), new.bucket_key()) {
        (Some(have), Some(want)) => field
            .buckets()
            .unwrap_or_default()
            .iter()
            .any(|b| b.matches(&have) && b.matches(&want)),
        _ => current == new,
    }
}

/// What a submitted line means for a field of `kind`: `Ok(None)` clears it.
fn line_value(
    kind: LineKind,
    input: &str,
    today: time::Date,
) -> Result<Option<FieldValue>, String> {
    match kind {
        LineKind::Text => {
            let t = input.trim();
            Ok((!t.is_empty()).then(|| FieldValue::Text(t.to_string())))
        }
        LineKind::Number => parse_number_input(input).map(|n| n.map(FieldValue::Number)),
        LineKind::Date => {
            parse_date_input(input, today).map(|d| d.map(|d| FieldValue::Date(format_iso_date(d))))
        }
    }
}

/// How a popover's title names the item: its number, or "draft". The item's own title is
/// already on screen (the highlighted card or the open detail), and repeating it read like a
/// message ("Edit · Crash when …").
fn item_ref(item: &crate::model::Item) -> String {
    match item.number() {
        Some(n) => format!("#{n}"),
        None => "draft".into(),
    }
}

impl App {
    /// The user's local date.
    pub(crate) fn today(&self) -> time::Date {
        local_date(time::OffsetDateTime::now_utc(), self.utc_offset)
    }

    pub(crate) fn open_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
        self.mode = Mode::Popup;
    }

    pub(crate) fn close_popup(&mut self) {
        self.popup = None;
        self.mode = self.base_mode();
    }

    /// Space: every operation `action_item` allows, each with its key; or why there are none.
    pub(crate) fn open_action_menu(&mut self) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        let key = |a: Action| {
            self.keymap
                .keys_for(a)
                .first()
                .map(|k| k.label())
                .unwrap_or_default()
        };
        let mut choices: Vec<Choice<MenuEntry>> = Vec::new();
        for &action in edit::EDIT_ACTIONS {
            if let Some(kind) = self.allowed_kind(action, &item) {
                choices.push(
                    Choice::new(edit::menu_label(action, kind), MenuEntry::Key(action))
                        .with_detail(key(action)),
                );
            }
        }
        if item.url().is_some() {
            choices.push(
                Choice::new("Open in the browser", MenuEntry::Key(Action::OpenBrowser))
                    .with_detail(key(Action::OpenBrowser)),
            );
        }
        if choices.is_empty() {
            // Nothing at all: say why, with the first reason an edit key was refused.
            let reason = edit::EDIT_ACTIONS
                .iter()
                .flat_map(|a| a.kinds())
                .find_map(|k| self.check_here(*k, &item).err());
            self.status.flash =
                Some(reason.unwrap_or_else(|| "Nothing to do with this item".into()));
            return Vec::new();
        }
        let mut picker = ListPicker::new(format!("Actions · {}", item_ref(&item)), choices);
        picker.typing = false;
        self.open_popup(Popup::Actions {
            item: item.id,
            picker,
        });
        Vec::new()
    }

    /// `e`: the menu of the item's editable fields, each with its current value.
    pub(crate) fn open_field_menu(&mut self) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        if let Err(reason) = self.check_here(OpKind::SetField, &item) {
            self.status.flash = Some(reason);
            return Vec::new();
        }
        let Some(project) = self.project() else {
            return Vec::new();
        };
        let choices: Vec<Choice<FieldTarget>> = project
            .fields
            .iter()
            .filter(|f| editable(&f.kind))
            .map(|f| {
                Choice::new(f.name.clone(), FieldTarget::Field(f.id.clone()))
                    .with_detail(item.field_value(f).map(|v| v.display()).unwrap_or_default())
            })
            .collect();
        if choices.is_empty() {
            self.status.flash = Some("This project has no fields to edit here".into());
            return Vec::new();
        }
        let title = format!("Edit field · {}", item_ref(&item));
        self.open_popup(Popup::Fields {
            item: item.id,
            picker: ListPicker::new(title, choices),
        });
        Vec::new()
    }

    /// The editor for `field` of `item`, by field type.
    fn open_field_editor(&mut self, item: &ItemId, field: &FieldId) -> Vec<Command> {
        let found = self
            .project()
            .and_then(|p| p.field(field).cloned())
            .zip(self.snapshot().and_then(|s| s.items.get(item)).cloned());
        let Some((f, it)) = found else {
            self.close_popup();
            return Vec::new();
        };
        let current = it.value(&f.id).cloned();
        let title = format!("{} · {}", f.name, item_ref(&it));
        let (item, field) = (item.clone(), f.id.clone());
        let line = |kind: LineKind, input: String| Popup::Line {
            item: item.clone(),
            field: field.clone(),
            kind,
            title: title.clone(),
            input,
            error: None,
        };
        let popup = match &f.kind {
            FieldKind::SingleSelect { .. } => {
                // One row per bucket: options sharing a name are one row, and the
                // canonical (last) option's id is what gets sent.
                let set = current.as_ref().and_then(|v| v.bucket_key());
                let mut choices: Vec<Choice<Option<FieldValue>>> = f
                    .buckets()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|b| {
                        let value = f.value_for_bucket(b.key.as_deref()?)?;
                        let mark = if set.as_deref().is_some_and(|k| b.matches(k)) {
                            "✓"
                        } else {
                            ""
                        };
                        Some(Choice::new(b.title.clone(), Some(value)).with_detail(mark))
                    })
                    .collect();
                if current.is_some() {
                    choices.push(Choice::new(format!("Clear {}", f.name), None));
                }
                Popup::Value {
                    item,
                    field,
                    picker: value_picker(title, choices),
                }
            }
            FieldKind::Iteration { .. } => {
                let choices = iteration_choices(&f, current.as_ref(), self.today());
                Popup::Value {
                    item,
                    field,
                    picker: value_picker(title, choices),
                }
            }
            FieldKind::MultiSelect { options } => {
                let chosen: Vec<OptionId> = match &current {
                    Some(FieldValue::MultiSelect { option_ids, .. }) => option_ids.clone(),
                    _ => Vec::new(),
                };
                let options = options
                    .iter()
                    .map(|o| MultiOption {
                        label: o.name.clone(),
                        color: None,
                        value: o.id.clone(),
                    })
                    .collect();
                Popup::Options {
                    item,
                    field,
                    select: MultiSelect::new(title, options, |id| chosen.contains(id)),
                }
            }
            FieldKind::Text => line(
                LineKind::Text,
                current.map(|v| v.display()).unwrap_or_default(),
            ),
            FieldKind::Number => line(
                LineKind::Number,
                current.map(|v| v.display()).unwrap_or_default(),
            ),
            FieldKind::Date => line(
                LineKind::Date,
                current.map(|v| v.display()).unwrap_or_default(),
            ),
            _ => {
                self.close_popup();
                self.status.flash = Some(format!("{} can't be edited here", f.name));
                return Vec::new();
            }
        };
        self.open_popup(popup);
        Vec::new()
    }

    /// Sets or clears a field from a popover; nothing when clearing an empty field.
    fn submit_value(
        &mut self,
        item: ItemId,
        field: FieldId,
        value: Option<FieldValue>,
    ) -> Vec<Command> {
        self.close_popup();
        // Keep the cursor on this item when the edit re-sorts or regroups the view.
        self.selected_id = Some(item.clone());
        let current = self
            .snapshot()
            .and_then(|s| s.items.get(&item))
            .and_then(|i| i.value(&field))
            .cloned();
        match value {
            Some(value) => {
                let same = self
                    .project()
                    .and_then(|p| p.field(&field))
                    .is_some_and(|f| is_unchanged(f, current.as_ref(), &value));
                if same {
                    return Vec::new();
                }
                self.submit(OpKind::SetField, Op::SetField { item, field, value })
            }
            None => {
                if current.is_some() {
                    self.submit(OpKind::SetField, Op::ClearField { item, field })
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub(crate) fn handle_popup_key(&mut self, key: KeyEvent) -> Vec<Command> {
        let Some(mut popup) = self.popup.take() else {
            self.mode = self.base_mode();
            return Vec::new();
        };
        let today = self.today();
        match &mut popup {
            Popup::Actions { picker, .. } => match picker.handle(&key) {
                ListOutcome::Chosen(MenuEntry::Key(action)) => {
                    self.close_popup();
                    return self.run_action(action);
                }
                ListOutcome::Key(k) => {
                    let listed = self
                        .keymap
                        .action(&k)
                        .filter(|a| picker.choices.iter().any(|c| c.value == MenuEntry::Key(*a)));
                    if let Some(action) = listed {
                        self.close_popup();
                        return self.run_action(action);
                    }
                }
                ListOutcome::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                ListOutcome::None => {}
            },
            Popup::Fields { item, picker } => match picker.handle(&key) {
                ListOutcome::Chosen(FieldTarget::Field(field)) => {
                    let item = item.clone();
                    return self.open_field_editor(&item, &field);
                }
                ListOutcome::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                ListOutcome::None | ListOutcome::Key(_) => {}
            },
            Popup::Value {
                item,
                field,
                picker,
            } => match picker.handle(&key) {
                ListOutcome::Chosen(value) => {
                    let (item, field) = (item.clone(), field.clone());
                    return self.submit_value(item, field, value);
                }
                ListOutcome::Cancel => {
                    let field = field.clone();
                    return self.back_to_menu(&field);
                }
                ListOutcome::None | ListOutcome::Key(_) => {}
            },
            Popup::Options {
                item,
                field,
                select,
            } => match select.handle(&key) {
                MultiOutcome::Apply {
                    added,
                    removed,
                    chosen,
                } => {
                    let (item, field) = (item.clone(), field.clone());
                    if added.is_empty() && removed.is_empty() {
                        self.close_popup();
                        return Vec::new();
                    }
                    let names = self.option_names(&field, &chosen);
                    let value = (!chosen.is_empty()).then_some(FieldValue::MultiSelect {
                        option_ids: chosen,
                        names,
                    });
                    return self.submit_value(item, field, value);
                }
                MultiOutcome::Cancel => {
                    let field = field.clone();
                    return self.back_to_menu(&field);
                }
                MultiOutcome::None => {}
            },
            Popup::Line {
                item,
                field,
                kind,
                input,
                error,
                ..
            } => match edit_line(input, &key) {
                LineOutcome::Submit => match line_value(*kind, input, today) {
                    Ok(value) => {
                        let (item, field) = (item.clone(), field.clone());
                        return self.submit_value(item, field, value);
                    }
                    Err(e) => *error = Some(e),
                },
                LineOutcome::Cancel => {
                    let field = field.clone();
                    return self.back_to_menu(&field);
                }
                LineOutcome::Typing => *error = None,
            },
        }
        self.popup = Some(popup);
        Vec::new()
    }

    /// Esc in a value editor: back to the field menu, on the field that was being edited.
    fn back_to_menu(&mut self, field: &FieldId) -> Vec<Command> {
        self.close_popup();
        let cmds = self.open_field_menu();
        if let Some(Popup::Fields { picker, .. }) = &mut self.popup
            && let Some(at) = picker
                .choices
                .iter()
                .position(|c| c.value == FieldTarget::Field(field.clone()))
        {
            picker.selected = at;
        }
        cmds
    }

    /// The names of multi-select options, in the order given.
    fn option_names(&self, field: &FieldId, ids: &[OptionId]) -> Vec<String> {
        let Some(FieldKind::MultiSelect { options }) =
            self.project().and_then(|p| p.field(field)).map(|f| &f.kind)
        else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| options.iter().find(|o| &o.id == id).map(|o| o.name.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::test_app;
    use crate::ui::fixtures::{code, key};
    use time::macros::date;

    fn type_text(a: &mut App, text: &str) {
        for c in text.chars() {
            a.handle_key(key(c));
        }
    }

    fn value(a: &App, item: &str, field: &str) -> Option<FieldValue> {
        a.snapshot().unwrap().items[&ItemId::new(item)]
            .value(&FieldId::new(field))
            .cloned()
    }

    fn field_labels(a: &App) -> Vec<String> {
        match &a.popup {
            Some(Popup::Fields { picker, .. }) => {
                picker.visible().iter().map(|c| c.label.clone()).collect()
            }
            other => panic!("expected the field menu, got {other:?}"),
        }
    }

    #[test]
    fn e_lists_the_editable_fields_with_current_values() {
        let mut a = test_app();
        assert!(a.handle_key(key('e')).is_empty());
        assert_eq!(a.mode, Mode::Popup);
        assert_eq!(
            field_labels(&a),
            [
                "Status", "Priority", "Notes", "Size", "Due", "Sprint", "Areas"
            ]
        );
        let Some(Popup::Fields { picker, .. }) = &a.popup else {
            unreachable!()
        };
        assert_eq!(picker.choices[0].detail, "Todo");
    }

    #[test]
    fn choosing_an_option_sets_it() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "sta");
        a.handle_key(code(KeyCode::Enter));
        assert!(matches!(a.popup, Some(Popup::Value { .. })));
        type_text(&mut a, "done");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(
            value(&a, "a", "F_status").map(|v| v.display()).as_deref(),
            Some("Done")
        );
    }

    #[test]
    fn clear_is_offered_when_the_item_has_a_value() {
        let mut a = test_app();
        a.handle_key(key('e'));
        a.handle_key(code(KeyCode::Enter)); // Status
        type_text(&mut a, "clear");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(value(&a, "a", "F_status"), None);
    }

    #[test]
    fn a_bad_date_keeps_the_input_open_with_the_reason() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "due");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "soon");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let Some(Popup::Line { error, .. }) = &a.popup else {
            panic!("still the date input")
        };
        assert!(error.as_deref().unwrap().contains("not a date"));
        for _ in 0..4 {
            a.handle_key(code(KeyCode::Backspace));
        }
        type_text(&mut a, "2026-10-03");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            value(&a, "a", "F_due"),
            Some(FieldValue::Date("2026-10-03".into()))
        );
    }

    #[test]
    fn text_and_numbers_are_set_and_an_empty_input_clears() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "notes");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "Needs a repro");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(
            value(&a, "a", "F_notes"),
            Some(FieldValue::Text("Needs a repro".into()))
        );
        a.handle_key(key('e'));
        type_text(&mut a, "size");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "2.5");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(value(&a, "a", "F_size"), Some(FieldValue::Number(2.5)));
        a.handle_key(key('e'));
        type_text(&mut a, "notes");
        a.handle_key(code(KeyCode::Enter));
        let Some(Popup::Line { input, .. }) = &a.popup else {
            panic!("text input")
        };
        assert_eq!(input, "Needs a repro", "pre-filled");
        for _ in 0.."Needs a repro".len() {
            a.handle_key(code(KeyCode::Backspace));
        }
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(value(&a, "a", "F_notes"), None);
        assert_eq!(a.pending_count(), 3);
    }

    #[test]
    fn multi_select_applies_the_new_set() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "areas");
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            value(&a, "a", "F_areas"),
            Some(FieldValue::MultiSelect {
                option_ids: vec![OptionId::new("m_api")],
                names: vec!["api".into()]
            })
        );
    }

    #[test]
    fn iteration_choices_put_the_current_one_first() {
        let p = crate::ui::fixtures::project();
        let sprint = p.fields.iter().find(|f| f.name == "Sprint").unwrap();
        let current = FieldValue::Iteration {
            iteration_id: IterationId::new("it3"),
            title: "Sprint 3".into(),
            start_date: "2026-10-15".into(),
        };
        let choices = iteration_choices(sprint, Some(&current), date!(2026 - 10 - 12));
        let rows: Vec<(&str, &str)> = choices
            .iter()
            .map(|c| (c.label.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Sprint 2", "current"),
                ("Sprint 3", "✓"),
                ("Sprint 1", "completed"),
                ("Clear Sprint", "")
            ]
        );
    }

    #[test]
    fn esc_closes_the_popover_and_returns_to_the_detail() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('e'));
        assert_eq!(a.mode, Mode::Popup);
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Detail);
        assert!(a.popup.is_none());
    }

    #[test]
    fn a_private_item_says_why_instead_of_opening() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key('e')).is_empty());
        assert_eq!(a.mode, Mode::Normal);
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .starts_with("You can't see")
        );
    }

    fn two_todos(a: &mut App) {
        let mut snap = a.snapshot().unwrap().clone();
        let f = snap
            .project
            .fields
            .iter_mut()
            .find(|f| f.name == "Status")
            .unwrap();
        let FieldKind::SingleSelect { options } = &mut f.kind else {
            unreachable!()
        };
        let todo = options.iter().find(|o| o.name == "Todo").unwrap().clone();
        options.push(crate::model::SelectOption {
            id: OptionId::new("todo_dup"),
            ..todo
        });
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
    }

    fn rows(a: &App) -> Vec<(String, String)> {
        match &a.popup {
            Some(Popup::Value { picker, .. }) => picker
                .choices
                .iter()
                .map(|c| (c.label.clone(), c.detail.clone()))
                .collect(),
            other => panic!("expected a value picker, got {other:?}"),
        }
    }

    #[test]
    fn options_sharing_a_name_are_one_row_and_send_the_canonical_id() {
        let mut a = test_app();
        two_todos(&mut a);
        a.handle_key(key('e'));
        a.handle_key(code(KeyCode::Enter));
        let rows = rows(&a);
        assert_eq!(rows.iter().filter(|r| r.0 == "Todo").count(), 1, "{rows:?}");
        assert!(rows.contains(&("Todo".into(), "✓".into())));
        // The item is on the alias (`o_todo`); the dup is canonical.
        a.handle_key(code(KeyCode::Esc));
        a.handle_key(code(KeyCode::Esc));
        a.handle_key(key('e'));
        a.handle_key(code(KeyCode::Enter));
        assert!(rows_checked(&a, "Todo"));
        type_text(&mut a, "in");
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('e'));
        a.handle_key(code(KeyCode::Enter));
        // Back to Todo: sends the canonical (last) id.
        type_text(&mut a, "todo");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(2)]);
        assert_eq!(
            value(&a, "a", "F_status"),
            Some(FieldValue::SingleSelect {
                option_id: OptionId::new("todo_dup"),
                name: "Todo".into()
            })
        );
    }

    fn rows_checked(a: &App, label: &str) -> bool {
        rows(a).iter().any(|r| r.0 == label && r.1.starts_with('✓'))
    }

    #[test]
    fn the_cursor_starts_on_the_current_value_and_choosing_it_sends_nothing() {
        let mut a = test_app();
        a.handle_key(key('e'));
        a.handle_key(code(KeyCode::Enter));
        let Some(Popup::Value { picker, .. }) = &a.popup else {
            panic!()
        };
        assert_eq!(picker.current().unwrap().label, "Todo");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(a.pending_count(), 0);
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn an_unchanged_line_or_multi_select_sends_nothing() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "notes");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "x");
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.pending_count(), 1);
        a.handle_key(key('e'));
        type_text(&mut a, "notes");
        a.handle_key(code(KeyCode::Enter));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(a.pending_count(), 1);
        a.handle_key(key('e'));
        type_text(&mut a, "areas");
        a.handle_key(code(KeyCode::Enter));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(a.pending_count(), 1);
    }

    #[test]
    fn an_invalid_number_stays_open_with_the_reason() {
        let mut a = test_app();
        a.handle_key(key('e'));
        type_text(&mut a, "size");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "abc");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let Some(Popup::Line { error, .. }) = &a.popup else {
            panic!("still open")
        };
        assert!(error.is_some());
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn esc_goes_back_one_level_from_the_board_and_from_the_detail() {
        for detail in [false, true] {
            let mut a = test_app();
            if detail {
                a.handle_key(code(KeyCode::Enter));
            }
            let base = a.mode;
            a.handle_key(key('e'));
            type_text(&mut a, "notes");
            a.handle_key(code(KeyCode::Enter));
            assert!(matches!(a.popup, Some(Popup::Line { .. })));
            a.handle_key(code(KeyCode::Esc));
            assert!(
                matches!(a.popup, Some(Popup::Fields { .. })),
                "back to the menu"
            );
            assert_eq!(a.mode, Mode::Popup);
            a.handle_key(code(KeyCode::Esc));
            assert!(a.popup.is_none());
            assert_eq!(a.mode, base);
        }
    }

    #[test]
    fn submitting_from_the_detail_returns_to_it() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('e'));
        type_text(&mut a, "notes");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "hi");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(a.mode, Mode::Detail);
    }

    #[test]
    fn the_iteration_picker_marks_current_and_chosen_through_the_app() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.items
            .get_mut(&ItemId::new("a"))
            .unwrap()
            .values
            .insert(
                FieldId::new("F_sprint"),
                FieldValue::Iteration {
                    iteration_id: IterationId::new("it1"),
                    title: "Sprint 1".into(),
                    start_date: "2026-09-17".into(),
                },
            );
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.handle_key(key('e'));
        type_text(&mut a, "sprint");
        a.handle_key(code(KeyCode::Enter));
        assert!(rows_checked(&a, "Sprint 1"));
        let Some(Popup::Value { picker, .. }) = &a.popup else {
            panic!()
        };
        assert_eq!(
            picker.current().unwrap().label,
            "Sprint 1",
            "cursor on the current value"
        );
        type_text(&mut a, "sprint 3");
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(
            value(&a, "a", "F_sprint").map(|v| v.display()).as_deref(),
            Some("Sprint 3")
        );
    }

    #[test]
    fn the_current_iteration_keeps_its_label_when_chosen() {
        let p = crate::ui::fixtures::project();
        let sprint = p.fields.iter().find(|f| f.name == "Sprint").unwrap();
        let current = FieldValue::Iteration {
            iteration_id: IterationId::new("it2"),
            title: "Sprint 2".into(),
            start_date: "2026-10-01".into(),
        };
        let choices = iteration_choices(sprint, Some(&current), date!(2026 - 10 - 12));
        assert_eq!(choices[0].detail, "✓ current");
    }

    #[test]
    fn today_is_the_local_date() {
        use time::macros::{datetime, offset};
        let now = datetime!(2026-10-12 02:00 UTC);
        assert_eq!(local_date(now, offset!(-5)), date!(2026 - 10 - 11));
        assert_eq!(local_date(now, time::UtcOffset::UTC), date!(2026 - 10 - 12));
        let mut a = test_app();
        a.utc_offset = offset!(+14);
        assert_eq!(
            a.today(),
            local_date(time::OffsetDateTime::now_utc(), offset!(+14))
        );
    }

    #[test]
    fn archived_and_read_only_items_refuse_with_their_reason() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().archived = true;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key('e')).is_empty());
        assert_eq!(a.mode, Mode::Detail);
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .starts_with("Archived items")
        );

        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.handle_key(key('e'));
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
    }

    #[test]
    fn e_with_no_item_says_so() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.clear();
        snap.order.clear();
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.handle_key(key('e'));
        assert_eq!(a.status.flash.as_deref(), Some("Select an item first"));
    }

    fn menu(a: &App) -> Vec<(String, String)> {
        match &a.popup {
            Some(Popup::Actions { picker, .. }) => picker
                .choices
                .iter()
                .map(|c| (c.label.clone(), c.detail.clone()))
                .collect(),
            other => panic!("expected the action menu, got {other:?}"),
        }
    }

    #[test]
    fn space_lists_every_operation_the_item_allows_with_its_key() {
        let mut a = test_app();
        a.handle_key(key(' '));
        let rows = menu(&a);
        // Later tasks add entries; these must stay, the moves first.
        assert_eq!(
            rows[0],
            ("Move to the previous column".to_string(), "<".to_string())
        );
        for (label, key) in [
            ("Move to the next column", ">"),
            ("Move up", "K"),
            ("Move down", "J"),
            ("Edit a field", "e"),
            ("Open in the browser", "o"),
        ] {
            assert!(
                rows.iter().any(|(l, d)| l == label && d == key),
                "{label} ({key}) in {rows:?}"
            );
        }
    }

    #[test]
    fn choosing_from_the_menu_or_pressing_a_listed_key_runs_it() {
        let mut a = test_app();
        a.handle_key(key(' '));
        a.handle_key(key('j'));
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Send(1)],
            "move to the next column"
        );
        assert_eq!(a.mode, Mode::Normal);
        a.handle_key(key(' '));
        assert!(a.handle_key(key('e')).is_empty());
        assert!(
            matches!(a.popup, Some(Popup::Fields { .. })),
            "e opened the field menu"
        );
    }

    #[test]
    fn the_menu_ignores_unlisted_keys_and_esc_closes_it() {
        let mut a = test_app();
        a.handle_key(key(' '));
        a.handle_key(key('q'));
        a.handle_key(key('z'));
        assert!(matches!(a.popup, Some(Popup::Actions { .. })), "still open");
        a.handle_key(code(KeyCode::Esc));
        assert!(a.popup.is_none());
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn a_read_only_board_still_offers_the_browser_and_hints_nothing() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key(' ')).is_empty());
        assert_eq!(
            menu(&a),
            [("Open in the browser".to_string(), "o".to_string())]
        );
        assert_eq!(a.edit_hints(), None);
    }

    #[test]
    fn with_nothing_to_do_the_menu_says_why() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key(' ')).is_empty());
        assert!(a.popup.is_none());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
    }

    #[test]
    fn rebinding_a_key_changes_its_menu_label_and_hint() {
        let mut a = test_app();
        let mut o = std::collections::BTreeMap::new();
        o.insert("edit_field".to_string(), "E".to_string());
        o.insert("action_menu".to_string(), "m".to_string());
        a.keymap = crate::ui::keymap::Keymap::with_overrides(&o).0;
        let hints = a.edit_hints().unwrap();
        assert!(
            hints.contains("E field") && hints.ends_with("m actions"),
            "{hints}"
        );
        a.handle_key(key('m'));
        assert!(menu(&a).contains(&("Edit a field".to_string(), "E".to_string())));
        a.handle_key(key('E'));
        assert!(matches!(a.popup, Some(Popup::Fields { .. })));
    }

    #[test]
    fn hints_list_the_keys_the_item_allows() {
        let a = test_app();
        let hints = a.edit_hints().unwrap();
        // Asserted piece by piece: later tasks insert `a assignees`, `c comment`, `X close`, ...
        assert!(hints.contains("< > move · J K reorder"), "{hints}");
        assert!(hints.contains("e field"), "{hints}");
        assert!(hints.ends_with("space actions"), "{hints}");
        let mut a = a;
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        let hints = a.edit_hints().unwrap();
        assert!(
            hints.contains("< > move") && hints.ends_with("space actions"),
            "{hints}"
        );
        assert!(
            !hints.contains("reorder") && !hints.contains("field"),
            "{hints}"
        );
    }

    /// Pre-flight ruling 7: what the view rules out is neither offered nor hinted.
    #[test]
    fn a_sorted_view_neither_offers_nor_hints_reorder() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.views[0].sort_by = vec![crate::model::SortSpec {
            field: FieldId::new("F_prio"),
            direction: crate::model::SortDirection::Desc,
        }];
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        let hints = a.edit_hints().unwrap();
        assert!(
            !hints.contains("reorder") && hints.contains("< > move"),
            "{hints}"
        );
        a.handle_key(key(' '));
        let rows = menu(&a);
        assert!(
            !rows.iter().any(|(l, _)| l == "Move up" || l == "Move down"),
            "{rows:?}"
        );
        assert!(
            rows.iter().any(|(l, _)| l == "Move to the next column"),
            "{rows:?}"
        );
    }

    #[test]
    fn space_works_from_the_detail_too() {
        let mut a = test_app();
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key(' '));
        assert!(matches!(a.popup, Some(Popup::Actions { .. })));
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Detail);
    }
}
