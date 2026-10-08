use crate::model::field::{Field, FieldKind};
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
    pub fn field(&self, id: &FieldId) -> Option<&Field> {
        self.fields.iter().find(|f| &f.id == id)
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
