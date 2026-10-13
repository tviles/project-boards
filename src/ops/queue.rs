//! When each pending edit goes out: edits to one item one at a time in the order made, at
//! most `MAX_IN_FLIGHT` across items (GitHub's secondary rate limit on mutations), and none
//! while a secondary rate limit pauses writes (spec section 7).

use crate::model::ItemId;
use crate::ops::OpId;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

pub const MAX_IN_FLIGHT: usize = 4;

#[derive(Debug, Default)]
pub struct OpQueue {
    waiting: VecDeque<(OpId, ItemId)>,
    running: Vec<(OpId, ItemId)>,
    paused_until: Option<Instant>,
}

impl OpQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, id: OpId, item: ItemId) {
        self.waiting.push_back((id, item));
    }

    /// The ops to send now, marked running: the first waiting op of every item that has none
    /// running, oldest first, until `MAX_IN_FLIGHT` run. Nothing while writes are paused.
    pub fn ready(&mut self, now: Instant) -> Vec<OpId> {
        if self.paused_until(now).is_some() {
            return Vec::new();
        }
        self.paused_until = None;
        let mut busy: HashSet<ItemId> = self.running.iter().map(|(_, i)| i.clone()).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.waiting.len() && self.running.len() < MAX_IN_FLIGHT {
            if busy.contains(&self.waiting[i].1) {
                i += 1;
                continue;
            }
            let (id, item) = self.waiting.remove(i).expect("index in range");
            busy.insert(item.clone());
            self.running.push((id, item));
            out.push(id);
        }
        out
    }

    /// Op `id` finished, whether it succeeded or failed.
    pub fn finished(&mut self, id: OpId) {
        self.running.retain(|(r, _)| *r != id);
    }

    /// Op `id` hit a secondary rate limit: it goes back to the front, and nothing is sent
    /// before `until`.
    pub fn retry_after(&mut self, id: OpId, until: Instant) {
        if let Some(pos) = self.running.iter().position(|(r, _)| *r == id) {
            let entry = self.running.remove(pos);
            self.waiting.push_front(entry);
        }
        self.paused_until = Some(self.paused_until.map_or(until, |p| p.max(until)));
    }

    /// When writes resume, while they are paused.
    pub fn paused_until(&self, now: Instant) -> Option<Instant> {
        self.paused_until.filter(|until| now < *until)
    }

    pub fn is_idle(&self) -> bool {
        self.waiting.is_empty() && self.running.is_empty()
    }

    /// Forgets every op (another board opened).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(s: &str) -> ItemId {
        ItemId::new(s)
    }

    #[test]
    fn ops_on_one_item_go_one_at_a_time_in_order() {
        let now = Instant::now();
        let mut q = OpQueue::new();
        q.push(1, item("a"));
        q.push(2, item("a"));
        q.push(3, item("a"));
        assert_eq!(q.ready(now), [1]);
        assert!(q.ready(now).is_empty(), "the second waits for the first");
        q.finished(1);
        assert_eq!(q.ready(now), [2]);
        q.finished(2);
        assert_eq!(q.ready(now), [3]);
    }

    #[test]
    fn different_items_run_together_up_to_the_limit() {
        let now = Instant::now();
        let mut q = OpQueue::new();
        for (id, i) in [(1, "a"), (2, "b"), (3, "c"), (4, "d"), (5, "e"), (6, "a")] {
            q.push(id, item(i));
        }
        assert_eq!(q.ready(now), [1, 2, 3, 4]);
        assert_eq!(MAX_IN_FLIGHT, 4);
        q.finished(2);
        assert_eq!(q.ready(now), [5], "6 is on a's queue behind 1");
        q.finished(1);
        assert_eq!(q.ready(now), [6]);
    }

    #[test]
    fn a_failed_op_still_releases_its_item() {
        let now = Instant::now();
        let mut q = OpQueue::new();
        q.push(1, item("a"));
        q.push(2, item("a"));
        q.ready(now);
        q.finished(1); // failed or not, the next edit to the item still goes out
        assert_eq!(q.ready(now), [2]);
    }

    #[test]
    fn a_rate_limited_op_goes_back_first_and_writes_pause() {
        let now = Instant::now();
        let mut q = OpQueue::new();
        q.push(1, item("a"));
        q.push(2, item("a"));
        q.push(3, item("b"));
        assert_eq!(q.ready(now), [1, 3]);
        let until = now + Duration::from_secs(30);
        q.retry_after(1, until);
        q.finished(3);
        assert_eq!(q.paused_until(now), Some(until));
        assert!(q.ready(now + Duration::from_secs(29)).is_empty());
        assert_eq!(q.ready(until), [1], "1 before 2");
        assert_eq!(q.paused_until(until), None);
    }

    #[test]
    fn clear_forgets_everything() {
        let now = Instant::now();
        let mut q = OpQueue::new();
        q.push(1, item("a"));
        q.ready(now);
        q.push(2, item("b"));
        assert!(!q.is_idle());
        q.clear();
        assert!(q.is_idle());
        assert!(q.ready(now).is_empty());
    }
}
