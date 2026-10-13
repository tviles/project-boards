//! One `Op` per user action. `apply` is what the edit does to a board snapshot; the store
//! replays pending ops with it on top of the confirmed board. `send` (ops::send) carries
//! an op to GitHub. Generated GraphQL types never appear here.

pub mod queue;
pub mod send;

use crate::model::{
    CloseReason, ContentFields, ContentHit, ContentId, ContentRef, ContentState, FieldId,
    FieldKind, FieldValue, Item, ItemContent, ItemId, Label, LabelRef, MilestoneRef, RepoRef,
    StateReason, UserRef,
};
use crate::store::BoardSnapshot;
use std::collections::HashMap;

pub type OpId = u64;

/// What a new item is made from.
#[derive(Debug, Clone, PartialEq)]
pub enum NewItem {
    Draft {
        title: String,
        body: String,
    },
    Issue {
        repo: RepoRef,
        title: String,
        body: String,
    },
    /// An existing issue or pull request, found by search or URL.
    Existing {
        hit: ContentHit,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Sets a project field: text, number, date, single-select, multi-select or iteration.
    /// Moving a card between columns is this op on the column field.
    SetField {
        item: ItemId,
        field: FieldId,
        value: FieldValue,
    },
    ClearField {
        item: ItemId,
        field: FieldId,
    },
    /// Puts `item` right after `after` in the project's order; `None` puts it first.
    Reorder {
        item: ItemId,
        after: Option<ItemId>,
    },
    /// Adds and removes assignees; `all` is the whole new list, which a draft sends.
    Assignees {
        item: ItemId,
        content: ContentId,
        add: Vec<UserRef>,
        remove: Vec<UserRef>,
        all: Vec<UserRef>,
    },
    Labels {
        item: ItemId,
        content: ContentId,
        add: Vec<LabelRef>,
        remove: Vec<LabelRef>,
    },
    /// Sets the milestone, or clears it with `None`.
    Milestone {
        item: ItemId,
        content: ContentId,
        milestone: Option<MilestoneRef>,
    },
    Title {
        item: ItemId,
        content: ContentId,
        title: String,
    },
    Body {
        item: ItemId,
        content: ContentId,
        body: String,
    },
    Comment {
        item: ItemId,
        content: ContentId,
        body: String,
    },
    /// Closes an issue (with a reason) or a pull request (`reason: None`).
    Close {
        item: ItemId,
        content: ContentId,
        reason: Option<CloseReason>,
    },
    Reopen {
        item: ItemId,
        content: ContentId,
    },
    Convert {
        item: ItemId,
        repo: RepoRef,
    },
    Archive {
        item: ItemId,
    },
    Unarchive {
        item: ItemId,
    },
    /// Removes the item from the project (a draft is deleted with it).
    Remove {
        item: ItemId,
    },
    /// A new item, shown at once as the placeholder `temp`; `presets` are the column and
    /// swimlane values it was created in.
    Create {
        temp: ItemId,
        new: NewItem,
        presets: Vec<(FieldId, FieldValue)>,
    },
}

/// What GitHub answered when an op succeeded.
#[derive(Debug, Clone, PartialEq)]
// One value per finished op and never stored in bulk, so boxing the item buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum Confirmed {
    /// Applied as sent.
    Applied,
    /// Applied; this is the item as GitHub now has it.
    Item(Item),
    /// A new item: the placeholder `temp` became `item`. `warning`: a preset value GitHub
    /// refused (the item exists without it).
    Created {
        temp: ItemId,
        item: Item,
        warning: Option<crate::github::GithubError>,
    },
    /// Not a success: GitHub created the new issue `issue` (its node id) but did not add it
    /// to the project. The App logs it as a failure whose retry adds that issue
    /// (`Op::add_created`) and never creates another.
    NotAdded {
        issue: String,
        error: crate::github::GithubError,
    },
}

/// At most 40 characters of a title, for messages.
fn short(title: &str) -> String {
    if title.chars().count() <= 40 {
        title.to_string()
    } else {
        format!("{}…", title.chars().take(39).collect::<String>())
    }
}

/// Moves `item` to just after `after` (`None`: first). False when either is missing.
fn move_after(order: &mut Vec<ItemId>, item: &ItemId, after: Option<&ItemId>) -> bool {
    let Some(from) = order.iter().position(|i| i == item) else {
        return false;
    };
    let target = match after {
        None => 0,
        Some(a) if a == item => return false,
        Some(a) => match order.iter().position(|i| i == a) {
            Some(p) if p < from => p + 1,
            Some(p) => p,
            None => return false,
        },
    };
    let id = order.remove(from);
    order.insert(target, id);
    true
}

