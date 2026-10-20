use crate::model::field::{Field, OptionKind, find_field};
use crate::model::ids::{FieldId, ViewId};
use serde::{Deserialize, Serialize};

/// What decides a view's items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ViewFilter {
    /// GitHub filter syntax, sent as `items(query:)`.
    Github(String),
    /// A Linear custom view by id; Linear applies its stored filter (`customView.issues`).
    /// `hide_closed` travels with the view because a cached board asks for lists before it
    /// resolves.
    LinearView {
        id: String,
        #[serde(default)]
        hide_closed: bool,
    },
    /// A Linear `IssueFilter`, built in code for the built-in tabs.
    Linear(serde_json::Value),
}

impl ViewFilter {
    /// The GitHub query text; empty for Linear filters.
    pub fn github_query(&self) -> &str {
        match self {
            Self::Github(q) => q,
            Self::LinearView { .. } | Self::Linear(_) => "",
        }
    }

    /// The workflow-state kinds a Linear filter leaves out at its top level
    /// (`state.type.nin`); empty for anything else.
    pub fn excluded_state_kinds(&self) -> Vec<OptionKind> {
        let Self::Linear(v) = self else {
            return Vec::new();
        };
        v["state"]["type"]["nin"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k.as_str().and_then(OptionKind::from_linear))
            .collect()
    }

    /// Whether the filter selects every item, so no list is needed.
    pub fn is_everything(&self) -> bool {
        match self {
            Self::Github(t) => t.trim().is_empty(),
            Self::Linear(v) => v.is_null() || v.as_object().is_some_and(|o| o.is_empty()),
            Self::LinearView { .. } => false,
        }
    }
}

/// A view's list request: which items to list and whether archived ones count.
#[derive(Debug, Clone, PartialEq)]
pub struct ListQuery {
    pub filter: ViewFilter,
    pub include_archived: bool,
}

impl ListQuery {
    /// A GitHub list for `text`.
    pub fn github(text: impl Into<String>) -> Self {
        Self {
            filter: ViewFilter::Github(text.into()),
            include_archived: false,
        }
    }
}

impl Default for ViewFilter {
    fn default() -> Self {
        Self::Github(String::new())
    }
}

impl From<&str> for ViewFilter {
    fn from(q: &str) -> Self {
        Self::Github(q.to_string())
    }
}

impl From<String> for ViewFilter {
    fn from(q: String) -> Self {
        Self::Github(q)
    }
}

impl std::fmt::Display for ViewFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.github_query())
    }
}

/// Serialized as `"table"`, `"board"`, `"roadmap"`; the capitalised names are accepted so
/// caches written before the rename still load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    #[serde(alias = "Table")]
    Table,
    #[serde(alias = "Board")]
    Board,
    /// Shown as a table in 0.1.
    #[serde(alias = "Roadmap")]
    Roadmap,
}

