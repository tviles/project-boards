use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Width in terminal cells, counting control characters as one cell (they render as spaces).
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            if c.is_control() {
                1
            } else {
                c.width().unwrap_or(0)
            }
        })
        .sum()
}

/// At most `width` cells. Control characters become spaces; a cut string ends with `…`.
/// Never splits a character, so wide characters (emoji, CJK) are dropped whole.
pub fn truncate_to_width(s: &str, width: usize) -> String {
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.width() <= width {
        return clean;
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in clean.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Exactly `width` cells: truncated, then padded with spaces.
pub fn pad_to_width(s: &str, width: usize) -> String {
    let t = truncate_to_width(s, width);
    let w = t.width();
    format!("{t}{}", " ".repeat(width.saturating_sub(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_strings_are_untouched() {
        assert_eq!(truncate_to_width("Fix crash", 20), "Fix crash");
        assert_eq!(truncate_to_width("exact", 5), "exact");
    }

    #[test]
    fn long_strings_end_with_an_ellipsis_within_width() {
        let t = truncate_to_width("Support iteration columns", 10);
        assert_eq!(t, "Support i…");
        assert_eq!(t.width(), 10);
    }

    #[test]
    fn wide_characters_are_never_split_or_overflow() {
        for width in 0..12 {
            let t = truncate_to_width("Emoji 🚀 中文字", width);
            assert!(
                t.width() <= width,
                "width {width}: {t:?} is {} cells",
                t.width()
            );
        }
        assert_eq!(truncate_to_width("🚀🚀🚀", 4), "🚀…");
        assert_eq!(truncate_to_width("中文字", 5), "中文…");
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(truncate_to_width("a\nb\tc", 10), "a b c");
    }

    #[test]
    fn pad_fills_to_exact_width() {
        assert_eq!(pad_to_width("ab", 4), "ab  ");
        assert_eq!(pad_to_width("中文字", 5).width(), 5);
    }
}
