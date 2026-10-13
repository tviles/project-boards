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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_ids_expose_their_node_id() {
        assert_eq!(ContentId::Issue("I_1".into()).node_id(), "I_1");
        assert_eq!(ContentId::Draft("DI_1".into()).node_id(), "DI_1");
    }
}
