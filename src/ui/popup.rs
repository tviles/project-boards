//! The popovers every editor shares: a list to pick one entry from (typing filters it) and
//! a one-line text input. Both are plain state machines; `render_*` draws them.

use crate::ui::text::{display_width, pad_to_width, sanitize, truncate_to_width};
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub struct Choice<T> {
    pub label: String,
    /// Dim text at the right: a key, "current", "completed".
    pub detail: String,
    pub value: T,
}

impl<T> Choice<T> {
    pub fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            detail: String::new(),
            value,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListPicker<T> {
    pub title: String,
    pub choices: Vec<Choice<T>>,
    pub input: String,
    pub selected: usize,
    /// The choices are still loading.
    pub loading: bool,
    /// Why the choices could not load.
    pub error: Option<String>,
    /// Typing filters the list. Off in menus, whose letters are keys.
    pub typing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListOutcome<T> {
    None,
    Chosen(T),
    Cancel,
    /// A key a menu (`typing: false`) does not use itself.
    Key(KeyEvent),
}

/// Whether the characters of `query` (spaces ignored) appear in `text` in order, ignoring case.
pub fn fuzzy_match(query: &str, text: &str) -> bool {
    let mut hay = text.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .all(|q| hay.any(|h| h == q))
}

/// The last `width` cells of `s` (sanitised), with a leading `…` when it was cut, so the end of
/// what is being typed stays visible.
fn tail_to_width(s: &str, width: usize) -> String {
    let clean = sanitize(s);
    if display_width(&clean) <= width {
        return clean;
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 1;
    for g in clean.graphemes(true).rev() {
        let w = display_width(g);
        if used + w > width {
            break;
        }
        kept.push(g);
        used += w;
    }
    kept.reverse();
    if width == 0 {
        String::new()
    } else {
        format!("…{}", kept.concat())
    }
}

fn plain_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(c)
        }
        _ => None,
    }
}

impl<T: Clone> ListPicker<T> {
    pub fn new(title: impl Into<String>, choices: Vec<Choice<T>>) -> Self {
        Self {
            title: title.into(),
            choices,
            input: String::new(),
            selected: 0,
            loading: false,
            error: None,
            typing: true,
        }
    }

    pub fn loading(title: impl Into<String>) -> Self {
        let mut p = Self::new(title, Vec::new());
        p.loading = true;
        p
    }

    /// The choices arrived; what was typed meanwhile stays.
    pub fn set_choices(&mut self, choices: Vec<Choice<T>>) {
        self.choices = choices;
        self.loading = false;
        self.error = None;
        self.clamp();
    }

    pub fn visible(&self) -> Vec<&Choice<T>> {
        self.choices
            .iter()
            .filter(|c| !self.typing || fuzzy_match(&self.input, &c.label))
            .collect()
    }

    pub fn current(&self) -> Option<&Choice<T>> {
        self.visible().get(self.selected).copied()
    }

    pub fn handle(&mut self, key: &KeyEvent) -> ListOutcome<T> {
        match key.code {
            KeyCode::Esc => return ListOutcome::Cancel,
            KeyCode::Enter => {
                if let Some(c) = self.current() {
                    return ListOutcome::Chosen(c.value.clone());
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected += 1,
            KeyCode::Backspace if self.typing => {
                self.input.pop();
                self.selected = 0;
            }
            _ => match (plain_char(key), self.typing) {
                (Some(c), true) => {
                    self.input.push(c);
                    self.selected = 0;
                }
                (Some('j'), false) => self.selected += 1,
                (Some('k'), false) => self.selected = self.selected.saturating_sub(1),
                (_, false) => return ListOutcome::Key(*key),
                (None, true) => {}
            },
        }
        self.clamp();
        ListOutcome::None
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOutcome {
    Typing,
    Submit,
    Cancel,
}

/// One-line editing: characters append, Backspace deletes, Enter submits, Esc cancels.
pub fn edit_line(input: &mut String, key: &KeyEvent) -> LineOutcome {
    match key.code {
        KeyCode::Enter => LineOutcome::Submit,
        KeyCode::Esc => LineOutcome::Cancel,
        KeyCode::Backspace => {
            input.pop();
            LineOutcome::Typing
        }
        _ => {
            if let Some(c) = plain_char(key) {
                input.push(c);
            }
            LineOutcome::Typing
        }
    }
}

/// A `width`×`height` box centred in `area`, clamped to it.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn boxed(frame: &mut Frame, rect: Rect, title: &str, lines: Vec<Line<'static>>) {
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
            " {} ",
            truncate_to_width(title, rect.width.saturating_sub(4) as usize)
        ))),
        rect,
    );
}

