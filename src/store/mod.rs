//! The board snapshot, the store trait the UI reads through, and the on-disk cache.

pub mod cache;
pub mod snapshot;

pub use snapshot::{BoardSnapshot, ViewList};

use crate::model::{Item, Project, ViewId};

#[derive(Debug, Clone, PartialEq)]
pub enum StoreUpdate {
    Replace(BoardSnapshot),
    Project(Project),
    UpsertItems(Vec<Item>),
    ReplaceItems(Vec<Item>),
    ViewIds {
        view: ViewId,
        list: ViewList,
    },
    /// Drops a view's cached id list (its filter changed).
    ClearViewIds(ViewId),
    FetchedAt(String),
}

impl StoreUpdate {
    /// The kind of update and the ids it touches, for logs. Never titles or field values:
    /// board content stays out of the log file.
    pub fn summary(&self) -> String {
        let ids = |items: &[Item]| {
            items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Self::Replace(s) => format!("Replace({} items)", s.items.len()),
            Self::Project(p) => format!("Project({})", p.id),
            Self::UpsertItems(items) => format!("UpsertItems({})", ids(items)),
            Self::ReplaceItems(items) => format!("ReplaceItems({})", ids(items)),
            Self::ViewIds { view, list } => format!("ViewIds({view}, {} ids)", list.ids.len()),
            Self::ClearViewIds(view) => format!("ClearViewIds({view})"),
            Self::FetchedAt(t) => format!("FetchedAt({t})"),
        }
    }
}

/// What the UI reads the board through. A later milestone adds the pending-edit layer and
/// may move this behind a daemon; callers only see this trait.
pub trait Store: Send {
    fn snapshot(&self) -> Option<&BoardSnapshot>;
    fn apply(&mut self, update: StoreUpdate);
}

pub struct MemoryStore {
    snapshot: Option<BoardSnapshot>,
}

impl MemoryStore {
    pub fn new(snapshot: Option<BoardSnapshot>) -> Self {
        Self { snapshot }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Option<&BoardSnapshot> {
        self.snapshot.as_ref()
    }

    fn apply(&mut self, update: StoreUpdate) {
        match (update, self.snapshot.as_mut()) {
            (StoreUpdate::Replace(s), _) => self.snapshot = Some(s),
            (StoreUpdate::Project(p), Some(s)) => s.set_project(p),
            (StoreUpdate::Project(p), None) => self.snapshot = Some(BoardSnapshot::new(p)),
            (StoreUpdate::UpsertItems(items), Some(s)) => s.upsert_items(items),
            (StoreUpdate::ReplaceItems(items), Some(s)) => s.replace_items(items),
            (StoreUpdate::ViewIds { view, list }, Some(s)) => s.set_view_ids(view, list),
            (StoreUpdate::ClearViewIds(view), Some(s)) => {
                s.views.remove(&view);
            }
            (StoreUpdate::FetchedAt(t), Some(s)) => s.fetched_at = Some(t),
            (other, None) => tracing::warn!(
                update = %other.summary(),
                "store update before the project was known; ignored"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;
    use crate::store::snapshot::tests::project;

    #[test]
    fn items_before_project_are_ignored_then_project_creates_snapshot() {
        let mut store = MemoryStore::new(None);
        store.apply(StoreUpdate::UpsertItems(vec![issue("a", 1, "A")]));
        assert!(store.snapshot().is_none());
        store.apply(StoreUpdate::Project(project()));
        store.apply(StoreUpdate::UpsertItems(vec![issue("a", 1, "A")]));
        assert_eq!(store.snapshot().unwrap().all_items().len(), 1);
    }

    #[test]
    fn update_summaries_name_item_ids_but_no_content() {
        let mut secret = issue("PVTI_a", 1, "Secret launch plan");
        secret.values.insert(
            crate::model::FieldId::new("F"),
            crate::model::FieldValue::Text("private note".into()),
        );
        let summary = StoreUpdate::UpsertItems(vec![secret, issue("PVTI_b", 2, "x")]).summary();
        assert_eq!(summary, "UpsertItems(PVTI_a, PVTI_b)");
        assert_eq!(
            StoreUpdate::Project(project()).summary(),
            format!("Project({})", project().id)
        );
    }

    #[test]
    fn clear_view_ids_drops_that_list() {
        let mut store =
            MemoryStore::new(Some(crate::store::snapshot::BoardSnapshot::new(project())));
        let list = ViewList {
            ids: vec![],
            total: 0,
            truncated: false,
        };
        store.apply(StoreUpdate::ViewIds {
            view: ViewId::new("V"),
            list,
        });
        assert!(
            store
                .snapshot()
                .unwrap()
                .views
                .contains_key(&ViewId::new("V"))
        );
        store.apply(StoreUpdate::ClearViewIds(ViewId::new("V")));
        assert!(
            !store
                .snapshot()
                .unwrap()
                .views
                .contains_key(&ViewId::new("V"))
        );
    }
}
