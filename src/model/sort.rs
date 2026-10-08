use crate::model::field::{Field, FieldKind};
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

/// Stable sort by the view's sort specs. Items without a value always sort last.
pub fn sort_items(items: &mut [&Item], specs: &[SortSpec], fields: &[Field]) {
    if specs.is_empty() {
        return;
    }
    items.sort_by(|a, b| {
        for spec in specs {
            let field = fields.iter().find(|f| f.id == spec.field);
            let ka = a.value(&spec.field).map(|v| key(v, field));
            let kb = b.value(&spec.field).map(|v| key(v, field));
            let ord = match (ka, kb) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(x), Some(y)) => {
                    let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
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
        Ordering::Equal
    });
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
    fn no_specs_keeps_board_order() {
        let (a, b) = (issue("a", 1, "z"), issue("b", 2, "a"));
        let mut items = vec![&a, &b];
        sort_items(&mut items, &[], &[]);
        assert_eq!(items[0].id.as_str(), "a");
    }
}
