use crate::model::Item;

/// Quick search: every whitespace-separated word must appear (case-insensitively) in the
/// title, `#number`, an assignee (`@login` or `login`) or a label.
pub fn matches(item: &Item, query: &str) -> bool {
    let mut haystack = item.title().to_lowercase();
    if let Some(n) = item.number() {
        haystack.push_str(&format!(" #{n}"));
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
}
