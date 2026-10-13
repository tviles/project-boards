//! One `Op` per user action. `apply` is what the edit does to a board snapshot; the store
//! replays pending ops with it on top of the confirmed board. `send` (ops::send) carries
//! an op to GitHub. Generated GraphQL types never appear here.

use crate::model::{FieldId, FieldValue, Item, ItemId};
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
            Op::SetField { item, .. } | Op::ClearField { item, .. } | Op::Reorder { item, .. } => {
                item
            }
        }
    }

    /// The op's name for logs, which never carry titles or values.
    pub fn name(&self) -> &'static str {
        match self {
            Op::SetField { .. } => "SetField",
            Op::ClearField { .. } => "ClearField",
            Op::Reorder { .. } => "Reorder",
        }
    }

    /// Applies the edit to `snap`. Applying twice is the same as once; an item that is not
    /// in the snapshot is left alone.
    pub fn apply(&self, snap: &mut BoardSnapshot) {
        match self {
            Op::SetField { item, field, value } => {
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
}
