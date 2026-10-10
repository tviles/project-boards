use crate::model::field::{Field, FieldKind, find_field};
use crate::model::ids::{BoardRef, FieldId, ProjectId, ViewId};
use crate::model::view::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerKind {
    User,
    Organization,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub board: BoardRef,
    pub owner_kind: OwnerKind,
    pub title: String,
    pub url: String,
    pub viewer_can_update: bool,
    pub fields: Vec<Field>,
    pub views: Vec<View>,
}

impl Project {
    /// The field with `id`, matching GitHub's other id prefix too (see `find_field`).
    pub fn field(&self, id: &FieldId) -> Option<&Field> {
        find_field(&self.fields, id)
    }

    pub fn view(&self, id: &ViewId) -> Option<&View> {
        self.views.iter().find(|v| &v.id == id)
    }

    pub fn title_field(&self) -> Option<&Field> {
        self.fields.iter().find(|f| f.kind == FieldKind::Title)
    }
}

/// A board as listed in the picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub board: BoardRef,
    pub title: String,
    pub closed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::field::FieldKind;

    #[test]
    fn field_lookup_falls_back_to_the_id_suffix() {
        let field = |id: &str, name: &str| Field {
            id: FieldId::new(id),
            name: name.into(),
            kind: FieldKind::Text,
        };
        let p = Project {
            id: crate::model::ProjectId::new("P"),
            board: "tviles/3".parse().unwrap(),
            owner_kind: OwnerKind::User,
            title: "t".into(),
            url: "u".into(),
            viewer_can_update: false,
            fields: vec![field("PVTSSF_abc", "Status"), field("PVTF_abc2", "Other")],
            views: vec![],
        };
        let name = |id: &str| p.field(&FieldId::new(id)).map(|f| f.name.clone());
        assert_eq!(name("PVTSSF_abc").as_deref(), Some("Status"));
        assert_eq!(name("PVTF_abc").as_deref(), Some("Status"));
        assert_eq!(
            name("PVTF_abc2").as_deref(),
            Some("Other"),
            "exact match first"
        );
        assert_eq!(name("PVTF_nope"), None);
        assert_eq!(name("abc"), None, "an id without `_` has no suffix");
    }
}
