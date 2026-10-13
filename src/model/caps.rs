//! The capability matrix (spec section 4): the single source of truth for which operations
//! an item allows. The UI offers, hints and runs only what `check` allows, and shows the
//! `Denial` message for anything else.

use crate::model::ids::ItemId;
use crate::model::item::{ContentState, Item, ItemContent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OpKind {
    Move,
    Reorder,
    SetField,
    Archive,
    Unarchive,
    Remove,
    Title,
    Body,
    Assignees,
    Labels,
    Milestone,
    Comment,
    Close,
    Reopen,
    Convert,
}

impl OpKind {
    pub const ALL: [OpKind; 15] = [
        OpKind::Move,
        OpKind::Reorder,
        OpKind::SetField,
        OpKind::Archive,
        OpKind::Unarchive,
        OpKind::Remove,
        OpKind::Title,
        OpKind::Body,
        OpKind::Assignees,
        OpKind::Labels,
        OpKind::Milestone,
        OpKind::Comment,
        OpKind::Close,
        OpKind::Reopen,
        OpKind::Convert,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OpKind::Move => "move",
            OpKind::Reorder => "reorder",
            OpKind::SetField => "edit a field",
            OpKind::Archive => "archive",
            OpKind::Unarchive => "unarchive",
            OpKind::Remove => "remove from project",
            OpKind::Title => "edit title",
            OpKind::Body => "edit body",
            OpKind::Assignees => "assignees",
            OpKind::Labels => "labels",
            OpKind::Milestone => "milestone",
            OpKind::Comment => "comment",
            OpKind::Close => "close",
            OpKind::Reopen => "reopen",
            OpKind::Convert => "convert to issue",
        }
    }

    /// Operations on the project item itself; every content type, private ones included,
    /// allows these (spec: "move and archive only" for redacted items, plus remove).
    fn item_level(self) -> bool {
        matches!(self, OpKind::Move | OpKind::Archive | OpKind::Remove)
    }
}

/// What the board and the token allow, whatever the item.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditContext {
    /// `ProjectV2.viewerCanUpdate`.
    pub viewer_can_update: bool,
    /// Classic token scopes from `X-OAuth-Scopes`. `None` for fine-grained tokens and before
    /// the first response: nothing is known, so nothing is blocked up front.
    pub scopes: Option<Vec<String>>,
    /// Scopes GitHub refused an edit for (`INSUFFICIENT_SCOPES`), whatever `scopes` says.
    /// A repo refusal must record both "repo" and "public_repo", because either grants repo
    /// access and only a refusal of every alias counts.
    pub refused: Vec<String>,
}

impl EditContext {
    fn granted(&self, any_of: &[&str]) -> bool {
        let refused = any_of.iter().all(|s| self.refused.iter().any(|r| r == s));
        let listed = self
            .scopes
            .as_ref()
            .is_none_or(|s| s.iter().any(|x| any_of.contains(&x.as_str())));
        listed && !refused
    }

    pub fn has_project_scope(&self) -> bool {
        self.granted(&["project"])
    }

