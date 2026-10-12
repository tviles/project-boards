use crate::model::{Item, ItemId, Project, ViewId};
use crate::store::cache::CACHE_VERSION;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The item ids a view's filter matched, in board order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewList {
    pub ids: Vec<ItemId>,
    pub total: usize,
    /// True when `total` exceeded `max_items` and the list was cut.
    pub truncated: bool,
}

/// The confirmed board: what the server last reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub version: u32,
    pub project: Project,
    pub items: BTreeMap<ItemId, Item>,
    /// Board position order of every item in `items`.
    pub order: Vec<ItemId>,
    pub views: BTreeMap<ViewId, ViewList>,
    /// RFC 3339 time of the last successful full or incremental fetch.
    pub fetched_at: Option<String>,
}

impl BoardSnapshot {
    pub fn new(project: Project) -> Self {
        Self {
            version: CACHE_VERSION,
            project,
            items: BTreeMap::new(),
            order: Vec::new(),
            views: BTreeMap::new(),
            fetched_at: None,
        }
    }

    pub fn set_project(&mut self, project: Project) {
        self.views.retain(|id, _| project.view(id).is_some());
        self.project = project;
    }

    pub fn upsert_items(&mut self, items: Vec<Item>) {
        for item in items {
            if !self.items.contains_key(&item.id) {
                self.order.push(item.id.clone());
            }
            self.items.insert(item.id.clone(), item);
        }
    }

    /// Replaces the whole item set after a complete full fetch.
    pub fn replace_items(&mut self, items: Vec<Item>) {
        self.order = items.iter().map(|i| i.id.clone()).collect();
        self.items = items.into_iter().map(|i| (i.id.clone(), i)).collect();
    }

    pub fn set_view_ids(&mut self, view: ViewId, list: ViewList) {
        self.views.insert(view, list);
    }

    pub fn all_items(&self) -> Vec<&Item> {
        self.order
            .iter()
            .filter_map(|id| self.items.get(id))
            .collect()
    }

    /// The view's items in list order, skipping ids not loaded yet. `None` before the
    /// view's list has ever been fetched.
    pub fn view_items(&self, view: &ViewId) -> Option<Vec<&Item>> {
        let list = self.views.get(view)?;
        Some(
            list.ids
                .iter()
                .filter_map(|id| self.items.get(id))
                .collect(),
        )
    }

    pub fn missing_ids(&self, view: &ViewId) -> Vec<ItemId> {
        self.views
            .get(view)
            .map(|l| {
                l.ids
                    .iter()
                    .filter(|id| !self.items.contains_key(*id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::item::tests::issue;
    use crate::model::{BoardRef, OwnerKind, ProjectId};

    pub fn project() -> Project {
        Project {
            id: ProjectId::new("P"),
            board: BoardRef {
                owner: "tviles".into(),
                number: 3,
            },
            owner_kind: OwnerKind::User,
            title: "T".into(),
            url: "u".into(),
            viewer_can_update: true,
            fields: vec![],
            views: vec![],
        }
    }

    fn ids(items: &[&Item]) -> Vec<String> {
        items.iter().map(|i| i.id.0.clone()).collect()
    }

    #[test]
    fn upsert_updates_in_place_and_appends_new() {
        let mut s = BoardSnapshot::new(project());
        s.upsert_items(vec![issue("a", 1, "A"), issue("b", 2, "B")]);
        s.upsert_items(vec![issue("a", 1, "A2"), issue("c", 3, "C")]);
        assert_eq!(ids(&s.all_items()), ["a", "b", "c"]);
        assert_eq!(s.items[&ItemId::new("a")].title(), "A2");
    }

    #[test]
    fn replace_drops_items_that_are_gone() {
        let mut s = BoardSnapshot::new(project());
        s.upsert_items(vec![issue("a", 1, "A"), issue("b", 2, "B")]);
        s.replace_items(vec![issue("b", 2, "B")]);
        assert_eq!(ids(&s.all_items()), ["b"]);
        assert!(!s.items.contains_key(&ItemId::new("a")));
    }

    #[test]
    fn view_items_follow_the_list_and_report_missing() {
        let mut s = BoardSnapshot::new(project());
        s.upsert_items(vec![issue("a", 1, "A"), issue("b", 2, "B")]);
        let view = ViewId::new("V");
        assert!(s.view_items(&view).is_none());
        s.set_view_ids(
            view.clone(),
            ViewList {
                ids: vec![ItemId::new("b"), ItemId::new("z"), ItemId::new("a")],
                total: 3,
                truncated: false,
            },
        );
        assert_eq!(ids(&s.view_items(&view).unwrap()), ["b", "a"]);
        assert_eq!(s.missing_ids(&view), vec![ItemId::new("z")]);
    }

    #[test]
    fn set_project_drops_lists_of_deleted_views() {
        let mut s = BoardSnapshot::new(project());
        s.set_view_ids(
            ViewId::new("gone"),
            ViewList {
                ids: vec![],
                total: 0,
                truncated: false,
            },
        );
        s.set_project(project());
        assert!(s.views.is_empty());
    }
}
