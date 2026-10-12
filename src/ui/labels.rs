//! Labels drawn as GitHub-coloured pills, shared by the table, the board and the detail view.

use crate::model::Label;
use crate::ui::text::{display_width, sanitize, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::style::Style;
use ratatui::text::Span;

/// `labels` as spans no wider than `width` cells.
///
/// With colour, each label is its name on its own colours, pills separated by one space. As
/// many whole pills as fit are shown, then `+N` for the rest when it fits; if not even the
/// first pill fits, its name is cut with `…`. On a `selected` line the gaps and `+N` take the
/// selected style, so the line reads as one bar with the pills in it; otherwise `+N` is dim.
/// Without colour the names are plain text joined by ", " and cut with `…`.
pub fn label_spans(
    labels: &[&Label],
    width: usize,
    theme: &Theme,
    selected: bool,
) -> Vec<Span<'static>> {
    if labels.is_empty() || width == 0 {
        return Vec::new();
    }
    if !theme.color {
        let names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
        return vec![Span::raw(truncate_to_width(&names.join(", "), width))];
    }
    let pills: Vec<(String, &Label)> = labels.iter().map(|l| (sanitize(&l.name), *l)).collect();
    let widths: Vec<usize> = pills.iter().map(|(t, _)| display_width(t)).collect();
    // Cells used by the first `k` pills with a separator between them.
    let used = |k: usize| widths[..k].iter().sum::<usize>() + k.saturating_sub(1);
    let fitting = (0..=pills.len()).take_while(|&k| used(k) <= width).last();
    let Some(mut shown) = fitting.filter(|&k| k > 0) else {
        let (name, label) = &pills[0];
        let name = truncate_to_width(name, width);
        return vec![Span::styled(name, theme.label(&label.color))];
    };
    let mut more = None;
    if shown < pills.len() {
        let plus = |k: usize| format!("+{}", pills.len() - k);
        let fits = |k: usize| used(k) + 1 + display_width(&plus(k)) <= width;
        if let Some(k) = (1..=shown).rev().find(|&k| fits(k)) {
            shown = k;
            more = Some(plus(k));
        }
    }
    let (gap, counter) = if selected {
        (theme.selected(), theme.selected())
    } else {
        (Style::default(), theme.dim())
    };
    let mut spans = Vec::new();
    for (i, (text, label)) in pills[..shown].iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ", gap));
        }
        spans.push(Span::styled(text.clone(), theme.label(&label.color)));
    }
    if let Some(plus) = more {
        spans.push(Span::styled(" ", gap));
        spans.push(Span::styled(plus, counter));
    }
    spans
}

