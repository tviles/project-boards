use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedRef {
    pub number: u32,
    pub title: String,
    pub state: String,
}

/// Everything the item detail shows beyond what the board already has.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ItemDetail {
    pub body: String,
    /// Oldest first.
    pub comments: Vec<Comment>,
    pub comments_total: usize,
    /// Cursor for loading older comments, when there are any.
    pub older_cursor: Option<String>,
    pub sub_issues: Vec<LinkedRef>,
    pub parent: Option<LinkedRef>,
    pub issue_type: Option<String>,
    pub linked_prs: Vec<LinkedRef>,
}
