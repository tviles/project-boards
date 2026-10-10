use crate::model::field::Field;
use crate::model::ids::{FieldId, ViewId};
use serde::{Deserialize, Serialize};

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
    pub filter: String,
    pub visible_fields: Vec<FieldId>,
    pub group_by: Vec<FieldId>,
    pub vertical_group_by: Vec<FieldId>,
    pub sort_by: Vec<SortSpec>,
}

impl View {
    /// The field whose buckets become board columns: the view's column-by field, else a
    /// single-select called "Status", else the first field that can be a column.
    pub fn column_field<'a>(&self, fields: &'a [Field]) -> Option<&'a Field> {
        let by_id = |id: &FieldId| fields.iter().find(|f| &f.id == id);
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
        self.group_by
            .first()
            .and_then(|id| fields.iter().find(|f| &f.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            filter: String::new(),
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
}
