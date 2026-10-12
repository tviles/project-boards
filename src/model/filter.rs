//! Reads what a GitHub view filter says about one field, so the board can hide the columns
//! the filter excludes. Only that field's qualifiers are read; everything else is ignored.

use crate::model::field::Field;
use std::collections::BTreeSet;

/// The columns a filter allows for one field. The default constrains nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnConstraint {
    /// Option or iteration titles (lowercase) named by include terms; `None` without any.
    include: Option<BTreeSet<String>>,
    /// Titles (lowercase) named by exclude terms.
    exclude: BTreeSet<String>,
    /// `-no:field` or `has:field`: the "No <field>" column is hidden.
    hide_no_value: bool,
    /// `no:field` or `-has:field`: items without a value match, so the "No <field>" column
    /// stays even when include terms name other values.
    show_no_value: bool,
}

impl ColumnConstraint {
    /// True when the filter limits the field's columns at all.
    pub fn is_constrained(&self) -> bool {
        self.include.is_some() || !self.exclude.is_empty() || self.hide_no_value
    }

    /// Whether the column titled `title` stays; `no_value` marks the "No <field>" column.
    pub fn allows(&self, title: &str, no_value: bool) -> bool {
        if no_value {
            // Include terms (`status:Todo`) only match items that have one of those values,
            // so the "No <field>" column is empty and hidden unless `no:field` asks for it.
            return !self.hide_no_value && (self.include.is_none() || self.show_no_value);
        }
        let title = title.to_lowercase();
        let included = self.include.as_ref().is_none_or(|set| set.contains(&title));
        included && !self.exclude.contains(&title)
    }
}

