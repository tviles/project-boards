use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One grapheme as it is drawn: control characters become a single space; everything else
/// keeps its text and its string width (so VS16 and ZWJ sequences agree with the terminal).
fn cell(g: &str) -> (&str, usize) {
    if g.chars().next().is_some_and(char::is_control) {
        (" ", 1)
    } else {
        (g, UnicodeWidthStr::width(g))
    }
}

/// `s` with control characters replaced by spaces.
pub fn sanitize(s: &str) -> String {
    s.graphemes(true).map(|g| cell(g).0).collect()
}

/// Width in terminal cells, counting control characters as one cell (they render as spaces).
pub fn display_width(s: &str) -> usize {
    s.graphemes(true).map(|g| cell(g).1).sum()
}

/// At most `width` cells. Control characters become spaces; a cut string ends with `…`.
/// Never splits a grapheme cluster, so wide characters (emoji, CJK) are dropped whole.
pub fn truncate_to_width(s: &str, width: usize) -> String {
    let cells: Vec<(&str, usize)> = s.graphemes(true).map(cell).collect();
    if cells.iter().map(|c| c.1).sum::<usize>() <= width {
        return cells.iter().map(|c| c.0).collect();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for (g, w) in cells {
        if used + w + 1 > width {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// Exactly `width` cells: truncated, then padded with spaces.
pub fn pad_to_width(s: &str, width: usize) -> String {
    let t = truncate_to_width(s, width);
    let w = display_width(&t);
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

    #[test]
    fn grapheme_clusters_stay_whole_and_within_width() {
        for text in ["❤️ love", "👨‍👩‍👧 family", "e\u{301}clair"] {
            for width in 0..12 {
                let t = truncate_to_width(text, width);
                assert!(display_width(&t) <= width, "{text:?} at {width}: {t:?}");
                assert!(
                    !t.ends_with('\u{200d}') && !t.ends_with('\u{fe0f}'),
                    "{t:?}"
                );
                assert_eq!(
                    display_width(&pad_to_width(text, width)),
                    width,
                    "{text:?} at {width}"
                );
            }
        }
    }
}
