//! Runs fetches and reports results as events; never touches the store.

use crate::github::Github;
use crate::github::GithubError;
use crate::model::*;
use crate::store::ViewList;
use crate::sync::{IncrementalMode, SyncEvent, SyncTask, incremental_query};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Runs fetches and reports results as events. Each method is one unit of work the runtime
/// spawns; none of them touch the store directly.
pub struct Syncer {
    gh: Arc<Github>,
    tx: mpsc::UnboundedSender<SyncEvent>,
    max_items: usize,
    mode: IncrementalMode,
}

impl Syncer {
    pub fn new(
        gh: Arc<Github>,
        tx: mpsc::UnboundedSender<SyncEvent>,
        max_items: usize,
        mode: IncrementalMode,
    ) -> Self {
        Self {
            gh,
            tx,
            max_items,
            mode,
        }
    }

    fn send(&self, event: SyncEvent) {
        let _ = self.tx.send(event);
    }

    fn fail(&self, task: SyncTask, error: GithubError) {
        tracing::warn!(?task, %error, "sync task failed");
        self.send(SyncEvent::Failed { task, error });
    }

    pub async fn resolve(&self, board: &BoardRef) -> Option<Project> {
        match self.gh.resolve_project(board).await {
            Ok(p) => {
                self.send(SyncEvent::Project(p.clone()));
                Some(p)
            }
            Err(e) => {
                self.fail(SyncTask::Resolve, e);
                None
            }
        }
    }

    pub async fn refresh_project(&self, project: &Project) {
        match self.gh.fetch_project(&project.id, &project.board).await {
            Ok(p) => self.send(SyncEvent::Project(p)),
            Err(e) => self.fail(SyncTask::Resolve, e),
        }
    }

    /// Every item, page by page, up to `max_items`. `ItemsComplete` only when all pages arrived.
    pub async fn full_load(&self, project: &ProjectId) {
        let started = now_rfc3339();
        let mut items: Vec<Item> = Vec::new();
        let mut after: Option<String> = None;
        let (total, truncated) = loop {
            match self.gh.fetch_items_page(project, "", after.take()).await {
                Ok(page) => {
                    let total = page.total;
                    items.extend(page.nodes.iter().cloned());
                    self.send(SyncEvent::ItemsPage {
                        items: page.nodes,
                        loaded: items.len(),
                        total,
                    });
                    match page.next {
                        Some(cursor) if items.len() < self.max_items => after = Some(cursor),
                        Some(_) => break (total, true),
                        None => break (total, false),
                    }
                }
                Err(e) => return self.fail(SyncTask::FullLoad, e),
            }
        };
        let truncated = truncated || items.len() > self.max_items;
        items.truncate(self.max_items);
        self.send(SyncEvent::ItemsComplete {
            items,
            fetched_at: started,
            total,
            truncated,
        });
        self.send(SyncEvent::Rate(self.gh.rate()));
    }