/// Moves `item` within `list` to where its project rank puts it: before the first item
/// ranked after it. Ids without a rank (not loaded yet) count as last.
///
/// View id lists are kept in project (POSITION) order. Sorted views are ordered on display
/// by `sort_items`, which reads field values (stable, so ties keep list order), and reorder
/// is refused in sorted views (`check_here`, Task 11), so re-ranking every list is safe.
fn place_by_rank(list: &mut Vec<ItemId>, item: &ItemId, rank: &HashMap<ItemId, usize>) {
    let (Some(from), Some(&mine)) = (list.iter().position(|i| i == item), rank.get(item)) else {
        return;
    };
    let id = list.remove(from);
    let at = list
        .iter()
        .position(|i| rank.get(i).is_none_or(|&r| r > mine))
        .unwrap_or(list.len());
    list.insert(at, id);
}

/// Where `item` keeps the value of a built-in field: the key it already uses for such a
/// value, else the project's field of `kind`.
fn value_key(
    snap: &BoardSnapshot,
    item: &ItemId,
    kind: FieldKind,
    is_value: impl Fn(&FieldValue) -> bool,
) -> Option<FieldId> {
    snap.items
        .get(item)
        .and_then(|i| {
            i.values
                .iter()
                .find(|(_, v)| is_value(v))
                .map(|(k, _)| k.clone())
        })
        .or_else(|| {
            snap.project
                .fields
                .iter()
                .find(|f| f.kind == kind)
                .map(|f| f.id.clone())
        })
}

fn set_state(content: &mut ItemContent, to: ContentState) {
    if let ItemContent::Issue { state, .. } | ItemContent::PullRequest { state, .. } = content {
        *state = to;
    }
}

/// What a new item looks like until GitHub has created it.
fn placeholder(temp: &ItemId, new: &NewItem, presets: &[(FieldId, FieldValue)]) -> Item {
    let content = match new {
        NewItem::Draft { title, .. } => ItemContent::Draft {
            title: title.clone(),
        },
        NewItem::Issue { repo, title, .. } => ItemContent::Issue {
            reference: ContentRef {
                repo: repo.slug.to_string(),
                number: 0,
                url: String::new(),
            },
            title: title.clone(),
            state: ContentState::Open,
        },
        NewItem::Existing { hit } => {
            let reference = ContentRef {
                repo: hit.repo.clone(),
                number: hit.number,
                url: String::new(),
            };
            let state = if hit.open {
                ContentState::Open
            } else {
                ContentState::Closed
            };
            if hit.pull {
                ItemContent::PullRequest {
                    reference,
                    title: hit.title.clone(),
                    state,
                    is_draft: false,
                }
            } else {
                ItemContent::Issue {
                    reference,
                    title: hit.title.clone(),
                    state,
                }
            }
        }
    };
    Item {
        id: temp.clone(),
        content,
        archived: false,
        updated_at: String::new(),
        values: presets.iter().cloned().collect(),
        content_fields: ContentFields::default(),
    }
}

fn reorder(snap: &mut BoardSnapshot, item: &ItemId, after: Option<&ItemId>) {
    if !snap.items.contains_key(item) || !move_after(&mut snap.order, item, after) {
        return;
    }
    let rank: HashMap<ItemId, usize> = snap
        .order
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), i))
        .collect();
    for list in snap.views.values_mut() {
        place_by_rank(&mut list.ids, item, &rank);
    }
}

impl Op {
    pub fn item(&self) -> &ItemId {
        match self {
            Op::SetField { item, .. }
            | Op::ClearField { item, .. }
            | Op::Reorder { item, .. }
            | Op::Assignees { item, .. }
            | Op::Labels { item, .. }
            | Op::Milestone { item, .. }
            | Op::Title { item, .. }
            | Op::Body { item, .. }
            | Op::Comment { item, .. }
            | Op::Close { item, .. }
            | Op::Reopen { item, .. }
            | Op::Convert { item, .. }
            | Op::Archive { item }
            | Op::Unarchive { item }
            | Op::Remove { item } => item,
            Op::Create { temp, .. } => temp,
        }
    }

    /// For a new-issue `Create` whose issue GitHub already made (`issue`, its node id): the
    /// op that only adds that issue to the project, with the same placeholder and presets,
    /// so a retry never creates a second issue. `None` for any other op.
    pub fn add_created(&self, issue: &str) -> Option<Op> {
        let Op::Create {
            temp,
            new: NewItem::Issue { repo, title, .. },
            presets,
        } = self
        else {
            return None;
        };
        Some(Op::Create {
            temp: temp.clone(),
            new: NewItem::Existing {
                hit: ContentHit {
                    id: issue.to_string(),
                    pull: false,
                    repo: repo.slug.to_string(),
                    // Not known: `createIssue` returns only the id. Shown as no number.
                    number: 0,
                    title: title.clone(),
                    open: true,
                },
            },
            presets: presets.clone(),
        })
    }