/// Total display width of `spans`.
pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::render_to_string;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    fn label(name: &str, color: &str) -> Label {
        Label {
            name: name.into(),
            color: color.into(),
        }
    }

    fn colour() -> Theme {
        Theme {
            color: true,
            truecolor: true,
        }
    }

    fn draw(labels: &[Label], width: usize, theme: &Theme) -> String {
        let refs: Vec<&Label> = labels.iter().collect();
        let spans = label_spans(&refs, width, theme, false);
        assert!(spans_width(&spans) <= width, "{spans:?} wider than {width}");
        // Drawn through the buffer too: nothing may spill past `width` cells.
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let screen = render_to_string(20, 1, |f| {
            f.render_widget(Paragraph::new(Line::from(spans)), f.area())
        });
        assert_eq!(screen, text.trim_end());
        text
    }

    fn two() -> Vec<Label> {
        vec![label("bug", "d73a4a"), label("ui", "fef2c0")]
    }

    #[test]
    fn pills_are_bare_names_separated_by_one_space() {
        assert_eq!(draw(&two(), 10, &colour()), "bug ui");
    }

    fn three() -> Vec<Label> {
        vec![
            label("bug", "d73a4a"),
            label("ui", "fff"),
            label("docs", "000"),
        ]
    }

    #[test]
    fn overflow_ends_with_the_number_left_out() {
        assert_eq!(draw(&three(), 6, &colour()), "bug +2");
        assert_eq!(draw(&three(), 9, &colour()), "bug ui +1");
        assert_eq!(draw(&three(), 11, &colour()), "bug ui docs");
        // "+2" would not fit after the first pill, so no counter, but the pill still shows.
        assert_eq!(draw(&three(), 4, &colour()), "bug");
    }

    #[test]
    fn counter_gives_up_pills_to_fit() {
        let three = vec![label("a", "fff"), label("b", "fff"), label("c", "fff")];
        // Two pills fit in 4 cells, but not with " +1"; one pill and " +2" do.
        assert_eq!(draw(&three, 4, &colour()), "a +2");
    }

    #[test]
    fn a_first_pill_that_does_not_fit_is_cut_inside_the_pill() {
        let long = vec![label("enhancement", "a2eeef")];
        assert_eq!(draw(&long, 7, &colour()), "enhanc…");
        assert_eq!(draw(&long, 1, &colour()), "…");
        assert_eq!(draw(&long, 0, &colour()), "");
    }

    #[test]
    fn wide_characters_count_by_display_width() {
        let wide = vec![label("中文", "fff"), label("🚀🚀", "fff")];
        // "中文" is 4 cells, "🚀🚀" is 4: both fit in 9.
        assert_eq!(draw(&wide, 9, &colour()), "中文 🚀🚀");
        assert_eq!(draw(&wide, 8, &colour()), "中文 +1");
        assert_eq!(draw(&[label("中文字", "fff")], 4, &colour()), "中…");
    }

    #[test]
    fn control_characters_in_names_are_neutralised() {
        assert_eq!(draw(&[label("a\nb", "fff")], 10, &colour()), "a b");
        assert_eq!(draw(&[label("a\nbcdef", "fff")], 4, &colour()), "a b…");
    }

    #[test]
    fn on_a_selected_line_the_gaps_and_counter_take_the_selected_style() {
        let t = colour();
        let labels = three();
        let refs: Vec<&Label> = labels.iter().collect();
        let styles = |selected: bool| -> Vec<(String, Style)> {
            label_spans(&refs, 9, &t, selected)
                .into_iter()
                .map(|s| (s.content.to_string(), s.style))
                .collect()
        };
        let sel = styles(true);
        assert_eq!(sel[1], (" ".into(), t.selected()));
        assert_eq!(sel[3], (" ".into(), t.selected()));
        assert_eq!(sel[4], ("+1".into(), t.selected()));
        assert_eq!(sel[0].1, t.label("d73a4a"), "pills keep their colours");
        let plain = styles(false);
        assert_eq!(plain[1], (" ".into(), Style::default()));
        assert_eq!(plain[4], ("+1".into(), t.dim()), "the counter stays dim");
    }

    #[test]
    fn without_colour_labels_are_names_joined_by_commas() {
        let plain = Theme::plain();
        assert_eq!(draw(&two(), 20, &plain), "bug, ui");
        assert_eq!(draw(&two(), 5, &plain), "bug,…");
        let spans = label_spans(&two().iter().collect::<Vec<_>>(), 20, &plain, false);
        assert!(spans.iter().all(|s| s.style == Default::default()));
    }

    #[test]
    fn pill_cells_carry_the_label_colours() {
        let mut terminal = Terminal::new(TestBackend::new(12, 1)).unwrap();
        let buf = terminal
            .draw(|f| {
                let line = Line::from(label_spans(
                    &two().iter().collect::<Vec<_>>(),
                    12,
                    &colour(),
                    false,
                ));
                f.render_widget(Paragraph::new(line), f.area());
            })
            .unwrap()
            .buffer
            .clone();
        // "bug" covers x 0..3; x 3 is the separator; "ui" covers x 4..6.
        let red = &buf[(0, 0)];
        assert_eq!(red.symbol(), "b");
        assert_eq!(red.bg, Color::Rgb(0xd7, 0x3a, 0x4a));
        assert_eq!(red.fg, Color::White);
        assert_eq!(buf[(2, 0)].bg, Color::Rgb(0xd7, 0x3a, 0x4a));
        assert_eq!(buf[(3, 0)].bg, Color::Reset);
        let cream = &buf[(4, 0)];
        assert_eq!(cream.symbol(), "u");
        assert_eq!(cream.bg, Color::Rgb(0xfe, 0xf2, 0xc0));
        assert_eq!(cream.fg, Color::Black);
    }
}
