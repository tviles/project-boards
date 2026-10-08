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
    PullRequests(Vec<u32>),
    Reviewers(Vec<String>),
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
            Self::PullRequests(numbers) => numbers
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", "),
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

    pub fn label_names(&self) -> Vec<&str> {
        self.values
            .values()
            .filter_map(|v| match v {
                FieldValue::Labels(l) => Some(l),
                _ => None,
            })
            .flatten()
            .map(|l| l.name.as_str())
            .collect()
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
