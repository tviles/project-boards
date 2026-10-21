//! The fixed field and option ids of a Linear board, and the board's own issue filter.
//!
//! No id contains `_`: `find_field` falls back to the part after the first `_`, which would
//! let two of these ids match each other.

use crate::model::teams::{CYCLE_FIELD, GROUP_FIELD_PREFIX, STATUS_FIELD};
use crate::model::{BoardKey, FieldId, OptionId};
use serde_json::{Value, json};

pub const TITLE: &str = "linear:title";
/// From `model::teams`, which resolves team-owned values and cannot see this module.
pub const STATUS: &str = STATUS_FIELD;
pub const PRIORITY: &str = "linear:priority";
pub const ASSIGNEES: &str = "linear:assignees";
pub const LABELS: &str = "linear:labels";
pub const CYCLE: &str = CYCLE_FIELD;
pub const ESTIMATE: &str = "linear:estimate";
pub const DUE: &str = "linear:due";
pub const PROJECT: &str = "linear:project";
pub const CREATED: &str = "linear:created";
pub const UPDATED: &str = "linear:updated";
pub const CLOSED: &str = "linear:closed";
pub const PARENT: &str = "linear:parent";
pub const SUB_ISSUES: &str = "linear:sub-issues";

/// The field of the single-select label group `label_id`.
pub fn group(label_id: &str) -> FieldId {
    FieldId::new(format!("{GROUP_FIELD_PREFIX}{label_id}"))
}

/// The Priority option for Linear's priority `n` (1 Urgent through 4 Low).
pub fn priority_option(n: u8) -> OptionId {
    OptionId::new(format!("linear:priority:{n}"))
}

/// Linear's priority (1 Urgent through 4 Low) of a Priority option; `None` for any other
/// option.
pub fn priority_of(option: &OptionId) -> Option<u8> {
    option
        .as_str()
        .strip_prefix("linear:priority:")?
        .parse()
        .ok()
        .filter(|n| (1..=4).contains(n))
}

/// The `IssueFilter` that keeps a board's issues: its project's or its team's. `Null` for a
/// GitHub board, which has none.
pub fn board_filter(board: &BoardKey) -> Value {
    match board {
        BoardKey::LinearProject { id } => json!({"project": {"id": {"eq": id}}}),
        BoardKey::LinearTeam { id } => json!({"team": {"id": {"eq": id}}}),
        BoardKey::Github(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_no_underscore_and_are_distinct() {
        let all = [
            TITLE, STATUS, PRIORITY, ASSIGNEES, LABELS, CYCLE, ESTIMATE, DUE, PROJECT, CREATED,
            UPDATED, CLOSED, PARENT, SUB_ISSUES,
        ];
        for id in all {
            assert!(!id.contains('_'), "{id}");
        }
        let mut sorted = all.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len());
        assert_eq!(STATUS, "linear:status");
        assert_eq!(CYCLE, "linear:cycle");
        assert_eq!(group("g-1"), FieldId::new("linear:group:g-1"));
        assert_eq!(priority_option(2), OptionId::new("linear:priority:2"));
    }

    #[test]
    fn a_priority_option_reads_back_as_its_priority() {
        for n in 1..=4 {
            assert_eq!(priority_of(&priority_option(n)), Some(n));
        }
        for other in [
            "linear:priority:0",
            "linear:priority:5",
            "linear:priority:x",
            "p0",
            "s-a-todo",
        ] {
            assert_eq!(priority_of(&OptionId::new(other)), None, "{other}");
        }
    }

    #[test]
    fn board_filters_select_the_boards_issues() {
        assert_eq!(
            board_filter(&BoardKey::LinearProject { id: "p1".into() }),
            json!({"project": {"id": {"eq": "p1"}}})
        );
        assert_eq!(
            board_filter(&BoardKey::LinearTeam { id: "t1".into() }),
            json!({"team": {"id": {"eq": "t1"}}})
        );
        assert_eq!(board_filter(&BoardKey::github("acme", 3)), Value::Null);
    }
}
