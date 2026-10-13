//! References to GitHub objects an edit names by node id: users, labels, milestones,
//! repositories, and an item's own content.

use crate::model::ids::RepoSlug;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserRef {
    pub id: String,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LabelRef {
    pub id: String,
    pub name: String,
    /// Hex colour without `#`.
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MilestoneRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
    pub id: String,
    pub slug: RepoSlug,
}

/// Why an issue is closed. Pull requests close without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CloseReason {
    Completed,
    NotPlanned,
}

/// The node an issue, pull request or draft edit goes to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentId {
    Issue(String),
    PullRequest(String),
    Draft(String),
}

impl ContentId {
    pub fn node_id(&self) -> &str {
        match self {
            ContentId::Issue(id) | ContentId::PullRequest(id) | ContentId::Draft(id) => id,
        }
    }
}

/// What the assignee, label and milestone editors offer for one repository.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoCandidates {
    pub repo: RepoRef,
    pub users: Vec<UserRef>,
    pub labels: Vec<LabelRef>,
    pub milestones: Vec<MilestoneRef>,
    /// True when users, labels or milestones hit their cap, so a list is partial.
    pub truncated: bool,
}

/// Repositories the viewer can create issues in, most recently pushed first.
#[derive(Debug, Clone, PartialEq)]
pub struct PushableRepos {
    pub repos: Vec<RepoRef>,
    /// True when more repositories exist than the pages read.
    pub truncated: bool,
}

/// An issue or pull request found by search or by URL, to add to the board.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentHit {
    /// The node id `addProjectV2ItemById` takes.
    pub id: String,
    pub pull: bool,
    /// `owner/name`
    pub repo: String,
    pub number: u32,
    pub title: String,
    pub open: bool,
}

/// Owner and repository names: letters, digits, `-`, `_` and `.` only.
fn plain_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn slug_of(owner: &str, name: &str) -> Option<RepoSlug> {
    (plain_name(owner) && plain_name(name)).then(|| RepoSlug {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// `https://github.com/owner/name/issues/12`, `…/pull/12` (trailing path, query and fragment
/// allowed) or `owner/name#12`. Any other host or shape is `None`.
pub fn parse_content_url(text: &str) -> Option<(RepoSlug, u32)> {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("https://github.com/") {
        let path = rest.split(['?', '#']).next().unwrap_or("");
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 4 && matches!(parts[2], "issues" | "pull") {
            return Some((slug_of(parts[0], parts[1])?, parts[3].parse().ok()?));
        }
        return None;
    }
    let (repo, number) = t.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    Some((slug_of(owner, name)?, number.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_ids_expose_their_node_id() {
        assert_eq!(ContentId::Issue("I_1".into()).node_id(), "I_1");
        assert_eq!(ContentId::Draft("DI_1".into()).node_id(), "DI_1");
    }

    #[test]
    fn issue_and_pull_request_urls_parse() {
        let r = |s: &str| parse_content_url(s).map(|(repo, n)| (repo.to_string(), n));
        assert_eq!(
            r("https://github.com/acme/app/issues/12"),
            Some(("acme/app".into(), 12))
        );
        assert_eq!(
            r(" https://github.com/acme/app/pull/7/files "),
            Some(("acme/app".into(), 7))
        );
        assert_eq!(
            r("https://github.com/acme/app/issues/5#issuecomment-9"),
            Some(("acme/app".into(), 5))
        );
        assert_eq!(r("acme/app#3"), Some(("acme/app".into(), 3)));
        assert_eq!(r("crash on start"), None);
        assert_eq!(r("https://github.com/acme/app"), None);
        assert_eq!(r("https://example.com/acme/app/issues/1"), None);
        assert_eq!(r("https://github.com.evil.io/acme/app/issues/1"), None);
        assert_eq!(r("http://github.com/acme/app/issues/1"), None);
        assert_eq!(r("https://github.com/acme/app/issues/x"), None);
        assert_eq!(r("https://github.com/acme/app/discussions/1"), None);
        assert_eq!(r("fix the a/b#1 bug"), None);
        assert_eq!(r("a/b/c#1"), None);
    }
}