    /// The op's name for logs, which never carry titles or values.
    pub fn name(&self) -> &'static str {
        match self {
            Op::SetField { .. } => "SetField",
            Op::Assignees { .. } => "Assignees",
            Op::Labels { .. } => "Labels",
            Op::Milestone { .. } => "Milestone",
            Op::Title { .. } => "Title",
            Op::Body { .. } => "Body",
            Op::Comment { .. } => "Comment",
            Op::Close { .. } => "Close",
            Op::Reopen { .. } => "Reopen",
            Op::ClearField { .. } => "ClearField",
            Op::Reorder { .. } => "Reorder",
            Op::Convert { .. } => "Convert",
            Op::Archive { .. } => "Archive",
            Op::Unarchive { .. } => "Unarchive",
            Op::Remove { .. } => "Remove",
            Op::Create { .. } => "Create",
        }
    }

    /// Applies the edit to `snap`. Applying twice is the same as once; an item that is not
    /// in the snapshot is left alone.
    pub fn apply(&self, snap: &mut BoardSnapshot) {
        match self {
            Op::SetField { item, field, value } => {
                // A field the project does not have cannot be set.
                if snap.project.field(field).is_none() {
                    return;
                }
                if let Some(i) = snap.items.get_mut(item) {
                    i.values.insert(field.clone(), value.clone());
                }
            }
            Op::ClearField { item, field } => {
                if let Some(i) = snap.items.get_mut(item) {
                    i.values.remove(field);
                }
            }
            Op::Reorder { item, after } => reorder(snap, item, after.as_ref()),
            Op::Assignees {
                item, add, remove, ..
            } => {
                let key = value_key(snap, item, FieldKind::Assignees, |v| {
                    matches!(v, FieldValue::Users(_))
                });
                if let Some(i) = snap.items.get_mut(item) {
                    let mut logins: Vec<String> =
                        i.assignees().iter().map(|s| s.to_string()).collect();
                    logins.retain(|l| !remove.iter().any(|u| &u.login == l));
                    for u in add {
                        if !logins.contains(&u.login) {
                            logins.push(u.login.clone());
                        }
                    }
                    for u in remove {
                        i.content_fields.assignee_ids.remove(&u.login);
                    }
                    for u in add {
                        i.content_fields
                            .assignee_ids
                            .insert(u.login.clone(), u.id.clone());
                    }
                    if let Some(key) = key {
                        i.values.insert(key, FieldValue::Users(logins));
                    }
                }
            }
            Op::Labels {
                item, add, remove, ..
            } => {
                let key = value_key(snap, item, FieldKind::Labels, |v| {
                    matches!(v, FieldValue::Labels(_))
                });
                if let Some(i) = snap.items.get_mut(item) {
                    let mut labels: Vec<Label> = i.labels().into_iter().cloned().collect();
                    labels.retain(|l| !remove.iter().any(|r| r.name == l.name));
                    for l in add {
                        if !labels.iter().any(|x| x.name == l.name) {
                            labels.push(Label {
                                name: l.name.clone(),
                                color: l.color.clone(),
                            });
                        }
                    }
                    for l in remove {
                        i.content_fields.label_ids.remove(&l.name);
                    }
                    for l in add {
                        i.content_fields
                            .label_ids
                            .insert(l.name.clone(), l.id.clone());
                    }
                    if let Some(key) = key {
                        i.values.insert(key, FieldValue::Labels(labels));
                    }
                }
            }
            Op::Milestone {
                item, milestone, ..
            } => {
                let key = value_key(snap, item, FieldKind::Milestone, |v| {
                    matches!(v, FieldValue::Milestone(_))
                });
                if let Some(i) = snap.items.get_mut(item) {
                    i.content_fields.milestone = milestone.clone();
                    if let Some(key) = key {
                        match milestone {
                            Some(m) => i.values.insert(key, FieldValue::Milestone(m.title.clone())),
                            None => i.values.remove(&key),
                        };
                    }
                }
            }
            Op::Title { item, title, .. } => {
                if let Some(i) = snap.items.get_mut(item) {
                    match &mut i.content {
                        ItemContent::Issue { title: t, .. }
                        | ItemContent::PullRequest { title: t, .. }
                        | ItemContent::Draft { title: t } => *t = title.clone(),
                        ItemContent::Redacted | ItemContent::Unknown { .. } => {}
                    }
                }
            }
            // Bodies and comments live in the detail, not on the board.
            Op::Body { .. } | Op::Comment { .. } => {}
            Op::Close { item, reason, .. } => {
                if let Some(i) = snap.items.get_mut(item) {
                    set_state(&mut i.content, ContentState::Closed);
                    // GitHub closes an issue as completed unless told otherwise; a pull
                    // request has no reason.
                    i.content_fields.state_reason = match (&i.content, reason) {
                        (ItemContent::Issue { .. }, Some(CloseReason::NotPlanned)) => {
                            Some(StateReason::NotPlanned)
                        }
                        (ItemContent::Issue { .. }, _) => Some(StateReason::Completed),
                        _ => None,
                    };
                }
            }
            Op::Reopen { item, .. } => {
                if let Some(i) = snap.items.get_mut(item) {
                    set_state(&mut i.content, ContentState::Open);
                    i.content_fields.state_reason = match i.content {
                        ItemContent::Issue { .. } => Some(StateReason::Reopened),
                        _ => None,
                    };
                }
            }
            // The issue is GitHub's answer; until then the draft stays a draft.
            Op::Convert { .. } => {}
            Op::Archive { item } | Op::Unarchive { item } => {
                if let Some(i) = snap.items.get_mut(item) {
                    i.archived = matches!(self, Op::Archive { .. });
                }
            }
            Op::Remove { item } => {
                snap.items.remove(item);
                snap.order.retain(|i| i != item);
                for list in snap.views.values_mut() {
                    list.ids.retain(|i| i != item);
                }
            }
            Op::Create { temp, new, presets } => {
                if !snap.items.contains_key(temp) {
                    snap.upsert_items(vec![placeholder(temp, new, presets)]);
                }
            }
        }
    }

    /// A short description for the status line and the error log.
    pub fn describe(&self, snap: Option<&BoardSnapshot>) -> String {
        let title = snap
            .and_then(|s| s.items.get(self.item()))
            .map(|i| short(i.title()))
            .unwrap_or_else(|| "an item".into());
        let field_name = |id: &FieldId| {
            snap.and_then(|s| s.project.field(id))
                .map(|f| f.name.clone())
                .unwrap_or_else(|| "a field".into())
        };
        match self {
            Op::SetField { field, value, .. } => {
                format!(
                    "set {} to {} on '{title}'",
                    field_name(field),
                    value.display()
                )
            }
            Op::ClearField { field, .. } => format!("clear {} on '{title}'", field_name(field)),
            Op::Reorder { .. } => format!("reorder '{title}'"),
            Op::Assignees { .. } => format!("change assignees on '{title}'"),
            Op::Labels { .. } => format!("change labels on '{title}'"),
            Op::Milestone {
                milestone: Some(m), ..
            } => format!("set milestone {} on '{title}'", m.title),
            Op::Milestone {
                milestone: None, ..
            } => format!("clear the milestone on '{title}'"),
            Op::Title { .. } => format!("retitle '{title}'"),
            Op::Body { .. } => format!("edit the body of '{title}'"),
            Op::Comment { .. } => format!("comment on '{title}'"),
            Op::Close { .. } => format!("close '{title}'"),
            Op::Reopen { .. } => format!("reopen '{title}'"),
            Op::Convert { repo, .. } => {
                format!("convert '{title}' to an issue in {}", repo.slug)
            }
            Op::Archive { .. } => format!("archive '{title}'"),
            Op::Unarchive { .. } => format!("unarchive '{title}'"),
            Op::Remove { .. } => format!("remove '{title}' from the project"),
            Op::Create { new, .. } => match new {
                NewItem::Draft { title, .. } => format!("add the draft '{}'", short(title)),
                NewItem::Issue { repo, title, .. } => {
                    format!("create '{}' in {}", short(title), repo.slug)
                }
                // An issue this app created whose add failed (`add_created`).
                NewItem::Existing { hit } if hit.number == 0 => format!(
                    "add the new issue '{}' ({}) to the project",
                    short(&hit.title),
                    hit.repo
                ),
                NewItem::Existing { hit } => format!("add {}#{}", hit.repo, hit.number),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OptionId, ViewId};
    use crate::store::ViewList;
    use crate::ui::fixtures::snapshot;

    fn done() -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new("o_done"),
            name: "Done".into(),
        }
    }

    fn status(s: &BoardSnapshot, item: &str) -> Option<String> {
        s.items[&ItemId::new(item)]
            .value(&FieldId::new("F_status"))
            .map(|v| v.display())
    }

    fn ids(list: &[ItemId]) -> Vec<&str> {
        list.iter().map(|i| i.as_str()).collect()
    }

    #[test]
    fn set_and_clear_change_one_value_and_repeat_safely() {
        let mut s = snapshot();
        let set = Op::SetField {
            item: ItemId::new("a"),
            field: FieldId::new("F_status"),
            value: done(),
        };
        set.apply(&mut s);
        set.apply(&mut s);
        assert_eq!(status(&s, "a").as_deref(), Some("Done"));
        assert_eq!(
            status(&s, "b").as_deref(),
            Some("In Progress"),
            "other items untouched"
        );
        Op::ClearField {
            item: ItemId::new("a"),
            field: FieldId::new("F_status"),
        }
        .apply(&mut s);
        assert_eq!(status(&s, "a"), None);
    }

    #[test]
    fn an_op_on_a_missing_item_changes_nothing() {
        let mut s = snapshot();
        let before = s.clone();
        Op::SetField {
            item: ItemId::new("gone"),
            field: FieldId::new("F_status"),
            value: done(),
        }
        .apply(&mut s);
        Op::Reorder {
            item: ItemId::new("gone"),
            after: None,
        }
        .apply(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn reorder_moves_the_item_in_project_order_and_in_view_lists() {
        let mut s = snapshot();
        s.set_view_ids(
            ViewId::new("V_bugs"),
            ViewList {
                ids: vec![ItemId::new("a"), ItemId::new("c"), ItemId::new("e")],
                total: 3,
                truncated: false,
            },
        );
        let op = Op::Reorder {
            item: ItemId::new("e"),
            after: Some(ItemId::new("a")),
        };
        op.apply(&mut s);
        op.apply(&mut s);
        assert_eq!(ids(&s.order), ["a", "e", "b", "c", "d"]);
        assert_eq!(ids(&s.views[&ViewId::new("V_bugs")].ids), ["a", "e", "c"]);
        Op::Reorder {
            item: ItemId::new("c"),
            after: None,
        }
        .apply(&mut s);
        assert_eq!(ids(&s.order), ["c", "a", "e", "b", "d"]);
        assert_eq!(ids(&s.views[&ViewId::new("V_bugs")].ids), ["c", "a", "e"]);
        Op::Reorder {
            item: ItemId::new("a"),
            after: Some(ItemId::new("d")),
        }
        .apply(&mut s);
        assert_eq!(ids(&s.order), ["c", "e", "b", "d", "a"]);
        assert_eq!(ids(&s.views[&ViewId::new("V_bugs")].ids), ["c", "e", "a"]);
    }

    fn date(d: &str) -> FieldValue {
        FieldValue::Date(d.into())
    }

    #[test]
    fn clear_on_a_missing_item_or_twice_changes_nothing_more() {
        let mut s = snapshot();
        let before = s.clone();
        Op::ClearField {
            item: ItemId::new("gone"),
            field: FieldId::new("F_status"),
        }
        .apply(&mut s);
        assert_eq!(s, before);
        let clear = Op::ClearField {
            item: ItemId::new("a"),
            field: FieldId::new("F_status"),
        };
        clear.apply(&mut s);
        let once = s.clone();
        clear.apply(&mut s);
        assert_eq!(s, once);
    }

    #[test]
    fn reorder_edge_cases_leave_the_order_alone_or_settle() {
        let mut s = snapshot();
        let before = s.clone();
        Op::Reorder {
            item: ItemId::new("b"),
            after: Some(ItemId::new("gone")),
        }
        .apply(&mut s);
        Op::Reorder {
            item: ItemId::new("b"),
            after: Some(ItemId::new("b")),
        }
        .apply(&mut s);
        Op::Reorder {
            item: ItemId::new("b"),
            after: Some(ItemId::new("a")),
        }
        .apply(&mut s);
        assert_eq!(s, before, "missing after, after == item, already in place");
        let first = Op::Reorder {
            item: ItemId::new("c"),
            after: None,
        };
        first.apply(&mut s);
        first.apply(&mut s);
        assert_eq!(ids(&s.order), ["c", "a", "b", "d", "e"]);
    }

    #[test]
    fn reorder_leaves_views_without_the_item_and_places_unranked_ids_last() {
        let mut s = snapshot();
        let list = |v: &[&str]| ViewList {
            ids: v.iter().map(|i| ItemId::new(*i)).collect(),
            total: v.len(),
            truncated: false,
        };
        s.set_view_ids(ViewId::new("V_bugs"), list(&["a", "c"]));
        s.set_view_ids(ViewId::new("V_board"), list(&["a", "zz", "d"]));
        Op::Reorder {
            item: ItemId::new("d"),
            after: None,
        }
        .apply(&mut s);
        assert_eq!(
            ids(&s.views[&ViewId::new("V_bugs")].ids),
            ["a", "c"],
            "item not in list"
        );
        assert_eq!(ids(&s.views[&ViewId::new("V_board")].ids), ["d", "a", "zz"]);
        Op::Reorder {
            item: ItemId::new("a"),
            after: Some(ItemId::new("b")),
        }
        .apply(&mut s);
        assert_eq!(ids(&s.views[&ViewId::new("V_board")].ids), ["d", "a", "zz"]);
    }

    #[test]
    fn set_works_for_text_number_and_date_values_and_repeats_safely() {
        let mut s = snapshot();
        for (field, value) in [
            ("F_notes", FieldValue::Text("hi".into())),
            ("F_created", date("2026-10-03")),
            ("F_notes", FieldValue::Number(3.0)),
        ] {
            let op = Op::SetField {
                item: ItemId::new("b"),
                field: FieldId::new(field),
                value: value.clone(),
            };
            op.apply(&mut s);
            let once = s.clone();
            op.apply(&mut s);
            assert_eq!(s, once);
            assert_eq!(
                s.items[&ItemId::new("b")].value(&FieldId::new(field)),
                Some(&value)
            );
        }
    }

    #[test]
    fn set_on_a_field_the_project_lacks_changes_nothing() {
        let mut s = snapshot();
        let before = s.clone();
        Op::SetField {
            item: ItemId::new("a"),
            field: FieldId::new("F_nope"),
            value: done(),
        }
        .apply(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn describe_names_the_field_value_and_item() {
        let s = snapshot();
        let set = Op::SetField {
            item: ItemId::new("a"),
            field: FieldId::new("F_status"),
            value: done(),
        };
        assert_eq!(set.describe(Some(&s)), "set Status to Done on 'Fix crash'");
        let clear = Op::ClearField {
            item: ItemId::new("a"),
            field: FieldId::new("F_status"),
        };
        assert_eq!(clear.describe(Some(&s)), "clear Status on 'Fix crash'");
        assert_eq!(
            Op::Reorder {
                item: ItemId::new("zz"),
                after: None
            }
            .describe(Some(&s)),
            "reorder 'an item'"
        );
        assert_eq!(set.name(), "SetField");
    }

    use crate::model::{CloseReason, ContentState, ItemContent, LabelRef, MilestoneRef, UserRef};

    fn user(login: &str) -> UserRef {
        UserRef {
            id: format!("U_{login}"),
            login: login.into(),
        }
    }

    fn label(name: &str) -> LabelRef {
        LabelRef {
            id: format!("L_{name}"),
            name: name.into(),
            color: "d73a4a".into(),
        }
    }

    fn content() -> ContentId {
        ContentId::Issue("I_a".into())
    }

    fn item_a(s: &BoardSnapshot) -> &Item {
        &s.items[&ItemId::new("a")]
    }

    #[test]
    fn assignee_and_label_changes_apply_to_values_and_ids() {
        let mut s = snapshot();
        let op = Op::Assignees {
            item: ItemId::new("a"),
            content: content(),
            add: vec![user("octo")],
            remove: vec![user("tviles")],
            all: vec![user("octo")],
        };
        op.apply(&mut s);
        op.apply(&mut s);
        assert_eq!(item_a(&s).assignees(), ["octo"]);
        assert_eq!(
            item_a(&s)
                .content_fields
                .assignee_ids
                .get("octo")
                .map(String::as_str),
            Some("U_octo")
        );
        assert!(
            !item_a(&s)
                .content_fields
                .assignee_ids
                .contains_key("tviles")
        );
        Op::Labels {
            item: ItemId::new("a"),
            content: content(),
            add: vec![label("docs")],
            remove: vec![label("bug")],
        }
        .apply(&mut s);
        assert_eq!(item_a(&s).label_names(), ["docs"]);
        assert_eq!(
            item_a(&s)
                .content_fields
                .label_ids
                .get("docs")
                .map(String::as_str),
            Some("L_docs")
        );
    }

    #[test]
    fn milestone_title_close_and_reopen_apply() {
        let mut s = snapshot();
        let a = || ItemId::new("a");
        Op::Milestone {
            item: a(),
            content: content(),
            milestone: Some(MilestoneRef {
                id: "MI_1".into(),
                title: "v1".into(),
            }),
        }
        .apply(&mut s);
        assert_eq!(
            item_a(&s)
                .content_fields
                .milestone
                .as_ref()
                .map(|m| m.title.as_str()),
            Some("v1")
        );
        Op::Title {
            item: a(),
            content: content(),
            title: "Fix the crash".into(),
        }
        .apply(&mut s);
        assert_eq!(item_a(&s).title(), "Fix the crash");
        Op::Close {
            item: a(),
            content: content(),
            reason: Some(CloseReason::NotPlanned),
        }
        .apply(&mut s);
        assert!(matches!(
            item_a(&s).content,
            ItemContent::Issue {
                state: ContentState::Closed,
                ..
            }
        ));
        assert_eq!(
            item_a(&s).content_fields.state_reason,
            Some(crate::model::StateReason::NotPlanned)
        );
        Op::Reopen {
            item: a(),
            content: content(),
        }
        .apply(&mut s);
        assert!(matches!(
            item_a(&s).content,
            ItemContent::Issue {
                state: ContentState::Open,
                ..
            }
        ));
        Op::Milestone {
            item: a(),
            content: content(),
            milestone: None,
        }
        .apply(&mut s);
        assert_eq!(item_a(&s).content_fields.milestone, None);
        let before = s.clone();
        Op::Body {
            item: a(),
            content: content(),
            body: "x".into(),
        }
        .apply(&mut s);
        Op::Comment {
            item: a(),
            content: content(),
            body: "x".into(),
        }
        .apply(&mut s);
        assert_eq!(
            s, before,
            "bodies and comments live in the detail, not the board"
        );
    }

    #[test]
    fn issue_ops_describe_themselves() {
        let s = snapshot();
        let a = || ItemId::new("a");
        let label_op = Op::Labels {
            item: a(),
            content: content(),
            add: vec![label("docs")],
            remove: vec![],
        };
        assert_eq!(label_op.describe(Some(&s)), "change labels on 'Fix crash'");
        assert_eq!(
            Op::Close {
                item: a(),
                content: content(),
                reason: None
            }
            .describe(Some(&s)),
            "close 'Fix crash'"
        );
        assert_eq!(
            Op::Comment {
                item: a(),
                content: content(),
                body: "x".into()
            }
            .describe(Some(&s)),
            "comment on 'Fix crash'"
        );
    }

    #[test]
    fn a_retitle_reorders_a_title_sorted_view() {
        use crate::model::{SortDirection, SortSpec, sort_items};
        let mut s = snapshot();
        let order = |s: &BoardSnapshot| {
            let mut items: Vec<&Item> = ["a", "b"]
                .iter()
                .map(|i| &s.items[&ItemId::new(*i)])
                .collect();
            sort_items(
                &mut items,
                &[SortSpec {
                    field: FieldId::new("F_title"),
                    direction: SortDirection::Asc,
                }],
                &s.project.fields,
            );
            items
                .iter()
                .map(|i| i.id.as_str().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&s),
            ["b", "a"],
            "'Add iteration columns' sorts before 'Fix crash'"
        );
        Op::Title {
            item: ItemId::new("a"),
            content: content(),
            title: "Aardvark".into(),
        }
        .apply(&mut s);
        assert_eq!(order(&s), ["a", "b"]);
    }

    #[test]
    fn close_and_reopen_set_the_reason_github_would() {
        let mut s = snapshot();
        let a = || ItemId::new("a");
        let close = Op::Close {
            item: a(),
            content: content(),
            reason: None,
        };
        close.apply(&mut s);
        let once = s.clone();
        close.apply(&mut s);
        assert_eq!(s, once);
        assert_eq!(
            item_a(&s).content_fields.state_reason,
            Some(StateReason::Completed)
        );
        let reopen = Op::Reopen {
            item: a(),
            content: content(),
        };
        reopen.apply(&mut s);
        let once = s.clone();
        reopen.apply(&mut s);
        assert_eq!(s, once);
        assert_eq!(
            item_a(&s).content_fields.state_reason,
            Some(StateReason::Reopened)
        );
    }

    #[test]
    fn close_and_reopen_a_pull_request_carry_no_reason() {
        let mut s = snapshot();
        let id = ItemId::new("a");
        s.items.get_mut(&id).unwrap().content = ItemContent::PullRequest {
            reference: crate::model::ContentRef {
                repo: "o/r".into(),
                number: 9,
                url: "u".into(),
            },
            title: "pr".into(),
            state: ContentState::Open,
            is_draft: false,
        };
        let pr = ContentId::PullRequest("PR_a".into());
        Op::Close {
            item: id.clone(),
            content: pr.clone(),
            reason: None,
        }
        .apply(&mut s);
        let i = item_a(&s);
        assert!(matches!(
            i.content,
            ItemContent::PullRequest {
                state: ContentState::Closed,
                ..
            }
        ));
        assert_eq!(i.content_fields.state_reason, None);
        Op::Reopen {
            item: id,
            content: pr,
        }
        .apply(&mut s);
        let i = item_a(&s);
        assert!(matches!(
            i.content,
            ItemContent::PullRequest {
                state: ContentState::Open,
                ..
            }
        ));
        assert_eq!(i.content_fields.state_reason, None);
    }

    #[test]
    fn labels_milestone_and_removals_are_idempotent() {
        let mut s = snapshot();
        let a = || ItemId::new("a");
        let labels = Op::Labels {
            item: a(),
            content: content(),
            add: vec![label("docs")],
            remove: vec![label("bug"), label("never-had")],
        };
        labels.apply(&mut s);
        let once = s.clone();
        labels.apply(&mut s);
        assert_eq!(s, once);
        let people = Op::Assignees {
            item: a(),
            content: content(),
            add: vec![],
            remove: vec![user("nobody")],
            all: vec![user("tviles")],
        };
        people.apply(&mut s);
        assert_eq!(
            item_a(&s).assignees(),
            ["tviles"],
            "removing an absent assignee changes nothing"
        );
        s.project.fields.push(crate::model::Field {
            id: FieldId::new("F_milestone"),
            name: "Milestone".into(),
            kind: FieldKind::Milestone,
        });
        let set = Op::Milestone {
            item: a(),
            content: content(),
            milestone: Some(MilestoneRef {
                id: "MI_1".into(),
                title: "v1".into(),
            }),
        };
        set.apply(&mut s);
        let once = s.clone();
        set.apply(&mut s);
        assert_eq!(s, once);
        assert_eq!(
            item_a(&s).values.get(&FieldId::new("F_milestone")),
            Some(&FieldValue::Milestone("v1".into()))
        );
        Op::Milestone {
            item: a(),
            content: content(),
            milestone: None,
        }
        .apply(&mut s);
        assert!(
            !item_a(&s)
                .values
                .values()
                .any(|v| matches!(v, FieldValue::Milestone(_)))
        );
    }

    use crate::model::{ContentHit, RepoRef};

    fn new_issue() -> NewItem {
        NewItem::Issue {
            repo: RepoRef {
                id: "R_1".into(),
                slug: "tviles/t".parse().unwrap(),
            },
            title: "New bug".into(),
            body: String::new(),
        }
    }

    #[test]
    fn create_adds_a_placeholder_with_its_presets_at_the_end() {
        let mut s = snapshot();
        let op = Op::Create {
            temp: ItemId::new("local:1"),
            new: new_issue(),
            presets: vec![(FieldId::new("F_status"), done())],
        };
        op.apply(&mut s);
        op.apply(&mut s);
        assert_eq!(s.order.last().map(|i| i.as_str()), Some("local:1"));
        assert_eq!(s.order.len(), 6);
        let p = &s.items[&ItemId::new("local:1")];
        assert_eq!((p.title(), p.number()), ("New bug", None));
        assert_eq!(status(&s, "local:1").as_deref(), Some("Done"));
        let hit = ContentHit {
            id: "I_9".into(),
            pull: true,
            repo: "acme/app".into(),
            number: 9,
            title: "Fix".into(),
            open: true,
        };
        Op::Create {
            temp: ItemId::new("local:2"),
            new: NewItem::Existing { hit },
            presets: vec![],
        }
        .apply(&mut s);
        assert_eq!(s.items[&ItemId::new("local:2")].number(), Some(9));
    }

    #[test]
    fn archive_unarchive_and_remove_apply() {
        let mut s = snapshot();
        s.set_view_ids(
            ViewId::new("V"),
            ViewList {
                ids: vec![ItemId::new("a"), ItemId::new("b")],
                total: 2,
                truncated: false,
            },
        );
        Op::Archive {
            item: ItemId::new("a"),
        }
        .apply(&mut s);
        assert!(s.items[&ItemId::new("a")].archived);
        Op::Unarchive {
            item: ItemId::new("a"),
        }
        .apply(&mut s);
        assert!(!s.items[&ItemId::new("a")].archived);
        Op::Remove {
            item: ItemId::new("a"),
        }
        .apply(&mut s);
        assert!(!s.items.contains_key(&ItemId::new("a")));
        assert!(!s.order.contains(&ItemId::new("a")));
        assert_eq!(ids(&s.views[&ViewId::new("V")].ids), ["b"]);
    }

    #[test]
    fn item_ops_name_and_describe_themselves() {
        let s = snapshot();
        let a = || ItemId::new("a");
        let repo = RepoRef {
            id: "R_1".into(),
            slug: "acme/app".parse().unwrap(),
        };
        let hit = ContentHit {
            id: "I_9".into(),
            pull: false,
            repo: "acme/app".into(),
            number: 9,
            title: "Fix".into(),
            open: true,
        };
        let create = |temp: &str, new: NewItem| Op::Create {
            temp: ItemId::new(temp),
            new,
            presets: vec![],
        };
        let cases = [
            (
                Op::Convert { item: a(), repo },
                "Convert",
                "convert 'Fix crash' to an issue in acme/app",
            ),
            (Op::Archive { item: a() }, "Archive", "archive 'Fix crash'"),
            (
                Op::Unarchive { item: a() },
                "Unarchive",
                "unarchive 'Fix crash'",
            ),
            (
                Op::Remove { item: a() },
                "Remove",
                "remove 'Fix crash' from the project",
            ),
            (
                create(
                    "local:1",
                    NewItem::Draft {
                        title: "Idea".into(),
                        body: String::new(),
                    },
                ),
                "Create",
                "add the draft 'Idea'",
            ),
            (
                create("local:2", new_issue()),
                "Create",
                "create 'New bug' in tviles/t",
            ),
            (
                create("local:3", NewItem::Existing { hit }),
                "Create",
                "add acme/app#9",
            ),
        ];
        for (op, name, text) in cases {
            assert_eq!((op.name(), op.describe(Some(&s)).as_str()), (name, text));
        }
        assert_eq!(create("local:4", new_issue()).item().as_str(), "local:4");
    }

    /// Controller ruling E15: a retry after "created but not added" only adds.
    #[test]
    fn a_created_issue_is_added_again_without_creating_another() {
        let presets = vec![(FieldId::new("F_status"), done())];
        let op = Op::Create {
            temp: ItemId::new("local:1"),
            new: new_issue(),
            presets: presets.clone(),
        };
        let add = op.add_created("I_new").expect("a new issue");
        let Op::Create {
            temp,
            new: NewItem::Existing { hit },
            presets: kept,
        } = &add
        else {
            panic!("expected an add of the existing issue, got {add:?}")
        };
        assert_eq!(temp.as_str(), "local:1");
        assert_eq!(kept, &presets);
        assert_eq!(
            (hit.id.as_str(), hit.repo.as_str(), hit.title.as_str()),
            ("I_new", "tviles/t", "New bug")
        );
        assert!(!hit.pull && hit.open);
        assert_eq!(
            add.describe(None),
            "add the new issue 'New bug' (tviles/t) to the project"
        );
        let mut s = snapshot();
        add.apply(&mut s);
        assert_eq!(s.items[&ItemId::new("local:1")].number(), None, "no #0");
        assert_eq!(add.add_created("I_x"), None, "an add is not a new issue");
        assert_eq!(
            Op::Archive {
                item: ItemId::new("a")
            }
            .add_created("I_x"),
            None
        );
    }
}