pub fn render_list_picker<T>(frame: &mut Frame, area: Rect, picker: &ListPicker<T>, theme: &Theme)
where
    T: Clone,
{
    let visible = picker.visible();
    // A status line (error, loading) shares the box with the choices that are already there.
    let status =
        u16::from(picker.error.is_some() || picker.loading) * u16::from(!visible.is_empty());
    let rows = visible.len().clamp(1, 15) as u16 + u16::from(picker.typing) + status;
    let rect = centered(area, area.width.saturating_sub(4).min(64), rows + 2);
    let inner = rect.width.saturating_sub(2) as usize;
    let mut lines = Vec::new();
    if picker.typing {
        lines.push(Line::styled(
            format!(
                "> {}",
                tail_to_width(&picker.input, inner.saturating_sub(2))
            ),
            theme.accent(),
        ));
    }
    if let Some(e) = &picker.error {
        lines.push(Line::styled(truncate_to_width(e, inner), theme.error()));
    } else if picker.loading {
        lines.push(Line::styled("Loading…", theme.dim()));
    } else if visible.is_empty() {
        lines.push(Line::styled("No matches", theme.dim()));
    }
    let room = (rect.height as usize).saturating_sub(2 + lines.len());
    let start = picker.selected.saturating_sub(room.saturating_sub(1));
    for (i, choice) in visible.iter().enumerate().skip(start).take(room) {
        let detail_w = display_width(&choice.detail);
        let label_w = inner.saturating_sub(detail_w + 1);
        let style = if i == picker.selected {
            theme.selected()
        } else {
            ratatui::style::Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                pad_to_width(&truncate_to_width(&choice.label, label_w), label_w),
                style,
            ),
            Span::styled(" ", style),
            Span::styled(
                truncate_to_width(&choice.detail, inner.saturating_sub(label_w + 1)),
                style.patch(theme.dim()),
            ),
        ]));
    }
    boxed(frame, rect, &picker.title, lines);
}

