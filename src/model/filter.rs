//! Reads GitHub-style view filters. `Filter` parses one and evaluates it on the client;
//! `column_constraint` reads what a filter says about one field, so the board can hide the
//! columns the filter excludes; `still_matches` estimates whether an edited GitHub item
//! still belongs in its view.

use crate::model::field::{Field, FieldKind, Iteration};
use crate::model::ids::FieldId;
use crate::model::input::parse_iso_date;
use crate::model::item::{ContentState, FieldValue, Item, ItemContent};
use std::collections::BTreeSet;
use std::slice;

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

/// Whether `item` still matches `filter`, as far as can be told without GitHub: every
/// single-select and iteration field the filter names allows the item's value, and an item
/// is archived exactly when the filter has `is:archived`. Qualifiers this cannot read count
/// as matching; GitHub's next list for the view decides those.
pub fn still_matches(filter: &str, item: &Item, fields: &[Field]) -> bool {
    let archived_view = tokens(filter)
        .iter()
        .any(|t| t.eq_ignore_ascii_case("is:archived"));
    if item.archived != archived_view {
        return false;
    }
    fields.iter().filter(|f| f.kind.can_be_column()).all(|f| {
        let constraint = column_constraint(filter, f);
        if !constraint.is_constrained() {
            return true;
        }
        let buckets = f.buckets().unwrap_or_default();
        let bucket = item
            .value(&f.id)
            .and_then(|v| v.bucket_key())
            .and_then(|key| buckets.iter().find(|b| b.matches(&key)).cloned());
        match bucket {
            Some(b) => constraint.allows(&b.title, false),
            None => constraint.allows("", true),
        }
    })
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

/// What a filter is evaluated against.
#[derive(Debug, Clone, Copy)]
pub struct FilterContext<'a> {
    /// The board's fields, which field keys name.
    pub fields: &'a [Field],
    /// The viewer's user id, compared with `ContentFields::assignee_ids` values for `@me`.
    pub me: Option<&'a str>,
    /// The viewer's date, for `@today` and an iteration's `@current`, `@next` and
    /// `@previous`. Without it those values match nothing.
    pub today: Option<time::Date>,
}

/// A GitHub-style filter, parsed once and evaluated on the client.
///
/// Terms are ANDed; a comma in values ORs them; `-` negates a qualifier; keys and values are
/// case-insensitive; a field key is the field's name, or its name with spaces as hyphens.
/// A key that names no field matches nothing and is reported by `unknown_keys`. `OR`, `AND`
/// and parentheses are free text.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    terms: Vec<Term>,
    /// Keys that name nothing: lowercase, in typed order, once each.
    unknown: Vec<String>,
    /// `is:` values this build does not read, as `is:<value>`: lowercase, in typed order,
    /// once each.
    unknown_values: Vec<String>,
    /// `OR`, `AND` or parentheses appear, so `constraint` reads nothing.
    grouped: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Term {
    /// Free text (lowercase, quotes removed), found in the title or the reference.
    Text(String),
    /// `[-]key:a,b`, values trimmed and lowercase.
    Qualifier {
        negated: bool,
        key: Key,
        values: Vec<String>,
    },
    /// `[-]no:key`
    NoValue { negated: bool, key: Key },
    /// `[-]has:key`
    Has { negated: bool, key: Key },
    /// `[-]is:a,b`
    Is { negated: bool, what: Vec<IsWhat> },
}

#[derive(Debug, Clone, PartialEq)]
enum Key {
    Field(FieldId),
    /// `assignee`, `assignees`, or the name of a field of kind `Assignees`.
    Assignee,
    /// `label`, `labels`, or the name of a field of kind `Labels`.
    Label,
    Unknown(String),
}

impl Key {
    /// The key for `key` (lowercase): a field by name first, then the built-in keys.
    fn resolve(key: &str, fields: &[Field]) -> Key {
        match fields.iter().find(|f| names_field(key, f)) {
            Some(f) if matches!(f.kind, FieldKind::Assignees) => Key::Assignee,
            Some(f) if matches!(f.kind, FieldKind::Labels) => Key::Label,
            Some(f) => Key::Field(f.id.clone()),
            None => match key {
                "assignee" | "assignees" => Key::Assignee,
                "label" | "labels" => Key::Label,
                _ => Key::Unknown(key.to_string()),
            },
        }
    }

    /// Whether this key is `field`.
    fn is(&self, field: &Field) -> bool {
        matches!(self, Key::Field(id) if *id == field.id)
    }

