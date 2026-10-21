use crate::model::field::{Field, FieldKind, find_field};
use crate::model::ids::{BoardKey, FieldId, ProjectId, ViewId};
use crate::model::view::View;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerKind {
    User,
    Organization,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub board: BoardKey,
    pub owner_kind: OwnerKind,
    pub title: String,
    pub url: String,
    pub viewer_can_update: bool,
    pub fields: Vec<Field>,
    pub views: Vec<View>,
    /// Repositories linked to the project: candidates for new issues and draft assignees.
    #[serde(default)]
    pub linked_repos: Vec<crate::model::RepoRef>,
    /// The linked repositories the viewer can create issues in (write access, issues on).
    #[serde(default)]
    pub issue_repos: Vec<crate::model::RepoRef>,
    /// Workspace facts for a Linear board; `None` on GitHub boards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<LinearMeta>,
}

/// What a Linear board needs from its workspace beyond the common project fields.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LinearMeta {
    /// The workspace's URL key.
    pub url_key: String,
    /// The viewer's user id, for the `me` filter.
    pub viewer_id: String,
    pub team_keys: Vec<String>,
    /// The board's teams. Empty on a board cached by 0.4: team-owned edits then need a
    /// refresh (spec section 6).
    #[serde(default)]
    pub teams: Vec<TeamMeta>,
    /// The team id owning each workflow state, cycle and team-level single-select label
    /// group, by its id. Workspace-level groups are absent.
    #[serde(default)]
    pub owners: BTreeMap<String, String>,
}

/// One of a Linear board's teams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMeta {
    pub id: String,
    /// The team key, as in `TBA-12`; an issue's `ContentRef.container`.
    pub key: String,
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
    pub board: BoardKey,
    pub title: String,
    pub closed: bool,
}

/// The picker's sections, in the order it draws them (Decision 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PickerSection {
    Github,
    LinearProjects,
    LinearTeams,
}

impl ProjectSummary {
    /// The picker section this board is listed under, from its provider and kind.
    pub fn section(&self) -> PickerSection {
        match self.board {
            BoardKey::Github(_) => PickerSection::Github,
            BoardKey::LinearProject { .. } => PickerSection::LinearProjects,
            BoardKey::LinearTeam { .. } => PickerSection::LinearTeams,
        }
    }
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
            linked_repos: vec![],
            issue_repos: vec![],
            linear: None,
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

    #[test]
    fn a_github_project_serializes_without_linear_meta() {
        let p = crate::store::snapshot::tests::project();
        assert_eq!(p.linear, None);
        let mut json = serde_json::to_value(&p).unwrap();
        assert!(json.get("linear").is_none());
        json.as_object_mut().unwrap().remove("linear");
        let back: Project = serde_json::from_value(json).unwrap();
        assert_eq!(back.linear, None);
    }

    #[test]
    fn a_04_linear_meta_loads_with_no_teams() {
        let old = r#"{"url_key": "acme", "viewer_id": "u1", "team_keys": ["TBA"]}"#;
        let meta: LinearMeta = serde_json::from_str(old).unwrap();
        assert_eq!(meta.team_keys, ["TBA"]);
        assert!(meta.teams.is_empty() && meta.owners.is_empty());

        let new = LinearMeta {
            teams: vec![TeamMeta {
                id: "team-a".into(),
                key: "TBA".into(),
            }],
            owners: BTreeMap::from([("s-todo".to_string(), "team-a".to_string())]),
            ..meta
        };
        let back: LinearMeta = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }
}
