use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub created_at: String,
    pub body: String,
    /// 0 for a top-level comment, 1 for a reply (Linear threads).
    #[serde(default)]
    pub depth: u8,
}

/// A link attached to a Linear issue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedRef {
    pub number: u32,
    pub title: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl LinkedRef {
    /// `ENG-123`, else `#12`.
    pub fn label(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| format!("#{}", self.number))
    }
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
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
