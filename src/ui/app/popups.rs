//! The popovers: what each one edits and what its keys do.

use super::*;
use crate::model::{
    FieldValue, Iteration, OptionId, check, format_iso_date, parse_date_input, parse_iso_date,
    parse_number_input,
};
use crate::ops::Op;
use crate::ui::multiselect::{MultiOption, MultiOutcome, MultiSelect};
use crate::ui::popup::{Choice, LineOutcome, ListOutcome, ListPicker, edit_line};
use crate::ui::text::truncate_to_width;

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

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
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

/// The date "today" means. UTC: a typed `+1d` near midnight may be a day off locally.
fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
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
        let detail = if set.as_deref() == Some(i.id.as_str()) {
            "✓".to_string()
        } else {
            detail.to_string()
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

/// What a submitted line means for a field of `kind`: `Ok(None)` clears it.
fn line_value(kind: LineKind, input: &str) -> Result<Option<FieldValue>, String> {
    match kind {
        LineKind::Text => {
            let t = input.trim();
            Ok((!t.is_empty()).then(|| FieldValue::Text(t.to_string())))
        }
        LineKind::Number => parse_number_input(input).map(|n| n.map(FieldValue::Number)),
        LineKind::Date => parse_date_input(input, today())
            .map(|d| d.map(|d| FieldValue::Date(format_iso_date(d)))),
    }
}

impl App {
    pub(crate) fn open_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
        self.mode = Mode::Popup;
    }

    pub(crate) fn close_popup(&mut self) {
        self.popup = None;
        self.mode = self.base_mode();
    }

    /// `e`: the menu of the item's editable fields, each with its current value.
    pub(crate) fn open_field_menu(&mut self) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            return Vec::new();
        };
        if let Err(denial) = check(OpKind::SetField, &item, &self.edit_context()) {
            self.status.flash = Some(self.denial_text(denial));
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
        let title = format!("Edit · {}", truncate_to_width(item.title(), 40));
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
        let title = format!("{} · {}", f.name, truncate_to_width(it.title(), 40));
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
            FieldKind::SingleSelect { options } => {
                let set = current.as_ref().and_then(|v| v.bucket_key());
                let mut choices: Vec<Choice<Option<FieldValue>>> = options
                    .iter()
                    .map(|o| {
                        let mark = if set.as_deref() == Some(o.id.as_str()) {
                            "✓"
                        } else {
                            ""
                        };
                        Choice::new(
                            o.name.clone(),
                            Some(FieldValue::SingleSelect {
                                option_id: o.id.clone(),
                                name: o.name.clone(),
                            }),
                        )
                        .with_detail(mark)
                    })
                    .collect();
                if current.is_some() {
                    choices.push(Choice::new(format!("Clear {}", f.name), None));
                }
                Popup::Value {
                    item,
                    field,
                    picker: ListPicker::new(title, choices),
                }
            }
            FieldKind::Iteration { .. } => {
                let choices = iteration_choices(&f, current.as_ref(), today());
                Popup::Value {
                    item,
                    field,
                    picker: ListPicker::new(title, choices),
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
        match value {
            Some(value) => self.submit(OpKind::SetField, Op::SetField { item, field, value }),
            None => {
                let has_value = self
                    .snapshot()
                    .and_then(|s| s.items.get(&item))
                    .is_some_and(|i| i.value(&field).is_some());
                if has_value {
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
        match &mut popup {
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
                    self.close_popup();
                    return Vec::new();
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
                    self.close_popup();
                    return Vec::new();
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
                LineOutcome::Submit => match line_value(*kind, input) {
                    Ok(value) => {
                        let (item, field) = (item.clone(), field.clone());
                        return self.submit_value(item, field, value);
                    }
                    Err(e) => *error = Some(e),
                },
                LineOutcome::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                LineOutcome::Typing => *error = None,
            },
        }
        self.popup = Some(popup);
        Vec::new()
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
}
