//! The checkbox popover for assignees, labels and multi-select fields: type to filter,
//! ↑/↓ move, Space toggles, Enter applies (only what changed), Esc cancels.

use crate::ui::popup::{centered, fuzzy_match, plain_char, pop_grapheme, tail_to_width};
use crate::ui::text::{display_width, sanitize, truncate_to_width};
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Debug, Clone, PartialEq)]
pub struct MultiOption<T> {
    pub label: String,
    /// A GitHub label colour (hex), drawn as a pill.
    pub color: Option<String>,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MultiSelect<T> {
    pub title: String,
    pub options: Vec<MultiOption<T>>,
    pub checked: Vec<bool>,
    initial: Vec<bool>,
    pub input: String,
    /// Index into `visible()`.
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
    /// The candidates are only the first page of what the repository has.
    pub truncated: bool,
    /// How many candidates a page holds, said when `truncated`.
    pub page: usize,
    /// At most one option is checked: ticking one unticks the others.
    single: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MultiOutcome<T> {
    None,
    /// Enter: what was checked and unchecked, and everything checked now.
    Apply {
        added: Vec<T>,
        removed: Vec<T>,
        chosen: Vec<T>,
    },
    Cancel,
}

impl<T: Clone> MultiSelect<T> {
    /// The options in the order given, checked ones first.
    pub fn new(
        title: impl Into<String>,
        options: Vec<MultiOption<T>>,
        is_checked: impl Fn(&T) -> bool,
    ) -> Self {
        let (mut on, off): (Vec<_>, Vec<_>) =
            options.into_iter().partition(|o| is_checked(&o.value));
        let n = on.len();
        on.extend(off);
        let checked: Vec<bool> = (0..on.len()).map(|i| i < n).collect();
        Self {
            title: title.into(),
            options: on,
            initial: checked.clone(),
            checked,
            input: String::new(),
            selected: 0,
            loading: false,
            error: None,
            truncated: false,
            page: 100,
            single: false,
        }
    }

    /// The same checklist with at most one option checked.
    pub fn single(mut self) -> Self {
        self.single = true;
        self
    }

    pub fn loading(title: impl Into<String>) -> Self {
        let mut m = Self::new(title, Vec::new(), |_| false);
        m.loading = true;
        m
    }

    /// The options arrived (or were fetched again): what was typed stays, and so do the
    /// user's choices. Options are matched to the old ones by label (the only bound `T` has);
    /// an option seen before keeps its checked state and its original (item) state, so the
    /// applied difference stays against the item's real values. New options start as
    /// `is_checked` says. The highlight stays on the same option when it is still there.
    pub fn set_options(&mut self, options: Vec<MultiOption<T>>, is_checked: impl Fn(&T) -> bool) {
        let highlighted = self
            .visible()
            .get(self.selected)
            .map(|&i| self.options[i].label.clone());
        let mut old: Vec<Option<(String, bool, bool)>> = std::mem::take(&mut self.options)
            .into_iter()
            .zip(self.checked.iter().zip(&self.initial))
            .map(|(o, (&c, &i))| Some((o.label, c, i)))
            .collect();
        // (option, checked now, checked originally)
        let mut rows: Vec<(MultiOption<T>, bool, bool)> = options
            .into_iter()
            .map(|o| {
                let seen = old
                    .iter_mut()
                    .find(|e| e.as_ref().is_some_and(|(l, _, _)| *l == o.label))
                    .and_then(Option::take);
                match seen {
                    Some((_, now, was)) => (o, now, was),
                    None => {
                        let on = is_checked(&o.value);
                        (o, on, on)
                    }
                }
            })
            .collect();
        rows.sort_by_key(|r| !r.2);
        self.options = rows.iter().map(|r| r.0.clone()).collect();
        self.checked = rows.iter().map(|r| r.1).collect();
        self.initial = rows.iter().map(|r| r.2).collect();
        self.loading = false;
        self.error = None;
        self.selected = highlighted
            .and_then(|l| {
                self.visible()
                    .iter()
                    .position(|&i| self.options[i].label == l)
            })
            .unwrap_or(0);
        self.clamp();
    }

    /// Indexes of the options the typed text matches.
    pub fn visible(&self) -> Vec<usize> {
        (0..self.options.len())
            .filter(|&i| fuzzy_match(&self.input, &self.options[i].label))
            .collect()
    }

    pub fn handle(&mut self, key: &KeyEvent) -> MultiOutcome<T> {
        match key.code {
            KeyCode::Esc => return MultiOutcome::Cancel,
            KeyCode::Enter if !self.loading => return self.apply(),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected += 1,
            KeyCode::Backspace => {
                if !self.input.is_empty() {
                    pop_grapheme(&mut self.input);
                    self.selected = 0;
                }
            }
            _ => match plain_char(key) {
                Some(' ') => {
                    if let Some(&i) = self.visible().get(self.selected) {
                        let on = !self.checked[i];
                        if on && self.single {
                            self.checked.fill(false);
                        }
                        self.checked[i] = on;
                    }
                }
                Some(c) => {
                    self.input.push(c);
                    self.selected = 0;
                }
                None => {}
            },
        }
        self.clamp();
        MultiOutcome::None
    }

    fn apply(&self) -> MultiOutcome<T> {
        let pick = |f: &dyn Fn(usize) -> bool| -> Vec<T> {
            (0..self.options.len())
                .filter(|&i| f(i))
                .map(|i| self.options[i].value.clone())
                .collect()
        };
        MultiOutcome::Apply {
            added: pick(&|i| self.checked[i] && !self.initial[i]),
            removed: pick(&|i| !self.checked[i] && self.initial[i]),
            chosen: pick(&|i| self.checked[i]),
        }
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }
}

pub fn render_multiselect<T: Clone>(
    frame: &mut Frame,
    area: Rect,
    m: &MultiSelect<T>,
    theme: &Theme,
) {
    let visible = m.visible();
    let status = m.error.is_some() || m.loading || visible.is_empty() || m.truncated;
    // Input line, status line, the options, the hint line, and the border.
    let height = 3 + u16::from(status) + visible.len().min(15) as u16 + 1;
    let rect = centered(area, area.width.saturating_sub(4).min(64), height);
    let inner = rect.width.saturating_sub(2) as usize;
    let mut lines = vec![Line::styled(
        format!("> {}", tail_to_width(&m.input, inner.saturating_sub(2))),
        theme.accent(),
    )];
    if let Some(e) = &m.error {
        lines.push(Line::styled(truncate_to_width(e, inner), theme.error()));
    } else if m.loading {
        lines.push(Line::styled("Loading…", theme.dim()));
    } else if visible.is_empty() {
        lines.push(Line::styled("No matches", theme.dim()));
    } else if m.truncated {
        lines.push(Line::styled(
            truncate_to_width(
                &format!("showing the first {} — type to narrow", m.page),
                inner,
            ),
            theme.dim(),
        ));
    }
    let room = (rect.height as usize).saturating_sub(3 + lines.len());
    let start = m.selected.saturating_sub(room.saturating_sub(1));
    for (row, &i) in visible.iter().enumerate().skip(start).take(room) {
        let option = &m.options[i];
        let style = if row == m.selected {
            theme.selected()
        } else {
            Style::default()
        };
        let mark = if m.checked[i] { "[x] " } else { "[ ] " };
        let label = truncate_to_width(&sanitize(&option.label), inner.saturating_sub(4));
        let label_style = match &option.color {
            Some(hex) if theme.color => theme.label(hex).patch(style),
            _ => style,
        };
        let pad = inner.saturating_sub(4 + display_width(&label));
        lines.push(Line::from(vec![
            Span::styled(mark, style),
            Span::styled(label, label_style),
            Span::styled(" ".repeat(pad), style),
        ]));
    }
    lines.push(Line::styled(
        truncate_to_width("space toggles · enter applies · esc cancels", inner),
        theme.dim(),
    ));
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
            " {} ",
            truncate_to_width(&m.title, rect.width.saturating_sub(4) as usize)
        ))),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{code, key, render_to_string};

    fn labels() -> Vec<MultiOption<&'static str>> {
        ["docs", "Good First Issue", "🐛 bug"]
            .into_iter()
            .map(|l| MultiOption {
                label: l.to_string(),
                color: Some("0075ca".into()),
                value: l,
            })
            .collect()
    }

    #[test]
    fn prechecked_first_and_apply_sends_only_the_difference() {
        let mut m = MultiSelect::new("Labels", labels(), |v| *v == "🐛 bug");
        assert_eq!(m.options[0].value, "🐛 bug");
        assert_eq!(m.checked, [true, false, false]);
        m.handle(&key(' ')); // uncheck bug
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' ')); // check docs
        match m.handle(&code(KeyCode::Enter)) {
            MultiOutcome::Apply {
                added,
                removed,
                chosen,
            } => {
                assert_eq!(
                    (added, removed, chosen),
                    (vec!["docs"], vec!["🐛 bug"], vec!["docs"])
                );
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    #[test]
    fn filter_is_case_insensitive_and_space_toggles() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        for c in "good".chars() {
            m.handle(&key(c));
        }
        assert_eq!(m.visible().len(), 1);
        m.handle(&key(' '));
        assert_eq!(m.input, "good", "space toggled, it did not type");
        let MultiOutcome::Apply { added, .. } = m.handle(&code(KeyCode::Enter)) else {
            panic!("expected Apply")
        };
        assert_eq!(added, ["Good First Issue"]);
        let mut e = MultiSelect::new("Labels", labels(), |_| false);
        for c in "BUG".chars() {
            e.handle(&key(c));
        }
        assert_eq!(e.visible().len(), 1, "matches 🐛 bug");
    }

    #[test]
    fn esc_cancels_and_an_unchanged_set_applies_nothing() {
        let mut m = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        assert_eq!(m.handle(&code(KeyCode::Esc)), MultiOutcome::Cancel);
        let MultiOutcome::Apply { added, removed, .. } = m.handle(&code(KeyCode::Enter)) else {
            panic!("expected Apply")
        };
        assert!(added.is_empty() && removed.is_empty());
    }

    #[test]
    fn options_that_load_later_keep_the_typed_filter() {
        let mut m: MultiSelect<&str> = MultiSelect::loading("Assignees");
        m.handle(&key('d'));
        m.handle(&key('o'));
        assert_eq!(
            m.handle(&key(' ')),
            MultiOutcome::None,
            "nothing to toggle yet"
        );
        m.set_options(labels(), |v| *v == "docs");
        assert!(!m.loading);
        assert_eq!(m.input, "do");
        assert_eq!(m.visible(), [0], "docs, checked, is first");
    }

    #[test]
    fn renders_checkboxes() {
        let m = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        let screen = render_to_string(60, 10, |f| {
            render_multiselect(f, f.area(), &m, &Theme::plain())
        });
        assert!(
            screen.contains("[x] docs") && screen.contains("[ ] Good First Issue"),
            "{screen}"
        );
        assert!(screen.contains("space toggles"), "{screen}");
    }

    #[test]
    fn truncated_candidates_show_a_dim_narrowing_hint() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        let draw = |m: &MultiSelect<&str>| {
            render_to_string(60, 10, |f| {
                render_multiselect(f, f.area(), m, &Theme::plain())
            })
        };
        assert!(!draw(&m).contains("showing the first"));
        m.truncated = true;
        let screen = draw(&m);
        assert!(
            screen.contains("showing the first 100 — type to narrow"),
            "{screen}"
        );
    }

    #[test]
    fn multiselect_single_unticks_the_others() {
        let mut m = MultiSelect::loading("Assignee").single();
        m.set_options(labels(), |v| *v == "docs");
        assert_eq!(m.checked, [true, false, false]);
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' '));
        assert_eq!(m.checked, [false, true, false], "ticking one unticks docs");
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' '));
        assert_eq!(m.checked, [false, false, true]);
        let MultiOutcome::Apply {
            added,
            removed,
            chosen,
        } = m.handle(&code(KeyCode::Enter))
        else {
            panic!("expected Apply")
        };
        assert_eq!(
            (added, removed, chosen),
            (vec!["🐛 bug"], vec!["docs"], vec!["🐛 bug"])
        );
        m.handle(&key(' '));
        assert_eq!(m.checked, [false, false, false], "unticking leaves none");
        let MultiOutcome::Apply { chosen, .. } = m.handle(&code(KeyCode::Enter)) else {
            panic!("expected Apply")
        };
        assert!(chosen.is_empty());
        // A plain checklist still ticks several.
        let mut several = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        several.handle(&code(KeyCode::Down));
        several.handle(&key(' '));
        assert_eq!(several.checked, [true, true, false]);
    }

    #[test]
    fn a_longer_page_says_its_own_size() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        m.truncated = true;
        m.page = 250;
        let screen = render_to_string(60, 10, |f| {
            render_multiselect(f, f.area(), &m, &Theme::plain())
        });
        assert!(
            screen.contains("showing the first 250 — type to narrow"),
            "{screen}"
        );
    }

    #[test]
    fn backspace_deletes_a_grapheme_and_chords_do_not_type() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        for c in ['e', '\u{301}'] {
            m.handle(&key(c));
        }
        m.handle(&code(KeyCode::Backspace));
        assert_eq!(m.input, "");
        let ctrl = KeyEvent::new(KeyCode::Char('x'), crossterm::event::KeyModifiers::CONTROL);
        m.handle(&ctrl);
        assert_eq!(m.input, "");
    }

    fn many(n: usize) -> Vec<MultiOption<usize>> {
        (0..n)
            .map(|i| MultiOption {
                label: format!("opt{i:02}"),
                color: None,
                value: i,
            })
            .collect()
    }

    #[test]
    fn a_refresh_keeps_toggles_filter_and_the_original_diff() {
        let mut m = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' ')); // check Good First Issue
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' ')); // check bug
        m.handle(&code(KeyCode::Down));
        m.handle(&key(' ')); // the highlight stays on the last row: bug is unchecked again
        let before = m.checked.clone();
        m.error = Some("boom".into());
        m.set_options(labels(), |v| *v == "docs");
        assert_eq!(m.error, None);
        let by_label = |m: &MultiSelect<&str>, l: &str| {
            let i = m.options.iter().position(|o| o.label == l).unwrap();
            m.checked[i]
        };
        assert!(by_label(&m, "docs"));
        assert!(by_label(&m, "Good First Issue"));
        assert_eq!(
            m.checked.iter().filter(|c| **c).count(),
            before.iter().filter(|c| **c).count()
        );
        let MultiOutcome::Apply { added, removed, .. } = m.handle(&code(KeyCode::Enter)) else {
            panic!("expected Apply")
        };
        assert_eq!(added, ["Good First Issue"]);
        assert!(removed.is_empty());
    }

    #[test]
    fn a_refresh_keeps_the_highlight_and_the_filter() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        m.handle(&code(KeyCode::Down));
        m.handle(&code(KeyCode::Down));
        m.set_options(labels().into_iter().rev().collect(), |_| false);
        assert_eq!(m.options[m.visible()[m.selected]].label, "🐛 bug");
        m.handle(&key('b'));
        m.set_options(labels(), |_| false);
        assert_eq!(m.input, "b");
    }

    #[test]
    fn retry_after_an_error_loads_the_options() {
        let mut m: MultiSelect<&str> = MultiSelect::loading("Labels");
        m.loading = false;
        m.error = Some("offline".into());
        m.set_options(labels(), |_| false);
        assert!(m.error.is_none() && !m.loading);
        assert_eq!(m.options.len(), 3);
    }

    #[test]
    fn backspace_on_empty_input_keeps_the_highlight() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        m.handle(&code(KeyCode::Down));
        m.handle(&code(KeyCode::Backspace));
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn toggling_while_filtered_survives_clearing_the_filter() {
        let mut m = MultiSelect::new("Labels", labels(), |_| false);
        for c in "docs".chars() {
            m.handle(&key(c));
        }
        m.handle(&key(' '));
        for _ in 0..4 {
            m.handle(&code(KeyCode::Backspace));
        }
        assert_eq!(m.visible().len(), 3);
        assert_eq!(m.checked, [true, false, false]);
    }

    #[test]
    fn loading_and_error_render() {
        let theme = Theme::plain();
        let m: MultiSelect<&str> = MultiSelect::loading("Assignees");
        let s = render_to_string(60, 10, |f| render_multiselect(f, f.area(), &m, &theme));
        assert!(s.contains("Loading…"), "{s}");
        let mut e = MultiSelect::new("Labels", labels(), |_| false);
        e.error = Some("could not load".into());
        let s = render_to_string(60, 10, |f| render_multiselect(f, f.area(), &e, &theme));
        assert!(
            s.contains("could not load") && s.contains("[ ] docs"),
            "{s}"
        );
    }

    #[test]
    fn a_tiny_screen_does_not_panic() {
        let m = MultiSelect::new("Labels", labels(), |_| false);
        for (w, h) in [(3, 3), (1, 1), (0, 0), (10, 2)] {
            render_to_string(w, h, |f| {
                render_multiselect(f, f.area(), &m, &Theme::plain())
            });
        }
    }

    #[test]
    fn scrolling_keeps_the_highlight_visible() {
        let mut m = MultiSelect::new("Pick", many(40), |_| false);
        for _ in 0..30 {
            m.handle(&code(KeyCode::Down));
        }
        let screen = render_to_string(60, 30, |f| {
            render_multiselect(f, f.area(), &m, &Theme::plain())
        });
        assert!(screen.contains("opt30"), "{screen}");
        assert!(!screen.contains("opt00"), "{screen}");
    }

    #[test]
    fn the_highlighted_row_stands_out_on_a_label_pill() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Modifier;
        let theme = Theme {
            color: true,
            truecolor: true,
        };
        let m = MultiSelect::new("Labels", labels(), |_| false);
        let mut t = Terminal::new(TestBackend::new(60, 10)).unwrap();
        t.draw(|f| render_multiselect(f, f.area(), &m, &theme))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let find = |text: &str| {
            let rows: Vec<String> = (0..10)
                .map(|y| (0..60).map(|x| buf[(x, y)].symbol().to_string()).collect())
                .collect();
            let y = rows.iter().position(|r| r.contains(text)).unwrap() as u16;
            let x = rows[y as usize]
                .find(text)
                .map(|b| rows[y as usize][..b].chars().count())
                .unwrap() as u16;
            buf[(x, y)].modifier
        };
        assert!(
            find("docs").contains(Modifier::REVERSED),
            "highlighted pill"
        );
        assert!(!find("Good").contains(Modifier::REVERSED), "other pill");
    }

    #[test]
    fn a_filtered_list() {
        let mut m = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        for c in "go".chars() {
            m.handle(&key(c));
        }
        let screen = render_to_string(60, 12, |f| {
            render_multiselect(f, f.area(), &m, &Theme::plain())
        });
        insta::assert_snapshot!("multiselect_filtered", screen);
    }

    /// Spec section 9: pickers are snapshot-tested at 60, 100 and 200 columns.
    #[test]
    fn the_multiselect_at_three_widths() {
        let m = MultiSelect::new("Labels", labels(), |v| *v == "docs");
        for w in [60u16, 100, 200] {
            let screen = render_to_string(w, 12, |f| {
                render_multiselect(f, f.area(), &m, &Theme::plain())
            });
            insta::assert_snapshot!(format!("multiselect_{w}"), screen);
        }
    }
}
