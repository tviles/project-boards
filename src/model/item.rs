use crate::model::field::{Field, FieldKind, OptionColor};
use crate::model::ids::{FieldId, ItemId, IterationId, OptionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    /// Hex colour without `#`, as GitHub returns it.
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    Text(String),
    Number(f64),
    Date(String),
    SingleSelect {
        option_id: OptionId,
        name: String,
    },
    MultiSelect {
        option_ids: Vec<OptionId>,
        names: Vec<String>,
    },
    Iteration {
        iteration_id: IterationId,
        title: String,
        start_date: String,
    },
    Labels(Vec<Label>),
    Users(Vec<String>),
    Milestone(String),
    Repository(String),
    PullRequests(Vec<LinkedPullRequest>),
    Reviewers(Vec<String>),
    /// Built-in fields read from the item's content (see `Item::field_value`).
    Parent(ParentIssue),
    SubIssues(SubIssuesSummary),
    IssueType(IssueType),
}

impl FieldValue {
    pub fn display(&self) -> String {
        match self {
            Self::Text(t) | Self::Date(t) | Self::Milestone(t) | Self::Repository(t) => t.clone(),
            Self::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Self::SingleSelect { name, .. } => name.clone(),
            Self::MultiSelect { names, .. } => names.join(", "),
            Self::Iteration { title, .. } => title.clone(),
            Self::Labels(labels) => labels
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            Self::Users(users) | Self::Reviewers(users) => users
                .iter()
                .map(|u| format!("@{u}"))
                .collect::<Vec<_>>()
                .join(", "),
            Self::PullRequests(prs) => prs
                .iter()
                .map(|p| format!("#{}", p.number))
                .collect::<Vec<_>>()
                .join(", "),
            Self::Parent(parent) => parent.title.clone(),
            Self::SubIssues(s) => format!("{}/{}", s.completed, s.total),
            Self::IssueType(t) => t.name.clone(),
        }
    }

