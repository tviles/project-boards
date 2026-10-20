//! Runs fetches and reports results as events; never touches the store.

use crate::backend::{Backend, Backends, SyncProfile, profile_for};
use crate::error::{BackendError, GraphqlErrorEntry};
use crate::linear::client::board_of;
use crate::linear::convert::Polled;
use crate::model::*;
use crate::store::ViewList;
use crate::sync::{SyncEvent, SyncTask, incremental_query, search_query};
use crate::transport::Page;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A board of one provider sent to another provider's backend.
fn disagree() -> BackendError {
    BackendError::Decode("board and backend disagree".into())
}

/// The picker's line for a provider whose credentials did not resolve.
fn missing_credentials_note(provider: Provider) -> &'static str {
    match provider {
        Provider::Github => "GitHub boards: run `gh auth login` or set GH_TOKEN",
        Provider::Linear => "Linear boards: set LINEAR_API_KEY or linear_api_key_command",
    }
}

/// GitHub's boards for the picker: `linked`'s boards first, then every board the viewer sees.
async fn github_boards(
    backend: &Backend,
    linked: Option<&RepoSlug>,
) -> Result<Vec<ProjectSummary>, BackendError> {
    let Backend::Github(gh) = backend else {
        return Err(disagree());
    };
    let linked_list = match linked {
        Some(repo) => gh.list_repo_projects(repo).await?,
        None => Vec::new(),
    };
    let all = gh.list_viewer_projects().await?;
    Ok(crate::board_choice::picker_candidates(all, &linked_list))
}

