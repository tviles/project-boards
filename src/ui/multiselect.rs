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
        }
    }

    pub fn loading(title: impl Into<String>) -> Self {
        let mut m = Self::new(title, Vec::new(), |_| false);
        m.loading = true;
        m
    }

    /// The options arrived; what was typed stays.
    pub fn set_options(&mut self, options: Vec<MultiOption<T>>, is_checked: impl Fn(&T) -> bool) {
        let input = std::mem::take(&mut self.input);
        *self = Self::new(std::mem::take(&mut self.title), options, is_checked);
        self.input = input;
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
                pop_grapheme(&mut self.input);
                self.selected = 0;
            }
            _ => match plain_char(key) {
                Some(' ') => {
                    if let Some(&i) = self.visible().get(self.selected) {
                        self.checked[i] = !self.checked[i];
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
    let status = m.error.is_some() || m.loading || visible.is_empty();
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
            Some(hex) if theme.color => theme.label(hex),
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

    /// Review Focus 4.
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