    /// The bucket this value falls in when its field groups a board or table.
    pub fn bucket_key(&self) -> Option<String> {
        match self {
            Self::SingleSelect { option_id, .. } => Some(option_id.0.clone()),
            Self::Iteration { iteration_id, .. } => Some(iteration_id.0.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentState {
    Open,
    Closed,
    Merged,
    Unknown,
}

impl ContentState {
    pub fn from_api(value: &str) -> Self {
        match value {
            "OPEN" => Self::Open,
            "CLOSED" => Self::Closed,
            "MERGED" => Self::Merged,
            _ => Self::Unknown,
        }
    }
}

/// Why an issue was closed (`Issue.stateReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateReason {
    Completed,
    NotPlanned,
    Duplicate,
    Reopened,
    Unknown,
}

impl StateReason {
    pub fn from_api(value: &str) -> Self {
        match value {
            "COMPLETED" => Self::Completed,
            "NOT_PLANNED" => Self::NotPlanned,
            "DUPLICATE" => Self::Duplicate,
            "REOPENED" => Self::Reopened,
            _ => Self::Unknown,
        }
    }
}

/// A pull request linked to an issue (one that closes it when merged).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedPullRequest {
    pub number: u32,
    pub state: ContentState,
    pub is_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentIssue {
    pub number: u32,
    pub title: String,
    pub state: ContentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubIssuesSummary {
    pub total: u32,
    pub completed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueType {
    pub name: String,
    /// GitHub's issue type colours share the option colour names.
    pub color: OptionColor,
}

/// What an issue or pull request says about itself beyond its title and state: the values
/// of the built-in fields GitHub derives from content. Empty for drafts and other content.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ContentFields {
    /// RFC 3339
    pub created_at: Option<String>,
    /// RFC 3339
    pub updated_at: Option<String>,
    /// RFC 3339
    pub closed_at: Option<String>,
    pub state_reason: Option<StateReason>,
    pub issue_type: Option<IssueType>,
    pub parent: Option<ParentIssue>,
    pub sub_issues: Option<SubIssuesSummary>,
    pub linked_prs: Vec<LinkedPullRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentRef {
    /// `owner/name`
    pub repo: String,
    pub number: u32,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemContent {
    Issue {
        reference: ContentRef,
        title: String,
        state: ContentState,
    },
    PullRequest {
        reference: ContentRef,
        title: String,
        state: ContentState,
        is_draft: bool,
    },
    Draft {
        title: String,
    },
    /// Content the viewer is not allowed to see.
    Redacted,
    /// A content type newer than this build.
    Unknown {
        type_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub content: ItemContent,
    pub archived: bool,
    /// RFC 3339
    pub updated_at: String,
    pub values: BTreeMap<FieldId, FieldValue>,
    #[serde(default)]
    pub content_fields: ContentFields,
}

impl Item {
    pub fn title(&self) -> &str {
        match &self.content {
            ItemContent::Issue { title, .. }
            | ItemContent::PullRequest { title, .. }
            | ItemContent::Draft { title } => title,
            ItemContent::Redacted => "Private item",
            ItemContent::Unknown { .. } => "Unsupported item",
        }
    }

    pub fn reference(&self) -> Option<&ContentRef> {
        match &self.content {
            ItemContent::Issue { reference, .. } | ItemContent::PullRequest { reference, .. } => {
                Some(reference)
            }
            _ => None,
        }
    }

    pub fn number(&self) -> Option<u32> {
        self.reference().map(|r| r.number)
    }

    pub fn url(&self) -> Option<&str> {
        self.reference().map(|r| r.url.as_str())
    }

    pub fn value(&self, field: &FieldId) -> Option<&FieldValue> {
        self.values.get(field)
    }

    /// The item's value for `field`. Built-in fields GitHub derives from content (dates,
    /// parent, sub-issue progress, issue type, linked pull requests) are read from
    /// `content_fields`; dates as `YYYY-MM-DD`, and no progress when there are no sub-issues.
    /// Everything else is the stored value.
    pub fn field_value(&self, field: &Field) -> Option<FieldValue> {
        let c = &self.content_fields;
        let date = |d: &Option<String>| {
            d.as_deref()
                .map(|d| FieldValue::Date(d.get(..10).unwrap_or(d).to_string()))
        };
        match &field.kind {
            FieldKind::Created => date(&c.created_at),
            FieldKind::Updated => date(&c.updated_at),
            FieldKind::Closed => date(&c.closed_at),
            FieldKind::ParentIssue => c.parent.clone().map(FieldValue::Parent),
            FieldKind::SubIssuesProgress => c
                .sub_issues
                .filter(|s| s.total > 0)
                .map(FieldValue::SubIssues),
            FieldKind::IssueType => c.issue_type.clone().map(FieldValue::IssueType),
            FieldKind::LinkedPullRequests if !c.linked_prs.is_empty() => {
                Some(FieldValue::PullRequests(c.linked_prs.clone()))
            }
            _ => self.value(&field.id).cloned(),
        }
    }

    pub fn assignees(&self) -> Vec<&str> {
        self.values
            .values()
            .filter_map(|v| match v {
                FieldValue::Users(u) => Some(u),
                _ => None,
            })
            .flatten()
            .map(String::as_str)
            .collect()
    }

    pub fn labels(&self) -> Vec<&Label> {
        self.values
            .values()
            .filter_map(|v| match v {
                FieldValue::Labels(l) => Some(l),
                _ => None,
            })
            .flatten()
            .collect()
    }

    pub fn label_names(&self) -> Vec<&str> {
        self.labels().into_iter().map(|l| l.name.as_str()).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A test helper used by other modules' tests too.
    pub fn issue(id: &str, number: u32, title: &str) -> Item {
        Item {
            id: ItemId::new(id),
            content: ItemContent::Issue {
                reference: ContentRef {
                    repo: "tviles/t".into(),
                    number,
                    url: format!("https://github.com/tviles/t/issues/{number}"),
                },
                title: title.into(),
                state: ContentState::Open,
            },
            archived: false,
            updated_at: "2026-10-01T00:00:00Z".into(),
            values: BTreeMap::new(),
            content_fields: ContentFields::default(),
        }
    }

    #[test]
    fn numbers_display_without_trailing_zero() {
        assert_eq!(FieldValue::Number(3.0).display(), "3");
        assert_eq!(FieldValue::Number(2.5).display(), "2.5");
    }

    #[test]
    fn users_and_labels_display() {
        assert_eq!(
            FieldValue::Users(vec!["a".into(), "b".into()]).display(),
            "@a, @b"
        );
        let l = |n: &str| Label {
            name: n.into(),
            color: "fff".into(),
        };
        assert_eq!(
            FieldValue::Labels(vec![l("bug"), l("ui")]).display(),
            "bug, ui"
        );
    }

    #[test]
    fn built_in_fields_are_derived_from_content() {
        let field = |kind: FieldKind| Field {
            id: FieldId::new("F"),
            name: "f".into(),
            kind,
        };
        let mut item = issue("I", 1, "x");
        item.content_fields = ContentFields {
            created_at: Some("2026-08-19T23:00:00Z".into()),
            updated_at: Some("2026-08-21".into()),
            closed_at: None,
            state_reason: None,
            issue_type: Some(IssueType {
                name: "Bug".into(),
                color: OptionColor::Red,
            }),
            parent: Some(ParentIssue {
                number: 3,
                title: "Epic".into(),
                state: ContentState::Open,
            }),
            sub_issues: Some(SubIssuesSummary {
                total: 4,
                completed: 1,
            }),
            linked_prs: vec![LinkedPullRequest {
                number: 9,
                state: ContentState::Merged,
                is_draft: false,
            }],
        };
        let value = |item: &Item, kind| item.field_value(&field(kind)).map(|v| v.display());
        assert_eq!(
            value(&item, FieldKind::Created).as_deref(),
            Some("2026-08-19")
        );
        assert_eq!(
            value(&item, FieldKind::Updated).as_deref(),
            Some("2026-08-21")
        );
        assert_eq!(value(&item, FieldKind::Closed), None);
        assert_eq!(value(&item, FieldKind::IssueType).as_deref(), Some("Bug"));
        assert_eq!(
            value(&item, FieldKind::ParentIssue).as_deref(),
            Some("Epic")
        );
        assert_eq!(
            value(&item, FieldKind::SubIssuesProgress).as_deref(),
            Some("1/4")
        );
        assert_eq!(
            value(&item, FieldKind::LinkedPullRequests).as_deref(),
            Some("#9")
        );
        item.content_fields.sub_issues = Some(SubIssuesSummary {
            total: 0,
            completed: 0,
        });
        assert_eq!(
            value(&item, FieldKind::SubIssuesProgress),
            None,
            "no sub-issues"
        );
        // Other kinds read the stored value.
        item.values
            .insert(FieldId::new("F"), FieldValue::Text("note".into()));
        assert_eq!(value(&item, FieldKind::Text).as_deref(), Some("note"));
    }

    #[test]
    fn redacted_items_have_a_placeholder_title_and_no_number() {
        let mut item = issue("I", 1, "x");
        item.content = ItemContent::Redacted;
        assert_eq!(item.title(), "Private item");
        assert_eq!(item.number(), None);
    }

    #[test]
    fn assignees_and_labels_come_from_values() {
        let mut item = issue("I", 1, "x");
        item.values
            .insert(FieldId::new("A"), FieldValue::Users(vec!["tviles".into()]));
        item.values.insert(
            FieldId::new("L"),
            FieldValue::Labels(vec![Label {
                name: "bug".into(),
                color: "f00".into(),
            }]),
        );
        assert_eq!(item.assignees(), ["tviles"]);
        assert_eq!(item.label_names(), ["bug"]);
    }
}