impl Layout {
    pub fn from_api(value: &str) -> Self {
        match value {
            "BOARD_LAYOUT" => Self::Board,
            "ROADMAP_LAYOUT" => Self::Roadmap,
            _ => Self::Table,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortSpec {
    pub field: FieldId,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: ViewId,
    pub number: u32,
    pub name: String,
    pub layout: Layout,
    pub filter: ViewFilter,
    pub visible_fields: Vec<FieldId>,
    pub group_by: Vec<FieldId>,
    pub vertical_group_by: Vec<FieldId>,
    pub sort_by: Vec<SortSpec>,
}

impl View {
    /// The field whose buckets become board columns: the view's column-by field, else a
    /// single-select called "Status", else the first field that can be a column.
    pub fn column_field<'a>(&self, fields: &'a [Field]) -> Option<&'a Field> {
        let by_id = |id: &FieldId| find_field(fields, id);
        self.vertical_group_by
            .iter()
            .filter_map(by_id)
            .find(|f| f.kind.can_be_column())
            .or_else(|| {
                fields
                    .iter()
                    .find(|f| f.name == "Status" && f.kind.can_be_column())
            })
            .or_else(|| fields.iter().find(|f| f.kind.can_be_column()))
    }

    /// The table group-by field, or the board swimlane field.
    pub fn group_field<'a>(&self, fields: &'a [Field]) -> Option<&'a Field> {
        self.group_by.first().and_then(|id| find_field(fields, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_reads_old_capitalised_names_and_writes_lowercase() {
        for (old, layout) in [
            ("\"Table\"", Layout::Table),
            ("\"Board\"", Layout::Board),
            ("\"Roadmap\"", Layout::Roadmap),
        ] {
            assert_eq!(serde_json::from_str::<Layout>(old).unwrap(), layout);
        }
        assert_eq!(serde_json::to_string(&Layout::Board).unwrap(), "\"board\"");
        assert_eq!(
            serde_json::from_str::<Layout>("\"board\"").unwrap(),
            Layout::Board
        );
    }
    use crate::model::field::{FieldKind, OptionColor, SelectOption};
    use crate::model::ids::OptionId;

    fn select(id: &str, name: &str) -> Field {
        Field {
            id: FieldId::new(id),
            name: name.into(),
            kind: FieldKind::SingleSelect {
                options: vec![SelectOption {
                    id: OptionId::new("o"),
                    name: "x".into(),
                    color: OptionColor::Gray,
                    kind: None,
                }],
            },
        }
    }

    fn view(vertical: &[&str]) -> View {
        View {
            id: ViewId::new("V"),
            number: 1,
            name: "v".into(),
            layout: Layout::Board,
            filter: ViewFilter::default(),
            visible_fields: vec![],
            group_by: vec![],
            vertical_group_by: vertical.iter().map(|s| FieldId::new(*s)).collect(),
            sort_by: vec![],
        }
    }

    #[test]
    fn column_field_prefers_the_views_choice_then_status() {
        let fields = vec![select("P", "Priority"), select("S", "Status")];
        assert_eq!(view(&["P"]).column_field(&fields).unwrap().name, "Priority");
        assert_eq!(view(&[]).column_field(&fields).unwrap().name, "Status");
        let text = vec![Field {
            id: FieldId::new("T"),
            name: "Notes".into(),
            kind: FieldKind::Text,
        }];
        assert!(view(&[]).column_field(&text).is_none());
    }

    #[test]
    fn layouts_map_from_api() {
        assert_eq!(Layout::from_api("BOARD_LAYOUT"), Layout::Board);
        assert_eq!(Layout::from_api("ROADMAP_LAYOUT"), Layout::Roadmap);
        assert_eq!(Layout::from_api("TABLE_LAYOUT"), Layout::Table);
        assert_eq!(Layout::from_api("FUTURE"), Layout::Table);
    }

    #[test]
    fn view_filters_convert_from_text_and_show_only_github_queries() {
        let f: ViewFilter = "label:bug -status:Done".into();
        assert_eq!(f, ViewFilter::Github("label:bug -status:Done".into()));
        assert_eq!(f.to_string(), "label:bug -status:Done");
        assert_eq!(f.github_query(), "label:bug -status:Done");
        assert_eq!(ViewFilter::default(), ViewFilter::Github(String::new()));
        let view = ViewFilter::LinearView {
            id: "cv1".into(),
            hide_closed: false,
        };
        let built = ViewFilter::Linear(serde_json::json!({"assignee": {"isMe": {"eq": true}}}));
        assert_eq!(view.github_query(), "");
        assert_eq!(built.to_string(), "");
    }

    #[test]
    fn view_filters_round_trip_through_the_cache() {
        for f in [
            ViewFilter::Github("is:open".into()),
            ViewFilter::LinearView {
                id: "cv1".into(),
                hide_closed: false,
            },
            ViewFilter::Linear(serde_json::json!({"state": {"type": {"nin": ["canceled"]}}})),
        ] {
            let back: ViewFilter =
                serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
            assert_eq!(back, f);
        }
    }

    #[test]
    fn the_board_tab_excludes_canceled_and_duplicate() {
        use crate::model::field::OptionKind;
        let board = ViewFilter::Linear(crate::linear::views::board_tab_filter());
        assert_eq!(
            board.excluded_state_kinds(),
            [OptionKind::Canceled, OptionKind::Duplicate]
        );
        assert!(
            ViewFilter::Linear(serde_json::json!({}))
                .excluded_state_kinds()
                .is_empty()
        );
        assert!(
            ViewFilter::Github("x".into())
                .excluded_state_kinds()
                .is_empty()
        );
    }

    #[test]
    fn everything_means_no_list() {
        use serde_json::{Value, json};
        assert!(ViewFilter::Github("  ".into()).is_everything());
        assert!(ViewFilter::Linear(json!({})).is_everything());
        assert!(ViewFilter::Linear(Value::Null).is_everything());
        assert!(!ViewFilter::Github("is:open".into()).is_everything());
        assert!(!ViewFilter::Linear(json!({"assignee": {"isMe": {"eq": true}}})).is_everything());
        assert!(
            !ViewFilter::LinearView {
                id: "cv1".into(),
                hide_closed: false
            }
            .is_everything()
        );
    }
}
