//! Labels drawn as GitHub-coloured pills, shared by the table, the board and the detail view.

use crate::model::Label;
use crate::ui::text::{display_width, sanitize, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::text::Span;

/// A pill's text: the sanitised name with one space of padding each side.
fn pill_text(name: &str) -> String {
    format!(" {name} ")
}

/// `labels` as spans no wider than `width` cells.
///
/// With colour, each label is a ` name ` pill in its own colours, pills separated by one
/// unstyled space. As many whole pills as fit are shown, then a dim `+N` for the rest when it
/// fits; if not even the first pill fits, its name is cut with `…` inside the pill. Without
/// colour the names are plain text joined by ", " and cut with `…`.
pub fn label_spans(labels: &[&Label], width: usize, theme: &Theme) -> Vec<Span<'static>> {
    if labels.is_empty() || width == 0 {
        return Vec::new();
    }
    if !theme.color {
        let names: Vec<&str> = labels.iter().map(|l| l.name.as_str()).collect();
        return vec![Span::raw(truncate_to_width(&names.join(", "), width))];
    }
    let pills: Vec<(String, &Label)> = labels
        .iter()
        .map(|l| (pill_text(&sanitize(&l.name)), *l))
        .collect();
    let widths: Vec<usize> = pills.iter().map(|(t, _)| display_width(t)).collect();
    // Cells used by the first `k` pills with a separator between them.
    let used = |k: usize| widths[..k].iter().sum::<usize>() + k.saturating_sub(1);
    let fitting = (0..=pills.len()).take_while(|&k| used(k) <= width).last();
    let Some(mut shown) = fitting.filter(|&k| k > 0) else {
        let (_, label) = &pills[0];
        let Some(room) = width.checked_sub(2).filter(|&r| r > 0) else {
            return Vec::new();
        };
        let name = truncate_to_width(&label.name, room);
        return vec![Span::styled(pill_text(&name), theme.label(&label.color))];
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
    let mut spans = Vec::new();
    for (i, (text, label)) in pills[..shown].iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(text.clone(), theme.label(&label.color)));
    }
    if let Some(plus) = more {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(plus, theme.dim()));
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
        let spans = label_spans(&refs, width, theme);
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
    fn pills_are_padded_and_separated_by_one_space() {
        assert_eq!(draw(&two(), 10, &colour()), " bug   ui ");
    }

    #[test]
    fn overflow_ends_with_the_number_left_out() {
        let three = vec![
            label("bug", "d73a4a"),
            label("ui", "fff"),
            label("docs", "000"),
        ];
        assert_eq!(draw(&three, 9, &colour()), " bug  +2");
        assert_eq!(draw(&three, 14, &colour()), " bug   ui  +1");
        // "+2" would not fit after the first pill, so no counter, but the pill still shows.
        assert_eq!(draw(&three, 6, &colour()), " bug ");
    }

    #[test]
    fn counter_gives_up_pills_to_fit() {
        let three = vec![label("a", "fff"), label("b", "fff"), label("c", "fff")];
        // Two pills fit in 7 cells, but not with " +1"; one pill and " +2" do.
        assert_eq!(draw(&three, 7, &colour()), " a  +2");
    }

    #[test]
    fn a_first_pill_that_does_not_fit_is_cut_inside_the_pill() {
        let long = vec![label("enhancement", "a2eeef")];
        assert_eq!(draw(&long, 7, &colour()), " enha… ");
        assert_eq!(draw(&long, 3, &colour()), " … ");
        assert_eq!(draw(&long, 2, &colour()), "");
        assert_eq!(draw(&long, 0, &colour()), "");
    }

    #[test]
    fn wide_characters_count_by_display_width() {
        let wide = vec![label("中文", "fff"), label("🚀🚀", "fff")];
        // " 中文 " is 6 cells, " 🚀🚀 " is 6: both fit in 13.
        assert_eq!(draw(&wide, 13, &colour()), " 中文   🚀🚀 ");
        assert_eq!(draw(&wide, 9, &colour()), " 中文  +1");
        assert_eq!(draw(&[label("中文字", "fff")], 6, &colour()), " 中… ");
    }

    #[test]
    fn control_characters_in_names_are_neutralised() {
        assert_eq!(draw(&[label("a\nb", "fff")], 10, &colour()), " a b ");
    }

    #[test]
    fn without_colour_labels_are_names_joined_by_commas() {
        let plain = Theme::plain();
        assert_eq!(draw(&two(), 20, &plain), "bug, ui");
        assert_eq!(draw(&two(), 5, &plain), "bug,…");
        let spans = label_spans(&two().iter().collect::<Vec<_>>(), 20, &plain);
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
                ));
                f.render_widget(Paragraph::new(line), f.area());
            })
            .unwrap()
            .buffer
            .clone();
        // " bug " covers x 0..5; x 5 is the separator; " ui " covers x 6..10.
        let red = &buf[(1, 0)];
        assert_eq!(red.symbol(), "b");
        assert_eq!(red.bg, Color::Rgb(0xd7, 0x3a, 0x4a));
        assert_eq!(red.fg, Color::White);
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(0xd7, 0x3a, 0x4a));
        assert_eq!(buf[(5, 0)].bg, Color::Reset);
        let cream = &buf[(7, 0)];
        assert_eq!(cream.symbol(), "u");
        assert_eq!(cream.bg, Color::Rgb(0xfe, 0xf2, 0xc0));
        assert_eq!(cream.fg, Color::Black);
    }
}