    /// Items updated since `since`. Returns false when the mode has no incremental filter, so
    /// the caller runs a full load instead.
    pub async fn incremental(&self, project: &ProjectId, since: &str) -> bool {
        let Some(query) = incremental_query(self.mode, since) else {
            return false;
        };
        let started = now_rfc3339();
        let mut items = Vec::new();
        let mut after = None;
        loop {
            match self
                .gh
                .fetch_items_page(project, &query, after.take())
                .await
            {
                Ok(page) => {
                    items.extend(page.nodes);
                    match page.next {
                        Some(cursor) if items.len() < self.max_items => after = Some(cursor),
                        Some(_) => {
                            tracing::info!(
                                "incremental poll exceeded max_items; running a full load"
                            );
                            self.full_load(project).await;
                            return true;
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    self.fail(SyncTask::Incremental, e);
                    return true;
                }
            }
        }
        self.send(SyncEvent::ItemsUpdated {
            items,
            fetched_at: started,
        });
        self.send(SyncEvent::Rate(self.gh.rate()));
        true
    }

    /// The view's matching ids, then (with `hydrate`) any of those items not in `known`, 100 per
    /// request. The controller passes `hydrate: false` while a full load is fetching them anyway.
    pub async fn view_ids(
        &self,
        project: &ProjectId,
        view: &ViewId,
        filter: &str,
        known: &HashSet<ItemId>,
        hydrate: bool,
    ) {
        let mut ids: Vec<ItemId> = Vec::new();
        let mut after = None;
        let (total, truncated) = loop {
            match self
                .gh
                .fetch_view_ids_page(project, filter, after.take())
                .await
            {
                Ok(page) => {
                    let total = page.total;
                    ids.extend(page.nodes);
                    match page.next {
                        Some(cursor) if ids.len() < self.max_items => after = Some(cursor),
                        Some(_) => break (total, true),
                        None => break (total, false),
                    }
                }
                Err(e) => return self.fail(SyncTask::ViewIds(view.clone()), e),
            }
        };
        ids.truncate(self.max_items);
        let missing: Vec<ItemId> = ids
            .iter()
            .filter(|id| !known.contains(*id))
            .cloned()
            .collect();
        self.send(SyncEvent::ViewIds {
            view: view.clone(),
            list: ViewList {
                ids,
                total,
                truncated,
            },
        });
        if !hydrate {
            return;
        }
        for chunk in missing.chunks(100) {
            match self.gh.hydrate_items(chunk).await {
                Ok(items) => self.send(SyncEvent::Hydrated(items)),
                Err(e) => return self.fail(SyncTask::Hydrate, e),
            }
        }
    }

    /// The item's body, comments and links. `before` loads the comments older than a cursor.
    pub async fn detail(&self, item: &ItemId, before: Option<String>) {
        let older = before.is_some();
        match self.gh.fetch_item_detail(item, before).await {
            Ok(detail) => self.send(SyncEvent::Detail {
                item: item.clone(),
                detail,
                older,
            }),
            Err(e) => self.fail(SyncTask::Detail(item.clone()), e),
        }
    }

    /// The boards linked to `repo`, for the startup decision.
    pub async fn repo_projects(&self, repo: &RepoSlug) {
        match self.gh.list_repo_projects(repo).await {
            Ok(list) => self.send(SyncEvent::RepoProjects(list)),
            Err(e) => self.fail(SyncTask::Projects, e),
        }
    }

    /// Boards for the picker: the repo's linked boards first, then every board the viewer can see.
    pub async fn projects(&self, linked: Option<&RepoSlug>) {
        let linked_list = match linked {
            Some(repo) => match self.gh.list_repo_projects(repo).await {
                Ok(l) => l,
                Err(e) => return self.fail(SyncTask::Projects, e),
            },
            None => Vec::new(),
        };
        match self.gh.list_viewer_projects().await {
            Ok(all) => self.send(SyncEvent::Projects(crate::board_choice::picker_candidates(
                all,
                &linked_list,
            ))),
            Err(e) => self.fail(SyncTask::Projects, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::convert::tests::item_json;
    use crate::github::fixture::FixtureTransport;
    use serde_json::json;

    fn page(ids: &[&str], total: usize, next: Option<&str>) -> serde_json::Value {
        let nodes: Vec<_> = ids
            .iter()
            .map(|id| {
                let mut v = item_json();
                v["id"] = json!(id);
                v
            })
            .collect();
        json!({"node": {"items": {"totalCount": total, "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}}})
    }

    fn setup(
        max: usize,
    ) -> (
        Arc<FixtureTransport>,
        Syncer,
        mpsc::UnboundedReceiver<SyncEvent>,
    ) {
        let t = Arc::new(FixtureTransport::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let syncer = Syncer::new(
            Arc::new(Github::new(t.clone())),
            tx,
            max,
            IncrementalMode::DateTime,
        );
        (t, syncer, rx)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<SyncEvent>) -> Vec<SyncEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[tokio::test]
    async fn full_load_pages_then_completes() {
        let (t, s, mut rx) = setup(2000);
        t.push("ItemsPage", page(&["a", "b"], 3, Some("c1")));
        t.push("ItemsPage", page(&["c"], 3, None));
        s.full_load(&ProjectId::new("P")).await;
        let events = drain(&mut rx);
        assert!(matches!(
            &events[0],
            SyncEvent::ItemsPage {
                loaded: 2,
                total: 3,
                ..
            }
        ));
        assert!(matches!(&events[1], SyncEvent::ItemsPage { loaded: 3, .. }));
        assert!(
            matches!(&events[2], SyncEvent::ItemsComplete { items, truncated: false, .. } if items.len() == 3)
        );
        assert_eq!(t.requests()[1].variables["after"], "c1");
    }

    /// Review Focus 1: a page failing partway never produces a replace.
    #[tokio::test]
    async fn failure_mid_load_sends_no_complete() {
        let (t, s, mut rx) = setup(2000);
        t.push("ItemsPage", page(&["a", "b"], 4, Some("c1")));
        t.push_error("ItemsPage", GithubError::Network("connection reset".into()));
        s.full_load(&ProjectId::new("P")).await;
        let events = drain(&mut rx);
        assert!(matches!(&events[0], SyncEvent::ItemsPage { .. }));
        assert!(matches!(
            &events[1],
            SyncEvent::Failed {
                task: SyncTask::FullLoad,
                ..
            }
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SyncEvent::ItemsComplete { .. }))
        );
    }

    #[tokio::test]
    async fn full_load_stops_at_max_items_and_marks_truncated() {
        let (t, s, mut rx) = setup(2);
        t.push("ItemsPage", page(&["a", "b"], 9, Some("c1")));
        s.full_load(&ProjectId::new("P")).await;
        let events = drain(&mut rx);
        assert!(matches!(events.last(), Some(SyncEvent::Rate(_))));
        assert!(events.iter().any(|e| matches!(
            e,
            SyncEvent::ItemsComplete {
                truncated: true,
                total: 9,
                ..
            }
        )));
        assert_eq!(t.requests().len(), 1);
    }

    #[tokio::test]
    async fn view_ids_then_hydrates_only_missing_items() {
        let (t, s, mut rx) = setup(2000);
        t.push(
            "ViewItemIds",
            json!({"node": {"items": {"totalCount": 2, "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "known"}, {"id": "new"}]}}}),
        );
        let mut new_item = item_json();
        new_item["id"] = json!("new");
        t.push("HydrateItems", json!({"nodes": [new_item]}));
        let known: HashSet<ItemId> = [ItemId::new("known")].into();
        s.view_ids(
            &ProjectId::new("P"),
            &ViewId::new("V"),
            "label:bug",
            &known,
            true,
        )
        .await;
        let events = drain(&mut rx);
        assert!(
            matches!(&events[0], SyncEvent::ViewIds { list, .. } if list.ids.len() == 2 && !list.truncated)
        );
        assert!(matches!(&events[1], SyncEvent::Hydrated(items) if items[0].id.as_str() == "new"));
        assert_eq!(t.requests()[1].variables["ids"], json!(["new"]));
        assert_eq!(t.requests()[0].variables["query"], "label:bug");
    }

    #[tokio::test]
    async fn view_ids_without_hydrate_send_only_the_list() {
        let (t, s, mut rx) = setup(2000);
        t.push(
            "ViewItemIds",
            json!({"node": {"items": {"totalCount": 1, "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"id": "new"}]}}}),
        );
        s.view_ids(
            &ProjectId::new("P"),
            &ViewId::new("V"),
            "label:bug",
            &HashSet::new(),
            false,
        )
        .await;
        assert_eq!(drain(&mut rx).len(), 1);
        assert_eq!(t.requests().len(), 1, "no HydrateItems request");
    }

    #[tokio::test]
    async fn repo_projects_are_reported() {
        let (t, s, mut rx) = setup(2000);
        t.push(
            "RepoProjects",
            json!({"repository": {"projectsV2": {"nodes": [{"id": "PVT_3", "number": 3, "title": "B", "closed": false,
            "owner": {"__typename": "User", "login": "tviles"}}]}}}),
        );
        s.repo_projects(&"tviles/app".parse().unwrap()).await;
        assert!(
            matches!(&drain(&mut rx)[0], SyncEvent::RepoProjects(list) if list[0].board.to_string() == "tviles/3")
        );
    }

    #[tokio::test]
    async fn incremental_uses_the_updated_filter() {
        let (t, s, mut rx) = setup(2000);
        t.push("ItemsPage", page(&["a"], 1, None));
        assert!(
            s.incremental(&ProjectId::new("P"), "2026-10-01T00:00:00Z")
                .await
        );
        assert_eq!(
            t.requests()[0].variables["query"],
            "updated:>=2026-10-01T00:00:00Z"
        );
        assert!(
            matches!(&drain(&mut rx)[0], SyncEvent::ItemsUpdated { items, .. } if items.len() == 1)
        );
    }

    #[tokio::test]
    async fn incremental_unsupported_returns_false_without_requests() {
        let t = Arc::new(FixtureTransport::new());
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = Syncer::new(
            Arc::new(Github::new(t.clone())),
            tx,
            2000,
            IncrementalMode::Unsupported,
        );
        assert!(
            !s.incremental(&ProjectId::new("P"), "2026-10-01T00:00:00Z")
                .await
        );
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn incremental_over_the_cap_falls_back_to_a_full_load() {
        let (t, s, mut rx) = setup(2);
        t.push("ItemsPage", page(&["a", "b"], 5, Some("c1")));
        t.push("ItemsPage", page(&["a", "b"], 2, None));
        assert!(
            s.incremental(&ProjectId::new("P"), "2026-10-01T00:00:00Z")
                .await
        );
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::ItemsComplete { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SyncEvent::ItemsUpdated { .. }))
        );
        assert_eq!(t.requests()[1].variables["query"], "");
    }

    #[tokio::test]
    async fn detail_is_sent_with_the_older_flag() {
        let (t, s, mut rx) = setup(2000);
        let node = json!({"node": {"content": {"__typename": "Issue", "body": "b",
            "comments": {"totalCount": 0, "pageInfo": {"hasPreviousPage": false, "startCursor": null}, "nodes": []}}}});
        t.push("ItemDetail", node.clone());
        t.push("ItemDetail", node);
        s.detail(&ItemId::new("I1"), None).await;
        s.detail(&ItemId::new("I1"), Some("cur".into())).await;
        let events = drain(&mut rx);
        assert!(
            matches!(&events[0], SyncEvent::Detail { item, detail, older: false } if item.as_str() == "I1" && detail.body == "b")
        );
        assert!(matches!(&events[1], SyncEvent::Detail { older: true, .. }));
        assert_eq!(t.requests()[1].variables["before"], "cur");
    }
}
