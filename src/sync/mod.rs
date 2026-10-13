//! Loading and polling: when (scheduler) and what (syncer).

pub mod scheduler;
pub mod syncer;

use crate::github::GithubError;
use crate::github::transport::RateInfo;
use crate::model::{
    ContentHit, Item, ItemDetail, ItemId, Project, ProjectSummary, PushableRepos, RepoCandidates,
    RepoRef, RepoSlug, ViewId,
};
use crate::ops::{Confirmed, OpId};
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
    Candidates(RepoSlug),
    Repos,
    RepoLookup(RepoSlug),
    Search(String),
}

impl SyncTask {
    /// The task's name for logs. Never the search text or a repository: the log carries no
    /// board content (Global Constraints).
    pub fn kind(&self) -> &'static str {
        match self {
            SyncTask::Resolve => "Resolve",
            SyncTask::FullLoad => "FullLoad",
            SyncTask::Incremental => "Incremental",
            SyncTask::ViewIds(_) => "ViewIds",
            SyncTask::Hydrate => "Hydrate",
            SyncTask::Detail(_) => "Detail",
            SyncTask::Projects => "Projects",
            SyncTask::Candidates(_) => "Candidates",
            SyncTask::Repos => "Repos",
            SyncTask::RepoLookup(_) => "RepoLookup",
            SyncTask::Search(_) => "Search",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
// Events are sent one at a time over a channel; boxing `OpDone`'s items would only add noise.
#[allow(clippy::large_enum_variant)]
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
        /// The filter this list answers; stale answers are dropped by the App.
        filter: String,
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
    /// An edit finished. A secondary rate limit arrives as `Err(RateLimited)`; the controller
    /// re-queues those instead of failing them.
    OpDone {
        id: OpId,
        result: Result<Confirmed, GithubError>,
    },
    /// The token's classic scopes as the last response reported them (`None`: none reported).
    Access {
        scopes: Option<Vec<String>>,
    },
    Failed {
        task: SyncTask,
        error: GithubError,
    },
    Candidates {
        repo: RepoSlug,
        candidates: RepoCandidates,
    },
    Repos(PushableRepos),
    RepoFound {
        slug: RepoSlug,
        repo: Option<RepoRef>,
    },
    SearchResults {
        query: String,
        hits: Vec<ContentHit>,
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
        SyncEvent::ViewIds { view, list, .. } => Some(StoreUpdate::ViewIds {
            view: view.clone(),
            list: list.clone(),
        }),
        SyncEvent::RepoProjects(_)
        | SyncEvent::Projects(_)
        | SyncEvent::Detail { .. }
        | SyncEvent::Rate(_)
        | SyncEvent::OpDone { .. }
        | SyncEvent::Access { .. }
        | SyncEvent::Failed { .. }
        | SyncEvent::Candidates { .. }
        | SyncEvent::Repos(_)
        | SyncEvent::RepoFound { .. }
        | SyncEvent::SearchResults { .. } => None,
    }
}

/// True for a plain search word: nothing GitHub reads as an operator or qualifier.
fn plain_term(word: &str) -> bool {
    !matches!(word, "AND" | "OR" | "NOT")
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.'))
        && !word.starts_with('.')
}

/// `repo:owner/name` with a valid name: the one qualifier a query may carry.
fn repo_token(word: &str) -> Option<RepoSlug> {
    word.strip_prefix("repo:")?.parse().ok()
}

/// The GitHub search for "add existing": open issues and pull requests.
///
/// The user's words are text, never qualifiers: a word that is not plain is quoted (with `"`
/// and `\` escaped), so `is:pr`, `org:x`, `-label:y` or `OR` typed by the user cannot change
/// what is searched. Only an explicit `repo:owner/name` is kept, and it replaces the default
/// `scope` (the repositories of the board), widening the search as the spec allows.
pub fn search_query(query: &str, scope: &[RepoSlug]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut repos: Vec<String> = Vec::new();
    for word in query.split_whitespace() {
        if let Some(slug) = repo_token(word) {
            repos.push(format!("repo:{slug}"));
        } else if plain_term(word) {
            parts.push(word.to_string());
        } else {
            parts.push(format!(
                "\"{}\"",
                word.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
    }
    if repos.is_empty() {
        repos = scope.iter().map(|r| format!("repo:{r}")).collect();
    }
    parts.extend(repos);
    parts.push("is:open".into());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;

    #[test]
    fn failed_tasks_are_logged_by_kind_only() {
        assert_eq!(
            SyncTask::Search("secret launch plan".into()).kind(),
            "Search"
        );
        assert_eq!(
            SyncTask::Candidates("acme/app".parse().unwrap()).kind(),
            "Candidates"
        );
        assert_eq!(
            SyncTask::RepoLookup("acme/app".parse().unwrap()).kind(),
            "RepoLookup"
        );
        assert_eq!(SyncTask::ViewIds(ViewId::new("V")).kind(), "ViewIds");
    }

    fn scope() -> Vec<RepoSlug> {
        vec!["acme/app".parse().unwrap(), "acme/api".parse().unwrap()]
    }

    #[test]
    fn search_scopes_to_the_repositories_and_open_items() {
        assert_eq!(
            search_query(" crash ", &scope()),
            "crash repo:acme/app repo:acme/api is:open"
        );
        assert_eq!(
            search_query("", &scope()),
            "repo:acme/app repo:acme/api is:open"
        );
    }

    #[test]
    fn an_explicit_repo_replaces_the_scope() {
        assert_eq!(
            search_query("crash repo:octo/dots", &scope()),
            "crash repo:octo/dots is:open"
        );
        // Not a repository name: text, quoted, and the scope still applies.
        assert_eq!(
            search_query("repo:bad", &scope()),
            "\"repo:bad\" repo:acme/app repo:acme/api is:open"
        );
    }

    #[test]
    fn typed_qualifiers_are_text() {
        let s = scope();
        assert_eq!(
            search_query("foo org:other", &s),
            "foo \"org:other\" repo:acme/app repo:acme/api is:open"
        );
        assert_eq!(
            search_query("is:pr user:x label:bug", &s),
            "\"is:pr\" \"user:x\" \"label:bug\" repo:acme/app repo:acme/api is:open"
        );
        assert_eq!(
            search_query("crash is:closed", &[]),
            "crash \"is:closed\" is:open"
        );
        assert_eq!(
            search_query("-label:x OR y", &[]),
            "\"-label:x\" \"OR\" y is:open"
        );
        assert_eq!(
            search_query("say \"hi\" a\\b", &[]),
            "say \"\\\"hi\\\"\" \"a\\\\b\" is:open"
        );
    }

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
