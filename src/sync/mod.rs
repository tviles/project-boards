//! Loading and polling: when (scheduler) and what (syncer).

pub mod scheduler;
pub mod syncer;

use crate::error::BackendError;
use crate::github::transport::RateInfo;
use crate::model::{
    ContentHit, Item, ItemDetail, ItemId, ListQuery, Project, ProjectSummary, PushableRepos,
    RepoCandidates, RepoRef, RepoSlug, TeamCandidates, ViewId,
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
    /// A Linear team's members and labels, by team id.
    TeamCandidates(String),
    /// A Linear issue looked up by its key (`TBA-12`).
    IssueByKey(String),
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
            SyncTask::TeamCandidates(_) => "TeamCandidates",
            SyncTask::IssueByKey(_) => "IssueByKey",
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
        /// Items the poll found in Linear's trash; GitHub sends none. Archived Linear issues
        /// are not removed: they arrive in `items` with `archived: true`.
        removed: Vec<ItemId>,
        fetched_at: String,
    },
    ViewIds {
        view: ViewId,
        /// The request this list answers; stale answers are dropped by the App.
        query: ListQuery,
        list: ViewList,
    },
    Hydrated(Vec<Item>),
    /// The boards linked to the pane's repository, for the startup decision.
    RepoProjects(Vec<ProjectSummary>),
    /// Boards for the picker, and one line per provider whose boards are missing from it.
    Projects {
        boards: Vec<ProjectSummary>,
        notes: Vec<String>,
    },
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
        result: Result<Confirmed, BackendError>,
    },
    /// The token's classic scopes as the last response reported them (`None`: none reported).
    Access {
        scopes: Option<Vec<String>>,
    },
    Failed {
        task: SyncTask,
        error: BackendError,
    },
    Candidates {
        repo: RepoSlug,
        candidates: RepoCandidates,
    },
    /// A Linear team's members and labels, by team id.
    TeamCandidates {
        team: String,
        candidates: TeamCandidates,
    },
    /// The Linear issue `key` names, or `None` when there is no such issue.
    IssueFound {
        key: String,
        item: Option<Item>,
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
        SyncEvent::ItemsUpdated { items, removed, .. } => Some(StoreUpdate::Polled {
            upsert: items.clone(),
            remove: removed.clone(),
        }),
        SyncEvent::ViewIds { view, list, .. } => Some(StoreUpdate::ViewIds {
            view: view.clone(),
            list: list.clone(),
        }),
        SyncEvent::RepoProjects(_)
        | SyncEvent::Projects { .. }
        | SyncEvent::Detail { .. }
        | SyncEvent::Rate(_)
        | SyncEvent::OpDone { .. }
        | SyncEvent::Access { .. }
        | SyncEvent::Failed { .. }
        | SyncEvent::Candidates { .. }
        | SyncEvent::TeamCandidates { .. }
        | SyncEvent::IssueFound { .. }
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

/// `repo:owner/name`, the names made of letters, digits, `.`, `_` and `-` only (owner at most
/// 39 characters, name not empty): the one widening qualifier a query may carry.
fn repo_token(word: &str) -> Option<RepoSlug> {
    let (owner, name) = word.strip_prefix("repo:")?.split_once('/')?;
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    (plain(owner) && owner.len() <= 39 && plain(name))
        .then(|| format!("{owner}/{name}").parse().ok())
        .flatten()
}

/// The narrowing qualifiers a user may type, as typed (case-insensitive).
fn narrowing(word: &str) -> Option<&'static str> {
    ["is:open", "is:closed", "is:issue", "is:pr"]
        .into_iter()
        .find(|q| word.eq_ignore_ascii_case(q))
}

/// The longest search query sent; GitHub rejects longer ones.
pub const MAX_QUERY: usize = 256;

/// A built search: the query, how many default-scope repositories did not fit, and whether
/// it can be sent at all.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPlan {
    pub query: String,
    pub repos_dropped: usize,
    /// False when the user's text leaves no room for the scope (searching unscoped would
    /// widen it silently) or exceeds the cap on its own.
    pub fits: bool,
    /// A repository limits the search: the default scope or a `repo:` token. Without one the
    /// search would cover all of GitHub, so it is not sent.
    pub scoped: bool,
}