pub fn render_line_popup(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &str,
    hint: &str,
    error: Option<&str>,
    theme: &Theme,
) {
    let rect = centered(area, area.width.saturating_sub(4).min(64), 5);
    let inner = rect.width.saturating_sub(2) as usize;
    let lines = vec![
        Line::styled(
            format!("> {}", tail_to_width(input, inner.saturating_sub(2))),
            theme.accent(),
        ),
        Line::styled(truncate_to_width(hint, inner), theme.dim()),
        match error {
            Some(e) => Line::styled(truncate_to_width(e, inner), theme.error()),
            None => Line::raw(""),
        },
    ];
    boxed(frame, rect, title, lines);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{code, key, render_to_string};

    fn picker() -> ListPicker<u8> {
        ListPicker::new(
            "Status",
            vec![
                Choice::new("Todo", 1),
                Choice::new("In Progress", 2).with_detail("current"),
                Choice::new("Done", 3),
            ],
        )
    }

    #[test]
    fn fuzzy_matching_keeps_order_and_ignores_case_and_spaces() {
        assert!(fuzzy_match("inp", "In Progress"));
        assert!(fuzzy_match("in prog", "In Progress"));
        assert!(fuzzy_match("", "anything"));
        assert!(
            !fuzzy_match("pi", "In Progress"),
            "letters must come in order"
        );
        assert!(fuzzy_match("bug", "🐛 Bug"));
    }

    #[test]
    fn typing_filters_and_enter_chooses() {
        let mut p = picker();
        for c in "dn".chars() {
            assert_eq!(p.handle(&key(c)), ListOutcome::None);
        }
        assert_eq!(p.visible().len(), 1);
        assert_eq!(p.handle(&code(KeyCode::Enter)), ListOutcome::Chosen(3));
    }

    #[test]
    fn arrows_move_letters_type_and_esc_cancels() {
        let mut p = picker();
        p.handle(&code(KeyCode::Down));
        p.handle(&code(KeyCode::Down));
        p.handle(&code(KeyCode::Down));
        assert_eq!(p.current().unwrap().value, 3, "stops at the end");
        p.handle(&key('j'));
        assert_eq!(p.input, "j", "j is text in a typing picker");
        assert_eq!(
            p.handle(&code(KeyCode::Enter)),
            ListOutcome::None,
            "nothing matches"
        );
        assert_eq!(p.handle(&code(KeyCode::Esc)), ListOutcome::Cancel);
    }

    #[test]
    fn a_menu_moves_with_j_and_k_and_hands_other_keys_back() {
        let mut p = picker();
        p.typing = false;
        p.handle(&key('j'));
        assert_eq!(p.current().unwrap().value, 2);
        p.handle(&key('k'));
        assert_eq!(p.current().unwrap().value, 1);
        assert_eq!(p.handle(&key('e')), ListOutcome::Key(key('e')));
    }

    #[test]
    fn loading_then_choices_keep_the_typed_text() {
        let mut p: ListPicker<u8> = ListPicker::loading("Milestone");
        p.handle(&key('v'));
        p.set_choices(vec![Choice::new("v1", 1), Choice::new("next", 2)]);
        assert!(!p.loading);
        assert_eq!(p.visible().len(), 1);
    }

    #[test]
    fn edit_line_types_deletes_submits_and_cancels() {
        let mut s = String::from("ab");
        assert_eq!(edit_line(&mut s, &key('c')), LineOutcome::Typing);
        assert_eq!(
            edit_line(&mut s, &code(KeyCode::Backspace)),
            LineOutcome::Typing
        );
        assert_eq!(s, "ab");
        assert_eq!(
            edit_line(&mut s, &code(KeyCode::Enter)),
            LineOutcome::Submit
        );
        assert_eq!(edit_line(&mut s, &code(KeyCode::Esc)), LineOutcome::Cancel);
    }

    #[test]
    fn renders_title_input_choices_and_details() {
        let mut p = picker();
        p.handle(&key('o'));
        let screen = render_to_string(60, 12, |f| {
            render_list_picker(f, f.area(), &p, &Theme::plain())
        });
        assert!(screen.contains("Status"), "{screen}");
        assert!(screen.contains("> o"), "{screen}");
        assert!(
            screen.contains("Todo") && screen.contains("current"),
            "{screen}"
        );
        let line = render_to_string(60, 8, |f| {
            render_line_popup(
                f,
                f.area(),
                "Due · Fix crash",
                "fri",
                "2026-10-03, +3d, fri",
                Some("not a date"),
                &Theme::plain(),
            )
        });
        assert!(
            line.contains("> fri") && line.contains("not a date") && line.contains("+3d"),
            "{line}"
        );
    }

    #[test]
    fn chords_do_not_type_shift_does_and_a_long_list_keeps_the_selection_drawn() {
        let mut s = String::new();
        let ctrl = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(edit_line(&mut s, &ctrl), LineOutcome::Typing);
        assert_eq!(
            edit_line(
                &mut s,
                &KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT)
            ),
            LineOutcome::Typing
        );
        assert_eq!(s, "Q");
        let mut p = ListPicker::new(
            "Many",
            (0..40)
                .map(|i| Choice::new(format!("item{i:02}"), i))
                .collect(),
        );
        for _ in 0..30 {
            p.handle(&code(KeyCode::Down));
        }
        let screen = render_to_string(40, 12, |f| {
            render_list_picker(f, f.area(), &p, &Theme::plain())
        });
        assert!(screen.contains("item30"), "{screen}");
        let long = "x".repeat(100);
        let line = render_to_string(30, 7, |f| {
            render_line_popup(
                f,
                f.area(),
                "T",
                &format!("{long}END"),
                "h",
                None,
                &Theme::plain(),
            )
        });
        assert!(line.contains("END"), "{line}");
    }

    /// Spec section 9: pickers are snapshot-tested at 60, 100 and 200 columns.
    #[test]
    fn the_picker_at_three_widths() {
        let mut p = picker();
        p.handle(&key('o'));
        for w in [60u16, 100, 200] {
            let screen = render_to_string(w, 12, |f| {
                render_list_picker(f, f.area(), &p, &Theme::plain())
            });
            insta::assert_snapshot!(format!("list_picker_{w}"), screen);
        }
    }
}
