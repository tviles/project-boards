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

/// `s` word-wrapped into at most `max_lines` lines of at most `width` cells each. Words break
/// at whitespace; a word longer than a line is broken by width. When the text needs more
/// lines, the last one is cut with `…`. Control characters become spaces. Never splits a
/// grapheme cluster. At least one line (empty when `s` has no words) unless `max_lines` is 0.
pub fn wrap_to_width(s: &str, width: usize, max_lines: usize) -> Vec<String> {
    let clean = sanitize(s);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in clean.split_whitespace() {
        let w = display_width(word);
        if !line.is_empty() && used + 1 + w <= width {
            line.push(' ');
            line.push_str(word);
            used += 1 + w;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if w <= width {
            line.push_str(word);
            used = w;
            continue;
        }
        for g in word.graphemes(true) {
            let gw = display_width(g);
            if gw > width {
                // Wider than the whole line: it can never be drawn.
                continue;
            }
            if used + gw > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(g);
            used += gw;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    if lines.len() > max_lines {
        // Each line ends where the next word did not fit, so joining the next line on always
        // overflows and the cut ends with `…`.
        let next = lines[max_lines].clone();
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = truncate_to_width(&format!("{last} {next}"), width);
        }
    }
    lines
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
    fn wrap_breaks_between_words_within_width() {
        assert_eq!(
            wrap_to_width("Fix the crash on load", 9, 3),
            ["Fix the", "crash on", "load"]
        );
        assert_eq!(wrap_to_width("short", 9, 3), ["short"]);
        assert_eq!(wrap_to_width("", 9, 3), [""]);
        assert_eq!(wrap_to_width("  spaced \n out  ", 20, 3), ["spaced out"]);
    }

    #[test]
    fn wrap_cuts_the_last_line_with_an_ellipsis_when_out_of_lines() {
        let lines = wrap_to_width("one two three four five six seven", 9, 3);
        assert_eq!(lines, ["one two", "three", "four fiv…"]);
        assert_eq!(wrap_to_width("aaa bbb ccc", 3, 2), ["aaa", "bb…"]);
    }

    #[test]
    fn wrap_breaks_a_word_longer_than_the_line_by_width() {
        assert_eq!(wrap_to_width("abcdefghij", 4, 3), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_to_width("ab abcdefghij", 4, 3), ["ab", "abcd", "efg…"]);
        assert_eq!(wrap_to_width("abcdef xy", 4, 3), ["abcd", "ef", "xy"]);
    }

    #[test]
    fn wrap_counts_wide_characters_by_display_width() {
        assert_eq!(wrap_to_width("中文字中文", 5, 3), ["中文", "字中", "文"]);
        assert_eq!(wrap_to_width("🚀 中文 x", 4, 3), ["🚀", "中文", "x"]);
        for text in [
            "Emoji 🚀 title 中文 that is quite long",
            "👨‍👩‍👧 family e\u{301}clair",
        ] {
            for width in 2..12 {
                for line in wrap_to_width(text, width, 3) {
                    assert!(
                        display_width(&line) <= width,
                        "{text:?} at {width}: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn wrap_keeps_text_that_fits_exactly() {
        assert_eq!(wrap_to_width("ab cd", 5, 3), ["ab cd"]);
        assert_eq!(wrap_to_width("abcde", 5, 1), ["abcde"]);
        // Exactly three lines' worth: no ellipsis.
        assert_eq!(wrap_to_width("aaa bbb ccc", 3, 3), ["aaa", "bbb", "ccc"]);
        assert_eq!(wrap_to_width("abcdefghi", 3, 3), ["abc", "def", "ghi"]);
    }

    #[test]
    fn wrap_turns_control_characters_into_breaks() {
        assert_eq!(wrap_to_width("a\nb\tc", 10, 3), ["a b c"]);
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