/// The GitHub search for "add existing": issues and pull requests, open unless the user typed
/// `is:open` or `is:closed`.
///
/// The user's words are text, never qualifiers: a word that is not plain is quoted (with `"`
/// and `\` escaped), so `org:x`, `-label:y` or `OR` cannot change what is searched. Exceptions:
/// `is:open`, `is:closed`, `is:issue` and `is:pr` narrow the search, and an explicit
/// `repo:owner/name` replaces the default `scope` (the board's repositories) as the spec allows.
pub fn search_plan(query: &str, scope: &[RepoSlug]) -> SearchPlan {
    let mut parts: Vec<String> = Vec::new();
    let mut repos: Vec<String> = Vec::new();
    let mut state_typed = false;
    for word in query.split_whitespace() {
        if let Some(slug) = repo_token(word) {
            repos.push(format!("repo:{slug}"));
        } else if let Some(q) = narrowing(word) {
            state_typed |= matches!(q, "is:open" | "is:closed");
            parts.push(q.to_string());
        } else if plain_term(word) {
            parts.push(word.to_string());
        } else {
            parts.push(format!(
                "\"{}\"",
                word.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
    }
    parts.extend(repos.iter().cloned());
    let tail = if state_typed { "" } else { " is:open" };
    let mut line = parts.join(" ");
    let mut fits = line.len() + tail.len() <= MAX_QUERY;
    let mut dropped = 0;
    if repos.is_empty() {
        for (i, r) in scope.iter().enumerate() {
            let next = format!("{line} repo:{r}");
            if next.len() + tail.len() > MAX_QUERY {
                dropped = scope.len() - i;
                fits = fits && i > 0;
                break;
            }
            line = next;
        }
    }
    line.push_str(tail);
    SearchPlan {
        query: line.trim().to_string(),
        repos_dropped: dropped,
        fits,
        scoped: !repos.is_empty() || !scope.is_empty(),
    }
}

pub fn search_query(query: &str, scope: &[RepoSlug]) -> String {
    search_plan(query, scope).query
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
        assert_eq!(SyncTask::IssueByKey("TBA-12".into()).kind(), "IssueByKey");
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
    fn a_search_is_scoped_by_the_default_scope_or_a_repo_token() {
        assert!(search_plan("crash", &scope()).scoped);
        assert!(search_plan("crash repo:octo/dots", &[]).scoped);
        assert!(!search_plan("crash", &[]).scoped);
        assert!(
            !search_plan("crash repo:bad", &[]).scoped,
            "not a repository name"
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
            search_query("user:x label:bug", &s),
            "\"user:x\" \"label:bug\" repo:acme/app repo:acme/api is:open"
        );
        assert_eq!(search_query("crash org:x", &[]), "crash \"org:x\" is:open");
    }

    #[test]
    fn only_narrowing_qualifiers_pass_through() {
        assert_eq!(search_query("crash is:closed", &[]), "crash is:closed");
        assert_eq!(search_query("crash IS:Open", &[]), "crash is:open");
        assert_eq!(search_query("is:pr foo", &[]), "is:pr foo is:open");
        assert_eq!(
            search_query("is:issue is:closed", &[]),
            "is:issue is:closed"
        );
        assert_eq!(
            search_query("is:merged is:private", &[]),
            "\"is:merged\" \"is:private\" is:open"
        );
    }

    #[test]
    fn a_repo_token_must_be_a_plain_name() {
        for bad in [
            "repo:a/b\"",
            "repo:a/b)",
            "repo:a/b:is:pr",
            "repo:/b",
            "repo:a/",
            "repo:a/b/c",
        ] {
            let q = search_query(bad, &[]);
            assert!(
                q.starts_with('"') && q.ends_with("\" is:open"),
                "{bad} -> {q}"
            );
        }
        let long_owner = format!("repo:{}/x", "a".repeat(40));
        assert!(search_query(&long_owner, &[]).starts_with('"'));
        assert_eq!(
            search_query("repo:a-b.c/d_e", &[]),
            "repo:a-b.c/d_e is:open"
        );
    }

    #[test]
    fn the_query_is_capped_by_dropping_scope_repositories() {
        let scope: Vec<RepoSlug> = (0..40)
            .map(|i| format!("acme/repository-{i:02}").parse().unwrap())
            .collect();
        let plan = search_plan("crash", &scope);
        assert!(plan.query.len() <= MAX_QUERY && plan.fits);
        assert!(plan.repos_dropped > 0 && plan.query.starts_with("crash repo:acme/repository-00"));
        assert!(plan.query.ends_with(" is:open"));
        let long = "x".repeat(300);
        assert!(
            !search_plan(&long, &scope).fits,
            "the text alone is too long"
        );
        let wide = "w".repeat(250);
        assert!(
            !search_plan(&wide, &scope).fits,
            "no room left for any scope"
        );
        assert!(search_plan("crash", &[]).fits);
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
    fn polls_upsert_and_remove() {
        let event = SyncEvent::ItemsUpdated {
            items: vec![issue("x", 1, "X")],
            removed: vec![ItemId::new("y")],
            fetched_at: "t".into(),
        };
        assert_eq!(
            store_update(&event),
            Some(StoreUpdate::Polled {
                upsert: vec![issue("x", 1, "X")],
                remove: vec![ItemId::new("y")],
            })
        );
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
            error: crate::error::BackendError::Network("x".into()),
        };
        assert_eq!(store_update(&failed), None);
    }
}
