//! Loading and polling: when (scheduler) and what (syncer).

pub mod scheduler;
pub mod syncer;

use crate::github::GithubError;
use crate::github::transport::RateInfo;
use crate::model::{Item, ItemDetail, ItemId, Project, ProjectSummary, ViewId};
use crate::store::{StoreUpdate, ViewList};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncrementalMode {
    /// `updated:>=2026-10-01T12:34:56Z` filters exactly.
    DateTime,
    /// Only `updated:>=2026-10-01` works; polls re-fetch the whole day's changes.
    Date,
    /// No `updated:` filter; every poll is a full load.
    Unsupported,
}

/// Set from phase 0 finding 1 (docs/phase0-findings.md).
pub const INCREMENTAL_MODE: IncrementalMode = IncrementalMode::Date;

pub fn incremental_query(mode: IncrementalMode, since: &str) -> Option<String> {
    match mode {
        IncrementalMode::DateTime => Some(format!("updated:>={since}")),
        IncrementalMode::Date => Some(format!("updated:>={}", since.get(..10).unwrap_or(since))),
        IncrementalMode::Unsupported => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncTask {
    Resolve,
    FullLoad,
    Incremental,
    ViewIds(ViewId),
    Hydrate,
    Detail(ItemId),
    Projects,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    Project(Project),
    /// A page of a full load, applied as an upsert so a failed load never deletes anything.
    ItemsPage {
        items: Vec<Item>,
        loaded: usize,
        total: usize,
    },
    /// The whole item set, sent only when every page arrived.
    ItemsComplete {
        items: Vec<Item>,
        fetched_at: String,
        total: usize,
        truncated: bool,
    },
    ItemsUpdated {
        items: Vec<Item>,
        fetched_at: String,
    },
    ViewIds {
        view: ViewId,
        list: ViewList,
    },
    Hydrated(Vec<Item>),
    /// The boards linked to the pane's repository, for the startup decision.
    RepoProjects(Vec<ProjectSummary>),
    Projects(Vec<ProjectSummary>),
    Rate(RateInfo),
    Detail {
        item: ItemId,
        detail: ItemDetail,
        older: bool,
    },
    Failed {
        task: SyncTask,
        error: GithubError,
    },
}

/// The store change an event implies, if any.
pub fn store_update(event: &SyncEvent) -> Option<StoreUpdate> {
    match event {
        SyncEvent::Project(p) => Some(StoreUpdate::Project(p.clone())),
        SyncEvent::ItemsPage { items, .. } | SyncEvent::Hydrated(items) => {
            Some(StoreUpdate::UpsertItems(items.clone()))
        }
        SyncEvent::ItemsComplete { items, .. } => Some(StoreUpdate::ReplaceItems(items.clone())),
        SyncEvent::ItemsUpdated { items, .. } => Some(StoreUpdate::UpsertItems(items.clone())),
        SyncEvent::ViewIds { view, list } => Some(StoreUpdate::ViewIds {
            view: view.clone(),
            list: list.clone(),
        }),
        SyncEvent::RepoProjects(_)
        | SyncEvent::Projects(_)
        | SyncEvent::Detail { .. }
        | SyncEvent::Rate(_)
        | SyncEvent::Failed { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;

    #[test]
    fn incremental_query_per_mode() {
        let since = "2026-10-01T12:34:56Z";
        assert_eq!(
            incremental_query(IncrementalMode::DateTime, since).as_deref(),
            Some("updated:>=2026-10-01T12:34:56Z")
        );
        assert_eq!(
            incremental_query(IncrementalMode::Date, since).as_deref(),
            Some("updated:>=2026-10-01")
        );
        assert_eq!(incremental_query(IncrementalMode::Unsupported, since), None);
    }

    #[test]
    fn only_a_complete_load_replaces_items() {
        let page = SyncEvent::ItemsPage {
            items: vec![issue("a", 1, "A")],
            loaded: 1,
            total: 5,
        };
        assert!(matches!(
            store_update(&page),
            Some(StoreUpdate::UpsertItems(_))
        ));
        let done = SyncEvent::ItemsComplete {
            items: vec![],
            fetched_at: "t".into(),
            total: 0,
            truncated: false,
        };
        assert!(matches!(
            store_update(&done),
            Some(StoreUpdate::ReplaceItems(_))
        ));
        let failed = SyncEvent::Failed {
            task: SyncTask::FullLoad,
            error: crate::github::GithubError::Network("x".into()),
        };
        assert_eq!(store_update(&failed), None);
    }
}
