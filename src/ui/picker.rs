use crate::model::{BoardRef, ProjectSummary};
use crate::ui::text::truncate_to_width;
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub candidates: Vec<ProjectSummary>,
    pub input: String,
    pub selected: usize,
    pub loading: bool,
    /// No board is open yet: cancelling quits instead of returning to a board.
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerOutcome {
    None,
    Chosen(BoardRef),
    Cancel,
}

impl PickerState {
    pub fn loading(required: bool) -> Self {
        Self {
            candidates: Vec::new(),
            input: String::new(),
            selected: 0,
            loading: true,
            required,
        }
    }

    pub fn with(candidates: Vec<ProjectSummary>, required: bool) -> Self {
        Self {
            candidates,
            input: String::new(),
            selected: 0,
            loading: false,
            required,
        }
    }

    /// Candidates whose `owner/number title` contains every typed word.
    pub fn visible(&self) -> Vec<&ProjectSummary> {
        let words: Vec<String> = self
            .input
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.candidates
            .iter()
            .filter(|p| {
                let hay = format!("{} {}", p.board, p.title).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect()
    }

    pub fn handle(&mut self, key: &KeyEvent) -> PickerOutcome {
        match key.code {
            // A required picker has no board to go back to; Ctrl+C quits (handled by the App).
            KeyCode::Esc if self.required => {}
            KeyCode::Esc => return PickerOutcome::Cancel,
            KeyCode::Enter => {
                if let Some(p) = self.visible().get(self.selected) {
                    return PickerOutcome::Chosen(p.board.clone());
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected += 1,
            KeyCode::Backspace => {
                self.input.pop();
                self.selected = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
        PickerOutcome::None
    }
}

pub fn render_picker(frame: &mut Frame, area: Rect, state: &PickerState, theme: &Theme) {
    let w = area.width.saturating_sub(4).min(70);
    let h = area.height.saturating_sub(2).min(20);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("> ", theme.accent()),
            Span::raw(state.input.clone()),
        ]),
        Line::from(""),
    ];
    if state.loading {
        lines.push(Line::styled("Loading boards…", theme.dim()));
    } else if state.visible().is_empty() {
        lines.push(Line::styled("No boards match.", theme.dim()));
    }
    // The window follows the selection, so the selected row is always drawn.
    let window = h.saturating_sub(4).max(1) as usize;
    let offset = state.selected.saturating_sub(window - 1);
    for (i, p) in state
        .visible()
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(window)
    {
        let style = if i == state.selected {
            theme.selected()
        } else {
            ratatui::style::Style::default()
        };
        lines.push(Line::styled(
            truncate_to_width(
                &format!("{}  {}", p.board, p.title),
                w.saturating_sub(2) as usize,
            ),
            style,
        ));
    }
    let title = if state.required {
        " pick a board · ctrl+c quits "
    } else {
        " switch board · esc cancels "
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProjectId;
    use crate::ui::fixtures::{code, key};

    fn summary(board: &str, title: &str) -> ProjectSummary {
        ProjectSummary {
            id: ProjectId::new(board),
            board: board.parse().unwrap(),
            title: title.into(),
            closed: false,
        }
    }

    #[test]
    fn typing_filters_and_enter_chooses() {
        let mut p = PickerState::with(
            vec![
                summary("tviles/1", "Roadmap"),
                summary("acme/7", "Sprint board"),
            ],
            true,
        );
        for c in "spr".chars() {
            p.handle(&key(c));
        }
        assert_eq!(p.visible().len(), 1);
        assert_eq!(
            p.handle(&code(KeyCode::Enter)),
            PickerOutcome::Chosen("acme/7".parse().unwrap())
        );
        assert_eq!(p.handle(&code(KeyCode::Esc)), PickerOutcome::None);
        p.required = false;
        assert_eq!(p.handle(&code(KeyCode::Esc)), PickerOutcome::Cancel);
    }

    #[test]
    fn esc_on_a_required_picker_does_nothing_and_ctrl_keys_do_not_type() {
        let mut p = PickerState::with(vec![summary("tviles/1", "a")], true);
        assert_eq!(p.handle(&code(KeyCode::Esc)), PickerOutcome::None);
        p.handle(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        p.handle(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT));
        assert_eq!(p.input, "");
        p.handle(&KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(p.input, "A");
    }

    fn render(p: &PickerState, w: u16, h: u16) -> String {
        crate::ui::fixtures::render_to_string(w, h, |f| {
            render_picker(f, f.area(), p, &Theme::plain())
        })
    }

    #[test]
    fn the_selected_row_is_always_on_screen() {
        let list = (0..30)
            .map(|i| summary(&format!("acme/{}", i + 1), &format!("Board number {i}")))
            .collect();
        let mut p = PickerState::with(list, false);
        p.selected = 25;
        assert!(render(&p, 60, 10).contains("Board number 25"));
        p.selected = 0;
        assert!(render(&p, 60, 10).contains("Board number 0"));
    }

    #[test]
    fn renders_rows_title_and_survives_tiny_areas() {
        let p = PickerState::with(vec![summary("acme/7", "Sprint")], true);
        let s = render(&p, 60, 12);
        assert!(s.contains("acme/7  Sprint") && s.contains("ctrl+c quits"));
        assert!(render(&PickerState::loading(false), 60, 12).contains("Loading boards"));
        render(&p, 0, 0);
        render(&p, 40, 8);
    }

    #[test]
    fn selection_stays_in_range() {
        let mut p = PickerState::with(vec![summary("tviles/1", "a")], false);
        p.handle(&code(KeyCode::Down));
        p.handle(&code(KeyCode::Down));
        assert_eq!(p.selected, 0);
    }
}
