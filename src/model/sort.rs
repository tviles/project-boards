use crate::model::field::{Field, FieldKind, find_field};
use crate::model::item::{FieldValue, Item};
use crate::model::view::{SortDirection, SortSpec};
use std::cmp::Ordering;

/// A comparable key for one value. Option and iteration values sort in their field's order.
#[derive(Debug, PartialEq, PartialOrd)]
enum Key {
    Num(f64),
    Text(String),
}

fn key(value: &FieldValue, field: Option<&Field>) -> Key {
    match (value, field.map(|f| &f.kind)) {
        (FieldValue::Number(n), _) => Key::Num(*n),
        (FieldValue::SingleSelect { option_id, .. }, Some(FieldKind::SingleSelect { options })) => {
            Key::Num(
                options
                    .iter()
                    .position(|o| &o.id == option_id)
                    .map_or(f64::MAX, |p| p as f64),
            )
        }
        (FieldValue::Iteration { start_date, .. }, _) => Key::Text(start_date.clone()),
        (other, _) => Key::Text(other.display().to_lowercase()),
    }
}

/// Stable sort by the view's sort specs. Items without a value always sort last. Values are
/// the ones cells show (`Item::field_value`), read through the field the spec resolves to.
pub fn sort_items(items: &mut [&Item], specs: &[SortSpec], fields: &[Field]) {
    let specs: Vec<(&SortSpec, Option<&Field>)> = specs
        .iter()
        .map(|spec| (spec, find_field(fields, &spec.field)))
        .collect();
    // Each item's keys are built once, not on every comparison.
    let mut keyed: Vec<(Vec<Option<Key>>, &Item)> = items
        .iter()
        .map(|item| {
            let keys = specs
                .iter()
                .map(|(spec, field)| {
                    let value = match field {
                        Some(f) => item.field_value(f),
                        None => item.value(&spec.field).cloned(),
                    };
                    value.map(|v| key(&v, *field))
                })
                .collect();
            (keys, *item)
        })
        .collect();
    keyed.sort_by(|(a, ia), (b, ib)| {
        for ((spec, _), (ka, kb)) in specs.iter().zip(a.iter().zip(b)) {
            let ord = match (ka, kb) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(x), Some(y)) => {
                    let o = x.partial_cmp(y).unwrap_or(Ordering::Equal);
                    if spec.direction == SortDirection::Desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        // Manual order (Linear) last, ascending; an item with one sorts before an item
        // without (a total order). GitHub items have none, so stability keeps their order.
        match (ia.content_fields.sort_order, ib.content_fields.sort_order) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    });
    for (slot, (_, item)) in items.iter_mut().zip(keyed) {
        *slot = item;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::field::{Field, FieldKind, OptionColor, SelectOption};
    use crate::model::ids::{FieldId, OptionId};
    use crate::model::item::FieldValue;
    use crate::model::item::tests::issue;
    use crate::model::view::{SortDirection, SortSpec};

    fn priority() -> Field {
        let o = |id: &str| SelectOption {
            id: OptionId::new(id),
            name: id.into(),
            color: OptionColor::Gray,
            kind: None,
        };
        Field {
            id: FieldId::new("P"),
            name: "Priority".into(),
            kind: FieldKind::SingleSelect {
                options: vec![o("P0"), o("P1"), o("P2")],
            },
        }
    }

    fn with_priority(id: &str, p: Option<&str>) -> crate::model::Item {
        let mut i = issue(id, 1, id);
        if let Some(p) = p {
            i.values.insert(
                FieldId::new("P"),
                FieldValue::SingleSelect {
                    option_id: OptionId::new(p),
                    name: p.into(),
                },
            );
        }
        i
    }

    #[test]
    fn single_select_sorts_by_option_order_with_missing_last() {
        let fields = vec![priority()];
        let (a, b, c, d) = (
            with_priority("a", Some("P2")),
            with_priority("b", None),
            with_priority("c", Some("P0")),
            with_priority("d", Some("P1")),
        );
        let mut items = vec![&a, &b, &c, &d];
        sort_items(
            &mut items,
            &[SortSpec {
                field: FieldId::new("P"),
                direction: SortDirection::Asc,
            }],
            &fields,
        );
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c", "d", "a", "b"]);
    }

    #[test]
    fn descending_keeps_missing_last() {
        let fields = vec![priority()];
        let (a, b, c) = (
            with_priority("a", Some("P2")),
            with_priority("b", None),
            with_priority("c", Some("P0")),
        );
        let mut items = vec![&a, &b, &c];
        sort_items(
            &mut items,
            &[SortSpec {
                field: FieldId::new("P"),
                direction: SortDirection::Desc,
            }],
            &fields,
        );
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b"]);
    }

    #[test]
    fn built_in_fields_sort_by_their_derived_values() {
        let created = Field {
            id: FieldId::new("PVTF_created"),
            name: "Created".into(),
            kind: FieldKind::Created,
        };
        let at = |id: &str, date: Option<&str>| {
            let mut i = issue(id, 1, id);
            i.content_fields.created_at = date.map(String::from);
            i
        };
        let (a, b, c, d) = (
            at("a", Some("2026-09-02T00:00:00Z")),
            at("b", None),
            at("c", Some("2026-01-15T00:00:00Z")),
            at("d", Some("2026-05-30T00:00:00Z")),
        );
        let mut items = vec![&a, &b, &c, &d];
        // The view names the field by its other id prefix.
        sort_items(
            &mut items,
            &[SortSpec {
                field: FieldId::new("PVTSF_created"),
                direction: SortDirection::Asc,
            }],
            &[created],
        );
        let ids: Vec<_> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c", "d", "a", "b"]);
    }

    #[test]
    fn no_specs_keeps_board_order() {
        let (a, b) = (issue("a", 1, "z"), issue("b", 2, "a"));
        let mut items = vec![&a, &b];
        sort_items(&mut items, &[], &[]);
        assert_eq!(items[0].id.as_str(), "a");
    }

    fn manual(id: &str, order: Option<f64>, p: Option<&str>) -> crate::model::Item {
        let mut i = with_priority(id, p);
        i.content_fields.sort_order = order;
        i
    }

    fn ids(items: &[&crate::model::Item]) -> Vec<String> {
        items.iter().map(|i| i.id.as_str().to_string()).collect()
    }

    #[test]
    fn manual_order_breaks_ties_and_github_items_keep_their_order() {
        let (a, b, c) = (
            manual("a", Some(3.0), Some("P1")),
            manual("b", Some(1.0), Some("P1")),
            manual("c", Some(2.0), Some("P0")),
        );
        let mut items = vec![&a, &b, &c];
        sort_items(&mut items, &[], &[]);
        assert_eq!(ids(&items), ["b", "c", "a"]);

        let spec = SortSpec {
            field: FieldId::new("P"),
            direction: SortDirection::Asc,
        };
        let mut items = vec![&a, &b, &c];
        sort_items(&mut items, &[spec], &[priority()]);
        assert_eq!(ids(&items), ["c", "b", "a"]);

        let (x, y, z) = (
            manual("x", None, None),
            manual("y", None, None),
            manual("z", None, None),
        );
        let mut items = vec![&z, &x, &y];
        sort_items(&mut items, &[], &[]);
        assert_eq!(ids(&items), ["z", "x", "y"]);

        let (a, n1, c, n2) = (
            manual("a", Some(3.0), None),
            manual("n1", None, None),
            manual("c", Some(1.0), None),
            manual("n2", None, None),
        );
        let mut items = vec![&a, &n1, &c, &n2];
        sort_items(&mut items, &[], &[]);
        assert_eq!(ids(&items), ["c", "a", "n1", "n2"]);

        let spec = SortSpec {
            field: FieldId::new("P"),
            direction: SortDirection::Asc,
        };
        let (a, b, c, d) = (
            manual("a", Some(3.0), Some("P1")),
            manual("b", None, Some("P1")),
            manual("c", Some(1.0), Some("P1")),
            manual("d", None, Some("P0")),
        );
        let mut items = vec![&a, &b, &c, &d];
        sort_items(&mut items, &[spec], &[priority()]);
        assert_eq!(ids(&items), ["d", "c", "a", "b"]);
    }
}
