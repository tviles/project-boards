use crate::model::Item;

/// Quick search: every whitespace-separated word must appear (case-insensitively) in the
/// title, `#number` (`new` for an item still being created), an assignee (`@login` or
/// `login`) or a label.
pub fn matches(item: &Item, query: &str) -> bool {
    let mut haystack = item.title().to_lowercase();
    if item.number().is_some() || item.is_placeholder() {
        haystack.push(' ');
        haystack.push_str(&item.ref_label().to_lowercase());
    }
    for a in item.assignees() {
        haystack.push_str(&format!(" @{}", a.to_lowercase()));
    }
    for l in item.label_names() {
        haystack.push(' ');
        haystack.push_str(&l.to_lowercase());
    }
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::items;

    #[test]
    fn matches_title_number_assignee_and_label() {
        let all = items();
        assert!(matches(&all[0], "crash"));
        assert!(matches(&all[0], "#1"));
        assert!(matches(&all[0], "@tviles bug"));
        assert!(matches(&all[0], "TVILES"));
        assert!(!matches(&all[0], "crash enhancement"));
        assert!(matches(&all[2], "🚀"));
        assert!(matches(&all[3], ""), "empty query matches everything");
    }

    #[test]
    fn an_item_still_being_created_matches_new() {
        let mut item = items()[1].clone();
        assert!(!matches(&item, "new"));
        item.id = crate::model::ItemId::new("local:1");
        if let crate::model::ItemContent::Issue { reference, .. } = &mut item.content {
            reference.number = 0;
        }
        assert!(matches(&item, "new"));
        assert!(!matches(&item, "#0"));
    }
}
