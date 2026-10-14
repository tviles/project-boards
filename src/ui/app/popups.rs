//! The popovers: what each one edits and what its keys do.

use super::*;
use crate::model::{ContentId, LabelRef, MilestoneRef, RepoCandidates, UserRef};
use crate::model::{
    FieldValue, Iteration, OptionId, format_iso_date, parse_date_input, parse_iso_date,
    parse_number_input,
};
use crate::ops::{NewItem, Op};
use crate::ui::multiselect::{MultiOption, MultiOutcome, MultiSelect};
use crate::ui::popup::{Choice, LineOutcome, ListOutcome, ListPicker, edit_line};
use crate::ui::textform::{FormEvent, FormKind, TextForm};

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
    /// The title, in the text form.
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
    /// The title in the text form (`e` reaches it too).
    EditTitle,
    /// The body in the text form; it has no key.
    EditBody,
    /// Removes the item from the project, after asking; it has no key.
    Remove,
    /// Shows or hides archived items (`is:archived` in the session filter).
    ToggleArchived,
}

/// What `n` adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewKind {
    Issue,
    Draft,
    Existing,
}

/// What a chosen repository is for.
#[derive(Debug, Clone, PartialEq)]
pub enum RepoPurpose {
    NewIssue { presets: Vec<(FieldId, FieldValue)> },
    Convert { item: ItemId },
}

