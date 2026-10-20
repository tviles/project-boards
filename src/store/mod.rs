//! The board snapshot, the store trait the UI reads through, and the on-disk cache.

pub mod cache;
pub mod snapshot;

pub use snapshot::{BoardSnapshot, ViewList};

use crate::model::{Item, ItemId, Project, ViewId};
use crate::ops::{Confirmed, Op, OpId};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum StoreUpdate {
    Replace(BoardSnapshot),
    Project(Project),
    UpsertItems(Vec<Item>),
    /// An incremental poll: items changed and items gone. Removals apply after the recent
    /// layer is replayed, so a poll that dropped an item wins over a recent edit of it.
    Polled {
        upsert: Vec<Item>,
        remove: Vec<ItemId>,
    },
    ReplaceItems(Vec<Item>),
    ViewIds {
        view: ViewId,
        list: ViewList,
    },
    /// Drops a view's cached id list (its filter changed).
    ClearViewIds(ViewId),
    FetchedAt(String),
}

impl StoreUpdate {
    /// The kind of update and the ids it touches, for logs. Never titles or field values:
    /// board content stays out of the log file.
    pub fn summary(&self) -> String {
        let ids = |items: &[Item]| {
            items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Self::Replace(s) => format!("Replace({} items)", s.items.len()),
            Self::Project(p) => format!("Project({})", p.id),
            Self::UpsertItems(items) => format!("UpsertItems({})", ids(items)),
            Self::Polled { upsert, remove } => {
                format!(
                    "Polled({}; removed {})",
                    ids(upsert),
                    remove
                        .iter()
                        .map(|i| i.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Self::ReplaceItems(items) => format!("ReplaceItems({})", ids(items)),
            Self::ViewIds { view, list } => format!("ViewIds({view}, {} ids)", list.ids.len()),
            Self::ClearViewIds(view) => format!("ClearViewIds({view})"),
            Self::FetchedAt(t) => format!("FetchedAt({t})"),
        }
    }
}

/// How long an edit GitHub confirmed outranks what polls say about it. GitHub's item index
/// lags writes by more than a minute (phase 0 finding 0), so a poll in that window can still
/// carry the old value.
pub const RECENT_WINDOW: Duration = Duration::from_secs(5 * 60);

/// The time source of a store; tests move it by hand.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// An edit sent (or about to be sent) and not yet confirmed.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub id: OpId,
    pub op: Op,
}

/// What the UI reads the board through. A later milestone may move this behind a daemon;
/// callers only see this trait.
pub trait Store: Send {
    /// What the user sees: the confirmed board with the pending edits replayed on top.
    fn snapshot(&self) -> Option<&BoardSnapshot>;
    /// The board as GitHub last reported it, plus the edits it confirmed. The cache saves this.
    fn confirmed(&self) -> Option<&BoardSnapshot>;
    /// Server data. Edits confirmed within `RECENT_WINDOW` are re-applied on top of it.
    fn apply(&mut self, update: StoreUpdate);
    /// Adds an edit to the end of the pending list.
    fn begin(&mut self, op: Op) -> OpId;
    fn pending(&self) -> &[Pending];
    /// GitHub applied edit `id`: it leaves the pending list and enters the confirmed board.
    /// Returns `(placeholder, real)` when the edit created an item, which stays on the
    /// board through polls that do not list it yet for `RECENT_WINDOW`.
    fn settle_ok(&mut self, id: OpId, confirmed: Confirmed) -> Option<(ItemId, ItemId)>;
    /// Edit `id` failed: it leaves the pending list and the view is rebuilt without it.
    fn settle_failed(&mut self, id: OpId) -> Option<Op>;
}

/// An edit GitHub confirmed, kept for `RECENT_WINDOW`.
#[derive(Debug, Clone)]
enum Recent {
    Op(Op),
    /// An item this app created: polls may not list it yet.
    Item(Box<Item>),
    /// The issue a converted draft became: replaces the item while polls still list the draft.
    Converted(Box<Item>),
}

pub struct MemoryStore {
    confirmed: Option<BoardSnapshot>,
    pending: Vec<Pending>,
    /// `confirmed` with `pending` replayed; `None` while nothing is pending.
    derived: Option<BoardSnapshot>,
    /// Edits GitHub confirmed, with when, for `RECENT_WINDOW`.
    recent: Vec<(Instant, Recent)>,
    next_id: OpId,
    clock: Clock,
}

impl MemoryStore {
    pub fn new(snapshot: Option<BoardSnapshot>) -> Self {
        Self::with_clock(snapshot, Arc::new(Instant::now))
    }

    pub fn with_clock(snapshot: Option<BoardSnapshot>, clock: Clock) -> Self {
        Self {
            confirmed: snapshot,
            pending: Vec::new(),
            derived: None,
            recent: Vec::new(),
            next_id: 0,
            clock,
        }
    }

    fn rebuild(&mut self) {
        self.derived = match &self.confirmed {
            Some(c) if !self.pending.is_empty() => {
                let mut d = c.clone();
                for p in &self.pending {
                    p.op.apply(&mut d);
                }
                Some(d)
            }
            _ => None,
        };
    }

    /// Re-applies the edits GitHub confirmed within `RECENT_WINDOW`, in the order GitHub
    /// confirmed them (settle order), after server data that may predate them; forgets older
    /// ones. Edits to one item are sent one at a time, so for them that is the order made.
    fn reapply_recent(&mut self) {
        let now = (self.clock)();
        self.recent
            .retain(|(at, _)| now.saturating_duration_since(*at) < RECENT_WINDOW);
        let Some(c) = self.confirmed.as_mut() else {
            return;
        };
        for (_, recent) in &self.recent {
            match recent {
                Recent::Op(op) => op.apply(c),
                // Only while polls lack it; once listed, the poll's copy wins.
                Recent::Item(item) => {
                    if !c.items.contains_key(&item.id) {
                        c.upsert_items(vec![(**item).clone()]);
                    }
                }
                // A poll that dropped the item meanwhile wins, as in `settle_ok`.
                Recent::Converted(item) => {
                    if c.items.contains_key(&item.id) {
                        c.upsert_items(vec![(**item).clone()]);
                    }
                }
            }
        }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Option<&BoardSnapshot> {
        self.derived.as_ref().or(self.confirmed.as_ref())
    }

    fn confirmed(&self) -> Option<&BoardSnapshot> {
        self.confirmed.as_ref()
    }

    fn apply(&mut self, update: StoreUpdate) {
        let from_server = matches!(
            update,
            StoreUpdate::Replace(_)
                | StoreUpdate::UpsertItems(_)
                | StoreUpdate::Polled { .. }
                | StoreUpdate::ReplaceItems(_)
                | StoreUpdate::ViewIds { .. }
        );
        let mut removed = Vec::new();
        match (update, self.confirmed.as_mut()) {
            (StoreUpdate::Replace(s), _) => self.confirmed = Some(s),
            (StoreUpdate::Project(p), Some(s)) => s.set_project(p),
            (StoreUpdate::Project(p), None) => self.confirmed = Some(BoardSnapshot::new(p)),
            (StoreUpdate::UpsertItems(items), Some(s)) => s.upsert_items(items),
            (StoreUpdate::Polled { upsert, remove }, Some(s)) => {
                s.upsert_items(upsert);
                removed = remove;
            }
            (StoreUpdate::ReplaceItems(items), Some(s)) => s.replace_items(items),
            (StoreUpdate::ViewIds { view, list }, Some(s)) => s.set_view_ids(view, list),
            (StoreUpdate::ClearViewIds(view), Some(s)) => {
                s.views.remove(&view);
            }
            (StoreUpdate::FetchedAt(t), Some(s)) => s.fetched_at = Some(t),
            (other, None) => tracing::warn!(
                update = %other.summary(),
                "store update before the project was known; ignored"
            ),
        }
        if from_server {
            self.reapply_recent();
        }
        if let Some(s) = self.confirmed.as_mut() {
            s.remove_items(&removed);
        }
        self.rebuild();
    }

    fn begin(&mut self, op: Op) -> OpId {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.push(Pending { id, op });
        self.rebuild();
        id
    }

    fn pending(&self) -> &[Pending] {
        &self.pending
    }

    fn settle_ok(&mut self, id: OpId, confirmed: Confirmed) -> Option<(ItemId, ItemId)> {
        let pos = self.pending.iter().position(|p| p.id == id)?;
        let Pending { op, .. } = self.pending.remove(pos);
        let now = (self.clock)();
        let renamed = match confirmed {
            Confirmed::Created { temp, item, .. } => {
                if let Some(c) = self.confirmed.as_mut() {
                    c.upsert_items(vec![item.clone()]);
                }
                let real = item.id.clone();
                self.recent.push((now, Recent::Item(Box::new(item))));
                Some((temp, real))
            }
            // Nothing joined the board; the App logs it as a failure.
            Confirmed::NotAdded { .. } => None,
            other => {
                if let Some(c) = self.confirmed.as_mut() {
                    // A poll that removed the item meanwhile wins over the server's copy.
                    if let Confirmed::Item(item) = &other
                        && c.items.contains_key(&item.id)
                    {
                        c.upsert_items(vec![item.clone()]);
                    }
                    op.apply(c);
                }
                // `Convert` replays nothing: keep the returned issue instead.
                match (&op, other) {
                    (Op::Convert { .. }, Confirmed::Item(item)) => {
                        self.recent.push((now, Recent::Converted(Box::new(item))));
                    }
                    _ => self.recent.push((now, Recent::Op(op))),
                }
                None
            }
        };
        self.rebuild();
        renamed
    }

    fn settle_failed(&mut self, id: OpId) -> Option<Op> {
        let pos = self.pending.iter().position(|p| p.id == id)?;
        let Pending { op, .. } = self.pending.remove(pos);
        self.rebuild();
        Some(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;
    use crate::model::{FieldId, FieldValue, OptionId};
    use crate::store::snapshot::tests::project;
    use std::sync::Mutex;

    #[test]
    fn items_before_project_are_ignored_then_project_creates_snapshot() {
        let mut store = MemoryStore::new(None);
        store.apply(StoreUpdate::UpsertItems(vec![issue("a", 1, "A")]));
        assert!(store.snapshot().is_none());
        store.apply(StoreUpdate::Project(project()));
        store.apply(StoreUpdate::UpsertItems(vec![issue("a", 1, "A")]));
        assert_eq!(store.snapshot().unwrap().all_items().len(), 1);
    }

    #[test]
    fn update_summaries_name_item_ids_but_no_content() {
        let mut secret = issue("PVTI_a", 1, "Secret launch plan");
        secret.values.insert(
            crate::model::FieldId::new("F"),
            crate::model::FieldValue::Text("private note".into()),
        );
        let summary = StoreUpdate::UpsertItems(vec![secret, issue("PVTI_b", 2, "x")]).summary();
        assert_eq!(summary, "UpsertItems(PVTI_a, PVTI_b)");
        assert_eq!(
            StoreUpdate::Project(project()).summary(),
            format!("Project({})", project().id)
        );
    }

    #[test]
    fn clear_view_ids_drops_that_list() {
        let mut store =
            MemoryStore::new(Some(crate::store::snapshot::BoardSnapshot::new(project())));
        let list = ViewList {
            ids: vec![],
            total: 0,
            truncated: false,
        };
        store.apply(StoreUpdate::ViewIds {
            view: ViewId::new("V"),
            list,
        });
        assert!(
            store
                .snapshot()
                .unwrap()
                .views
                .contains_key(&ViewId::new("V"))
        );
        store.apply(StoreUpdate::ClearViewIds(ViewId::new("V")));
        assert!(
            !store
                .snapshot()
                .unwrap()
                .views
                .contains_key(&ViewId::new("V"))
        );
    }

    fn status_value(option: &str) -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new(option),
            name: option.into(),
        }
    }

    fn set(item: &str, option: &str) -> Op {
        Op::SetField {
            item: ItemId::new(item),
            field: FieldId::new("F_status"),
            value: status_value(option),
        }
    }

    fn status_of(s: Option<&BoardSnapshot>, item: &str) -> String {
        s.unwrap().items[&ItemId::new(item)]
            .value(&FieldId::new("F_status"))
            .map(|v| v.display())
            .unwrap_or_default()
    }

    /// A store over the UI fixture board and a clock the test moves by hand.
    fn layered() -> (MemoryStore, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let read = now.clone();
        let clock: Clock = Arc::new(move || *read.lock().unwrap());
        (
            MemoryStore::with_clock(Some(crate::ui::fixtures::snapshot()), clock),
            now,
        )
    }

    /// Item `a` as an old poll reports it: Status Todo.
    fn stale_a() -> crate::model::Item {
        crate::ui::fixtures::items().remove(0)
    }

    #[test]
    fn pending_edits_show_on_top_of_the_confirmed_board() {
        let (mut s, _) = layered();
        s.begin(set("a", "o_done"));
        assert_eq!(status_of(s.snapshot(), "a"), "o_done");
        assert_eq!(status_of(s.confirmed(), "a"), "Todo");
        assert_eq!(s.pending().len(), 1);
    }

    #[test]
    fn a_failed_edit_leaves_and_a_later_one_on_the_same_field_still_shows() {
        let (mut s, _) = layered();
        let first = s.begin(set("a", "o_done"));
        let second = s.begin(set("a", "o_prog"));
        assert_eq!(status_of(s.snapshot(), "a"), "o_prog");
        assert_eq!(s.settle_failed(first), Some(set("a", "o_done")));
        assert_eq!(
            status_of(s.snapshot(), "a"),
            "o_prog",
            "the later edit still replays"
        );
        assert_eq!(s.settle_ok(second, Confirmed::Applied), None);
        assert_eq!(status_of(s.confirmed(), "a"), "o_prog");
        assert!(s.pending().is_empty());
    }

    #[test]
    fn a_failed_edit_alone_leaves_the_board_as_github_has_it() {
        let (mut s, _) = layered();
        let id = s.begin(set("a", "o_done"));
        s.settle_failed(id);
        assert_eq!(status_of(s.snapshot(), "a"), "Todo");
        assert_eq!(s.settle_failed(id), None, "settling twice does nothing");
    }

    #[test]
    fn a_poll_never_overwrites_an_edit_in_flight() {
        let (mut s, _) = layered();
        s.begin(set("a", "o_done"));
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert_eq!(status_of(s.snapshot(), "a"), "o_done");
    }

    #[test]
    fn a_polled_removal_wins_over_a_recent_edit() {
        let (mut s, _) = layered();
        let id = s.begin(set("a", "o_done"));
        s.settle_ok(id, Confirmed::Applied);
        s.apply(StoreUpdate::Polled {
            upsert: vec![],
            remove: vec![ItemId::new("a")],
        });
        assert!(!s.snapshot().unwrap().items.contains_key(&ItemId::new("a")));
        assert!(!s.confirmed().unwrap().items.contains_key(&ItemId::new("a")));
    }

    /// Review Focus 1: GitHub's item index lags writes by over a minute.
    #[test]
    fn a_confirmed_edit_outranks_a_stale_poll_for_the_window() {
        let (mut s, now) = layered();
        let id = s.begin(set("a", "o_done"));
        s.settle_ok(id, Confirmed::Applied);
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert_eq!(status_of(s.confirmed(), "a"), "o_done");
        s.apply(StoreUpdate::ReplaceItems(crate::ui::fixtures::items()));
        assert_eq!(status_of(s.confirmed(), "a"), "o_done", "a full load too");
        *now.lock().unwrap() += RECENT_WINDOW + Duration::from_secs(1);
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert_eq!(
            status_of(s.confirmed(), "a"),
            "Todo",
            "after the window GitHub wins"
        );
    }

    #[test]
    fn a_returned_item_replaces_the_confirmed_one() {
        let (mut s, _) = layered();
        let id = s.begin(set("a", "o_done"));
        let mut server = stale_a();
        server
            .values
            .insert(FieldId::new("F_status"), status_value("o_done"));
        server
            .values
            .insert(FieldId::new("F_prio"), status_value("p2"));
        s.settle_ok(id, Confirmed::Item(server));
        let a = &s.confirmed().unwrap().items[&ItemId::new("a")];
        assert_eq!(
            a.value(&FieldId::new("F_prio"))
                .map(|v| v.display())
                .as_deref(),
            Some("p2")
        );
    }

    /// A poll that removed the item while its edit was in flight wins: the server's copy
    /// of the item does not bring it back.
    #[test]
    fn a_returned_item_does_not_bring_back_one_a_poll_removed() {
        let (mut s, _) = layered();
        let id = s.begin(set("a", "o_done"));
        let rest: Vec<_> = crate::ui::fixtures::items()
            .into_iter()
            .filter(|i| i.id.as_str() != "a")
            .collect();
        s.apply(StoreUpdate::ReplaceItems(rest));
        s.settle_ok(id, Confirmed::Item(stale_a()));
        assert!(!s.confirmed().unwrap().items.contains_key(&ItemId::new("a")));
        assert!(!s.snapshot().unwrap().items.contains_key(&ItemId::new("a")));
    }

    /// `Convert` replays nothing, so a stale poll inside the window that still lists the
    /// draft must not turn the converted item back into one.
    #[test]
    fn a_converted_item_stays_an_issue_through_stale_polls_for_the_window() {
        use crate::model::ItemContent;
        let (mut s, now) = layered();
        let draft = ItemId::new("d");
        let stale_draft = s.confirmed().unwrap().items[&draft].clone();
        assert!(matches!(stale_draft.content, ItemContent::Draft { .. }));
        let mut converted = issue("d", 7, "Converted");
        converted.values = stale_draft.values.clone();
        let id = s.begin(Op::Convert {
            item: draft.clone(),
            repo: crate::model::RepoRef {
                id: "R_1".into(),
                slug: "acme/app".parse().unwrap(),
            },
        });
        s.settle_ok(id, Confirmed::Item(converted));
        let is_issue = |s: &MemoryStore| {
            matches!(
                s.snapshot().unwrap().items[&draft].content,
                ItemContent::Issue { .. }
            )
        };
        assert!(is_issue(&s), "settled: the issue");
        s.apply(StoreUpdate::UpsertItems(vec![stale_draft.clone()]));
        assert!(is_issue(&s), "a stale poll lists the draft again");
        *now.lock().unwrap() += RECENT_WINDOW - Duration::from_secs(1);
        s.apply(StoreUpdate::UpsertItems(vec![stale_draft.clone()]));
        assert!(is_issue(&s), "still inside the window");
        *now.lock().unwrap() += Duration::from_secs(1);
        s.apply(StoreUpdate::UpsertItems(vec![stale_draft]));
        assert!(!is_issue(&s), "after the window the poll wins");
    }

    /// Ruling E11 (deferred T36 test): the converted issue kept for the window only replaces
    /// the item while polls list it; a poll that removed it wins.
    #[test]
    fn a_converted_item_does_not_bring_back_one_a_poll_removed() {
        let (mut s, _) = layered();
        let draft = ItemId::new("d");
        let mut converted = issue("d", 7, "Converted");
        converted.values = s.confirmed().unwrap().items[&draft].values.clone();
        let id = s.begin(Op::Convert {
            item: draft.clone(),
            repo: crate::model::RepoRef {
                id: "R_1".into(),
                slug: "acme/app".parse().unwrap(),
            },
        });
        s.settle_ok(id, Confirmed::Item(converted));
        assert!(s.snapshot().unwrap().items.contains_key(&draft));
        let rest: Vec<_> = crate::ui::fixtures::items()
            .into_iter()
            .filter(|i| i.id != draft)
            .collect();
        s.apply(StoreUpdate::ReplaceItems(rest));
        assert!(!s.confirmed().unwrap().items.contains_key(&draft));
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert!(
            !s.snapshot().unwrap().items.contains_key(&draft),
            "a later poll inside the window does not resurrect it"
        );
    }

    #[test]
    fn the_snapshot_equals_the_confirmed_board_when_nothing_is_pending() {
        let (mut s, _) = layered();
        assert_eq!(s.snapshot(), s.confirmed());
        let id = s.begin(set("a", "o_done"));
        assert_ne!(s.snapshot(), s.confirmed(), "the pending edit shows");
        s.settle_ok(id, Confirmed::Applied);
        assert_eq!(s.snapshot(), s.confirmed(), "settled: one board again");
    }

    #[test]
    fn the_window_is_exclusive_at_its_edge() {
        let (mut s, now) = layered();
        let id = s.begin(set("a", "o_done"));
        s.settle_ok(id, Confirmed::Applied);
        *now.lock().unwrap() += RECENT_WINDOW - Duration::from_secs(1);
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert_eq!(status_of(s.confirmed(), "a"), "o_done", "inside the window");
        *now.lock().unwrap() += Duration::from_secs(1);
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert_eq!(
            status_of(s.confirmed(), "a"),
            "Todo",
            "exactly RECENT_WINDOW: expired (`<`)"
        );
    }

    #[test]
    fn an_item_a_poll_removes_stays_gone_despite_pending_and_recent_edits() {
        let (mut s, _) = layered();
        let done = s.begin(set("a", "o_done"));
        s.settle_ok(done, Confirmed::Applied);
        s.begin(set("a", "o_prog"));
        let rest: Vec<_> = crate::ui::fixtures::items().into_iter().skip(1).collect();
        s.apply(StoreUpdate::ReplaceItems(rest));
        assert!(!s.confirmed().unwrap().items.contains_key(&ItemId::new("a")));
        assert!(!s.snapshot().unwrap().items.contains_key(&ItemId::new("a")));
    }

    #[test]
    fn a_recent_reorder_is_re_applied_after_a_view_list_refresh() {
        let (mut s, _) = layered();
        let view = ViewId::new("V");
        let list = ViewList {
            ids: ["a", "b", "c"].map(ItemId::new).to_vec(),
            total: 3,
            truncated: false,
        };
        s.apply(StoreUpdate::ViewIds {
            view: view.clone(),
            list: list.clone(),
        });
        let id = s.begin(Op::Reorder {
            item: ItemId::new("a"),
            after: Some(ItemId::new("c")),
        });
        s.settle_ok(id, Confirmed::Applied);
        // GitHub's list still has the old order.
        s.apply(StoreUpdate::ViewIds {
            view: view.clone(),
            list,
        });
        let ids: Vec<&str> = s.confirmed().unwrap().views[&view]
            .ids
            .iter()
            .map(|i| i.as_str())
            .collect();
        assert_eq!(ids, ["b", "c", "a"]);
    }

    #[test]
    fn a_created_item_replaces_its_placeholder_and_survives_a_stale_full_load() {
        let (mut s, now) = layered();
        let temp = ItemId::new("local:1");
        let id = s.begin(Op::Create {
            temp: temp.clone(),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![],
        });
        assert!(s.snapshot().unwrap().items.contains_key(&temp));
        let mut created = stale_a();
        created.id = ItemId::new("PVTI_new");
        let renamed = s.settle_ok(
            id,
            Confirmed::Created {
                temp: temp.clone(),
                item: created.clone(),
                refused: vec![],
            },
        );
        assert_eq!(renamed, Some((temp.clone(), ItemId::new("PVTI_new"))));
        let snap = s.snapshot().unwrap();
        assert!(!snap.items.contains_key(&temp) && snap.items.contains_key(&created.id));
        s.apply(StoreUpdate::ReplaceItems(crate::ui::fixtures::items()));
        assert!(
            s.confirmed().unwrap().items.contains_key(&created.id),
            "the index has not caught up yet"
        );
        *now.lock().unwrap() += RECENT_WINDOW + Duration::from_secs(1);
        s.apply(StoreUpdate::ReplaceItems(crate::ui::fixtures::items()));
        assert!(
            !s.confirmed().unwrap().items.contains_key(&created.id),
            "after the window GitHub decides"
        );
    }

    #[test]
    fn a_removed_item_stays_removed_through_a_stale_poll() {
        let (mut s, _) = layered();
        let id = s.begin(Op::Remove {
            item: ItemId::new("a"),
        });
        s.settle_ok(id, Confirmed::Applied);
        s.apply(StoreUpdate::UpsertItems(vec![stale_a()]));
        assert!(!s.confirmed().unwrap().items.contains_key(&ItemId::new("a")));
    }

    /// The poll's copy of a created item wins once the index has it; a later edit to the
    /// created item still replays after the stored copy.
    #[test]
    fn a_polled_created_item_wins_and_later_edits_replay_on_it() {
        let (mut s, _) = layered();
        let temp = ItemId::new("local:1");
        let id = s.begin(Op::Create {
            temp: temp.clone(),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![],
        });
        let mut created = stale_a();
        created.id = ItemId::new("PVTI_new");
        s.settle_ok(
            id,
            Confirmed::Created {
                temp,
                item: created.clone(),
                refused: vec![],
            },
        );
        let archive = s.begin(Op::Archive {
            item: created.id.clone(),
        });
        s.settle_ok(archive, Confirmed::Applied);
        let mut polled = created.clone();
        polled.updated_at = "2026-10-13T00:00:00Z".into();
        s.apply(StoreUpdate::UpsertItems(vec![polled]));
        let item = &s.confirmed().unwrap().items[&created.id];
        assert_eq!(item.updated_at, "2026-10-13T00:00:00Z", "the poll's copy");
        assert!(item.archived, "the confirmed archive still replays");
        s.apply(StoreUpdate::ReplaceItems(crate::ui::fixtures::items()));
        assert!(
            s.confirmed().unwrap().items[&created.id].archived,
            "re-added from the stored copy, then archived again"
        );
    }

    /// Controller ruling E15: nothing joins the board, and the edit leaves the pending list.
    #[test]
    fn an_issue_created_but_not_added_settles_with_nothing_on_the_board() {
        let (mut s, _) = layered();
        let before = s.confirmed().cloned();
        let id = s.begin(Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![],
        });
        let not_added = Confirmed::NotAdded {
            issue: "I_new".into(),
            error: crate::error::BackendError::Network("reset".into()),
        };
        assert_eq!(s.settle_ok(id, not_added), None);
        assert!(s.pending().is_empty());
        assert_eq!(s.confirmed().cloned(), before);
        assert_eq!(s.snapshot().cloned(), before);
    }
}