/// Linear's boards for the picker: open projects, then teams.
async fn linear_boards(backend: &Backend) -> Result<Vec<ProjectSummary>, BackendError> {
    match backend {
        Backend::Linear(linear) => linear.list_boards().await,
        Backend::Github(_) => Err(disagree()),
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Runs fetches and reports results as events. Each method is one unit of work the runtime
/// spawns; none of them touch the store directly.
pub struct Syncer {
    backends: Backends,
    /// The provider of the board the job was spawned for; `None` when no board was open.
    provider: Option<Provider>,
    tx: mpsc::UnboundedSender<SyncEvent>,
    max_items: usize,
    profile: SyncProfile,
}

impl Syncer {
    /// A syncer for a job of `provider`'s board, loading and polling as that provider does.
    pub fn new(
        backends: Backends,
        provider: Option<Provider>,
        tx: mpsc::UnboundedSender<SyncEvent>,
        max_items: usize,
    ) -> Self {
        let profile = profile_for(provider.unwrap_or(Provider::Github));
        Self::with_profile(backends, provider, tx, max_items, profile)
    }

    /// `new` with another profile: tests of other incremental modes.
    pub fn with_profile(
        backends: Backends,
        provider: Option<Provider>,
        tx: mpsc::UnboundedSender<SyncEvent>,
        max_items: usize,
        profile: SyncProfile,
    ) -> Self {
        Self {
            backends,
            provider,
            tx,
            max_items,
            profile,
        }
    }

    /// The backend of the job's board. A job spawned with no board open is GitHub's: only
    /// the picker's and the editors' lookups run then.
    fn board_backend(&self) -> Result<Arc<Backend>, BackendError> {
        self.backends.get(self.provider.unwrap_or(Provider::Github))
    }

    /// The backend of the GitHub-only lookups (linked boards, candidates, search, repositories).
    fn github(&self) -> Result<Arc<Backend>, BackendError> {
        self.backends.get(Provider::Github)
    }

    fn send(&self, event: SyncEvent) {
        let _ = self.tx.send(event);
    }

    fn fail(&self, task: SyncTask, error: BackendError) {
        tracing::warn!(task = task.kind(), error = error.kind(), "sync task failed");
        self.send(SyncEvent::Failed { task, error });
    }

    /// Resolves `board` on its own provider's backend, whatever the job's tag: a board opened
    /// with no board shown (the tag is `None`) still reaches its provider, or reports that
    /// provider's missing credentials.
    pub async fn resolve(&self, board: &BoardKey) -> Option<Project> {
        let result = match self.backends.get(board.provider()) {
            Ok(backend) => match (&*backend, board) {
                (Backend::Github(gh), BoardKey::Github(b)) => gh.resolve_project(b).await,
                (
                    Backend::Linear(linear),
                    BoardKey::LinearProject { .. } | BoardKey::LinearTeam { .. },
                ) => linear.resolve(board).await,
                (
                    Backend::Github(_),
                    BoardKey::LinearProject { .. } | BoardKey::LinearTeam { .. },
                )
                | (Backend::Linear(_), BoardKey::Github(_)) => Err(disagree()),
            },
            Err(e) => Err(e),
        };
        match result {
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

    /// `false` after a failure (already reported as `Failed { Resolve }`); the job must stop.
    pub async fn refresh_project(&self, project: &Project) -> bool {
        let result = match self.board_backend() {
            Ok(backend) => match &*backend {
                Backend::Github(gh) => gh.fetch_project(&project.id, &project.board).await,
                Backend::Linear(linear) => linear.resolve(&project.board).await,
            },
            Err(e) => Err(e),
        };
        match result {
            Ok(p) => {
                self.send(SyncEvent::Project(p));
                true
            }
            Err(e) => {
                self.fail(SyncTask::Resolve, e);
                false
            }
        }
    }

    /// Every item, page by page, up to `max_items`. `ItemsComplete` only when all pages arrived.
    pub async fn full_load(&self, project: &ProjectId) {
        let backend = match self.board_backend() {
            Ok(backend) => backend,
            Err(e) => return self.fail(SyncTask::FullLoad, e),
        };
        let started = now_rfc3339();
        let mut items: Vec<Item> = Vec::new();
        let mut after: Option<String> = None;
        let (total, truncated) = loop {
            let page = match &*backend {
                Backend::Github(gh) => {
                    gh.fetch_items_page(project, "", self.profile.full_page, after.take())
                        .await
                }
                Backend::Linear(linear) => match board_of(project) {
                    Ok(board) => {
                        linear
                            .issues_page(&board, self.profile.full_page, after.take())
                            .await
                    }
                    Err(e) => Err(e),
                },
            };
            match page {
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
        self.send(SyncEvent::Access {
            scopes: backend.scopes(),
        });
        self.send(SyncEvent::Rate(backend.rate()));
    }

    /// Sends one edit and always reports exactly one `OpDone`, even if the send panics (it runs
    /// in its own task). The log names the op, the item and the kind of error, never GitHub's
    /// text, titles or values.
    pub async fn mutate(&self, id: crate::ops::OpId, project: &ProjectId, op: &crate::ops::Op) {
        let backend = match self.board_backend() {
            Ok(backend) => backend,
            Err(e) => {
                tracing::warn!(op = op.name(), item = %op.item(), error = e.kind(), "edit failed");
                return self.send(SyncEvent::OpDone { id, result: Err(e) });
            }
        };
        let (sender, project_id, sent) = (backend.clone(), project.clone(), op.clone());
        let joined =
            tokio::spawn(async move { crate::ops::send::send(&sender, &project_id, &sent).await })
                .await;
        let result = joined
            .unwrap_or_else(|_| Err(BackendError::Network("the edit failed unexpectedly".into())));
        if let Err(e) = &result {
            tracing::warn!(op = op.name(), item = %op.item(), error = e.kind(), "edit failed");
        }
        self.send(SyncEvent::OpDone { id, result });
        self.send(SyncEvent::Rate(backend.rate()));
    }

    /// Items updated since `since`. Returns false when the mode has no incremental filter, so
    /// the caller runs a full load instead. Issues a Linear poll finds in the trash are
    /// reported as removed.
    pub async fn incremental(&self, project: &ProjectId, since: &str) -> bool {
        let Some(query) = incremental_query(self.profile.incremental, since) else {
            return false;
        };
        let backend = match self.board_backend() {
            Ok(backend) => backend,
            Err(e) => {
                self.fail(SyncTask::Incremental, e);
                return true;
            }
        };
        let started = now_rfc3339();
        let mut items = Vec::new();
        let mut removed = Vec::new();
        let mut after = None;
        loop {
            let page = match &*backend {
                Backend::Github(gh) => gh
                    .fetch_items_page(project, &query, self.profile.poll_page, after.take())
                    .await
                    .map(|page| Page {
                        nodes: page.nodes.into_iter().map(Polled::Item).collect(),
                        total: page.total,
                        next: page.next,
                    }),
                Backend::Linear(linear) => match board_of(project) {
                    Ok(board) => {
                        linear
                            .poll_page(&board, since, self.profile.poll_page, after.take())
                            .await
                    }
                    Err(e) => Err(e),
                },
            };
            match page {
                Ok(page) => {
                    for polled in page.nodes {
                        match polled {
                            Polled::Item(item) => items.push(item),
                            Polled::Trashed(id) => removed.push(id),
                        }
                    }
                    match page.next {
                        Some(cursor) if items.len() + removed.len() < self.max_items => {
                            after = Some(cursor)
                        }
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
            removed,
            fetched_at: started,
        });
        self.send(SyncEvent::Rate(backend.rate()));
        true
    }

    /// The view's matching ids, then (with `hydrate`) any of those items not in `known`, 100 per
    /// request. The controller passes `hydrate: false` while a full load is fetching them anyway.
    pub async fn view_ids(
        &self,
        project: &ProjectId,
        view: &ViewId,
        query: &ListQuery,
        known: &HashSet<ItemId>,
        hydrate: bool,
    ) {
        let backend = match self.board_backend() {
            Ok(backend) => backend,
            Err(e) => return self.fail(SyncTask::ViewIds(view.clone()), e),
        };
        let mut ids: Vec<ItemId> = Vec::new();
        let mut after = None;
        let (total, truncated) = loop {
            let page = match &*backend {
                Backend::Github(gh) => match &query.filter {
                    ViewFilter::Github(text) => {
                        gh.fetch_view_ids_page(project, text, after.take()).await
                    }
                    ViewFilter::LinearView { .. } | ViewFilter::Linear(_) => {
                        Err(BackendError::Api(vec![GraphqlErrorEntry {
                            message: "this list does not belong to a GitHub board".into(),
                            kind: None,
                        }]))
                    }
                },
                Backend::Linear(linear) => match board_of(project) {
                    Ok(board) => linear.view_ids_page(&board, query, after.take()).await,
                    Err(e) => Err(e),
                },
            };
            match page {
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
            query: query.clone(),
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
            let result = match &*backend {
                Backend::Github(gh) => gh.hydrate_items(chunk).await,
                Backend::Linear(linear) => linear.hydrate(chunk).await,
            };
            match result {
                Ok(items) => self.send(SyncEvent::Hydrated(items)),
                Err(e) => return self.fail(SyncTask::Hydrate, e),
            }
        }
    }

    /// The item's body, comments and links. `before` loads the comments older than a cursor.
    pub async fn detail(&self, item: &ItemId, before: Option<String>) {
        let older = before.is_some();
        let result = match self.board_backend() {
            Ok(backend) => match &*backend {
                Backend::Github(gh) => gh.fetch_item_detail(item, before).await,
                Backend::Linear(linear) => linear.detail(item, before).await,
            },
            Err(e) => Err(e),
        };
        match result {
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
        let result = match self.github().as_deref() {
            Ok(Backend::Github(gh)) => gh.list_repo_projects(repo).await,
            Ok(Backend::Linear(_)) => Err(disagree()),
            Err(e) => Err(e.clone()),
        };
        match result {
            Ok(list) => self.send(SyncEvent::RepoProjects(list)),
            Err(e) => self.fail(SyncTask::Projects, e),
        }
    }

    /// Boards for the picker: GitHub's (the repo's linked boards first, then every board the
    /// viewer can see), then Linear's projects and teams. A provider without credentials, or one
    /// whose list fails while another's loads, becomes a note; only when every provider with
    /// credentials fails is the list a failure (GitHub's error, else Linear's).
    pub async fn projects(&self, linked: Option<&RepoSlug>) {
        let (github, linear) = tokio::join!(
            async {
                match self.backends.github.as_deref() {
                    Some(backend) => Some(github_boards(backend, linked).await),
                    None => None,
                }
            },
            async {
                match self.backends.linear.as_deref() {
                    Some(backend) => Some(linear_boards(backend).await),
                    None => None,
                }
            },
        );
        let mut boards = Vec::new();
        let mut notes = Vec::new();
        let mut loaded = false;
        let mut first_error = None;
        for (provider, result) in [(Provider::Github, github), (Provider::Linear, linear)] {
            match result {
                None => notes.push(missing_credentials_note(provider).to_string()),
                Some(Ok(list)) => {
                    boards.extend(list);
                    loaded = true;
                }
                Some(Err(e)) => {
                    tracing::warn!(
                        task = SyncTask::Projects.kind(),
                        provider = %provider,
                        error = e.kind(),
                        "board list failed"
                    );
                    notes.push(format!(
                        "{provider} boards could not be loaded ({})",
                        e.kind()
                    ));
                    first_error.get_or_insert(e);
                }
            }
        }
        if loaded {
            return self.send(SyncEvent::Projects { boards, notes });
        }
        // No provider loaded: every configured one failed, or none has credentials (the setup
        // screen's case, reported as GitHub's missing token as before).
        let error = first_error.unwrap_or(BackendError::NoCredentials {
            provider: Provider::Github,
        });
        self.fail(SyncTask::Projects, error);
    }

    pub async fn candidates(&self, repo: &RepoSlug) {
        let result = match self.github().as_deref() {
            Ok(Backend::Github(gh)) => gh.repo_candidates(repo).await,
            Ok(Backend::Linear(_)) => Err(disagree()),
            Err(e) => Err(e.clone()),
        };
        match result {
            Ok(candidates) => self.send(SyncEvent::Candidates {
                repo: repo.clone(),
                candidates,
            }),
            Err(e) => self.fail(SyncTask::Candidates(repo.clone()), e),
        }
    }

    pub async fn repos(&self) {
        let result = match self.github().as_deref() {
            Ok(Backend::Github(gh)) => gh.pushable_repos().await,
            Ok(Backend::Linear(_)) => Err(disagree()),
            Err(e) => Err(e.clone()),
        };
        match result {
            Ok(list) => self.send(SyncEvent::Repos(list)),
            Err(e) => self.fail(SyncTask::Repos, e),
        }
    }

    pub async fn lookup_repo(&self, slug: &RepoSlug) {
        let result = match self.github().as_deref() {
            Ok(Backend::Github(gh)) => gh.lookup_repo(slug).await,
            Ok(Backend::Linear(_)) => Err(disagree()),
            Err(e) => Err(e.clone()),
        };
        match result {
            Ok(repo) => self.send(SyncEvent::RepoFound {
                slug: slug.clone(),
                repo,
            }),
            Err(e) => self.fail(SyncTask::RepoLookup(slug.clone()), e),
        }
    }

    /// A pasted issue or pull request URL is looked up directly; anything else is searched.
    pub async fn search(&self, query: &str, scope: &[RepoSlug]) {
        let result = match self.github().as_deref() {
            Ok(Backend::Github(gh)) => match parse_content_url(query) {
                Some((repo, number)) => gh
                    .content_by_number(&repo, number)
                    .await
                    .map(|hit| hit.into_iter().collect()),
                None => gh.search_content(&search_query(query, scope)).await,
            },
            Ok(Backend::Linear(_)) => Err(disagree()),
            Err(e) => Err(e.clone()),
        };
        match result {
            Ok(hits) => self.send(SyncEvent::SearchResults {
                query: query.to_string(),
                hits,
            }),
            Err(e) => self.fail(SyncTask::Search(query.to_string()), e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Github;
    use crate::github::convert::tests::item_json;
    use crate::github::fixture::FixtureTransport;
    use crate::linear::Linear;
    use crate::sync::IncrementalMode;
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
        let syncer = Syncer::with_profile(
            github_only(&t),
            Some(Provider::Github),
            tx,
            max,
            SyncProfile {
                incremental: IncrementalMode::DateTime,
                ..crate::backend::GITHUB_PROFILE
            },
        );
        (t, syncer, rx)
    }

    fn github_only(t: &Arc<FixtureTransport>) -> Backends {
        Backends {
            github: Some(Arc::new(Backend::Github(Github::new(t.clone())))),
            linear: None,
        }
    }

    const TEAM: &str = "1b4e28ba-2fa1-4d3b-a3f5-ef19b5a7633b";

    fn linear_team() -> BoardKey {
        BoardKey::LinearTeam { id: TEAM.into() }
    }

    /// A GitHub and a Linear transport, and a syncer for a job of `provider`'s board.
    fn setup_both(
        provider: Provider,
    ) -> (
        Arc<FixtureTransport>,
        Arc<FixtureTransport>,
        Syncer,
        mpsc::UnboundedReceiver<SyncEvent>,
    ) {
        let gh = Arc::new(FixtureTransport::new());
        let lin = Arc::new(FixtureTransport::new());
        let (tx, rx) = mpsc::unbounded_channel();
        let backends = Backends {
            github: Some(Arc::new(Backend::Github(Github::new(gh.clone())))),
            linear: Some(Arc::new(Backend::Linear(Linear::new(lin.clone())))),
        };
        (gh, lin, Syncer::new(backends, Some(provider), tx, 2000), rx)
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<SyncEvent>) -> Vec<SyncEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[tokio::test]
    async fn lookups_report_their_results() {
        let (t, s, mut rx) = setup(2000);
        t.push(
            "RepoCandidates",
            json!({"repository": {"id": "R_1", "assignableUsers": {"nodes": []},
            "labels": {"nodes": []}, "milestones": {"nodes": []}}}),
        );
        t.push(
            "ContentByNumber",
            json!({"repository": {"issueOrPullRequest": {"__typename": "Issue", "id": "I_1",
            "number": 12, "title": "Crash", "state": "OPEN", "repository": {"nameWithOwner": "acme/app"}}}}),
        );
        t.push_error("PushableRepos", BackendError::Network("offline".into()));
        let repo: RepoSlug = "acme/app".parse().unwrap();
        s.candidates(&repo).await;
        s.search("https://github.com/acme/app/issues/12", &[]).await;
        s.repos().await;
        let events = drain(&mut rx);
        assert!(matches!(&events[0], SyncEvent::Candidates { repo: r, .. } if r == &repo));
        assert!(
            matches!(&events[1], SyncEvent::SearchResults { hits, .. } if hits.len() == 1 && hits[0].number == 12)
        );
        assert!(matches!(
            &events[2],
            SyncEvent::Failed {
                task: SyncTask::Repos,
                ..
            }
        ));
        assert_eq!(
            t.requests()[1].operation_name,
            "ContentByNumber",
            "a URL is looked up, not searched"
        );
    }

    #[tokio::test]
    async fn a_search_sends_the_scoped_query_and_a_failure_names_no_text() {
        let (t, s, mut rx) = setup(2000);
        t.push("SearchContent", json!({"search": {"nodes": []}}));
        t.push_error("SearchContent", BackendError::Network("offline".into()));
        let scope: Vec<RepoSlug> = vec!["acme/app".parse().unwrap()];
        s.search("crash org:other", &scope).await;
        s.search("second", &scope).await;
        let events = drain(&mut rx);
        assert!(matches!(&events[0], SyncEvent::SearchResults { hits, .. } if hits.is_empty()));
        assert!(
            matches!(&events[1], SyncEvent::Failed { task: SyncTask::Search(q), .. } if q == "second")
        );
        let sent = serde_json::to_string(&t.requests()[0].variables).unwrap();
        assert!(
            sent.contains(r#"crash \"org:other\" repo:acme/app is:open"#),
            "{sent}"
        );
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
        assert_eq!(t.requests()[0].variables["first"], 100);
    }

    /// Review Focus 1: a page failing partway never produces a replace.
    #[tokio::test]
    async fn failure_mid_load_sends_no_complete() {
        let (t, s, mut rx) = setup(2000);
        t.push("ItemsPage", page(&["a", "b"], 4, Some("c1")));
        t.push_error(
            "ItemsPage",
            BackendError::Network("connection reset".into()),
        );
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
            &ListQuery::github("label:bug"),
            &known,
            true,
        )
        .await;
        let events = drain(&mut rx);
        assert!(
            matches!(&events[0], SyncEvent::ViewIds { query, list, .. } if query == &ListQuery::github("label:bug") && list.ids.len() == 2 && !list.truncated)
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
            &ListQuery::github("label:bug"),
            &HashSet::new(),
            false,
        )
        .await;
        assert_eq!(drain(&mut rx).len(), 1);
        assert_eq!(t.requests().len(), 1, "no HydrateItems request");
    }

    #[tokio::test]
    async fn a_linear_list_fails_on_a_github_board() {
        let (t, s, mut rx) = setup(2000);
        let query = ListQuery {
            filter: ViewFilter::LinearView {
                id: "cv1".into(),
                hide_closed: false,
            },
            include_archived: false,
        };
        s.view_ids(
            &ProjectId::new("P"),
            &ViewId::new("V"),
            &query,
            &HashSet::new(),
            true,
        )
        .await;
        let events = drain(&mut rx);
        assert!(matches!(
            &events[..],
            [SyncEvent::Failed { task: SyncTask::ViewIds(_), error }]
                if error.to_string().contains("this list does not belong to a GitHub board")
        ));
        assert!(t.requests().is_empty());
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

    /// Decision 9 on the cached path: a deleted GitHub board's refresh is not found, so the
    /// controller forgets it, whether GitHub answers a null node or a null node with a
    /// tolerated NOT_FOUND entry.
    #[tokio::test]
    async fn refreshing_a_deleted_github_board_is_not_found() {
        let (t, s, mut rx) = setup(2000);
        t.push("ProjectSchema", json!({"node": null}));
        t.push_body(
            "ProjectSchema",
            json!({"data": {"node": null}, "errors": [{"type": "NOT_FOUND",
                   "message": "Could not resolve to a node with the global id of 'P'"}]}),
        );
        let project = crate::store::snapshot::tests::project();
        for _ in 0..2 {
            assert!(!s.refresh_project(&project).await);
            let events = drain(&mut rx);
            assert!(
                matches!(&events[..], [SyncEvent::Failed { task: SyncTask::Resolve, error }]
                    if error.is_not_found() && error.to_string().contains("tviles/3")),
                "{events:?}"
            );
        }
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
    async fn incremental_polls_twenty_at_a_time_and_follows_pages() {
        let (t, s, mut rx) = setup(2000);
        t.push("ItemsPage", page(&["a", "b"], 3, Some("c1")));
        t.push("ItemsPage", page(&["c"], 3, None));
        assert!(
            s.incremental(&ProjectId::new("P"), "2026-10-01T00:00:00Z")
                .await
        );
        let requests = t.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.variables["first"] == 20));
        assert_eq!(requests[1].variables["after"], "c1");
        assert_eq!(
            requests[1].variables["query"],
            "updated:>=2026-10-01T00:00:00Z"
        );
        assert!(
            matches!(&drain(&mut rx)[0], SyncEvent::ItemsUpdated { items, .. } if items.len() == 3)
        );
    }

    #[tokio::test]
    async fn incremental_unsupported_returns_false_without_requests() {
        let t = Arc::new(FixtureTransport::new());
        let (tx, _rx) = mpsc::unbounded_channel();
        let s = Syncer::with_profile(
            github_only(&t),
            Some(Provider::Github),
            tx,
            2000,
            SyncProfile {
                incremental: IncrementalMode::Unsupported,
                ..crate::backend::GITHUB_PROFILE
            },
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

    #[tokio::test]
    async fn mutate_reports_the_result_then_the_rate() {
        let (t, s, mut rx) = setup(2000);
        t.push(
            "ClearFieldValue",
            json!({"clearProjectV2ItemFieldValue": {"projectV2Item": item_json()}}),
        );
        t.push_error("ClearFieldValue", BackendError::Network("offline".into()));
        let op = crate::ops::Op::ClearField {
            item: ItemId::new("PVTI_1"),
            field: FieldId::new("F_notes"),
        };
        s.mutate(7, &ProjectId::new("P"), &op).await;
        s.mutate(8, &ProjectId::new("P"), &op).await;
        let events = drain(&mut rx);
        assert!(matches!(
            &events[0],
            SyncEvent::OpDone {
                id: 7,
                result: Ok(crate::ops::Confirmed::Item(_))
            }
        ));
        assert!(matches!(&events[1], SyncEvent::Rate(_)));
        assert!(matches!(
            &events[2],
            SyncEvent::OpDone {
                id: 8,
                result: Err(BackendError::Network(_))
            }
        ));
    }

    #[tokio::test]
    async fn a_full_load_reports_the_token_scopes() {
        let (t, s, mut rx) = setup(2000);
        t.set_scopes(Some(vec!["project".into()]));
        t.push("ItemsPage", page(&["a"], 1, None));
        s.full_load(&ProjectId::new("P")).await;
        let events = drain(&mut rx);
        assert!(events.iter().any(
            |e| matches!(e, SyncEvent::Access { scopes: Some(s) } if s == &["project".to_string()])
        ));
        assert!(matches!(events.last(), Some(SyncEvent::Rate(_))));
    }

    struct Panicking;

    impl crate::github::transport::Transport for Panicking {
        fn execute(
            &self,
            _request: crate::github::transport::GraphqlRequest,
        ) -> futures::future::BoxFuture<
            '_,
            Result<crate::github::transport::GraphqlResponse, BackendError>,
        > {
            Box::pin(async { panic!("transport blew up") })
        }
    }

    #[tokio::test]
    async fn a_panicking_send_still_reports_one_op_done() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let s = Syncer::with_profile(
            Backends {
                github: Some(Arc::new(Backend::Github(Github::new(Arc::new(Panicking))))),
                linear: None,
            },
            Some(Provider::Github),
            tx,
            2000,
            SyncProfile {
                incremental: IncrementalMode::DateTime,
                ..crate::backend::GITHUB_PROFILE
            },
        );
        let op = crate::ops::Op::ClearField {
            item: ItemId::new("PVTI_1"),
            field: FieldId::new("F_notes"),
        };
        s.mutate(9, &ProjectId::new("P"), &op).await;
        let events = drain(&mut rx);
        let dones = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    SyncEvent::OpDone {
                        id: 9,
                        result: Err(_)
                    }
                )
            })
            .count();
        assert_eq!(dones, 1);
    }

    #[tokio::test]
    async fn a_linear_board_without_linear_fails_as_missing_credentials() {
        let t = Arc::new(FixtureTransport::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let s = Syncer::new(github_only(&t), Some(Provider::Linear), tx, 2000);
        let team: BoardKey = "linear:team:t1".parse().unwrap();
        assert_eq!(s.resolve(&team).await, None);
        let events = drain(&mut rx);
        assert!(matches!(
            events.as_slice(),
            [SyncEvent::Failed {
                task: SyncTask::Resolve,
                error: BackendError::NoCredentials {
                    provider: Provider::Linear
                },
            }]
        ));
        assert!(
            t.requests().is_empty(),
            "nothing is sent for a Linear board"
        );
    }

    #[tokio::test]
    async fn a_resolve_takes_its_backend_from_the_board_not_the_job() {
        // No board open (the job has no provider), and no Linear credentials.
        let t = Arc::new(FixtureTransport::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let s = Syncer::new(github_only(&t), None, tx, 2000);
        assert_eq!(s.resolve(&linear_team()).await, None);
        assert!(matches!(
            drain(&mut rx).as_slice(),
            [SyncEvent::Failed {
                task: SyncTask::Resolve,
                error: BackendError::NoCredentials {
                    provider: Provider::Linear
                },
            }]
        ));
        assert!(t.requests().is_empty());

        // A GitHub job tag does not send a Linear board to GitHub.
        let (gh, lin, s, mut rx) = setup_both(Provider::Github);
        lin.push(
            "ResolveTeamBoard",
            json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
                   "team": {"id": TEAM, "key": "TBA", "name": "Testbed A", "cyclesEnabled": true}}),
        );
        let schema = crate::linear::convert::tests::schema_json()
            .to_string()
            .replace("team-a", TEAM);
        lin.push("BoardSchema", serde_json::from_str(&schema).unwrap());
        lin.push(
            "TeamProjects",
            crate::linear::convert::tests::team_projects_json(),
        );
        assert!(s.resolve(&linear_team()).await.is_some());
        assert!(matches!(drain(&mut rx).as_slice(), [SyncEvent::Project(_)]));
        assert!(gh.requests().is_empty());
    }

    #[tokio::test]
    async fn a_linear_board_goes_to_the_linear_backend() {
        let (gh, lin, s, mut rx) = setup_both(Provider::Linear);
        lin.push(
            "ResolveTeamBoard",
            json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
                   "team": {"id": TEAM, "key": "TBA", "name": "Testbed A", "cyclesEnabled": true}}),
        );
        let schema = crate::linear::convert::tests::schema_json()
            .to_string()
            .replace("team-a", TEAM);
        lin.push("BoardSchema", serde_json::from_str(&schema).unwrap());
        lin.push(
            "TeamProjects",
            crate::linear::convert::tests::team_projects_json(),
        );
        let project = s.resolve(&linear_team()).await.expect("resolved");
        assert_eq!(project.board, linear_team());
        assert!(
            matches!(drain(&mut rx).as_slice(), [SyncEvent::Project(p)] if p.board == linear_team())
        );
        assert_eq!(lin.requests().len(), 3);
        assert!(gh.requests().is_empty(), "nothing goes to GitHub");
    }

    fn viewer_projects() -> serde_json::Value {
        json!({"viewer": {"login": "acme", "projectsV2": {"nodes": [
            {"id": "PVT_7", "number": 7, "title": "Sprint", "closed": false,
             "owner": {"__typename": "User", "login": "acme"}}]},
            "organizations": {"nodes": []}}})
    }

    fn list_boards() -> serde_json::Value {
        json!({"projects": {"nodes": [{"id": "p1", "name": "Launch"}]},
               "teams": {"nodes": [{"id": "t1", "key": "TBA", "name": "Testbed A"}]}})
    }

    fn boards_and_notes(events: &[SyncEvent]) -> (Vec<String>, Vec<String>) {
        match events {
            [SyncEvent::Projects { boards, notes }] => (
                boards.iter().map(|b| b.board.to_string()).collect(),
                notes.clone(),
            ),
            other => panic!("expected one Projects event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_failing_provider_is_a_note_and_both_failing_is_a_failure() {
        // Both load: GitHub's boards, then Linear's projects and teams, and no note.
        let (gh, lin, s, mut rx) = setup_both(Provider::Github);
        gh.push("ViewerProjects", viewer_projects());
        lin.push("ListBoards", list_boards());
        s.projects(None).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)),
            (
                vec![
                    "acme/7".to_string(),
                    "linear:project:p1".into(),
                    "linear:team:t1".into()
                ],
                vec![]
            )
        );

        // GitHub fails: Linear's boards and a note naming GitHub's error kind.
        gh.push_error("ViewerProjects", BackendError::Network("offline".into()));
        lin.push("ListBoards", list_boards());
        s.projects(None).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)),
            (
                vec!["linear:project:p1".to_string(), "linear:team:t1".into()],
                vec!["GitHub boards could not be loaded (Network)".to_string()]
            )
        );

        // The linked list is GitHub's too.
        let repo: RepoSlug = "acme/app".parse().unwrap();
        gh.push_error(
            "RepoProjects",
            BackendError::Unauthorized {
                provider: Provider::Github,
            },
        );
        lin.push("ListBoards", list_boards());
        s.projects(Some(&repo)).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)).1,
            ["GitHub boards could not be loaded (Unauthorized)"]
        );

        // Linear fails: GitHub's boards and a note naming Linear's error kind.
        gh.push("ViewerProjects", viewer_projects());
        lin.push_error("ListBoards", BackendError::Network("offline".into()));
        s.projects(None).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)),
            (
                vec!["acme/7".to_string()],
                vec!["Linear boards could not be loaded (Network)".to_string()]
            )
        );

        // Both fail: GitHub's error, as today.
        gh.push_error(
            "ViewerProjects",
            BackendError::Unauthorized {
                provider: Provider::Github,
            },
        );
        lin.push_error("ListBoards", BackendError::Network("offline".into()));
        s.projects(None).await;
        assert_eq!(
            drain(&mut rx),
            [SyncEvent::Failed {
                task: SyncTask::Projects,
                error: BackendError::Unauthorized {
                    provider: Provider::Github
                },
            }]
        );
    }

    #[tokio::test]
    async fn a_provider_without_credentials_is_a_note() {
        let syncer = |github: Option<&Arc<FixtureTransport>>,
                      linear: Option<&Arc<FixtureTransport>>| {
            let (tx, rx) = mpsc::unbounded_channel();
            let backends = Backends {
                github: github.map(|t| Arc::new(Backend::Github(Github::new(t.clone())))),
                linear: linear.map(|t| Arc::new(Backend::Linear(Linear::new(t.clone())))),
            };
            (Syncer::new(backends, None, tx, 2000), rx)
        };
        let gh = Arc::new(FixtureTransport::new());
        let lin = Arc::new(FixtureTransport::new());

        // A Linear-only user: Linear's boards and the GitHub fix, never a failure.
        let (s, mut rx) = syncer(None, Some(&lin));
        lin.push("ListBoards", list_boards());
        s.projects(Some(&"acme/app".parse().unwrap())).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)),
            (
                vec!["linear:project:p1".to_string(), "linear:team:t1".into()],
                vec!["GitHub boards: run `gh auth login` or set GH_TOKEN".to_string()]
            )
        );

        // A GitHub-only user: GitHub's boards and the Linear fix; nothing goes to Linear.
        let (s, mut rx) = syncer(Some(&gh), None);
        gh.push("ViewerProjects", viewer_projects());
        s.projects(None).await;
        assert_eq!(
            boards_and_notes(&drain(&mut rx)),
            (
                vec!["acme/7".to_string()],
                vec!["Linear boards: set LINEAR_API_KEY or linear_api_key_command".to_string()]
            )
        );

        // The only configured provider fails: its own error, never GitHub's missing token.
        let (s, mut rx) = syncer(None, Some(&lin));
        lin.push_error(
            "ListBoards",
            BackendError::Unauthorized {
                provider: Provider::Linear,
            },
        );
        s.projects(None).await;
        assert_eq!(
            drain(&mut rx),
            [SyncEvent::Failed {
                task: SyncTask::Projects,
                error: BackendError::Unauthorized {
                    provider: Provider::Linear
                },
            }]
        );

        // No provider at all: GitHub's missing token, as today.
        let (s, mut rx) = syncer(None, None);
        s.projects(None).await;
        assert_eq!(
            drain(&mut rx),
            [SyncEvent::Failed {
                task: SyncTask::Projects,
                error: BackendError::NoCredentials {
                    provider: Provider::Github
                },
            }]
        );
        assert_eq!(lin.requests().len(), 2);
        assert_eq!(gh.requests().len(), 1);
    }

    #[tokio::test]
    async fn github_only_jobs_never_use_linear() {
        let (gh, lin, s, mut rx) = setup_both(Provider::Linear);
        gh.push(
            "RepoProjects",
            json!({"repository": {"projectsV2": {"nodes": []}}}),
        );
        gh.push(
            "RepoCandidates",
            json!({"repository": {"id": "R_1", "assignableUsers": {"nodes": []},
            "labels": {"nodes": []}, "milestones": {"nodes": []}}}),
        );
        gh.push("SearchContent", json!({"search": {"nodes": []}}));
        let repo: RepoSlug = "acme/app".parse().unwrap();
        s.repo_projects(&repo).await;
        s.candidates(&repo).await;
        s.search("crash", &[]).await;
        let events = drain(&mut rx);
        assert!(
            !events.iter().any(|e| matches!(e, SyncEvent::Failed { .. })),
            "{events:?}"
        );
        let sent: Vec<String> = gh
            .requests()
            .into_iter()
            .map(|r| r.operation_name)
            .collect();
        assert_eq!(sent, ["RepoProjects", "RepoCandidates", "SearchContent"]);
        assert!(lin.requests().is_empty(), "nothing goes to Linear");
    }

    #[tokio::test]
    async fn a_linear_poll_reports_trashed_issues_as_removed() {
        let (gh, lin, s, mut rx) = setup_both(Provider::Linear);
        let mut issue = crate::linear::convert::tests::issue_json();
        issue["id"] = json!("i-1");
        lin.push(
            "PollIssues",
            json!({"issues": {"pageInfo": {"hasNextPage": false, "endCursor": null},
                   "nodes": [issue, {"id": "i-9", "trashed": true}]}}),
        );
        let project = ProjectId::new(linear_team().to_string());
        assert!(s.incremental(&project, "2026-10-01T11:58:00Z").await);
        let events = drain(&mut rx);
        assert!(
            matches!(&events[0], SyncEvent::ItemsUpdated { items, removed, .. }
                if items.len() == 1 && items[0].id.as_str() == "i-1"
                    && removed == &[ItemId::new("i-9")]),
            "{events:?}"
        );
        let request = &lin.requests()[0];
        assert_eq!(request.variables["first"], 50);
        assert_eq!(
            request.variables["filter"]["and"][1],
            json!({"updatedAt": {"gt": "2026-10-01T11:58:00Z"}}),
            "polled by date-time"
        );
        assert!(gh.requests().is_empty());
    }

    #[tokio::test]
    async fn a_linear_board_loads_lists_and_details_through_linear() {
        let (gh, lin, s, mut rx) = setup_both(Provider::Linear);
        let project = ProjectId::new(linear_team().to_string());
        let mut issue = crate::linear::convert::tests::issue_json();
        issue["id"] = json!("i-1");
        let page = |nodes: Vec<serde_json::Value>| json!({"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": nodes});
        lin.push("BoardIssues", json!({"issues": page(vec![issue.clone()])}));
        lin.push(
            "IssueIds",
            json!({"issues": page(vec![json!({"id": "i-2"})])}),
        );
        lin.push("IssuesById", json!({"issues": {"nodes": [issue]}}));
        lin.push(
            "IssueDetail",
            json!({"issue": {"id": "i-1", "comments": {"nodes": []}}}),
        );
        s.full_load(&project).await;
        let query = ListQuery {
            filter: ViewFilter::Linear(json!({"priority": {"eq": 1}})),
            include_archived: false,
        };
        s.view_ids(&project, &ViewId::new("V"), &query, &HashSet::new(), true)
            .await;
        s.detail(&ItemId::new("i-1"), None).await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::ItemsComplete { items, .. } if items.len() == 1))
        );
        assert!(events.iter().any(
            |e| matches!(e, SyncEvent::ViewIds { list, .. } if list.ids == [ItemId::new("i-2")])
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::Hydrated(items) if items.len() == 1))
        );
        assert!(events.iter().any(|e| matches!(e, SyncEvent::Detail { .. })));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::Access { scopes: None }))
        );
        let sent: Vec<String> = lin
            .requests()
            .into_iter()
            .map(|r| r.operation_name)
            .collect();
        assert_eq!(
            sent,
            ["BoardIssues", "IssueIds", "IssuesById", "IssueDetail"]
        );
        assert_eq!(lin.requests()[0].variables["first"], 100);
        assert!(gh.requests().is_empty());
    }
}