// The text form is far larger than the other popovers, but there is only ever one popover,
// so boxing it would save nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Popup {
    /// `n`: an issue or a draft; `presets` are the selected column's and lane's values.
    NewKind {
        presets: Vec<(FieldId, FieldValue)>,
        picker: ListPicker<NewKind>,
    },
    /// `n`, "add existing": a search (or a pasted URL) whose hits can be added. `searched` is
    /// the query the hits answer; Enter searches when the input differs from it, else adds.
    Search {
        presets: Vec<(FieldId, FieldValue)>,
        input: String,
        searched: Option<String>,
        hits: Vec<ContentHit>,
        selected: usize,
        loading: bool,
        error: Option<String>,
    },
    /// Which repository a new issue goes to. `wanted` is a typed `owner/name` being looked up.
    Repo {
        purpose: RepoPurpose,
        picker: ListPicker<RepoRef>,
        wanted: Option<RepoSlug>,
    },
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
    /// A title, body, comment or new item typed in the text form (Part C). `purpose` is what
    /// submitting it does, the same as the editor's text for that purpose.
    Text {
        purpose: EditPurpose,
        form: TextForm,
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

/// What a text form submits: what the editor would have saved. A new item with no title
/// submits nothing, so its body never becomes the title.
fn submitted_text(form: &TextForm) -> String {
    match form.kind() {
        FormKind::Title => form.title_text(),
        FormKind::NewItem if form.title_text().trim().is_empty() => String::new(),
        _ => form.editor_text(),
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

    /// The values a new item starts with: on a board, the selected column's and the selected
    /// card's swimlane (spec section 6, "Where a new item lands"); none in a table.
    pub(crate) fn new_item_presets(&self) -> Vec<(FieldId, FieldValue)> {
        if self.layout().0 != Layout::Board {
            return Vec::new();
        }
        let (Some(field), Some(view), Some(project)) =
            (self.column_field(), self.current_view(), self.project())
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(column) = self.board_columns().get(self.board_sel.column)
            && let Some(key) = &column.bucket.key
            && let Some(value) = field.value_for_bucket(key)
        {
            out.push((field.id.clone(), value));
        }
        if let (Some(lane), Some(item)) = (view.group_field(&project.fields), self.selected_item())
            && lane.kind.can_be_column()
            && let Some(value) = item.value(&lane.id)
        {
            out.push((lane.id.clone(), value.clone()));
        }
        out
    }

    /// `n`: what to add.
    pub(crate) fn open_new(&mut self) -> Vec<Command> {
        if let Err(denial) = check_new(false, &self.edit_context()) {
            self.status.flash = Some(self.denial_text(denial));
            return Vec::new();
        }
        let presets = self.new_item_presets();
        self.open_new_kind(presets);
        Vec::new()
    }

    /// Where "add existing" searches by default: the pane's repository and the project's
    /// linked ones (spec section 6, "Adding and creating items"). `repo:` in the query widens it.
    pub(crate) fn search_scope(&self) -> Vec<RepoSlug> {
        let mut scope: Vec<RepoSlug> = self.pane_repo.iter().cloned().collect();
        if let Some(p) = self.project() {
            for r in &p.linked_repos {
                if !scope.contains(&r.slug) {
                    scope.push(r.slug.clone());
                }
            }
        }
        scope
    }

    /// The board item that is this issue or pull request, if it is on the board.
    pub(crate) fn on_board(&self, hit: &ContentHit) -> Option<ItemId> {
        self.snapshot()?
            .items
            .values()
            .find(|i| {
                i.reference().is_some_and(|r| {
                    r.repo.eq_ignore_ascii_case(&hit.repo) && r.number == hit.number
                })
            })
            .map(|i| i.id.clone())
    }

    /// Adds the hit, or selects it when it is on the board already.
    fn add_existing(
        &mut self,
        hit: ContentHit,
        presets: Vec<(FieldId, FieldValue)>,
    ) -> Vec<Command> {
        if let Some(id) = self.on_board(&hit) {
            self.status.flash = Some(format!(
                "{}#{} is already on the board",
                hit.repo, hit.number
            ));
            self.selected_id = Some(id);
            self.restore_selection();
            return Vec::new();
        }
        let temp = self.next_temp();
        self.submit_new(
            Op::Create {
                temp,
                new: NewItem::Existing { hit },
                presets,
            },
            false,
        )
    }

    fn open_new_kind(&mut self, presets: Vec<(FieldId, FieldValue)>) {
        let picker = ListPicker::new(
            "New item",
            vec![
                Choice::new("New issue", NewKind::Issue),
                Choice::new("New draft", NewKind::Draft),
                Choice::new("Add an existing issue or pull request", NewKind::Existing),
            ],
        );
        self.open_popup(Popup::NewKind { presets, picker });
    }

    /// Repositories to pick from, in Decision 23's order and deterministic: the pane's
    /// repository, the project's linked ones, repositories found by typing `owner/name`
    /// (sorted by name), then those the user can push to (most recently pushed first). Each
    /// is listed once, at its first place. The flag says the list is still loading.
    pub(crate) fn repo_choices(&self) -> (Vec<Choice<RepoRef>>, bool) {
        let mut out: Vec<Choice<RepoRef>> = Vec::new();
        let mut push = |repo: &RepoRef, detail: &str| {
            if !out.iter().any(|c| c.value.slug == repo.slug) {
                out.push(Choice::new(repo.slug.to_string(), repo.clone()).with_detail(detail));
            }
        };
        if let Some(pane) = &self.pane_repo
            && let Some(Lookup::Ready(Some(repo))) = self.repo_lookups.get(pane)
        {
            push(repo, "this pane's repository");
        }
        if let Some(project) = self.project() {
            for repo in &project.issue_repos {
                push(repo, "linked");
            }
        }
        let mut found: Vec<&RepoRef> = self
            .repo_lookups
            .values()
            .filter_map(|l| match l {
                Lookup::Ready(Some(repo)) => Some(repo),
                _ => None,
            })
            .collect();
        found.sort_by(|a, b| a.slug.cmp(&b.slug));
        for repo in found {
            push(repo, "");
        }
        if let Some(Lookup::Ready(list)) = &self.repos {
            for repo in &list.repos {
                push(repo, "");
            }
        }
        let loading = matches!(self.repos, Some(Lookup::Loading) | None);
        (out, loading)
    }

    pub(crate) fn open_repo_picker(&mut self, purpose: RepoPurpose) -> Vec<Command> {
        let mut cmds = self.need_repos();
        if let Some(pane) = self.pane_repo.clone() {
            cmds.extend(self.need_repo_lookup(&pane));
        }
        let title = match &purpose {
            RepoPurpose::NewIssue { .. } => "Repository for the new issue",
            RepoPurpose::Convert { .. } => "Convert to an issue in…",
        };
        self.open_popup(Popup::Repo {
            purpose,
            picker: ListPicker::loading(title),
            wanted: None,
        });
        self.refresh_popup();
        cmds
    }

    fn repo_chosen(&mut self, purpose: RepoPurpose, repo: RepoRef) -> Vec<Command> {
        match purpose {
            RepoPurpose::NewIssue { presets } => self.new_issue_form(repo, presets),
            RepoPurpose::Convert { item } => {
                self.submit(OpKind::Convert, Op::Convert { item, repo })
            }
        }
    }

    /// The placeholder id of the next new item.
    pub(crate) fn next_temp(&mut self) -> ItemId {
        self.temp_counter += 1;
        ItemId::new(format!("local:{}", self.temp_counter))
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
            if check_new(false, &self.edit_context()).is_ok() {
                choices.push(
                    Choice::new("New item", MenuEntry::Key(Action::New))
                        .with_detail(key(Action::New)),
                );
            }
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
            Popup::NewKind { presets, picker } => match picker.handle(&key) {
                ListOutcome::Chosen(kind) => {
                    let presets = presets.clone();
                    self.close_popup();
                    return match kind {
                        NewKind::Draft => self.new_draft_form(presets),
                        NewKind::Existing => {
                            self.open_popup(Popup::Search {
                                presets,
                                input: String::new(),
                                searched: None,
                                hits: Vec::new(),
                                selected: 0,
                                loading: false,
                                error: None,
                            });
                            Vec::new()
                        }
                        NewKind::Issue => match check_new(true, &self.edit_context()) {
                            Ok(()) => self.open_repo_picker(RepoPurpose::NewIssue { presets }),
                            Err(denial) => {
                                self.status.flash = Some(self.denial_text(denial));
                                Vec::new()
                            }
                        },
                    };
                }
                ListOutcome::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                ListOutcome::None | ListOutcome::Key(_) => {}
            },
            Popup::Repo {
                purpose,
                picker,
                wanted,
            } => {
                // A typed owner/name that is not listed is looked up; once found it is listed
                // and Enter picks it.
                if key.code == KeyCode::Enter
                    && let Ok(slug) = picker.input.trim().parse::<RepoSlug>()
                {
                    // Typed in full, `owner/name` means that repository, not whatever the
                    // fuzzy filter happens to highlight.
                    let same = |s: &RepoSlug| s.to_string().eq_ignore_ascii_case(&slug.to_string());
                    if let Some(found) = picker.choices.iter().find(|c| same(&c.value.slug)) {
                        let repo = found.value.clone();
                        let purpose = purpose.clone();
                        self.close_popup();
                        return self.repo_chosen(purpose, repo);
                    }
                    *wanted = Some(slug.clone());
                    // A repository that was not found may exist now: ask again.
                    if matches!(self.repo_lookups.get(&slug), Some(Lookup::Ready(None))) {
                        self.repo_lookups.remove(&slug);
                    }
                    self.popup = Some(popup);
                    let cmds = self.need_repo_lookup(&slug);
                    self.refresh_popup();
                    return cmds;
                }
                match picker.handle(&key) {
                    ListOutcome::Chosen(repo) => {
                        let purpose = purpose.clone();
                        self.close_popup();
                        return self.repo_chosen(purpose, repo);
                    }
                    ListOutcome::Cancel => {
                        // One level back: the issue-or-draft question, same presets. A
                        // conversion has no earlier step: Esc closes.
                        if let RepoPurpose::NewIssue { presets } = purpose.clone() {
                            self.open_new_kind(presets);
                        } else {
                            self.close_popup();
                        }
                        return Vec::new();
                    }
                    ListOutcome::None | ListOutcome::Key(_) => {}
                }
            }
            Popup::Search {
                presets,
                input,
                searched,
                hits,
                selected,
                loading,
                error,
            } => match key.code {
                KeyCode::Esc => {
                    // One level back: the kind picker, same presets.
                    let presets = presets.clone();
                    self.open_new_kind(presets);
                    return Vec::new();
                }
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(hits.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let query = input.trim().to_string();
                    let answered = searched.as_deref() == Some(query.as_str())
                        && error.is_none()
                        && !hits.is_empty();
                    if query.is_empty() || (*loading && searched.as_deref() == Some(query.as_str()))
                    {
                    } else if !answered {
                        *searched = Some(query.clone());
                        *loading = true;
                        *error = None;
                        hits.clear();
                        *selected = 0;
                        self.popup = Some(popup);
                        let scope = self.search_scope();
                        return self.start_search(&query, scope);
                    } else if let Some(hit) = hits.get(*selected).cloned() {
                        let presets = presets.clone();
                        self.close_popup();
                        return self.add_existing(hit, presets);
                    }
                }
                _ => {
                    edit_line(input, &key);
                    *error = None;
                }
            },
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
                    // The title form replaces the menu.
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
            Popup::Text { purpose, form } => match form.handle_key(key) {
                FormEvent::None => {}
                FormEvent::Cancel => {
                    self.close_popup();
                    return Vec::new();
                }
                FormEvent::OpenEditor => {
                    // The form stays open underneath; the editor's text comes back into it.
                    let edit = Command::Edit {
                        purpose: purpose.clone(),
                        initial: form.editor_text(),
                    };
                    self.popup = Some(popup);
                    return vec![edit];
                }
                FormEvent::Submit => {
                    let cmds = self.on_text(purpose.clone(), submitted_text(form), false);
                    if cmds.iter().any(|c| matches!(c, Command::Send(_))) {
                        self.close_popup();
                        return cmds;
                    }
                    // Refused ("Empty title…"): the reason shows in the form, which keeps
                    // what was typed (decision C5).
                    form.set_message(self.status.flash.take());
                }
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
    fn title_in_the_e_menu_opens_the_title_form_and_closes_the_menu() {
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
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            text_form(&a),
            (EditPurpose::Title(ItemId::new("a")), "Fix crash".into())
        );
        a.handle_key(code(KeyCode::Esc));
        assert!(a.popup.is_none(), "the menu closed behind the form");
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
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            text_form(&a),
            (EditPurpose::Title(ItemId::new("a")), "Fix crash".into())
        );
        a.handle_key(code(KeyCode::Esc));
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

    use crate::ui::app::EditPurpose;

    /// The open text form's purpose and the text `Open in editor` would start from.
    fn text_form(a: &App) -> (EditPurpose, String) {
        match &a.popup {
            Some(Popup::Text { purpose, form }) => {
                let text = match form.kind() {
                    crate::ui::textform::FormKind::Title => form.title_text(),
                    _ => form.editor_text(),
                };
                (purpose.clone(), text)
            }
            other => panic!("expected the text form, got {other:?}"),
        }
    }

    fn alt_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::ALT)
    }

    #[test]
    fn a_new_draft_opens_a_form_and_an_empty_title_creates_nothing() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "draft");
        let Some(Popup::Text { form, .. }) = &a.popup else {
            panic!("expected the text form")
        };
        assert_eq!(form.heading(), "New draft");
        assert_eq!(form.kind(), crate::ui::textform::FormKind::NewItem);
        a.handle_key(code(KeyCode::Tab));
        type_text(&mut a, "only a body");
        assert!(a.handle_key(alt_enter()).is_empty());
        let Some(Popup::Text { form, .. }) = &a.popup else {
            panic!("the form stays open")
        };
        assert_eq!(form.message(), Some("Empty title; nothing was created"));
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn open_in_editor_on_a_new_item_sends_title_and_body_and_refills_both() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "draft");
        type_text(&mut a, "Idea");
        a.handle_key(code(KeyCode::Tab));
        type_text(&mut a, "Why");
        // Body, then [ Create ], then [ Open in editor ].
        a.handle_key(code(KeyCode::Tab));
        a.handle_key(code(KeyCode::Tab));
        let cmds = a.handle_key(code(KeyCode::Enter));
        let [Command::Edit { purpose, initial }] = &cmds[..] else {
            panic!("expected the editor, got {cmds:?}")
        };
        assert_eq!(initial, "Idea\n\nWhy");
        let purpose = purpose.clone();
        assert!(
            a.on_edited(purpose, Some("Better idea\n\nBecause\n".into()))
                .is_empty()
        );
        let Some(Popup::Text { form, .. }) = &a.popup else {
            panic!("the form stays open")
        };
        assert_eq!(
            (form.title_text().as_str(), form.body_text().as_str()),
            ("Better idea", "Because")
        );
        assert_eq!(a.pending_count(), 0);
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert!(matches!(
            &a.store.pending()[0].op,
            Op::Create { new: NewItem::Draft { title, body }, .. }
                if title == "Better idea" && body == "Because"
        ));
    }

    #[test]
    fn a_draft_title_form_says_draft() {
        let mut a = test_app();
        replace_item(&mut a, |i| {
            i.content = crate::model::ItemContent::Draft {
                title: "Idea".into(),
            }
        });
        a.edit_title();
        let Some(Popup::Text { form, .. }) = &a.popup else {
            panic!("expected the text form")
        };
        assert_eq!(form.heading(), "Edit title of Draft");
        assert_eq!(form.title_text(), "Idea");
    }

    fn choose(a: &mut App, label: &str) -> Vec<Command> {
        type_text(a, label);
        a.handle_key(code(KeyCode::Enter))
    }

    fn repo_ref(slug: &str) -> RepoRef {
        RepoRef {
            id: format!("R_{slug}"),
            slug: slug.parse().unwrap(),
        }
    }

    #[test]
    fn n_in_a_board_column_presets_its_column_and_lane() {
        let mut a = test_app();
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.handle_key(code(KeyCode::Home));
        a.handle_key(key('l'));
        assert_eq!(
            a.selected_item().unwrap().id.as_str(),
            "b",
            "In Progress, lane P1"
        );
        assert!(a.handle_key(key('n')).is_empty());
        assert!(choose(&mut a, "draft").is_empty());
        let (EditPurpose::NewDraft { presets }, initial) = text_form(&a) else {
            panic!("expected a new draft")
        };
        assert!(initial.trim().is_empty());
        let names: Vec<String> = presets
            .iter()
            .map(|(f, v)| format!("{}={}", f, v.display()))
            .collect();
        assert_eq!(names, ["F_status=In Progress", "F_prio=P1"]);
        type_text(&mut a, "Idea");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "A body");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert!(matches!(
            &a.store.pending()[0].op,
            Op::Create { new: NewItem::Draft { title, body }, presets: p, .. }
                if title == "Idea" && body == "A body" && *p == presets
        ));
        let temp = ItemId::new("local:1");
        assert_eq!(
            a.selected_item().unwrap().id,
            temp,
            "the new item is selected"
        );
        assert_eq!(a.highlight, Some(temp.clone()));
        assert_eq!(
            a.item(&temp)
                .unwrap()
                .value(&FieldId::new("F_status"))
                .map(|v| v.display())
                .as_deref(),
            Some("In Progress")
        );
        let mut created = a.item(&temp).unwrap().clone();
        created.id = ItemId::new("PVTI_new");
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(crate::ops::Confirmed::Created {
                temp,
                item: created,
                refused: vec![],
            }),
        });
        assert_eq!(
            a.selected_item().unwrap().id.as_str(),
            "PVTI_new",
            "the cursor follows the real item"
        );
    }

    #[test]
    fn a_new_issue_asks_for_the_repository_first() {
        let mut a = test_app();
        a.pane_repo = Some(repo());
        a.handle_key(key('n'));
        let cmds = choose(&mut a, "issue");
        assert_eq!(cmds, vec![Command::LoadRepos, Command::LookupRepo(repo())]);
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![repo_ref("acme/app"), repo_ref("tviles/t")],
            truncated: false,
        }));
        a.on_sync(SyncEvent::RepoFound {
            slug: repo(),
            repo: Some(repo_ref("tviles/t")),
        });
        let Some(Popup::Repo { picker, .. }) = &a.popup else {
            panic!("repository picker")
        };
        let rows: Vec<(&str, &str)> = picker
            .choices
            .iter()
            .map(|c| (c.label.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(
            rows,
            [("tviles/t", "this pane's repository"), ("acme/app", "")]
        );
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let (purpose, initial) = text_form(&a);
        assert_eq!(
            purpose,
            EditPurpose::NewIssue {
                repo: repo_ref("tviles/t"),
                presets: vec![]
            }
        );
        assert!(initial.trim().is_empty());
        let Some(Popup::Text { form, .. }) = &a.popup else {
            unreachable!()
        };
        assert_eq!(form.heading(), "New issue in tviles/t");
        type_text(&mut a, "Crash on start");
        a.handle_key(code(KeyCode::Enter));
        type_text(&mut a, "Steps");
        assert_eq!(a.handle_key(alt_enter()), vec![Command::Send(1)]);
        assert!(
            matches!(&a.store.pending()[0].op, Op::Create { new: crate::ops::NewItem::Issue { title, body, .. }, .. } if title == "Crash on start" && body == "Steps")
        );
    }

    #[test]
    fn a_typed_repository_is_looked_up_and_offered() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "issue");
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![],
            truncated: false,
        }));
        type_text(&mut a, "octo/dots");
        let octo: RepoSlug = "octo/dots".parse().unwrap();
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LookupRepo(octo.clone())]
        );
        a.on_sync(SyncEvent::RepoFound {
            slug: octo.clone(),
            repo: None,
        });
        let Some(Popup::Repo { picker, .. }) = &a.popup else {
            panic!("repository picker")
        };
        assert!(picker.error.as_deref().unwrap().contains("octo/dots"));
        a.repo_lookups.remove(&octo);
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LookupRepo(octo.clone())]
        );
        a.on_sync(SyncEvent::RepoFound {
            slug: octo.clone(),
            repo: Some(repo_ref("octo/dots")),
        });
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert!(
            matches!(text_form(&a), (EditPurpose::NewIssue { repo, .. }, _) if repo.slug == octo)
        );
    }

    /// Pre-flight ruling 21: Decision 23's order, the same every time.
    #[test]
    fn the_repository_picker_lists_pane_linked_found_then_pushable() {
        let mut a = test_app();
        a.pane_repo = Some(repo());
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.linked_repos = vec![
            repo_ref("acme/api"),
            repo_ref("tviles/t"),
            repo_ref("acme/ro"),
        ];
        snap.project.issue_repos = vec![repo_ref("acme/api"), repo_ref("tviles/t")];
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        for slug in ["zeta/z", "octo/dots", "tviles/t"] {
            let slug: RepoSlug = slug.parse().unwrap();
            a.repo_lookups.insert(
                slug.clone(),
                crate::ui::app::Lookup::Ready(Some(repo_ref(&slug.to_string()))),
            );
        }
        a.repos = Some(crate::ui::app::Lookup::Ready(PushableRepos {
            repos: vec![repo_ref("acme/app"), repo_ref("acme/api")],
            truncated: false,
        }));
        let (choices, loading) = a.repo_choices();
        let rows: Vec<(&str, &str)> = choices
            .iter()
            .map(|c| (c.label.as_str(), c.detail.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("tviles/t", "this pane's repository"),
                ("acme/api", "linked"),
                ("octo/dots", ""),
                ("zeta/z", ""),
                ("acme/app", ""),
            ]
        );
        assert!(!loading);
    }

    #[test]
    fn an_empty_new_item_creates_nothing_and_a_read_only_board_says_why() {
        let mut a = test_app();
        assert!(
            a.on_edited(
                EditPurpose::NewDraft { presets: vec![] },
                Some("\n\n".into())
            )
            .is_empty()
        );
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Empty title; nothing was created")
        );
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.viewer_can_update = false;
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        assert!(a.handle_key(key('n')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("This board is read-only for you")
        );
        assert!(a.popup.is_none());
    }

    #[test]
    fn a_refused_preset_is_reported_after_the_item_is_created() {
        let mut a = test_app();
        a.on_edited(
            EditPurpose::NewDraft { presets: vec![] },
            Some("Idea".into()),
        );
        let mut created = a.item(&ItemId::new("local:1")).unwrap().clone();
        created.id = ItemId::new("PVTI_new");
        let refused = vec![(
            FieldId::new("F_status"),
            crate::github::GithubError::Network("reset".into()),
        )];
        a.on_sync(SyncEvent::OpDone {
            id: 1,
            result: Ok(crate::ops::Confirmed::Created {
                temp: ItemId::new("local:1"),
                item: created,
                refused,
            }),
        });
        assert!(
            a.status
                .error
                .as_deref()
                .unwrap()
                .starts_with("Added 'Idea', but a field was not set")
        );
    }

    fn drafted(a: &mut App) -> Vec<(FieldId, FieldValue)> {
        a.handle_key(key('n'));
        choose(a, "draft");
        match text_form(a) {
            (EditPurpose::NewDraft { presets }, _) => presets,
            other => panic!("expected a new draft, got {other:?}"),
        }
    }

    #[test]
    fn esc_in_the_repository_picker_goes_back_one_level() {
        let mut a = test_app();
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.handle_key(code(KeyCode::Home));
        a.handle_key(key('l'));
        a.handle_key(key('n'));
        let before = a.popup.clone();
        choose(&mut a, "issue");
        assert!(matches!(a.popup, Some(Popup::Repo { .. })));
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.popup, before, "the same question, the same presets");
        assert_eq!(a.mode, Mode::Popup);
        a.handle_key(code(KeyCode::Esc));
        assert!(a.popup.is_none());
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn a_typed_name_looks_up_unless_a_listed_repository_matches_it_exactly() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "issue");
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![repo_ref("octo/website"), repo_ref("Octo/Dots")],
            truncated: false,
        }));
        type_text(&mut a, "octo/web");
        let web: RepoSlug = "octo/web".parse().unwrap();
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::LookupRepo(web)],
            "octo/website is only a fuzzy match"
        );
        assert!(matches!(a.popup, Some(Popup::Repo { .. })));
        for _ in 0.."octo/web".len() {
            a.handle_key(code(KeyCode::Backspace));
        }
        type_text(&mut a, "octo/dots");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let (purpose, _) = text_form(&a);
        assert!(
            matches!(&purpose, EditPurpose::NewIssue { repo, .. } if repo.id == "R_Octo/Dots"),
            "an exact match, in any case, is taken directly: {purpose:?}"
        );
    }

    #[test]
    fn a_repository_that_was_not_found_is_asked_for_again() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "issue");
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![],
            truncated: false,
        }));
        type_text(&mut a, "octo/dots");
        let octo: RepoSlug = "octo/dots".parse().unwrap();
        let ask = vec![Command::LookupRepo(octo.clone())];
        assert_eq!(a.handle_key(code(KeyCode::Enter)), ask);
        a.on_sync(SyncEvent::RepoFound {
            slug: octo.clone(),
            repo: None,
        });
        assert_eq!(a.handle_key(code(KeyCode::Enter)), ask, "Enter retries");
    }

    #[test]
    fn linked_repositories_without_write_access_or_issues_are_not_offered() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.linked_repos = vec![repo_ref("acme/ro"), repo_ref("acme/ok")];
        snap.project.issue_repos = vec![repo_ref("acme/ok")];
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        let (choices, _) = a.repo_choices();
        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["acme/ok"]);
    }

    #[test]
    fn the_truncated_note_is_dim_not_an_error() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "issue");
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![repo_ref("acme/app")],
            truncated: true,
        }));
        let Some(Popup::Repo { picker, .. }) = &a.popup else {
            panic!("repository picker")
        };
        assert!(picker.error.is_none());
        assert!(
            picker
                .note
                .as_deref()
                .unwrap()
                .starts_with("showing the first 500")
        );
    }

    #[test]
    fn a_table_gives_no_presets() {
        let mut a = test_app();
        assert!(drafted(&mut a).is_empty());
    }

    #[test]
    fn the_no_value_column_gives_no_column_preset() {
        let mut a = test_app();
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.board_sel.column = a.board_columns().len() - 1;
        let last = a.board_columns().pop().unwrap();
        assert!(last.bucket.key.is_none(), "No Status");
        let presets = drafted(&mut a);
        assert!(
            presets.iter().all(|(f, _)| f.as_str() != "F_status"),
            "{presets:?}"
        );
    }

    #[test]
    fn an_empty_column_still_presets_its_value() {
        let mut a = test_app();
        a.select_view(&crate::model::ViewId::new("V_board"));
        let cols = a.board_columns();
        let empty = cols
            .iter()
            .position(|c| c.items().is_empty() && c.bucket.key.is_some())
            .or_else(|| cols.iter().position(|c| c.bucket.key.is_some()))
            .unwrap();
        drop(cols);
        a.board_sel.column = empty;
        a.board_sel.index = 0;
        let presets = drafted(&mut a);
        assert_eq!(presets[0].0.as_str(), "F_status");
    }

    #[test]
    fn an_iteration_column_presets_its_iteration() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        for v in &mut snap.project.views {
            if v.id.as_str() == "V_board" {
                v.vertical_group_by = vec![FieldId::new("F_sprint")];
            }
        }
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.select_view(&crate::model::ViewId::new("V_board"));
        a.board_sel.column = 0;
        let presets = drafted(&mut a);
        assert_eq!(presets[0].0.as_str(), "F_sprint");
        assert!(
            matches!(presets[0].1, FieldValue::Iteration { .. }),
            "{presets:?}"
        );
    }

    #[test]
    fn a_merged_column_presets_its_canonical_option() {
        let mut a = test_app();
        let mut snap = a.snapshot().unwrap().clone();
        for f in &mut snap.project.fields {
            if let FieldKind::SingleSelect { options } = &mut f.kind
                && f.id.as_str() == "F_status"
            {
                let mut dup = options[0].clone();
                dup.id = OptionId::new("o_todo_twin");
                options.push(dup);
            }
        }
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.select_view(&crate::model::ViewId::new("V_board"));
        let cols = a.board_columns();
        let todo = cols.iter().position(|c| c.bucket.title == "Todo").unwrap();
        let key = cols[todo].bucket.key.clone().unwrap();
        drop(cols);
        assert_eq!(
            cols_len_of_todo(&a),
            1,
            "the two Todo options are one column"
        );
        a.board_sel.column = todo;
        let presets = drafted(&mut a);
        let FieldValue::SingleSelect { option_id, .. } = &presets[0].1 else {
            panic!("{presets:?}")
        };
        assert_eq!(option_id.0, key);
    }

    fn cols_len_of_todo(a: &App) -> usize {
        a.board_columns()
            .iter()
            .filter(|c| c.bucket.title == "Todo")
            .count()
    }

    #[test]
    fn n_shows_in_the_hints_only_without_a_selected_item() {
        let mut a = test_app();
        assert_eq!(a.new_hint(), None, "an item is selected");
        a.handle_key(key('f'));
        type_text(&mut a, "no-such-thing-anywhere");
        a.handle_key(code(KeyCode::Enter));
        assert!(a.selected_item().is_none());
        assert_eq!(a.new_hint().as_deref(), Some("n new"));
    }

    use crate::model::ContentHit;

    fn hit(repo: &str, number: u32, title: &str) -> ContentHit {
        ContentHit {
            id: format!("I_{number}"),
            pull: false,
            repo: repo.into(),
            number,
            title: title.into(),
            open: true,
        }
    }

    #[test]
    fn add_existing_searches_the_pane_and_linked_repositories() {
        let mut a = test_app();
        a.pane_repo = Some(repo());
        let mut snap = a.snapshot().unwrap().clone();
        snap.project.linked_repos = vec![repo_ref("acme/api"), repo_ref("tviles/t")];
        a.store.apply(crate::store::StoreUpdate::Replace(snap));
        a.handle_key(key('n'));
        choose(&mut a, "existing");
        type_text(&mut a, "crash");
        let scope: Vec<RepoSlug> = vec![repo(), "acme/api".parse().unwrap()];
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Search {
                query: "crash".into(),
                scope
            }]
        );
        a.on_sync(SyncEvent::SearchResults {
            query: "crash".into(),
            hits: vec![hit("acme/api", 9, "Crash in api")],
        });
        let Some(Popup::Search { hits, .. }) = &a.popup else {
            panic!("search")
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        let placeholder = a.item(&ItemId::new("local:1")).unwrap();
        assert_eq!(
            (placeholder.number(), placeholder.title()),
            (Some(9), "Crash in api")
        );
    }

    #[test]
    fn an_item_already_on_the_board_is_selected_instead_of_added() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "existing");
        type_text(&mut a, "https://github.com/tviles/t/issues/3");
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::SearchResults {
            query: "https://github.com/tviles/t/issues/3".into(),
            hits: vec![hit("tviles/t", 3, "Emoji")],
        });
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("tviles/t#3 is already on the board")
        );
        assert_eq!(a.selected_item().unwrap().id.as_str(), "c");
        assert_eq!(a.pending_count(), 0);
    }

    /// Controller ruling E22: with no pane repository, no linked repository and no `repo:`
    /// token there is nothing to scope the search to; it would search all of GitHub.
    #[test]
    fn with_no_repository_to_search_nothing_is_searched() {
        let mut a = test_app();
        assert!(a.search_scope().is_empty());
        a.handle_key(key('n'));
        choose(&mut a, "existing");
        type_text(&mut a, "crash");
        assert!(a.handle_key(code(KeyCode::Enter)).is_empty());
        let Some(Popup::Search { error, loading, .. }) = &a.popup else {
            panic!("search")
        };
        assert_eq!(
            error.as_deref(),
            Some(crate::ui::app::edit::NO_REPOS_TO_SEARCH)
        );
        assert!(!*loading);
        // A repo: token names one.
        type_text(&mut a, " repo:acme/app");
        assert_eq!(
            a.handle_key(code(KeyCode::Enter)),
            vec![Command::Search {
                query: "crash repo:acme/app".into(),
                scope: vec![]
            }]
        );
        // A pasted URL is looked up directly.
        let mut b = test_app();
        b.handle_key(key('n'));
        choose(&mut b, "existing");
        type_text(&mut b, "acme/app#12");
        assert_eq!(
            b.handle_key(code(KeyCode::Enter)),
            vec![Command::Search {
                query: "acme/app#12".into(),
                scope: vec![]
            }]
        );
    }

    #[test]
    fn a_failed_or_stale_search_does_not_replace_the_results() {
        let mut a = test_app();
        a.pane_repo = Some(repo());
        a.handle_key(key('n'));
        choose(&mut a, "existing");
        type_text(&mut a, "crash");
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::SearchResults {
            query: "older".into(),
            hits: vec![hit("tviles/t", 1, "x")],
        });
        let Some(Popup::Search { hits, loading, .. }) = &a.popup else {
            panic!("search")
        };
        assert!(
            hits.is_empty() && *loading,
            "an answer to another query is ignored"
        );
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Search("crash".into()),
            error: crate::github::GithubError::Network("offline".into()),
        });
        let Some(Popup::Search { error, loading, .. }) = &a.popup else {
            panic!("search")
        };
        assert!(error.as_deref().unwrap().contains("offline") && !*loading);
    }

    #[test]
    fn esc_in_the_search_goes_back_to_the_kind_picker() {
        let mut a = test_app();
        a.handle_key(key('n'));
        choose(&mut a, "existing");
        a.handle_key(code(KeyCode::Esc));
        assert!(matches!(a.popup, Some(Popup::NewKind { .. })));
    }

    fn search_popup(a: &mut App, text: &str) {
        a.pane_repo = Some(repo());
        a.handle_key(key('n'));
        choose(a, "existing");
        type_text(a, text);
        a.handle_key(code(KeyCode::Enter));
    }

    #[test]
    fn enter_retries_a_failed_search_and_one_that_found_nothing() {
        let mut a = test_app();
        search_popup(&mut a, "crash");
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Search("crash".into()),
            error: crate::github::GithubError::Network("offline".into()),
        });
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [Command::Search { .. }]),
            "retry after failure"
        );
        a.on_sync(SyncEvent::SearchResults {
            query: "crash".into(),
            hits: vec![],
        });
        let cmds = a.handle_key(code(KeyCode::Enter));
        assert!(
            matches!(cmds.as_slice(), [Command::Search { .. }]),
            "retry after nothing found"
        );
        assert_eq!(a.pending_count(), 0);
    }

    #[test]
    fn selection_survives_an_unrelated_lookup_and_editing_clears_the_error() {
        let mut a = test_app();
        search_popup(&mut a, "crash");
        a.on_sync(SyncEvent::SearchResults {
            query: "crash".into(),
            hits: vec![hit("tviles/t", 8, "a"), hit("tviles/t", 9, "b")],
        });
        a.handle_key(code(KeyCode::Down));
        a.on_sync(SyncEvent::RepoFound {
            slug: "acme/other".parse().unwrap(),
            repo: None,
        });
        let Some(Popup::Search { selected, .. }) = &a.popup else {
            panic!("search")
        };
        assert_eq!(*selected, 1);
        a.handle_key(key('x'));
        a.handle_key(code(KeyCode::Enter));
        a.on_sync(SyncEvent::Failed {
            task: crate::sync::SyncTask::Search("crashx".into()),
            error: crate::github::GithubError::Network("offline".into()),
        });
        a.handle_key(key('y'));
        let Some(Popup::Search { error, .. }) = &a.popup else {
            panic!("search")
        };
        assert!(error.is_none());
    }

    #[test]
    fn only_the_hit_in_the_right_repository_counts_as_on_the_board() {
        let a = test_app();
        assert!(a.on_board(&hit("tviles/t", 3, "x")).is_some());
        assert!(a.on_board(&hit("acme/other", 3, "x")).is_none());
    }

    #[test]
    fn c_converts_a_draft_in_a_chosen_repository() {
        let mut a = test_app();
        for _ in 0..3 {
            a.handle_key(key('j'));
        }
        a.handle_key(key('t'));
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Drafts have no labels — convert to issue first (`C`)"),
            "the spec's reason, with its key"
        );
        assert_eq!(a.handle_key(key('C')), vec![Command::LoadRepos]);
        a.on_sync(SyncEvent::Repos(PushableRepos {
            repos: vec![repo_ref("tviles/t")],
            truncated: false,
        }));
        assert_eq!(a.handle_key(code(KeyCode::Enter)), vec![Command::Send(1)]);
        assert!(
            matches!(&a.store.pending()[0].op, Op::Convert { item, repo } if item.as_str() == "d" && repo.slug == repo_ref("tviles/t").slug)
        );
        // Ruling E21: while the convert is sending, labels wait for it.
        a.handle_key(key('t'));
        assert_eq!(
            a.status.flash.as_deref(),
            Some(crate::ui::app::edit::CONVERTING)
        );
    }

    #[test]
    fn only_drafts_convert() {
        let mut a = test_app();
        assert!(a.handle_key(key('C')).is_empty());
        assert_eq!(
            a.status.flash.as_deref(),
            Some("Only drafts convert to issues")
        );
    }

    #[test]
    fn esc_closes_the_convert_repository_picker() {
        let mut a = test_app();
        for _ in 0..3 {
            a.handle_key(key('j'));
        }
        a.handle_key(key('C'));
        a.handle_key(code(KeyCode::Esc));
        assert!(a.popup.is_none(), "no earlier step: Esc closes");
        assert!(a.store.pending().is_empty());
    }
}
