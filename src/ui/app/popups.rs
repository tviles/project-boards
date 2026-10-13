//! The popovers: what each one edits and what its keys do.

use super::*;
use crate::model::{ContentId, LabelRef, MilestoneRef, RepoCandidates, UserRef};
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
    /// The title, in `$EDITOR`.
    Title,
    Assignees,
    Labels,
    Milestone,
}

/// An entry of the action menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuEntry {
    /// Runs the action, as its key would.
    Key(Action),
    /// The milestone picker (it has no key of its own; `e` reaches it too).
    Milestone,
    /// The title in `$EDITOR` (`e` reaches it too).
    EditTitle,
    /// The body in `$EDITOR`; it has no key.
    EditBody,
    /// Removes the item from the project, after asking; it has no key.
    Remove,
    /// Shows or hides archived items (`is:archived` in the session filter).
    ToggleArchived,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    /// Space: every operation the item allows, with its key.
    Actions {
        /// `None` when no item is selected: only the board's entries are listed.
        item: Option<ItemId>,
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
    /// `a`: the item's assignees, from its repository's assignable users.
    Assignees {
        item: ItemId,
        repo: RepoSlug,
        /// Opened from the `e` menu, so Esc goes back to it.
        from_menu: bool,
        select: MultiSelect<UserRef>,
    },
    /// `t`: the item's labels, from its repository's labels.
    Labels {
        item: ItemId,
        repo: RepoSlug,
        /// Opened from the `e` menu, so Esc goes back to it.
        from_menu: bool,
        select: MultiSelect<LabelRef>,
    },
    /// The item's milestone; `None` clears it.
    Milestone {
        item: ItemId,
        repo: RepoSlug,
        /// Opened from the `e` menu, so Esc goes back to it.
        from_menu: bool,
        picker: ListPicker<Option<MilestoneRef>>,
    },
    /// `X` on an open issue: why it closes. Choosing a reason is the confirmation.
    CloseReason {
        item: ItemId,
        picker: ListPicker<crate::model::CloseReason>,
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

/// Shown in a popover whose item left the board while its candidates loaded.
pub(super) const GONE: &str = "This item is no longer on the board";

/// The users to offer: the repository's, plus current assignees it did not list (so they
/// can be removed), and the logins assigned now.
pub(super) fn assignee_options(
    c: &RepoCandidates,
    item: &Item,
) -> (Vec<MultiOption<UserRef>>, Vec<String>) {
    let current: Vec<String> = item.assignees().iter().map(|s| s.to_string()).collect();
    let mut users = c.users.clone();
    for login in &current {
        if !users.iter().any(|u| &u.login == login)
            && let Some(id) = item.content_fields.assignee_ids.get(login)
        {
            users.push(UserRef {
                id: id.clone(),
                login: login.clone(),
            });
        }
    }
    let options = users
        .into_iter()
        .map(|u| MultiOption {
            label: u.login.clone(),
            color: None,
            value: u,
        })
        .collect();
    (options, current)
}

/// The labels to offer, current ones the repository did not list included.
pub(super) fn label_options(
    c: &RepoCandidates,
    item: &Item,
) -> (Vec<MultiOption<LabelRef>>, Vec<String>) {
    let current: Vec<String> = item.label_names().iter().map(|s| s.to_string()).collect();
    let mut labels = c.labels.clone();
    for l in item.labels() {
        if !labels.iter().any(|x| x.name == l.name)
            && let Some(id) = item.content_fields.label_ids.get(&l.name)
        {
            labels.push(LabelRef {
                id: id.clone(),
                name: l.name.clone(),
                color: l.color.clone(),
            });
        }
    }
    let options = labels
        .into_iter()
        .map(|l| MultiOption {
            label: l.name.clone(),
            color: Some(l.color.clone()),
            value: l,
        })
        .collect();
    (options, current)
}

/// Open milestones (the item's own marked `✓`, listed even when closed), then "No milestone".
pub(super) fn milestone_choices(
    c: &RepoCandidates,
    item: &Item,
) -> Vec<Choice<Option<MilestoneRef>>> {
    let current = item.content_fields.milestone.clone();
    let mut list = c.milestones.clone();
    if let Some(m) = &current
        && !list.iter().any(|x| x.id == m.id)
    {
        list.push(m.clone());
    }
    let mut out: Vec<_> = list
        .into_iter()
        .map(|m| {
            let mark = if current.as_ref().is_some_and(|c| c.id == m.id) {
                "✓"
            } else {
                ""
            };
            Choice::new(m.title.clone(), Some(m)).with_detail(mark)
        })
        .collect();
    if current.is_some() {
        out.push(Choice::new("No milestone", None));
    }
    out
}

/// Fills a waiting assignee or label checklist once its repository's candidates have loaded
/// (or failed). `options` lists the choices and the names checked now; `name` names a choice.
pub(super) fn fill_checklist<T: Clone>(
    select: &mut MultiSelect<T>,
    lookup: Option<&Lookup<RepoCandidates>>,
    item: Option<&Item>,
    options: impl Fn(&RepoCandidates, &Item) -> (Vec<MultiOption<T>>, Vec<String>),
    name: impl Fn(&T) -> String,
) {
    match lookup {
        Some(Lookup::Ready(_)) if item.is_none() => {
            select.loading = false;
            select.error = Some(GONE.into());
        }
        Some(Lookup::Ready(c)) => {
            if let Some(it) = item {
                let (choices, current) = options(c, it);
                select.set_options(choices, |v| current.contains(&name(v)));
                select.truncated = c.truncated;
            }
        }
        Some(Lookup::Failed(e)) => {
            select.loading = false;
            select.error = Some(e.clone());
        }
        Some(Lookup::Loading) | None => {}
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
        let item = self.action_item().cloned();
        let key = |a: Action| {
            self.keymap
                .keys_for(a)
                .first()
                .map(|k| k.label())
                .unwrap_or_default()
        };
        let mut choices: Vec<Choice<MenuEntry>> = Vec::new();
        if let Some(item) = &item {
            for &action in edit::EDIT_ACTIONS {
                if let Some(kind) = self.allowed_kind(action, item) {
                    choices.push(
                        Choice::new(edit::menu_label(action, kind), MenuEntry::Key(action))
                            .with_detail(key(action)),
                    );
                }
            }
            for (kind, label, entry) in [
                (OpKind::Title, "Edit title", MenuEntry::EditTitle),
                (OpKind::Body, "Edit body", MenuEntry::EditBody),
                (OpKind::Remove, "Remove from project", MenuEntry::Remove),
            ] {
                if self.allowed(kind, item) {
                    choices.push(Choice::new(label, entry));
                }
            }
            if self.allowed(OpKind::Milestone, item) {
                choices.push(Choice::new("Milestone", MenuEntry::Milestone));
            }
            if item.url().is_some() {
                choices.push(
                    Choice::new("Open in the browser", MenuEntry::Key(Action::OpenBrowser))
                        .with_detail(key(Action::OpenBrowser)),
                );
            }
        }
        if self.current_view().is_some() {
            let shown = self
                .extra_filter()
                .is_some_and(|f| edit::archived_token(f).is_some());
            let label = if self.view_shows_archived() {
                "Archived items are shown by this view"
            } else if shown {
                "Hide archived items"
            } else {
                "Show archived items"
            };
            choices.push(Choice::new(label, MenuEntry::ToggleArchived));
        }
        if choices.is_empty() {
            // Nothing at all: say why, with the first reason an edit key was refused.
            let reason = item.as_ref().and_then(|item| {
                edit::EDIT_ACTIONS
                    .iter()
                    .flat_map(|a| a.kinds())
                    .find_map(|k| self.check_here(*k, item).err())
            });
            self.status.flash = Some(reason.unwrap_or_else(|| {
                if item.is_some() {
                    "Nothing to do with this item".into()
                } else {
                    "Select an item first".into()
                }
            }));
            return Vec::new();
        }
        let title = match &item {
            Some(item) => format!("Actions · {}", item_ref(item)),
            None => "Actions".to_string(),
        };
        let mut picker = ListPicker::new(title, choices);
        picker.typing = false;
        self.open_popup(Popup::Actions {
            item: item.map(|i| i.id),
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
        let mut choices: Vec<Choice<FieldTarget>> = project
            .fields
            .iter()
            .filter(|f| editable(&f.kind))
            .map(|f| {
                Choice::new(f.name.clone(), FieldTarget::Field(f.id.clone()))
                    .with_detail(item.field_value(f).map(|v| v.display()).unwrap_or_default())
            })
            .collect();
        for (kind, label, target) in [
            (OpKind::Title, "Title", FieldTarget::Title),
            (OpKind::Assignees, "Assignees", FieldTarget::Assignees),
            (OpKind::Labels, "Labels", FieldTarget::Labels),
            (OpKind::Milestone, "Milestone", FieldTarget::Milestone),
        ] {
            if self.allowed(kind, &item) {
                let current = match kind {
                    OpKind::Title => item.title().to_string(),
                    OpKind::Assignees => item.assignees().join(", "),
                    OpKind::Labels => item.label_names().join(", "),
                    _ => item
                        .content_fields
                        .milestone
                        .as_ref()
                        .map(|m| m.title.clone())
                        .unwrap_or_default(),
                };
                choices.push(Choice::new(label, target).with_detail(current));
            }
        }
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

    /// `a`, `t`, or the milestone entry: a popover over the item's repository candidates,
    /// loaded on first need.
    pub(crate) fn open_candidates(&mut self, kind: OpKind, from_menu: bool) -> Vec<Command> {
        let Some(item) = self.action_item().cloned() else {
            self.status.flash = Some("Select an item first".into());
            return Vec::new();
        };
        if let Err(reason) = self.check_here(kind, &item) {
            self.status.flash = Some(reason);
            return Vec::new();
        }
        let Some(repo) = self.candidate_repo(&item) else {
            self.status.flash = Some(if item.reference().is_some() {
                "Can't tell this item's repository; refresh with r".into()
            } else {
                "Open the board from a repository, or link one to the project, to assign drafts"
                    .into()
            });
            return Vec::new();
        };
        let title = |what: &str| format!("{what} · {}", item_ref(&item));
        let (id, r) = (item.id.clone(), repo.clone());
        let popup = match kind {
            OpKind::Assignees => Popup::Assignees {
                item: id,
                repo: r,
                from_menu,
                select: MultiSelect::loading(title("Assignees")),
            },
            OpKind::Labels => Popup::Labels {
                item: id,
                repo: r,
                from_menu,
                select: MultiSelect::loading(title("Labels")),
            },
            _ => Popup::Milestone {
                item: id,
                repo: r,
                from_menu,
                picker: ListPicker::loading(title("Milestone")),
            },
        };
        self.open_popup(popup);
        let cmds = self.need_candidates(&repo);
        self.refresh_popup();
        cmds
    }

    /// Submits an op that needs the item's content node id; refuses (needs a refresh) without.
    fn submit_content_op(
        &mut self,
        kind: OpKind,
        item: ItemId,
        op: impl FnOnce(ContentId) -> Op,
    ) -> Vec<Command> {
        match self.item(&item).and_then(|i| i.content_id()) {
            Some(content) => self.submit(kind, op(content)),
            None => {
                self.status.flash = Some(self.denial_text(Denial::NeedsRefresh));
                Vec::new()
            }
        }
    }

    /// Enter or Esc in the assignee or label checklist: Esc closes; Enter sends only what
    /// changed (`op` builds the op from the item, its content id, added, removed and the whole
    /// new list). `None` while the checklist stays open.
    fn apply_checklist<T>(
        &mut self,
        outcome: MultiOutcome<T>,
        item: ItemId,
        kind: OpKind,
        back: Option<FieldTarget>,
        op: impl FnOnce(ItemId, ContentId, Vec<T>, Vec<T>, Vec<T>) -> Op,
    ) -> Option<Vec<Command>> {
        match outcome {
            MultiOutcome::None => None,
            MultiOutcome::Cancel => Some(match back {
                Some(target) => self.back_to_menu(&target),
                None => {
                    self.close_popup();
                    Vec::new()
                }
            }),
            MultiOutcome::Apply {
                added,
                removed,
                chosen,
            } => {
                self.close_popup();
                if added.is_empty() && removed.is_empty() {
                    return Some(Vec::new());
                }
                self.selected_id = Some(item.clone());
                let target = item.clone();
                Some(self.submit_content_op(kind, target, |content| {
                    op(item, content, added, removed, chosen)
                }))
            }
        }
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
                ListOutcome::Chosen(MenuEntry::Milestone) => {
                    self.close_popup();
                    return self.open_candidates(OpKind::Milestone, false);
                }
                ListOutcome::Chosen(MenuEntry::EditTitle) => {
                    self.close_popup();
                    return self.edit_title();
                }
                ListOutcome::Chosen(MenuEntry::EditBody) => {
                    self.close_popup();
                    return self.edit_body();
                }
                ListOutcome::Chosen(MenuEntry::Remove) => {
                    self.close_popup();
                    return self.confirm_remove();
                }
                ListOutcome::Chosen(MenuEntry::ToggleArchived) => {
                    self.close_popup();
                    return self.toggle_archived();
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
                ListOutcome::Chosen(FieldTarget::Title) => {
                    // The editor is a program of its own: the menu closes rather than
                    // waiting behind it.
                    self.close_popup();
                    return self.edit_title();
                }
                ListOutcome::Chosen(FieldTarget::Assignees) => {
                    return self.open_candidates(OpKind::Assignees, true);
                }
                ListOutcome::Chosen(FieldTarget::Labels) => {
                    return self.open_candidates(OpKind::Labels, true);
                }
                ListOutcome::Chosen(FieldTarget::Milestone) => {
                    return self.open_candidates(OpKind::Milestone, true);
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
                    let target = FieldTarget::Field(field.clone());
                    return self.back_to_menu(&target);
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
                    let target = FieldTarget::Field(field.clone());
                    return self.back_to_menu(&target);
                }
                MultiOutcome::None => {}
            },
            Popup::Assignees {
                item,
                select,
                from_menu,
                ..
            } => {
                let back = from_menu.then_some(FieldTarget::Assignees);
                let (item, outcome) = (item.clone(), select.handle(&key));
                let built = |item, content, add, remove, all| Op::Assignees {
                    item,
                    content,
                    add,
                    remove,
                    all,
                };
                if let Some(cmds) =
                    self.apply_checklist(outcome, item, OpKind::Assignees, back, built)
                {
                    return cmds;
                }
            }
            Popup::Labels {
                item,
                select,
                from_menu,
                ..
            } => {
                let back = from_menu.then_some(FieldTarget::Labels);
                let (item, outcome) = (item.clone(), select.handle(&key));
                let built = |item, content, add, remove, _all| Op::Labels {
                    item,
                    content,
                    add,
                    remove,
                };
                if let Some(cmds) = self.apply_checklist(outcome, item, OpKind::Labels, back, built)
                {
                    return cmds;
                }
            }
            Popup::Milestone {
                item,
                picker,
                from_menu,
                ..
            } => match picker.handle(&key) {
                ListOutcome::Chosen(milestone) => {
                    let item = item.clone();
                    self.close_popup();
                    let current = self
                        .item(&item)
                        .and_then(|i| i.content_fields.milestone.clone());
                    if current.as_ref().map(|m| &m.id) == milestone.as_ref().map(|m| &m.id) {
                        return Vec::new();
                    }
                    self.selected_id = Some(item.clone());
                    let target = item.clone();
                    return self.submit_content_op(OpKind::Milestone, target, |content| {
                        Op::Milestone {
                            item,
                            content,
                            milestone,
                        }
                    });
                }
                ListOutcome::Cancel => {
                    if *from_menu {
                        return self.back_to_menu(&FieldTarget::Milestone);
                    }
                    self.close_popup();
                    return Vec::new();
                }
                ListOutcome::None | ListOutcome::Key(_) => {}
            },
            Popup::CloseReason { item, picker } => match picker.handle(&key) {
                ListOutcome::Chosen(reason) => {
                    let item = item.clone();
                    self.close_popup();
                    // Choosing a reason is the confirmation (Decision 18).
                    let target = item.clone();
                    return self.submit_content_op(OpKind::Close, target, |content| Op::Close {
                        item,
                        content,
                        reason: Some(reason),
                    });
                }
                ListOutcome::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                ListOutcome::None | ListOutcome::Key(_) => {}
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
                    let target = FieldTarget::Field(field.clone());
                    return self.back_to_menu(&target);
                }
                LineOutcome::Typing => *error = None,
            },
        }
        self.popup = Some(popup);
        Vec::new()
    }

    /// Esc in a value editor: back to the field menu, on the field that was being edited.
    fn back_to_menu(&mut self, target: &FieldTarget) -> Vec<Command> {
        self.close_popup();
        let cmds = self.open_field_menu();
        if let Some(Popup::Fields { picker, .. }) = &mut self.popup
            && let Some(at) = picker.choices.iter().position(|c| c.value == *target)
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
                "Status",
                "Priority",
                "Notes",
                "Size",
                "Due",
                "Sprint",
                "Areas",
                "Title",
                "Assignees",
                "Labels",
                "Milestone"
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
    fn a_read_only_board_offers_show_archived_and_the_browser_and_keys_say_why() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key(' ')).is_empty());
        assert_eq!(
            menu(&a),
            [
                ("Open in the browser".to_string(), "o".to_string()),
                ("Show archived items".to_string(), String::new()),
            ],
            "reading a board can still show archived items"
        );
        assert_eq!(a.edit_hints(), None);
        assert_eq!(a.actions_hint(), None);
        a.handle_key(code(KeyCode::Esc));
        assert!(a.handle_key(key('>')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
    }

    #[test]
    fn nothing_to_do_with_the_item_leaves_only_the_boards_entries() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key(' ')).is_empty());
        assert_eq!(
            menu(&a),
            [("Show archived items".to_string(), String::new())]
        );
        a.handle_key(code(KeyCode::Esc));
        a.handle_key(key('e'));
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
        let hints = a.edit_hints().unwrap().join(" · ");
        assert!(hints.contains("E field"), "{hints}");
        assert_eq!(a.actions_hint().as_deref(), Some("m actions"));
        a.handle_key(key('m'));
        assert!(menu(&a).contains(&("Edit a field".to_string(), "E".to_string())));
        a.handle_key(key('E'));
        assert!(matches!(a.popup, Some(Popup::Fields { .. })));
    }

    #[test]
    fn hints_list_the_keys_the_item_allows() {
        let a = test_app();
        let hints = a.edit_hints().unwrap().join(" · ");
        // Asserted piece by piece: later tasks insert `a assignees`, `c comment`, `X close`, ...
        assert!(
            hints.contains("e field · < > move · X close · c comment · J K reorder · A archive"),
            "{hints}"
        );
        assert!(hints.contains("e field"), "{hints}");
        assert_eq!(a.actions_hint().as_deref(), Some("space actions"));
        let mut a = a;
        let mut snap = a.snapshot().unwrap().clone();
        snap.items.get_mut(&ItemId::new("a")).unwrap().content =
            crate::model::ItemContent::Redacted;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        let hints = a.edit_hints().unwrap().join(" · ");
        assert!(hints.contains("< > move"), "{hints}");
        assert_eq!(a.actions_hint().as_deref(), Some("space actions"));
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
        let hints = a.edit_hints().unwrap().join(" · ");
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

    use crate::model::{RepoRef, RepoSlug};
    use crate::sync::SyncEvent;

    fn repo() -> RepoSlug {
        "tviles/t".parse().unwrap()
    }

    fn candidates() -> RepoCandidates {
        let user = |login: &str| UserRef {
            id: format!("U_{login}"),
            login: login.into(),
        };
        let label = |name: &str| LabelRef {
            id: format!("L_{name}"),
            name: name.into(),
            color: "0075ca".into(),
        };
        RepoCandidates {
            repo: RepoRef {
                id: "R_t".into(),
                slug: repo(),
            },
            users: vec![user("octo"), user("tviles")],
            labels: vec![label("bug"), label("Good First Issue"), label("🐛 triage")],
            milestones: vec![
                MilestoneRef {
                    id: "MI_1".into(),
                    title: "v1".into(),
                },
                MilestoneRef {
                    id: "MI_2".into(),
                    title: "v2".into(),
                },
            ],
            truncated: false,
        }
    }

    fn pending_op(a: &App) -> Op {
        a.store.pending().last().unwrap().op.clone()
    }

    #[test]
    fn a_opens_with_current_assignees_checked_and_loads_candidates_once() {
        let mut a = test_app();
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadCandidates(repo())]
        );
        assert!(matches!(&a.popup, Some(Popup::Assignees { select, .. }) if select.loading));
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
        let Some(Popup::Assignees { select, .. }) = &a.popup else {
            panic!("assignees")
        };
        assert_eq!(
            select.options[0].label, "tviles",
            "the current one, checked, first"
        );
        assert_eq!(select.checked, [true, false]);
        a.handle_key(code(KeyCode::Down));
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        let Op::Assignees {
            add,
            remove,
            all,
            content,
            ..
        } = pending_op(&a)
        else {
            panic!("assignees op")
        };
        assert_eq!((add.len(), remove.len(), all.len()), (1, 0, 2));
        assert_eq!(add[0].login, "octo");
        assert_eq!(content, crate::model::ContentId::Issue("I_a".into()));
        assert_eq!(
            a.item(&ItemId::new("a")).unwrap().assignees(),
            ["tviles", "octo"]
        );
        a.handle_key(key('j'));
        a.handle_key(key('j'));
        assert!(a.handle_key(key('a')).is_empty(), "same repository: cached");
        assert!(matches!(&a.popup, Some(Popup::Assignees { select, .. }) if !select.loading));
    }

    /// Review Focus 4.
    #[test]
    fn labels_with_spaces_and_emoji_are_prechecked_and_found() {
        let mut a = test_app();
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
        a.handle_key(key('t'));
        let Some(Popup::Labels { select, .. }) = &a.popup else {
            panic!("labels")
        };
        assert_eq!(
            (select.options[0].label.as_str(), select.checked[0]),
            ("bug", true)
        );
        type_text(&mut a, "good");
        a.handle_key(key(' '));
        a.handle_key(code(KeyCode::Enter));
        let Op::Labels { add, remove, .. } = pending_op(&a) else {
            panic!("labels op")
        };
        assert_eq!(
            (add[0].name.as_str(), remove.len()),
            ("Good First Issue", 0)
        );
        a.handle_key(key('t'));
        type_text(&mut a, "TRI");
        let Some(Popup::Labels { select, .. }) = &a.popup else {
            panic!("labels")
        };
        assert_eq!(select.visible().len(), 1, "🐛 triage");
    }

    #[test]
    fn an_unchanged_set_sends_nothing_and_esc_cancels() {
        let mut a = test_app();
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
        a.handle_key(key('t'));
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!((a.mode, a.pending_count()), (Mode::Normal, 0));
        a.handle_key(key('a'));
        a.handle_key(code(KeyCode::Esc));
        assert_eq!((a.mode, a.pending_count()), (Mode::Normal, 0));
    }

    #[test]
    fn a_current_label_outside_the_candidates_can_still_be_removed() {
        let mut a = test_app();
        let mut few = candidates();
        few.labels.retain(|l| l.name != "bug");
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: few,
        });
        a.handle_key(key('t'));
        a.handle_key(key(' '));
        a.handle_key(code(KeyCode::Enter));
        let Op::Labels { remove, .. } = pending_op(&a) else {
            panic!("labels op")
        };
        assert_eq!(
            (remove[0].name.as_str(), remove[0].id.as_str()),
            ("bug", "L_bug")
        );
    }

    #[test]
    fn drafts_have_no_labels_but_take_assignees_from_the_pane_repository() {
        let mut a = test_app();
        a.pane_repo = Some(repo());
        for _ in 0..3 {
            a.handle_key(key('j'));
        }
        assert_eq!(a.selected_item().unwrap().id.as_str(), "d");
        assert!(a.handle_key(key('t')).is_empty());
        assert!(
            a.status
                .flash
                .as_deref()
                .unwrap()
                .starts_with("Drafts have no labels — convert to issue first")
        );
        assert_eq!(
            a.handle_key(key('a')),
            vec![Command::LoadCandidates(repo())]
        );
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
        a.handle_key(key(' '));
        a.handle_key(code(KeyCode::Enter));
        let Op::Assignees { content, all, .. } = pending_op(&a) else {
            panic!("assignees op")
        };
        assert_eq!(content, crate::model::ContentId::Draft("DI_d".into()));
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn a_failed_candidate_load_shows_in_the_popover() {
        let mut a = test_app();
        a.handle_key(key('a'));
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Candidates(repo()),
            error: crate::github::GithubError::Network("offline".into()),
        });
        let Some(Popup::Assignees { select, .. }) = &a.popup else {
            panic!("assignees")
        };
        assert!(select.error.as_deref().unwrap().contains("offline"));
        assert!(!select.loading);
    }

    #[test]
    fn truncated_candidates_say_so_in_the_popover() {
        let mut a = test_app();
        let mut many = candidates();
        many.truncated = true;
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: many,
        });
        a.handle_key(key('t'));
        let Some(Popup::Labels { select, .. }) = &a.popup else {
            panic!("labels")
        };
        assert!(select.truncated);
    }

    #[test]
    fn the_popover_titles_name_the_item_by_number() {
        let mut a = test_app();
        a.handle_key(key('a'));
        let Some(Popup::Assignees { select, .. }) = &a.popup else {
            panic!("assignees")
        };
        assert!(
            select.title.starts_with("Assignees · #"),
            "{}",
            select.title
        );
    }

    #[test]
    fn the_milestone_picker_sets_and_clears() {
        let mut a = test_app();
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
        a.handle_key(key('e'));
        type_text(&mut a, "milestone");
        a.handle_key(code(KeyCode::Enter));
        assert!(matches!(a.popup, Some(Popup::Milestone { .. })));
        type_text(&mut a, "v2");
        a.handle_key(code(KeyCode::Enter));
        assert!(
            matches!(pending_op(&a), Op::Milestone { milestone: Some(m), .. } if m.title == "v2")
        );
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        let at = picker
            .choices
            .iter()
            .position(|c| c.label == "Milestone")
            .expect("listed");
        for _ in 0..at {
            a.handle_key(key('j'));
        }
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "no milestone");
        a.handle_key(code(KeyCode::Enter));
        assert!(matches!(
            pending_op(&a),
            Op::Milestone {
                milestone: None,
                ..
            }
        ));
    }

    fn replace_item(a: &mut App, edit: impl FnOnce(&mut crate::model::Item)) {
        let mut snap = a.snapshot().unwrap().clone();
        edit(snap.items.get_mut(&ItemId::new("a")).unwrap());
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
    }

    fn ready(a: &mut App) {
        a.on_sync(SyncEvent::Candidates {
            repo: repo(),
            candidates: candidates(),
        });
    }

    #[test]
    fn esc_from_a_built_in_opened_via_e_goes_back_to_the_menu_on_its_entry() {
        for (name, target) in [
            ("assignees", FieldTarget::Assignees),
            ("labels", FieldTarget::Labels),
            ("milestone", FieldTarget::Milestone),
        ] {
            let mut a = test_app();
            ready(&mut a);
            a.handle_key(key('e'));
            type_text(&mut a, name);
            a.handle_key(code(KeyCode::Enter));
            assert!(!matches!(a.popup, Some(Popup::Fields { .. })), "{name}");
            a.handle_key(code(KeyCode::Esc));
            let Some(Popup::Fields { picker, .. }) = &a.popup else {
                panic!("{name}: back to the field menu")
            };
            assert_eq!(picker.current().unwrap().value, target, "{name}");
            a.handle_key(code(KeyCode::Esc));
            assert!(a.popup.is_none());
        }
    }

    #[test]
    fn esc_from_a_and_t_closes_and_returns_to_the_detail() {
        for key_char in ['a', 't'] {
            for detail in [false, true] {
                let mut a = test_app();
                ready(&mut a);
                if detail {
                    a.handle_key(code(KeyCode::Enter));
                }
                let base = a.mode;
                a.handle_key(key(key_char));
                assert_eq!(a.mode, Mode::Popup);
                a.handle_key(code(KeyCode::Esc));
                assert!(a.popup.is_none(), "closed, not the menu");
                assert_eq!(a.mode, base);
            }
        }
        let mut a = test_app();
        ready(&mut a);
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('a'));
        a.handle_key(code(KeyCode::Down));
        a.handle_key(key(' '));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert_eq!(a.mode, Mode::Detail);
    }

    #[test]
    fn a_milestone_opened_from_the_space_menu_closes_on_esc() {
        let mut a = test_app();
        ready(&mut a);
        a.handle_key(key(' '));
        let Some(Popup::Actions { picker, .. }) = &a.popup else {
            panic!("menu")
        };
        let at = picker
            .choices
            .iter()
            .position(|c| c.label == "Milestone")
            .unwrap();
        for _ in 0..at {
            a.handle_key(key('j'));
        }
        a.handle_key(code(KeyCode::Enter));
        assert!(matches!(a.popup, Some(Popup::Milestone { .. })));
        a.handle_key(code(KeyCode::Esc));
        assert!(a.popup.is_none());
    }

    #[test]
    fn a_renamed_current_milestone_is_matched_by_id() {
        let mut a = test_app();
        replace_item(&mut a, |i| {
            i.content_fields.milestone = Some(MilestoneRef {
                id: "MI_1".into(),
                title: "old name".into(),
            });
        });
        ready(&mut a);
        a.handle_key(key('e'));
        type_text(&mut a, "milestone");
        a.handle_key(code(KeyCode::Enter));
        let Some(Popup::Milestone { picker, .. }) = &a.popup else {
            panic!("milestone")
        };
        let titles: Vec<_> = picker.choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(titles, ["v1", "v2", "No milestone"], "no duplicate row");
        assert_eq!(picker.choices[0].detail, "✓");
        assert_eq!(picker.current().unwrap().label, "v1");
        // Choosing it again is a no-op even though the title differs.
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn an_item_that_leaves_while_candidates_load_stops_the_spinner() {
        for key_char in ['a', 't'] {
            let mut a = test_app();
            a.handle_key(key(key_char));
            let mut snap = a.snapshot().unwrap().clone();
            snap.items.remove(&ItemId::new("a"));
            a.store.apply(crate::store::StoreUpdate::Replace(snap));
            ready(&mut a);
            let (loading, error) = match &a.popup {
                Some(Popup::Assignees { select, .. }) => (select.loading, select.error.clone()),
                Some(Popup::Labels { select, .. }) => (select.loading, select.error.clone()),
                other => panic!("{other:?}"),
            };
            assert!(!loading);
            assert_eq!(
                error.as_deref(),
                Some("This item is no longer on the board")
            );
        }
    }

    #[test]
    fn a_non_draft_with_an_unreadable_repository_is_not_blamed_on_drafts() {
        let mut a = test_app();
        replace_item(&mut a, |i| {
            if let crate::model::ItemContent::Issue { reference, .. } = &mut i.content {
                reference.repo = "not a slug".into();
            }
        });
        assert!(a.handle_key(key('a')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Can't tell this item's repository; refresh with r")
        );
    }

    #[test]
    fn labels_with_spaces_and_emoji_are_prechecked_when_the_item_has_them() {
        let mut a = test_app();
        replace_item(&mut a, |i| {
            i.values.insert(
                FieldId::new("F_labels"),
                FieldValue::Labels(vec![
                    crate::model::Label {
                        name: "Good First Issue".into(),
                        color: "0075ca".into(),
                    },
                    crate::model::Label {
                        name: "🐛 triage".into(),
                        color: "0075ca".into(),
                    },
                ]),
            );
        });
        ready(&mut a);
        a.handle_key(key('t'));
        let Some(Popup::Labels { select, .. }) = &a.popup else {
            panic!("labels")
        };
        let on: Vec<_> = select
            .options
            .iter()
            .zip(&select.checked)
            .filter(|(_, c)| **c)
            .map(|(o, _)| o.label.as_str())
            .collect();
        assert_eq!(on, ["Good First Issue", "🐛 triage"]);
        a.handle_key(code(KeyCode::Enter));
        assert_eq!(a.pending_count(), 0, "unchanged sends nothing");
    }

    #[test]
    fn title_in_the_e_menu_opens_the_editor_and_closes_the_menu() {
        use crate::ui::app::EditPurpose;
        let mut a = test_app();
        a.handle_key(key('e'));
        let Some(Popup::Fields { picker, .. }) = &a.popup else {
            unreachable!()
        };
        let at = picker
            .choices
            .iter()
            .position(|c| c.value == FieldTarget::Title)
            .unwrap();
        assert_eq!(picker.choices[at].detail, "Fix crash");
        for _ in 0..at {
            a.handle_key(code(KeyCode::Down));
        }
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Edit {
                purpose: EditPurpose::Title(ItemId::new("a")),
                initial: "Fix crash".into()
            }]
        );
        assert!(a.popup.is_none());
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn the_action_menu_offers_comment_edit_title_and_edit_body() {
        use crate::ui::app::EditPurpose;
        let mut a = test_app();
        a.handle_key(key(' '));
        let rows = menu(&a);
        for row in [("Comment", "c"), ("Edit title", ""), ("Edit body", "")] {
            assert!(
                rows.iter().any(|(l, d)| l == row.0 && d == row.1),
                "{row:?} in {rows:?}"
            );
        }
        let at = rows.iter().position(|(l, _)| l == "Edit title").unwrap();
        for _ in 0..at {
            a.handle_key(code(KeyCode::Down));
        }
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Edit {
                purpose: EditPurpose::Title(ItemId::new("a")),
                initial: "Fix crash".into()
            }]
        );
        assert!(a.popup.is_none());
        a.handle_key(key(' '));
        let at = menu(&a).iter().position(|(l, _)| l == "Edit body").unwrap();
        for _ in 0..at {
            a.handle_key(code(KeyCode::Down));
        }
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LoadDetail {
                item: ItemId::new("a"),
                before: None
            }],
            "the body loads first"
        );
        assert_eq!(a.mode, Mode::Detail);
    }

    #[test]
    fn a_draft_offers_title_and_body_but_no_comment() {
        let mut a = test_app();
        replace_item(&mut a, |i| {
            i.content = crate::model::ItemContent::Draft {
                title: "Idea".into(),
            }
        });
        a.handle_key(key(' '));
        let rows = menu(&a);
        let labels: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        assert!(
            labels.contains(&"Edit title") && labels.contains(&"Edit body"),
            "{labels:?}"
        );
        assert!(!labels.contains(&"Comment"), "{labels:?}");
        a.handle_key(code(KeyCode::Esc));
        assert!(a.handle_key(key('c')).is_empty());
        assert!(a.status.flash.is_some(), "c says why");
    }
}