/// Splits on whitespace outside double quotes.
fn tokens(filter: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in filter.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Splits `text` on `sep` outside double quotes and strips the quotes from each part.
fn split_unquoted(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    out.push(current);
    out
}

/// `[-]key:values` as (negated, lowercase key, raw values); the key may be quoted
/// (`"Story points":3`). `None` for free text.
fn qualifier(token: &str) -> Option<(bool, String, &str)> {
    let (negated, rest) = match token.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    let (key, values) = match rest.strip_prefix('"') {
        Some(body) => {
            let end = body.find('"')?;
            (&body[..end], body[end + 1..].strip_prefix(':')?)
        }
        None => rest.split_once(':')?,
    };
    (!key.is_empty()).then(|| (negated, key.to_lowercase(), values))
}

/// Whether `key` (lowercase) names `field`, as written or with spaces as hyphens.
fn names_field(key: &str, field: &Field) -> bool {
    let name = field.name.to_lowercase();
    key == name || key == name.replace(' ', "-")
}

/// What `filter` says about the columns of `field`.
///
/// Understood for that field: `field:a,"b c"`, `-field:a`, `no:field`, `-no:field` and
/// `has:field`. Include terms add up; exclude terms always remove. An `@` value or a range
/// on the field means the filter is not understood, and nothing is constrained.
pub fn column_constraint(filter: &str, field: &Field) -> ColumnConstraint {
    let mut c = ColumnConstraint::default();
    let tokens = tokens(filter);
    // Boolean operators and grouping change what a qualifier means; don't guess.
    if tokens
        .iter()
        .any(|t| t == "OR" || t == "AND" || t.starts_with('(') || t.ends_with(')'))
    {
        return c;
    }
    for token in tokens {
        let Some((negated, key, values)) = qualifier(&token) else {
            continue;
        };
        if key == "no" || key == "has" {
            let named = split_unquoted(values, ',')
                .iter()
                .any(|v| names_field(&v.to_lowercase(), field));
            if !named {
                continue;
            }
            // `no:` and `-has:` keep only the "No <field>" column; `-no:` and `has:` hide it.
            if (key == "no") != negated {
                c.include.get_or_insert_with(BTreeSet::new);
                c.show_no_value = true;
            } else {
                c.hide_no_value = true;
            }
            continue;
        }
        if !names_field(&key, field) {
            continue;
        }
        let values: Vec<String> = split_unquoted(values, ',')
            .into_iter()
            .map(|v| v.trim().to_lowercase())
            .collect();
        let unsupported = |v: &String| {
            v.is_empty() || v.starts_with('@') || v.starts_with(['>', '<']) || v.contains("..")
        };
        if values.iter().any(unsupported) {
            return ColumnConstraint::default();
        }
        if negated {
            c.exclude.extend(values);
        } else {
            c.include.get_or_insert_with(BTreeSet::new).extend(values);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::field::{FieldKind, Iteration, OptionColor, SelectOption};
    use crate::model::ids::{FieldId, IterationId, OptionId};

    fn field(name: &str, options: &[&str]) -> Field {
        Field {
            id: FieldId::new("F"),
            name: name.into(),
            kind: FieldKind::SingleSelect {
                options: options
                    .iter()
                    .map(|o| SelectOption {
                        id: OptionId::new(*o),
                        name: (*o).into(),
                        color: OptionColor::Gray,
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn include_terms_hide_the_no_value_column_unless_no_is_asked_for() {
        let status = crate::ui::fixtures::project()
            .fields
            .iter()
            .find(|f| f.name == "Status")
            .unwrap()
            .clone();
        let todo_only = column_constraint("status:Todo", &status);
        assert!(!todo_only.allows("", true));
        assert!(todo_only.allows("Todo", false));
        let with_none = column_constraint("status:Todo no:status", &status);
        assert!(with_none.allows("", true));
        assert!(with_none.allows("Todo", false));
        assert!(!with_none.allows("Done", false));
        let none_only = column_constraint("no:status", &status);
        assert!(none_only.allows("", true));
        assert!(!none_only.allows("Todo", false));
        assert!(column_constraint("-status:Done", &status).allows("", true));
    }

    #[test]
    fn boolean_operators_and_grouping_leave_columns_alone() {
        for f in [
            "status:Todo OR label:bug",
            "(status:Todo label:bug)",
            "status:Todo AND -status:Done",
        ] {
            assert!(!column_constraint(f, &status()).is_constrained(), "{f}");
        }
    }

    fn status() -> Field {
        field(
            "Status",
            &["Todo", "Design", "Design Review", "In Progress", "Done"],
        )
    }

    /// The columns `filter` leaves for `f`, the "No" column last as "-".
    fn shown(filter: &str, f: &Field) -> Vec<String> {
        let c = column_constraint(filter, f);
        let mut out: Vec<String> = f
            .buckets()
            .unwrap()
            .iter()
            .filter(|b| c.allows(&b.title, b.key.is_none()))
            .map(|b| {
                if b.key.is_none() {
                    "-".to_string()
                } else {
                    b.title.clone()
                }
            })
            .collect();
        if !c.is_constrained() {
            out.insert(0, "*".into());
        }
        out
    }

    #[test]
    fn qualifiers_on_the_column_field_choose_its_columns() {
        let s = status();
        let all = "* Todo Design Design Review In Progress Done -";
        let cases: &[(&str, &str)] = &[
            ("", all),
            ("status:Todo", "Todo"),
            ("status:todo,DONE", "Todo Done"),
            ("status:\"Design Review\"", "Design Review"),
            ("status:Todo,\"In Progress\",Done", "Todo In Progress Done"),
            ("-status:Done", "Todo Design Design Review In Progress -"),
            (
                "-status:\"Design Review\",Design",
                "Todo In Progress Done -",
            ),
            ("STATUS:Todo Status:Done", "Todo Done"),
            ("status:Todo,Done -status:Done", "Todo"),
            ("no:status", "-"),
            ("no:status status:Todo", "Todo -"),
            ("-no:status", "Todo Design Design Review In Progress Done"),
            ("has:status", "Todo Design Design Review In Progress Done"),
            ("-has:status", "-"),
            ("has:priority", all),
            (
                "label:bug assignee:@me fix crash -status:Done is:open",
                "Todo Design Design Review In Progress -",
            ),
            ("priority:P0 status:Todo", "Todo"),
            ("status:@current", all),
            ("status:Todo,@next", all),
            ("status:>Todo", all),
            ("status:a..b", all),
            ("statusish:Todo", all),
            ("status:Nope", ""), // the board falls back to every column,
        ];
        for (filter, want) in cases {
            assert_eq!(shown(filter, &s).join(" "), *want, "filter {filter:?}");
        }
    }

    #[test]
    fn multi_word_fields_match_quoted_or_hyphenated() {
        let f = field("Design Stage", &["Draft", "Final"]);
        assert_eq!(shown("design-stage:Draft", &f).join(" "), "Draft");
        assert_eq!(shown("\"Design Stage\":Final", &f).join(" "), "Final");
        assert_eq!(shown("-\"design stage\":Final", &f).join(" "), "Draft -");
        assert_eq!(shown("no:design-stage", &f).join(" "), "-");
        assert_eq!(shown("-no:\"Design Stage\"", &f).join(" "), "Draft Final");
        assert_eq!(shown("design:Draft", &f)[0], "*");
    }

    #[test]
    fn iteration_values_match_titles_and_at_values_do_not_constrain() {
        let it = |id: &str| Iteration {
            id: IterationId::new(id),
            title: id.into(),
            start_date: "2026-10-01".into(),
            duration_days: 14,
        };
        let f = Field {
            id: FieldId::new("S"),
            name: "Sprint".into(),
            kind: FieldKind::Iteration {
                iterations: vec![it("Sprint 2"), it("Sprint 3")],
                completed: vec![],
            },
        };
        assert_eq!(shown("sprint:\"sprint 3\"", &f).join(" "), "Sprint 3");
        assert_eq!(shown("sprint:@current", &f)[0], "*");
        assert_eq!(shown("-sprint:@previous", &f)[0], "*");
    }
}
