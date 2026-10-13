//! One `Op` per user action. `apply` is what the edit does to a board snapshot; the store
//! replays pending ops with it on top of the confirmed board. `send` (ops::send) carries
//! an op to GitHub. Generated GraphQL types never appear here.

pub mod queue;
pub mod send;

use crate::model::{
    CloseReason, ContentId, ContentState, FieldId, FieldKind, FieldValue, Item, ItemContent,
    ItemId, Label, LabelRef, MilestoneRef, StateReason, UserRef,
};
use crate::store::BoardSnapshot;
use std::collections::HashMap;

pub type OpId = u64;

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
            | Op::Reopen { item, .. } => item,
        }
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
                    i.content_fields.state_reason = reason.map(|r| match r {
                        CloseReason::Completed => StateReason::Completed,
                        CloseReason::NotPlanned => StateReason::NotPlanned,
                    });
                }
            }
            Op::Reopen { item, .. } => {
                if let Some(i) = snap.items.get_mut(item) {
                    set_state(&mut i.content, ContentState::Open);
                    i.content_fields.state_reason = None;
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
}
