use crate::model::{BoardRef, ProjectSummary};
use crate::ui::text::truncate_to_width;
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent};
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
            KeyCode::Char(c) => {
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
    for (i, p) in state
        .visible()
        .into_iter()
        .enumerate()
        .take(h.saturating_sub(4) as usize)
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
        " pick a board · esc quits "
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
        assert_eq!(p.handle(&code(KeyCode::Esc)), PickerOutcome::Cancel);
    }

    #[test]
    fn selection_stays_in_range() {
        let mut p = PickerState::with(vec![summary("tviles/1", "a")], false);
        p.handle(&code(KeyCode::Down));
        p.handle(&code(KeyCode::Down));
        assert_eq!(p.selected, 0);
    }
}
