use crate::model::ids::{FieldId, IterationId, OptionId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionColor {
    Gray,
    Blue,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Unknown,
}

impl OptionColor {
    pub fn from_api(value: &str) -> Self {
        match value {
            "GRAY" => Self::Gray,
            "BLUE" => Self::Blue,
            "GREEN" => Self::Green,
            "YELLOW" => Self::Yellow,
            "ORANGE" => Self::Orange,
            "RED" => Self::Red,
            "PINK" => Self::Pink,
            "PURPLE" => Self::Purple,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectOption {
    pub id: OptionId,
    pub name: String,
    pub color: OptionColor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Iteration {
    pub id: IterationId,
    pub title: String,
    /// `YYYY-MM-DD`
    pub start_date: String,
    pub duration_days: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldKind {
    Text,
    Number,
    Date,
    SingleSelect {
        options: Vec<SelectOption>,
    },
    MultiSelect {
        options: Vec<SelectOption>,
    },
    Iteration {
        iterations: Vec<Iteration>,
        completed: Vec<Iteration>,
    },
    Title,
    Assignees,
    Labels,
    Milestone,
    Repository,
    LinkedPullRequests,
    Reviewers,
    ParentIssue,
    SubIssuesProgress,
    IssueType,
    Tracks,
    TrackedBy,
    Created,
    Updated,
    Closed,
    Unsupported {
        type_name: String,
    },
}

impl FieldKind {
    /// Maps `ProjectV2Field.dataType` for plain custom fields and built-in fields.
    pub fn from_data_type(data_type: &str) -> Self {
        match data_type {
            "TEXT" => Self::Text,
            "NUMBER" => Self::Number,
            "DATE" => Self::Date,
            "TITLE" => Self::Title,
            "ASSIGNEES" => Self::Assignees,
            "LABELS" => Self::Labels,
            "MILESTONE" => Self::Milestone,
            "REPOSITORY" => Self::Repository,
            "LINKED_PULL_REQUESTS" => Self::LinkedPullRequests,
            "REVIEWERS" => Self::Reviewers,
            "PARENT_ISSUE" => Self::ParentIssue,
            "SUB_ISSUES_PROGRESS" => Self::SubIssuesProgress,
            "ISSUE_TYPE" => Self::IssueType,
            "TRACKS" => Self::Tracks,
            "TRACKED_BY" => Self::TrackedBy,
            "CREATED" => Self::Created,
            "UPDATED" => Self::Updated,
            "CLOSED" => Self::Closed,
            other => Self::Unsupported {
                type_name: other.to_string(),
            },
        }
    }

    /// Whether a board can use this field for its columns.
    pub fn can_be_column(&self) -> bool {
        matches!(self, Self::SingleSelect { .. } | Self::Iteration { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: FieldId,
    pub name: String,
    pub kind: FieldKind,
}

/// The part of a field id after its first `_`. GitHub lists a single-select under
/// `PVTSSF_…` in a project's fields but under `PVTF_…` in a view's visible fields; the part
/// after the prefix is the same.
fn id_suffix(id: &FieldId) -> Option<&str> {
    id.as_str()
        .split_once('_')
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
}

/// The field with `id`, or failing that the one whose id has the same suffix after the first
/// `_` (see `id_suffix`).
pub fn find_field<'a>(fields: &'a [Field], id: &FieldId) -> Option<&'a Field> {
    fields.iter().find(|f| &f.id == id).or_else(|| {
        let suffix = id_suffix(id)?;
        fields.iter().find(|f| id_suffix(&f.id) == Some(suffix))
    })
}

/// One board column or table group: an option, an iteration, or "no value" (`key: None`).
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub key: Option<String>,
    pub title: String,
    pub color: OptionColor,
    /// A completed iteration; boards hide these when empty.
    pub completed: bool,
    /// Other option ids shown as this bucket: GitHub shows options that share a name as one
    /// column.
    pub aliases: Vec<String>,
}

impl Bucket {
    /// Whether an item's bucket key (an option or iteration id) belongs in this bucket.
    pub fn matches(&self, key: &str) -> bool {
        self.key.as_deref() == Some(key) || self.aliases.iter().any(|a| a == key)
    }
}

impl Field {
    /// Buckets in GitHub order (completed iterations first, oldest first), ending with
    /// "No <field>". `None` for fields that cannot group a board.
    pub fn buckets(&self) -> Option<Vec<Bucket>> {
        let mut out: Vec<Bucket> = match &self.kind {
            // Options sharing a name become one bucket, at the last one's place: that is how
            // GitHub's board shows them.
            FieldKind::SingleSelect { options } => options
                .iter()
                .enumerate()
                .filter(|(i, o)| !options[i + 1..].iter().any(|later| later.name == o.name))
                .map(|(_, o)| Bucket {
                    key: Some(o.id.0.clone()),
                    title: o.name.clone(),
                    color: o.color,
                    completed: false,
                    aliases: options
                        .iter()
                        .filter(|other| other.name == o.name && other.id != o.id)
                        .map(|other| other.id.0.clone())
                        .collect(),
                })
                .collect(),
            FieldKind::Iteration {
                iterations,
                completed,
            } => {
                let mut done: Vec<&Iteration> = completed.iter().collect();
                done.sort_by(|a, b| a.start_date.cmp(&b.start_date));
                done.into_iter()
                    .map(|i| (i, true))
                    .chain(iterations.iter().map(|i| (i, false)))
                    .map(|(i, completed)| Bucket {
                        key: Some(i.id.0.clone()),
                        title: i.title.clone(),
                        color: OptionColor::Gray,
                        completed,
                        aliases: Vec::new(),
                    })
                    .collect()
            }
            _ => return None,
        };
        out.push(Bucket {
            key: None,
            title: format!("No {}", self.name),
            color: OptionColor::Gray,
            completed: false,
            aliases: Vec::new(),
        });
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(id: &str, name: &str) -> SelectOption {
        SelectOption {
            id: OptionId::new(id),
            name: name.into(),
            color: OptionColor::Blue,
        }
    }

    #[test]
    fn data_types_map_to_kinds() {
        assert_eq!(FieldKind::from_data_type("TITLE"), FieldKind::Title);
        assert_eq!(
            FieldKind::from_data_type("ISSUE_TYPE"),
            FieldKind::IssueType
        );
        assert_eq!(
            FieldKind::from_data_type("SOMETHING_NEW"),
            FieldKind::Unsupported {
                type_name: "SOMETHING_NEW".into()
            }
        );
    }

    #[test]
    fn single_select_buckets_end_with_no_value() {
        let f = Field {
            id: FieldId::new("F"),
            name: "Status".into(),
            kind: FieldKind::SingleSelect {
                options: vec![opt("a", "Todo"), opt("b", "Done")],
            },
        };
        let titles: Vec<_> = f.buckets().unwrap().into_iter().map(|b| b.title).collect();
        assert_eq!(titles, ["Todo", "Done", "No Status"]);
    }

    #[test]
    fn options_sharing_a_name_are_one_bucket_at_the_last_ones_place() {
        let f = Field {
            id: FieldId::new("F"),
            name: "Status".into(),
            kind: FieldKind::SingleSelect {
                options: vec![
                    opt("a", "Todo"),
                    opt("d1", "Done"),
                    opt("r", "Review"),
                    opt("d2", "Done"),
                ],
            },
        };
        let b = f.buckets().unwrap();
        let titles: Vec<_> = b.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, ["Todo", "Review", "Done", "No Status"]);
        let done = &b[2];
        assert!(done.matches("d1") && done.matches("d2") && !done.matches("r"));
    }

    #[test]
    fn iteration_buckets_put_completed_first() {
        let it = |id: &str, start: &str| Iteration {
            id: IterationId::new(id),
            title: id.into(),
            start_date: start.into(),
            duration_days: 14,
        };
        let f = Field {
            id: FieldId::new("S"),
            name: "Sprint".into(),
            kind: FieldKind::Iteration {
                iterations: vec![it("s3", "2026-10-15")],
                completed: vec![it("s2", "2026-10-01"), it("s1", "2026-09-17")],
            },
        };
        let b = f.buckets().unwrap();
        let titles: Vec<_> = b.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, ["s1", "s2", "s3", "No Sprint"]);
        assert!(b[0].completed && !b[2].completed);
    }

    #[test]
    fn text_fields_have_no_buckets() {
        let f = Field {
            id: FieldId::new("T"),
            name: "Notes".into(),
            kind: FieldKind::Text,
        };
        assert!(f.buckets().is_none());
        assert!(!f.kind.can_be_column());
    }
}