    pub fn has_repo_scope(&self) -> bool {
        self.granted(&["repo", "public_repo"])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    ReadOnly,
    NoProjectScope,
    NoRepoScope,
    Archived,
    NotArchived,
    Creating,
    Private,
    /// A content type newer than this build.
    Unsupported,
    DraftHasNo(OpKind),
    DraftNotClosable,
    NotADraft,
    AlreadyClosed,
    AlreadyOpen,
    Merged,
    StateUnknown,
    /// The item was cached before editing needed its content ids (Part B).
    NeedsRefresh,
}

impl Denial {
    /// The one-line reason shown when a key's operation is not allowed. `key` gives the key
    /// bound to an operation, for hints such as "(C)"; without one the hint is left out.
    pub fn message(&self, key: &dyn Fn(OpKind) -> Option<String>) -> String {
        let hint = |kind: OpKind, text: &str| match key(kind) {
            Some(k) => format!(" (`{k}`{text})"),
            None => String::new(),
        };
        match self {
            Denial::ReadOnly => "This board is read-only for you".into(),
            Denial::NoProjectScope => {
                "Editing needs the token's project scope: run `gh auth refresh -s project` and reopen the board".into()
            }
            Denial::NoRepoScope => {
                "Editing issues needs the repo scope: run `gh auth refresh -s repo` (or public_repo) and reopen the board".into()
            }
            Denial::Archived => format!(
                "Archived items can only be unarchived or removed{}",
                hint(OpKind::Unarchive, " unarchives")
            ),
            Denial::NotArchived => "Only archived items can be unarchived".into(),
            Denial::Creating => "Still being created on GitHub; try again in a moment".into(),
            Denial::Private => {
                "You can't see this item's content; only move, archive and remove work".into()
            }
            Denial::Unsupported => "This item type isn't supported yet".into(),
            Denial::DraftHasNo(kind) => {
                let what = match kind {
                    OpKind::Labels => "labels",
                    OpKind::Milestone => "milestone",
                    OpKind::Comment => "comments",
                    _ => "such field",
                };
                format!(
                    "Drafts have no {what} — convert to issue first{}",
                    hint(OpKind::Convert, "")
                )
            }
            Denial::DraftNotClosable => format!(
                "Drafts can't be closed — archive{} or remove it instead",
                hint(OpKind::Archive, "")
            ),
            Denial::NotADraft => "Only drafts convert to issues".into(),
            Denial::AlreadyClosed => "Already closed".into(),
            Denial::AlreadyOpen => "Already open".into(),
            Denial::Merged => "Merged pull requests can't be reopened".into(),
            Denial::StateUnknown => "This item's state is unknown; refresh with r".into(),
            Denial::NeedsRefresh => {
                "This item was loaded by an older version; refresh with r to edit it".into()
            }
        }
    }
}

/// Content edits go to the issue, pull request or draft node.
fn needs_content_id(item: &Item) -> Result<(), Denial> {
    if item.content_fields.content_id.is_some() {
        Ok(())
    } else {
        Err(Denial::NeedsRefresh)
    }
}

/// A new item that GitHub has not created yet.
pub fn is_placeholder(id: &ItemId) -> bool {
    id.as_str().starts_with("local:")
}

fn board_check(ctx: &EditContext) -> Result<(), Denial> {
    if !ctx.viewer_can_update {
        return Err(Denial::ReadOnly);
    }
    if !ctx.has_project_scope() {
        return Err(Denial::NoProjectScope);
    }
    Ok(())
}

/// Whether a new item can be added: a draft or existing item (`needs_repo: false`) needs
/// only the board; a new issue also needs the repo scope.
pub fn check_new(needs_repo: bool, ctx: &EditContext) -> Result<(), Denial> {
    board_check(ctx)?;
    if needs_repo && !ctx.has_repo_scope() {
        return Err(Denial::NoRepoScope);
    }
    Ok(())
}

/// Whether `kind` is allowed on `item`; the reason when it is not.
pub fn check(kind: OpKind, item: &Item, ctx: &EditContext) -> Result<(), Denial> {
    use OpKind::*;
    if is_placeholder(&item.id) {
        return Err(Denial::Creating);
    }
    board_check(ctx)?;
    if item.archived {
        return if matches!(kind, Unarchive | Remove) {
            Ok(())
        } else {
            Err(Denial::Archived)
        };
    }
    if kind == Unarchive {
        return Err(Denial::NotArchived);
    }
    if kind.item_level() {
        return Ok(());
    }
    let (state, is_pr) = match &item.content {
        ItemContent::Issue { state, .. } => (*state, false),
        ItemContent::PullRequest { state, .. } => (*state, true),
        ItemContent::Draft { .. } => {
            return match kind {
                Title | Body | Assignees => needs_content_id(item),
                Reorder | SetField => Ok(()),
                Labels | Milestone | Comment => Err(Denial::DraftHasNo(kind)),
                Close | Reopen => Err(Denial::DraftNotClosable),
                Convert if ctx.has_repo_scope() => Ok(()),
                Convert => Err(Denial::NoRepoScope),
                Move | Archive | Unarchive | Remove => Ok(()),
            };
        }
        ItemContent::Redacted => return Err(Denial::Private),
        ItemContent::Unknown { .. } => return Err(Denial::Unsupported),
    };
    match kind {
        Reorder | SetField => return Ok(()),
        Convert => return Err(Denial::NotADraft),
        Close => match state {
            ContentState::Open => {}
            ContentState::Closed | ContentState::Merged => return Err(Denial::AlreadyClosed),
            ContentState::Unknown => return Err(Denial::StateUnknown),
        },
        Reopen => match state {
            ContentState::Closed => {}
            ContentState::Merged if is_pr => return Err(Denial::Merged),
            ContentState::Open => return Err(Denial::AlreadyOpen),
            ContentState::Merged | ContentState::Unknown => return Err(Denial::StateUnknown),
        },
        _ => {}
    }
    if !matches!(kind, Reorder | SetField) {
        needs_content_id(item)?;
    }
    if !ctx.has_repo_scope() {
        return Err(Denial::NoRepoScope);
    }
    Ok(())
}

/// Every operation `item` allows, in `OpKind::ALL` order.
pub fn capabilities(item: &Item, ctx: &EditContext) -> Vec<OpKind> {
    OpKind::ALL
        .into_iter()
        .filter(|k| check(*k, item, ctx).is_ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;
    use crate::model::{ContentRef, ItemId};

    fn ctx() -> EditContext {
        EditContext {
            viewer_can_update: true,
            scopes: Some(vec!["project".into(), "repo".into()]),
            refused: vec![],
        }
    }

    fn pull(state: ContentState) -> Item {
        let mut i = issue("p", 4, "PR");
        i.content = ItemContent::PullRequest {
            reference: ContentRef {
                repo: "acme/app".into(),
                number: 4,
                url: "u".into(),
            },
            title: "PR".into(),
            state,
            is_draft: false,
        };
        i
    }

    fn with(content: ItemContent) -> Item {
        let mut i = issue("x", 1, "x");
        i.content = content;
        i
    }

    fn closed_issue() -> Item {
        let mut i = issue("c", 2, "c");
        if let ItemContent::Issue { state, .. } = &mut i.content {
            *state = ContentState::Closed;
        }
        i
    }

    use OpKind::*;

    #[test]
    fn an_item_cached_without_content_ids_needs_a_refresh() {
        let mut i = issue("i", 1, "x");
        i.content_fields.content_id = None;
        assert_eq!(check(Labels, &i, &ctx()), Err(Denial::NeedsRefresh));
        assert_eq!(
            check(Move, &i, &ctx()),
            Ok(()),
            "project ops need only the item id"
        );
        let mut d = with(ItemContent::Draft { title: "d".into() });
        d.content_fields.content_id = None;
        assert_eq!(check(Title, &d, &ctx()), Err(Denial::NeedsRefresh));
        assert_eq!(
            check(Labels, &d, &ctx()),
            Err(Denial::DraftHasNo(Labels)),
            "the draft rule first"
        );
    }

    #[test]
    fn the_matrix_per_content_type() {
        assert_eq!(
            capabilities(&issue("i", 1, "Open issue"), &ctx()),
            [
                Move, Reorder, SetField, Archive, Remove, Title, Body, Assignees, Labels,
                Milestone, Comment, Close
            ]
        );
        assert_eq!(
            capabilities(&closed_issue(), &ctx()),
            [
                Move, Reorder, SetField, Archive, Remove, Title, Body, Assignees, Labels,
                Milestone, Comment, Reopen
            ]
        );
        assert_eq!(
            capabilities(&pull(ContentState::Open), &ctx()),
            [
                Move, Reorder, SetField, Archive, Remove, Title, Body, Assignees, Labels,
                Milestone, Comment, Close
            ]
        );
        assert_eq!(
            capabilities(&pull(ContentState::Merged), &ctx()),
            [
                Move, Reorder, SetField, Archive, Remove, Title, Body, Assignees, Labels,
                Milestone, Comment
            ]
        );
        assert_eq!(
            capabilities(&with(ItemContent::Draft { title: "d".into() }), &ctx()),
            [
                Move, Reorder, SetField, Archive, Remove, Title, Body, Assignees, Convert
            ]
        );
        assert_eq!(
            capabilities(&with(ItemContent::Redacted), &ctx()),
            [Move, Archive, Remove]
        );
        assert_eq!(
            capabilities(
                &with(ItemContent::Unknown {
                    type_name: "Discussion".into()
                }),
                &ctx()
            ),
            [Move, Archive, Remove]
        );
    }

    #[test]
    fn archived_items_allow_only_unarchive_and_remove() {
        let mut i = issue("i", 1, "x");
        i.archived = true;
        assert_eq!(capabilities(&i, &ctx()), [Unarchive, Remove]);
        assert_eq!(check(Labels, &i, &ctx()), Err(Denial::Archived));
        assert_eq!(
            check(Unarchive, &issue("i", 1, "x"), &ctx()),
            Err(Denial::NotArchived)
        );
    }

    #[test]
    fn a_read_only_board_allows_nothing() {
        let ro = EditContext {
            viewer_can_update: false,
            ..ctx()
        };
        assert!(capabilities(&issue("i", 1, "x"), &ro).is_empty());
        assert_eq!(check(Move, &issue("i", 1, "x"), &ro), Err(Denial::ReadOnly));
        assert_eq!(check_new(false, &ro), Err(Denial::ReadOnly));
    }

    #[test]
    fn missing_scopes_block_what_they_cover() {
        let no_project = EditContext {
            scopes: Some(vec!["repo".into()]),
            ..ctx()
        };
        assert_eq!(
            check(Move, &issue("i", 1, "x"), &no_project),
            Err(Denial::NoProjectScope)
        );
        assert_eq!(check_new(false, &no_project), Err(Denial::NoProjectScope));
        let no_repo = EditContext {
            scopes: Some(vec!["project".into()]),
            ..ctx()
        };
        assert_eq!(check(Move, &issue("i", 1, "x"), &no_repo), Ok(()));
        assert_eq!(
            check(Labels, &issue("i", 1, "x"), &no_repo),
            Err(Denial::NoRepoScope)
        );
        assert_eq!(
            check(
                Title,
                &with(ItemContent::Draft { title: "d".into() }),
                &no_repo
            ),
            Ok(()),
            "drafts live in the project"
        );
        assert_eq!(
            check(
                Convert,
                &with(ItemContent::Draft { title: "d".into() }),
                &no_repo
            ),
            Err(Denial::NoRepoScope)
        );
        assert_eq!(check_new(true, &no_repo), Err(Denial::NoRepoScope));
        let public = EditContext {
            scopes: Some(vec!["project".into(), "public_repo".into()]),
            ..ctx()
        };
        assert_eq!(check(Labels, &issue("i", 1, "x"), &public), Ok(()));
        let fine_grained = EditContext {
            scopes: None,
            ..ctx()
        };
        assert_eq!(
            check(Labels, &issue("i", 1, "x"), &fine_grained),
            Ok(()),
            "unknown scopes block nothing"
        );
        let refused = EditContext {
            scopes: None,
            refused: vec!["project".into()],
            ..ctx()
        };
        assert_eq!(
            check(Move, &issue("i", 1, "x"), &refused),
            Err(Denial::NoProjectScope),
            "GitHub said so"
        );
    }

    #[test]
    fn placeholders_allow_nothing_until_created() {
        let mut i = issue("local:1", 0, "New");
        i.id = ItemId::new("local:1");
        assert!(is_placeholder(&i.id));
        assert!(capabilities(&i, &ctx()).is_empty());
        assert_eq!(check(Move, &i, &ctx()), Err(Denial::Creating));
    }

    #[test]
    fn draft_and_state_denials() {
        let draft = with(ItemContent::Draft { title: "d".into() });
        assert_eq!(
            check(Labels, &draft, &ctx()),
            Err(Denial::DraftHasNo(Labels))
        );
        assert_eq!(check(Close, &draft, &ctx()), Err(Denial::DraftNotClosable));
        assert_eq!(
            check(Convert, &issue("i", 1, "x"), &ctx()),
            Err(Denial::NotADraft)
        );
        assert_eq!(
            check(Reopen, &pull(ContentState::Merged), &ctx()),
            Err(Denial::Merged)
        );
        assert_eq!(
            check(Close, &closed_issue(), &ctx()),
            Err(Denial::AlreadyClosed)
        );
        assert_eq!(
            check(Reopen, &issue("i", 1, "x"), &ctx()),
            Err(Denial::AlreadyOpen)
        );
        assert_eq!(
            check(Close, &pull(ContentState::Unknown), &ctx()),
            Err(Denial::StateUnknown)
        );
        assert_eq!(
            check(SetField, &with(ItemContent::Redacted), &ctx()),
            Err(Denial::Private)
        );
    }

    #[test]
    fn messages_name_the_fix_and_the_key() {
        let keys = |k: OpKind| match k {
            Convert => Some("C".to_string()),
            Unarchive | Archive => Some("A".to_string()),
            _ => None,
        };
        assert_eq!(
            Denial::DraftHasNo(Labels).message(&keys),
            "Drafts have no labels — convert to issue first (`C`)"
        );
        assert_eq!(
            Denial::DraftHasNo(Labels).message(&|_| None),
            "Drafts have no labels — convert to issue first"
        );
        assert!(
            Denial::NoProjectScope
                .message(&keys)
                .contains("gh auth refresh -s project")
        );
        assert!(
            Denial::NoRepoScope
                .message(&keys)
                .contains("gh auth refresh -s repo")
        );
        assert!(Denial::Archived.message(&keys).contains("(`A` unarchives)"));
        assert_eq!(
            Denial::ReadOnly.message(&keys),
            "This board is read-only for you"
        );
    }

    #[test]
    fn every_denial_has_its_exact_message() {
        let keys = |k: OpKind| match k {
            Convert => Some("C".to_string()),
            Unarchive => Some("U".to_string()),
            Archive => Some("A".to_string()),
            _ => None,
        };
        let cases: Vec<(Denial, &str)> = vec![
            (Denial::ReadOnly, "This board is read-only for you"),
            (
                Denial::NoProjectScope,
                "Editing needs the token's project scope: run `gh auth refresh -s project` and reopen the board",
            ),
            (
                Denial::NoRepoScope,
                "Editing issues needs the repo scope: run `gh auth refresh -s repo` (or public_repo) and reopen the board",
            ),
            (
                Denial::Archived,
                "Archived items can only be unarchived or removed (`U` unarchives)",
            ),
            (Denial::NotArchived, "Only archived items can be unarchived"),
            (
                Denial::Creating,
                "Still being created on GitHub; try again in a moment",
            ),
            (
                Denial::Private,
                "You can't see this item's content; only move, archive and remove work",
            ),
            (Denial::Unsupported, "This item type isn't supported yet"),
            (
                Denial::DraftHasNo(Labels),
                "Drafts have no labels — convert to issue first (`C`)",
            ),
            (
                Denial::DraftHasNo(Milestone),
                "Drafts have no milestone — convert to issue first (`C`)",
            ),
            (
                Denial::DraftHasNo(Comment),
                "Drafts have no comments — convert to issue first (`C`)",
            ),
            (
                Denial::DraftNotClosable,
                "Drafts can't be closed — archive (`A`) or remove it instead",
            ),
            (Denial::NotADraft, "Only drafts convert to issues"),
            (Denial::AlreadyClosed, "Already closed"),
            (Denial::AlreadyOpen, "Already open"),
            (Denial::Merged, "Merged pull requests can't be reopened"),
            (
                Denial::StateUnknown,
                "This item's state is unknown; refresh with r",
            ),
            (
                Denial::NeedsRefresh,
                "This item was loaded by an older version; refresh with r to edit it",
            ),
        ];
        for (denial, want) in cases {
            assert_eq!(denial.message(&keys), want, "{denial:?}");
        }
        assert_eq!(
            Denial::DraftNotClosable.message(&|_| None),
            "Drafts can't be closed — archive or remove it instead"
        );
    }

    #[test]
    fn archived_items_of_every_kind_allow_only_unarchive_and_remove() {
        for content in [
            ItemContent::Draft { title: "d".into() },
            ItemContent::Redacted,
            ItemContent::Unknown {
                type_name: "Discussion".into(),
            },
        ] {
            let mut i = with(content);
            i.archived = true;
            assert_eq!(capabilities(&i, &ctx()), [Unarchive, Remove]);
            assert_eq!(check(Move, &i, &ctx()), Err(Denial::Archived));
        }
        let mut i = issue("i", 1, "x");
        i.archived = true;
        assert_eq!(check(Remove, &i, &ctx()), Ok(()));
    }

    #[test]
    fn unsupported_content_has_its_own_reason() {
        let i = with(ItemContent::Unknown {
            type_name: "Discussion".into(),
        });
        assert_eq!(check(Title, &i, &ctx()), Err(Denial::Unsupported));
        assert_eq!(
            check(Title, &with(ItemContent::Redacted), &ctx()),
            Err(Denial::Private)
        );
    }

    #[test]
    fn a_read_only_board_blocks_drafts_and_pull_requests_too() {
        let ro = EditContext {
            viewer_can_update: false,
            ..ctx()
        };
        assert!(capabilities(&with(ItemContent::Draft { title: "d".into() }), &ro).is_empty());
        assert!(capabilities(&pull(ContentState::Open), &ro).is_empty());
        assert_eq!(
            check(Close, &pull(ContentState::Open), &ro),
            Err(Denial::ReadOnly)
        );
    }

    #[test]
    fn without_the_repo_scope_project_level_edits_still_work() {
        let no_repo = EditContext {
            scopes: Some(vec!["project".into()]),
            ..ctx()
        };
        let i = issue("i", 1, "x");
        for k in [Move, Reorder, SetField] {
            assert_eq!(check(k, &i, &no_repo), Ok(()), "{k:?}");
        }
        assert_eq!(
            check(Close, &pull(ContentState::Open), &no_repo),
            Err(Denial::NoRepoScope)
        );
    }
}