    /// Whether `item` has a value for this key; `None` when the key names nothing in `ctx`.
    fn has_value(&self, item: &Item, ctx: &FilterContext) -> Option<bool> {
        Some(match self {
            Key::Unknown(_) => return None,
            Key::Assignee => !item.assignees().is_empty(),
            Key::Label => !item.labels().is_empty(),
            Key::Field(id) => {
                let field = ctx.fields.iter().find(|f| f.id == *id)?;
                item.field_value(field).is_some_and(|v| !is_blank(&v))
            }
        })
    }

    /// Whether `item`'s value for this key is one of `values`; `None` when the key names
    /// nothing in `ctx`.
    fn matches(&self, values: &[String], item: &Item, ctx: &FilterContext) -> Option<bool> {
        let any = |names: Vec<&str>, v: &String| names.iter().any(|n| n.to_lowercase() == *v);
        Some(match self {
            Key::Unknown(_) => return None,
            Key::Assignee => values.iter().any(|v| match v.as_str() {
                "@me" => ctx
                    .me
                    .is_some_and(|me| item.content_fields.assignee_ids.values().any(|id| id == me)),
                _ => any(item.assignees(), v),
            }),
            Key::Label => values.iter().any(|v| any(item.label_names(), v)),
            Key::Field(id) => {
                let field = ctx.fields.iter().find(|f| f.id == *id)?;
                item.field_value(field).is_some_and(|value| {
                    values
                        .iter()
                        .any(|v| value_matches(field, &value, v, ctx.today))
                })
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum IsWhat {
    Open,
    Closed,
    Issue,
    Archived,
    /// A value this build does not read; it matches nothing.
    Other,
}

impl IsWhat {
    fn parse(value: &str) -> IsWhat {
        match value.trim().to_lowercase().as_str() {
            "open" => IsWhat::Open,
            "closed" => IsWhat::Closed,
            "issue" => IsWhat::Issue,
            "archived" => IsWhat::Archived,
            _ => IsWhat::Other,
        }
    }

    fn matches(self, item: &Item) -> bool {
        let state = match &item.content {
            ItemContent::Issue { state, .. } | ItemContent::PullRequest { state, .. } => {
                Some(*state)
            }
            ItemContent::Draft { .. } => Some(ContentState::Open),
            ItemContent::Redacted | ItemContent::Unknown { .. } => None,
        };
        match self {
            IsWhat::Open => state == Some(ContentState::Open),
            IsWhat::Closed => matches!(state, Some(ContentState::Closed | ContentState::Merged)),
            IsWhat::Issue => matches!(item.content, ItemContent::Issue { .. }),
            IsWhat::Archived => item.archived,
            IsWhat::Other => false,
        }
    }
}

impl Term {
    fn matches(&self, item: &Item, ctx: &FilterContext) -> bool {
        match self {
            Term::Text(text) => {
                item.title().to_lowercase().contains(text)
                    || item.ref_label().to_lowercase().contains(text)
            }
            Term::Qualifier {
                negated,
                key,
                values,
            } => key
                .matches(values, item, ctx)
                .is_some_and(|m| m != *negated),
            Term::NoValue { negated, key } => {
                key.has_value(item, ctx).is_some_and(|has| has == *negated)
            }
            Term::Has { negated, key } => {
                key.has_value(item, ctx).is_some_and(|has| has != *negated)
            }
            Term::Is { negated, what } => what.iter().any(|w| w.matches(item)) != *negated,
        }
    }

    fn unknown_key(&self) -> Option<&str> {
        match self {
            Term::Qualifier { key, .. } | Term::NoValue { key, .. } | Term::Has { key, .. } => {
                match key {
                    Key::Unknown(k) => Some(k),
                    _ => None,
                }
            }
            Term::Text(_) | Term::Is { .. } => None,
        }
    }
}

/// `OR`, `AND` and parentheses: free text, and a reason not to guess at columns.
fn is_grouping(token: &str) -> bool {
    token == "OR" || token == "AND" || token.starts_with('(') || token.ends_with(')')
}

/// A value present but empty (no users, no labels) counts as no value.
fn is_blank(value: &FieldValue) -> bool {
    match value {
        FieldValue::Text(t) => t.is_empty(),
        FieldValue::MultiSelect { names, .. } => names.is_empty(),
        FieldValue::Labels(labels) => labels.is_empty(),
        FieldValue::Users(users) | FieldValue::Reviewers(users) => users.is_empty(),
        FieldValue::PullRequests(prs) => prs.is_empty(),
        _ => false,
    }
}

/// Whether `value` (of `field`) is `wanted` (lowercase): an option or iteration by name, a
/// number or date compared, text contained.
fn value_matches(
    field: &Field,
    value: &FieldValue,
    wanted: &str,
    today: Option<time::Date>,
) -> bool {
    let named = |name: &str| name.to_lowercase() == wanted;
    match value {
        FieldValue::SingleSelect { name, .. } => named(name),
        FieldValue::MultiSelect { names, .. } => names.iter().any(|n| named(n)),
        FieldValue::Iteration {
            iteration_id,
            title,
            ..
        } => {
            if wanted.starts_with('@') {
                today
                    .and_then(|today| relative_iteration(field, wanted, today))
                    .is_some_and(|i| i.id == *iteration_id)
            } else {
                named(title)
            }
        }
        FieldValue::Number(n) => compare(*n, wanted, |b| b.parse().ok()),
        FieldValue::Date(d) => parse_iso_date(d.get(..10).unwrap_or(d)).is_some_and(|date| {
            compare(date, wanted, |b| match b {
                "@today" => today,
                _ => parse_iso_date(b),
            })
        }),
        FieldValue::Text(t) => t.to_lowercase().contains(wanted),
        FieldValue::Labels(labels) => labels.iter().any(|l| named(&l.name)),
        FieldValue::Users(users) | FieldValue::Reviewers(users) => users.iter().any(|u| named(u)),
        other => named(&other.display()),
    }
}

/// Whether `value` satisfies `wanted`: `x`, `>x`, `>=x`, `<x`, `<=x` or `a..b` (both ends
/// included), each bound read by `bound`. A bound it cannot read matches nothing.
fn compare<T: PartialOrd>(value: T, wanted: &str, bound: impl Fn(&str) -> Option<T>) -> bool {
    let check = || {
        Some(if let Some(b) = wanted.strip_prefix(">=") {
            value >= bound(b)?
        } else if let Some(b) = wanted.strip_prefix("<=") {
            value <= bound(b)?
        } else if let Some(b) = wanted.strip_prefix('>') {
            value > bound(b)?
        } else if let Some(b) = wanted.strip_prefix('<') {
            value < bound(b)?
        } else if let Some((low, high)) = wanted.split_once("..") {
            bound(low)? <= value && value <= bound(high)?
        } else {
            value == bound(wanted)?
        })
    };
    check().unwrap_or(false)
}

/// The iteration of `field` that `at` names on `today`: `@current` (start ≤ today < end),
/// `@next` (the earliest starting after today) or `@previous` (the latest ended by today).
fn relative_iteration<'f>(field: &'f Field, at: &str, today: time::Date) -> Option<&'f Iteration> {
    let FieldKind::Iteration {
        iterations,
        completed,
    } = &field.kind
    else {
        return None;
    };
    let dated = iterations.iter().chain(completed).filter_map(|i| {
        let start = parse_iso_date(&i.start_date)?;
        let end = start.checked_add(time::Duration::days(i.duration_days.into()))?;
        Some((i, start, end))
    });
    let found = match at {
        "@current" => dated
            .filter(|(_, start, end)| *start <= today && today < *end)
            .min_by_key(|(_, start, _)| *start),
        "@next" => dated
            .filter(|(_, start, _)| *start > today)
            .min_by_key(|(_, start, _)| *start),
        "@previous" => dated
            .filter(|(_, _, end)| *end <= today)
            .max_by_key(|(_, _, end)| *end),
        _ => None,
    };
    found.map(|(i, _, _)| i)
}

impl Filter {
    /// Parses `text`; field keys are looked up in `fields`.
    pub fn parse(text: &str, fields: &[Field]) -> Filter {
        let tokens = tokens(text);
        let grouped = tokens.iter().any(|t| is_grouping(t));
        let mut terms = Vec::new();
        let mut unknown_values: Vec<String> = Vec::new();
        for token in tokens {
            let parsed = if is_grouping(&token) {
                None
            } else {
                qualifier(&token)
            };
            let Some((negated, key, values)) = parsed else {
                terms.push(Term::Text(token.replace('"', "").to_lowercase()));
                continue;
            };
            let values = split_unquoted(values, ',');
            match key.as_str() {
                // An empty value (`no:`, `has:status,`) is still being typed: no term, and no
                // unknown key.
                "no" | "has" => {
                    terms.extend(values.iter().filter(|v| !v.trim().is_empty()).map(|v| {
                        let named = Key::resolve(&v.to_lowercase(), fields);
                        if key == "no" {
                            Term::NoValue {
                                negated,
                                key: named,
                            }
                        } else {
                            Term::Has {
                                negated,
                                key: named,
                            }
                        }
                    }))
                }
                "is" => {
                    for v in values.iter().map(|v| v.trim().to_lowercase()) {
                        let typed = format!("is:{v}");
                        if !v.is_empty()
                            && IsWhat::parse(&v) == IsWhat::Other
                            && !unknown_values.contains(&typed)
                        {
                            unknown_values.push(typed);
                        }
                    }
                    terms.push(Term::Is {
                        negated,
                        what: values.iter().map(|v| IsWhat::parse(v)).collect(),
                    })
                }
                _ => terms.push(Term::Qualifier {
                    negated,
                    key: Key::resolve(&key, fields),
                    values: values.iter().map(|v| v.trim().to_lowercase()).collect(),
                }),
            }
        }
        let mut unknown: Vec<String> = Vec::new();
        for key in terms.iter().filter_map(Term::unknown_key) {
            if !unknown.iter().any(|k| k == key) {
                unknown.push(key.to_string());
            }
        }
        Filter {
            terms,
            unknown,
            unknown_values,
            grouped,
        }
    }

    /// True for a filter with no terms, which every live item matches.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Keys that name no field (`stauts` in `stauts:Todo`): lowercase, in typed order, once
    /// each.
    pub fn unknown_keys(&self) -> &[String] {
        &self.unknown
    }

    /// `is:` values this build does not read (`is:closd`): lowercase, as `is:<value>`, in
    /// typed order, once each. Like an unknown key, such a term matches nothing.
    pub fn unknown_values(&self) -> &[String] {
        &self.unknown_values
    }

    /// Whether the filter asks for archived items (`is:archived`); they match nothing else.
    pub fn wants_archived(&self) -> bool {
        self.terms.iter().any(
            |t| matches!(t, Term::Is { negated: false, what } if what.contains(&IsWhat::Archived)),
        )
    }

    /// Whether `item` matches every term. An archived item matches only a filter with
    /// `is:archived`, and a live one never does.
    pub fn matches(&self, item: &Item, ctx: &FilterContext) -> bool {
        item.archived == self.wants_archived() && self.terms.iter().all(|t| t.matches(item, ctx))
    }

    /// What the filter says about the columns of `field` (see `column_constraint`). With
    /// `ctx.today`, `@current`, `@next` and `@previous` on an iteration field name that
    /// iteration's column.
    pub fn constraint(&self, field: &Field, ctx: &FilterContext) -> ColumnConstraint {
        let mut c = ColumnConstraint::default();
        // Boolean operators and grouping change what a qualifier means; don't guess.
        if self.grouped {
            return c;
        }
        for term in &self.terms {
            match term {
                Term::NoValue { negated, key } | Term::Has { negated, key } if key.is(field) => {
                    // `no:` and `-has:` keep only the "No <field>" column; `-no:` and `has:`
                    // hide it.
                    if matches!(term, Term::NoValue { .. }) != *negated {
                        c.include.get_or_insert_with(BTreeSet::new);
                        c.show_no_value = true;
                    } else {
                        c.hide_no_value = true;
                    }
                }
                Term::Qualifier {
                    negated,
                    key,
                    values,
                } if key.is(field) => {
                    let mut titles = Vec::new();
                    for v in values {
                        let relative = match (&field.kind, ctx.today) {
                            (FieldKind::Iteration { .. }, Some(today))
                                if matches!(v.as_str(), "@current" | "@next" | "@previous") =>
                            {
                                Some(relative_iteration(field, v, today))
                            }
                            _ => None,
                        };
                        match relative {
                            Some(found) => titles.extend(found.map(|i| i.title.to_lowercase())),
                            None if v.is_empty()
                                || v.starts_with('@')
                                || v.starts_with(['>', '<'])
                                || v.contains("..") =>
                            {
                                return ColumnConstraint::default();
                            }
                            None => titles.push(v.clone()),
                        }
                    }
                    if *negated {
                        c.exclude.extend(titles);
                    } else {
                        c.include.get_or_insert_with(BTreeSet::new).extend(titles);
                    }
                }
                _ => {}
            }
        }
        c
    }
}

/// What `filter` says about the columns of `field`.
///
/// Understood for that field: `field:a,"b c"`, `-field:a`, `no:field`, `-no:field` and
/// `has:field`. Include terms add up; exclude terms always remove. An `@` value or a range
/// on the field means the filter is not understood, and nothing is constrained.
pub fn column_constraint(filter: &str, field: &Field) -> ColumnConstraint {
    let fields = slice::from_ref(field);
    let ctx = FilterContext {
        fields,
        me: None,
        today: None,
    };
    Filter::parse(filter, fields).constraint(field, &ctx)
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
                        kind: None,
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

    use crate::model::FieldValue;
    use crate::model::item::tests::issue;

    /// An item whose Status (the `status()` helper: field `F`, option ids equal to their
    /// names) is `option`.
    fn with_status(option: &str) -> crate::model::Item {
        let mut i = issue("i", 1, "x");
        i.values.insert(
            FieldId::new("F"),
            FieldValue::SingleSelect {
                option_id: OptionId::new(option),
                name: option.into(),
            },
        );
        i
    }

    #[test]
    fn still_matches_reads_select_qualifiers_and_archived() {
        let f = vec![status()];
        assert!(still_matches("status:Todo", &with_status("Todo"), &f));
        assert!(!still_matches("status:Todo", &with_status("Done"), &f));
        assert!(!still_matches("-status:Done", &with_status("Done"), &f));
        assert!(
            still_matches("-status:Done label:bug", &with_status("Todo"), &f),
            "other qualifiers are GitHub's call"
        );
        assert!(
            !still_matches("status:Todo", &issue("i", 1, "x"), &f),
            "no value, include terms"
        );
        assert!(still_matches("no:status", &issue("i", 1, "x"), &f));
        let mut archived = with_status("Todo");
        archived.archived = true;
        assert!(!still_matches("", &archived, &f));
        assert!(still_matches("is:archived", &archived, &f));
        assert!(!still_matches("is:archived", &with_status("Todo"), &f));
    }

    // ---- The client-side evaluator (`Filter`) ----

    use crate::model::item::{ItemContent, Label};

    /// A Linear-style board: two selects, a cycle, plain fields, assignees and labels.
    /// Option and iteration ids equal their names.
    fn linear_board() -> Vec<Field> {
        let select = |id: &str, name: &str, options: &[&str]| Field {
            id: FieldId::new(id),
            name: name.into(),
            kind: field(name, options).kind,
        };
        let plain = |id: &str, name: &str, kind: FieldKind| Field {
            id: FieldId::new(id),
            name: name.into(),
            kind,
        };
        let cycle = |title: &str, start: &str| Iteration {
            id: IterationId::new(title),
            title: title.into(),
            start_date: start.into(),
            duration_days: 14,
        };
        vec![
            select("status", "Status", &["Todo", "In Progress", "Done"]),
            select("priority", "Priority", &["Urgent", "High", "Medium", "Low"]),
            plain(
                "cycle",
                "Cycle",
                FieldKind::Iteration {
                    iterations: vec![cycle("Cycle 2", "2026-10-19")],
                    completed: vec![cycle("Cycle 1", "2026-10-05")],
                },
            ),
            plain("estimate", "Estimate", FieldKind::Number),
            plain("due", "Due date", FieldKind::Date),
            plain("notes", "Notes", FieldKind::Text),
            plain("assignees", "Assignees", FieldKind::Assignees),
            plain("labels", "Labels", FieldKind::Labels),
        ]
    }

    /// An issue keyed `key` (`TBA-7`) with `values` as (field name, value) pairs on
    /// `linear_board()`; assignees and labels are comma-separated.
    fn item(key: &str, title: &str, values: &[(&str, &str)]) -> Item {
        let fields = linear_board();
        let number = key
            .rsplit('-')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let mut i = issue(key, number, title);
        if let ItemContent::Issue { reference, .. } = &mut i.content {
            reference.key = Some(key.into());
        }
        for (name, value) in values {
            let f = fields.iter().find(|f| f.name == *name).unwrap();
            let v = match &f.kind {
                FieldKind::SingleSelect { .. } | FieldKind::Iteration { .. } => {
                    f.value_for_bucket(value).unwrap()
                }
                FieldKind::Number => FieldValue::Number(value.parse().unwrap()),
                FieldKind::Date => FieldValue::Date((*value).into()),
                FieldKind::Text => FieldValue::Text((*value).into()),
                FieldKind::Assignees => {
                    FieldValue::Users(value.split(',').map(Into::into).collect())
                }
                FieldKind::Labels => FieldValue::Labels(
                    value
                        .split(',')
                        .map(|n| Label {
                            name: n.into(),
                            color: "fff".into(),
                        })
                        .collect(),
                ),
                other => panic!("no test value for {other:?}"),
            };
            i.values.insert(f.id.clone(), v);
        }
        i
    }

    fn ctx(fields: &[Field]) -> FilterContext<'_> {
        FilterContext {
            fields,
            me: Some("u-ada"),
            today: Some(time::macros::date!(2026 - 10 - 20)),
        }
    }

    /// Checks each `(filter, item, expected)` row against `ctx`.
    fn check(fields: &[Field], ctx: &FilterContext, rows: &[(&str, &Item, bool)]) {
        for (q, i, want) in rows {
            assert_eq!(
                Filter::parse(q, fields).matches(i, ctx),
                *want,
                "filter {q:?} on {} {:?}",
                i.ref_label(),
                i.title()
            );
        }
    }

    #[test]
    fn free_text_matches_title_and_key() {
        let f = linear_board();
        let i = item("TBA-7", "Crash on start", &[]);
        for (q, want) in [
            ("crash", true),
            ("tba-7", true),
            ("\"on start\"", true),
            ("crash zzz", false),
            ("OR", false),
        ] {
            assert_eq!(Filter::parse(q, &f).matches(&i, &ctx(&f)), want, "{q}");
        }
    }

    #[test]
    fn unknown_keys_match_nothing_and_are_reported() {
        let f = linear_board();
        let parsed = Filter::parse("stauts:Todo label:bug Stauts:Done", &f);
        assert_eq!(parsed.unknown_keys(), ["stauts"]);
        assert!(!parsed.matches(&item("TBA-1", "x", &[("Status", "Todo")]), &ctx(&f)));
        let typo = Filter::parse("-stauts:Todo no:prority", &f);
        assert_eq!(typo.unknown_keys(), ["stauts", "prority"]);
        assert!(!typo.matches(&item("TBA-1", "x", &[]), &ctx(&f)));
        assert!(
            Filter::parse("status:Todo crash", &f)
                .unknown_keys()
                .is_empty()
        );
    }

    #[test]
    fn unknown_is_values_are_reported() {
        let f = linear_board();
        let parsed = Filter::parse("is:closd is:open IS:Closd", &f);
        assert_eq!(parsed.unknown_values(), ["is:closd"]);
        assert!(parsed.unknown_keys().is_empty());
        assert!(!parsed.matches(&item("TBA-1", "x", &[]), &ctx(&f)));
        for q in ["is:open,closed -is:archived is:", "is:issue,archived"] {
            assert!(Filter::parse(q, &f).unknown_values().is_empty(), "{q}");
        }
    }

    #[test]
    fn an_empty_no_or_has_value_is_not_an_unknown_key() {
        let f = linear_board();
        for q in ["no:", "has:", "has:status,", "-no:,priority", "no:\"\""] {
            assert!(Filter::parse(q, &f).unknown_keys().is_empty(), "{q}");
        }
        let half = Filter::parse("has:status,", &f);
        assert!(half.matches(&item("TBA-1", "x", &[("Status", "Todo")]), &ctx(&f)));
        assert!(!half.matches(&item("TBA-2", "x", &[]), &ctx(&f)));
        assert!(Filter::parse("no:", &f).matches(&item("TBA-3", "x", &[]), &ctx(&f)));
    }

    #[test]
    fn select_label_and_assignee_qualifiers() {
        let f = linear_board();
        let a = item(
            "TBA-1",
            "a",
            &[
                ("Status", "In Progress"),
                ("Priority", "High"),
                ("Labels", "bug,ui"),
                ("Assignees", "Ada"),
            ],
        );
        let b = item("TBA-2", "b", &[("Status", "Done"), ("Priority", "Low")]);
        let c = item("TBA-3", "c", &[]);
        check(
            &f,
            &ctx(&f),
            &[
                ("status:\"in progress\"", &a, true),
                ("status:\"in progress\"", &b, false),
                ("Status:DONE", &b, true),
                ("priority:high,urgent", &a, true),
                ("priority:high,urgent", &b, false),
                ("priority:high,urgent", &c, false),
                ("label:bug", &a, true),
                ("label:bug", &b, false),
                ("labels:UI", &a, true),
                ("assignee:Ada", &a, true),
                ("assignee:Ada", &b, false),
                ("assignees:ada", &a, true),
                ("assignees:bob,ada", &a, true),
                ("-label:bug", &a, false),
                ("-label:bug", &b, true),
                ("-status:Done,Todo", &a, true),
                ("-status:Done,Todo", &b, false),
                ("-status:Done,Todo", &c, true),
                ("status:\"in progress\" label:bug", &a, true),
                ("status:\"in progress\" label:feature", &a, false),
            ],
        );
    }

    #[test]
    fn no_and_has() {
        let f = linear_board();
        let full = item(
            "TBA-1",
            "a",
            &[
                ("Assignees", "Ada"),
                ("Labels", "bug"),
                ("Cycle", "Cycle 2"),
                ("Priority", "High"),
                ("Due date", "2026-10-20"),
            ],
        );
        let empty = item("TBA-2", "b", &[]);
        check(
            &f,
            &ctx(&f),
            &[
                ("no:assignee", &full, false),
                ("no:assignee", &empty, true),
                ("has:label", &full, true),
                ("has:label", &empty, false),
                ("no:cycle", &full, false),
                ("no:cycle", &empty, true),
                ("-no:priority", &full, true),
                ("-no:priority", &empty, false),
                ("has:due-date", &full, true),
                ("has:due-date", &empty, false),
                ("has:\"Due date\"", &full, true),
                ("-has:labels", &empty, true),
            ],
        );
    }

    #[test]
    fn at_me_uses_the_viewers_user_id() {
        let f = linear_board();
        let assigned = |name: &str, id: &str| {
            let mut i = item("TBA-1", "x", &[("Assignees", name)]);
            i.content_fields.assignee_ids.insert(name.into(), id.into());
            i
        };
        let ada = assigned("Ada", "u-ada");
        let bob = assigned("Bob", "u-bob");
        check(
            &f,
            &ctx(&f),
            &[
                ("assignee:@me", &ada, true),
                ("assignee:@me", &bob, false),
                ("-assignee:@me", &bob, true),
                ("assignee:bob,@me", &bob, true),
            ],
        );
        let anonymous = FilterContext {
            me: None,
            ..ctx(&f)
        };
        check(
            &f,
            &anonymous,
            &[("assignee:@me", &ada, false), ("assignee:@me", &bob, false)],
        );
    }

    #[test]
    fn cycle_at_values_use_today() {
        let f = linear_board();
        let one = item("TBA-1", "x", &[("Cycle", "Cycle 1")]);
        let two = item("TBA-2", "x", &[("Cycle", "Cycle 2")]);
        let none = item("TBA-3", "x", &[]);
        check(
            &f,
            &ctx(&f),
            &[
                ("cycle:@current", &two, true),
                ("cycle:@current", &one, false),
                ("cycle:@current", &none, false),
                ("cycle:@previous", &one, true),
                ("cycle:@previous", &two, false),
                ("cycle:@next", &one, false),
                ("cycle:@next", &two, false),
                ("cycle:\"cycle 1\"", &one, true),
                ("-cycle:@current", &one, true),
                ("-cycle:@current", &two, false),
            ],
        );
        let no_today = FilterContext {
            today: None,
            ..ctx(&f)
        };
        for q in ["cycle:@current", "cycle:@previous", "cycle:@next"] {
            for i in [&one, &two, &none] {
                assert!(!Filter::parse(q, &f).matches(i, &no_today), "{q}");
            }
        }
        let start = FilterContext {
            today: Some(time::macros::date!(2026 - 10 - 18)),
            ..ctx(&f)
        };
        check(
            &f,
            &start,
            &[
                ("cycle:@current", &one, true),
                ("cycle:@next", &two, true),
                ("cycle:@previous", &one, false),
            ],
        );
    }

    #[test]
    fn numbers_and_dates_compare() {
        let f = linear_board();
        let e = |n: &str| item("TBA-1", "x", &[("Estimate", n)]);
        let (one, three, five) = (e("1"), e("3"), e("5"));
        let due = |d: &str| item("TBA-2", "x", &[("Due date", d)]);
        let (soon, later) = (due("2026-10-20"), due("2026-11-05"));
        let note = |n: &str| item("TBA-3", "x", &[("Notes", n)]);
        let (flaky, solid) = (note("A Flaky test"), note("solid"));
        let bare = item("TBA-4", "x", &[]);
        check(
            &f,
            &ctx(&f),
            &[
                ("estimate:>2", &three, true),
                ("estimate:>2", &one, false),
                ("estimate:>2", &bare, false),
                ("estimate:>=3", &three, true),
                ("estimate:<3", &three, false),
                ("estimate:1..3", &one, true),
                ("estimate:1..3", &three, true),
                ("estimate:1..3", &five, false),
                ("estimate:<=1", &one, true),
                ("estimate:<=1", &three, false),
                ("estimate:3", &three, true),
                ("estimate:3,5", &five, true),
                ("estimate:3", &one, false),
                ("estimate:many", &one, false),
                ("due-date:<2026-11-01", &soon, true),
                ("due-date:<2026-11-01", &later, false),
                ("due-date:<2026-11-01", &bare, false),
                ("due-date:2026-11-05", &later, true),
                ("due-date:2026-10-01..2026-10-31", &soon, true),
                ("due-date:2026-10-01..2026-10-31", &later, false),
                ("due-date:@today", &soon, true),
                ("due-date:@today", &later, false),
                ("due-date:>@today", &later, true),
                ("notes:flaky", &flaky, true),
                ("notes:flaky", &solid, false),
                ("-notes:flaky", &solid, true),
            ],
        );
        let no_today = FilterContext {
            today: None,
            ..ctx(&f)
        };
        check(&f, &no_today, &[("due-date:@today", &soon, false)]);
    }

    #[test]
    fn is_reads_the_content_state_and_kind() {
        let f = linear_board();
        let open = item("TBA-1", "x", &[]);
        let mut closed = item("TBA-2", "x", &[]);
        if let ItemContent::Issue { state, .. } = &mut closed.content {
            *state = crate::model::ContentState::Closed;
        }
        let mut draft = item("TBA-3", "x", &[]);
        draft.content = ItemContent::Draft { title: "d".into() };
        check(
            &f,
            &ctx(&f),
            &[
                ("is:open", &open, true),
                ("is:open", &closed, false),
                ("is:open", &draft, true),
                ("is:closed", &closed, true),
                ("is:closed", &draft, false),
                ("-is:closed", &open, true),
                ("is:issue", &open, true),
                ("is:issue", &draft, false),
                ("is:open,closed", &closed, true),
            ],
        );
    }

    #[test]
    fn archived_items_match_only_is_archived() {
        let f = linear_board();
        let mut archived = item("TBA-1", "x", &[("Labels", "bug")]);
        archived.archived = true;
        let live = item("TBA-2", "x", &[("Labels", "bug")]);
        check(
            &f,
            &ctx(&f),
            &[
                ("", &archived, false),
                ("label:bug", &archived, false),
                ("is:archived", &archived, true),
                ("is:archived label:bug", &archived, true),
                ("is:archived", &live, false),
                ("label:bug", &live, true),
                ("-is:archived", &live, true),
                ("-is:archived", &archived, false),
            ],
        );
        for (q, want) in [
            ("is:archived", true),
            ("label:bug IS:ARCHIVED", true),
            ("", false),
            ("label:bug", false),
            ("-is:archived", false),
        ] {
            assert_eq!(Filter::parse(q, &f).wants_archived(), want, "{q}");
        }
        assert!(Filter::parse("", &f).is_empty());
        assert!(!Filter::parse("x", &f).is_empty());
    }

    #[test]
    fn constraint_resolves_at_current_when_today_is_known() {
        let f = linear_board();
        let cycle = f.iter().find(|x| x.name == "Cycle").unwrap();
        let current = Filter::parse("cycle:@current", &f).constraint(cycle, &ctx(&f));
        assert!(current.allows("Cycle 2", false));
        assert!(!current.allows("Cycle 1", false));
        assert!(!current.allows("", true));
        let not_previous = Filter::parse("-cycle:@previous", &f).constraint(cycle, &ctx(&f));
        assert!(!not_previous.allows("Cycle 1", false));
        assert!(not_previous.allows("Cycle 2", false));
        let no_today = FilterContext {
            today: None,
            ..ctx(&f)
        };
        let unknown = Filter::parse("cycle:@current", &f).constraint(cycle, &no_today);
        assert!(!unknown.is_constrained());
        assert_eq!(unknown, column_constraint("cycle:@current", cycle));
        let status = &f[0];
        assert_eq!(
            Filter::parse("status:Todo label:bug", &f).constraint(status, &ctx(&f)),
            column_constraint("status:Todo label:bug", status)
        );
    }
}
