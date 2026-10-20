//! What the table and the board mark on an item: edits still sending, a sticky item the view
//! no longer matches, a new item, and (on a Linear board) the state a card's dot shows.

use crate::model::ItemId;
use std::collections::HashSet;

pub const PENDING_MARK: &str = "⋯";
pub const UNMATCHED_MARK: &str = "⊘";
pub const UNMATCHED_TEXT: &str = "⊘ no longer matches filter";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemMarks {
    /// Items with edits still sending.
    pub pending: HashSet<ItemId>,
    /// Sticky items the view's filter no longer matches.
    pub unmatched: HashSet<ItemId>,
    /// A new item, highlighted until the next key press.
    pub highlight: Option<ItemId>,
    /// A Linear board's Status field: a keyed card's dot takes its state's colour even when
    /// the board's columns are another field. `None` on GitHub.
    pub status_field: Option<crate::model::Field>,
}

impl ItemMarks {
    /// What goes before an item's title or reference: `⋯ ` while edits are sending, `⊘ `
    /// when it no longer matches the view.
    pub fn prefix(&self, id: &ItemId) -> String {
        let mut out = String::new();
        if self.pending.contains(id) {
            out.push_str(PENDING_MARK);
            out.push(' ');
        }
        if self.unmatched.contains(id) {
            out.push_str(UNMATCHED_MARK);
            out.push(' ');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_lists_every_mark() {
        let mut m = ItemMarks::default();
        let a = ItemId::new("a");
        assert_eq!(m.prefix(&a), "");
        m.pending.insert(a.clone());
        m.unmatched.insert(a.clone());
        assert_eq!(m.prefix(&a), "⋯ ⊘ ");
    }
}
