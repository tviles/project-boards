use crate::model::{BoardKey, PickerSection, ProjectSummary};
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
    /// One line per provider whose boards are missing from the list, drawn dim at the end.
    pub notes: Vec<String>,
    /// No board is open yet: cancelling quits instead of returning to a board.
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickerOutcome {
    None,
    Chosen(BoardKey),
    Cancel,
}

impl PickerState {
    pub fn loading(required: bool) -> Self {
        Self {
            candidates: Vec::new(),
            input: String::new(),
            selected: 0,
            loading: true,
            notes: Vec::new(),
            required,
        }
    }

    pub fn with(candidates: Vec<ProjectSummary>, notes: Vec<String>, required: bool) -> Self {
        Self {
            candidates,
            input: String::new(),
            selected: 0,
            loading: false,
            notes,
            required,
        }
    }

    /// Candidates whose board key and title contain every typed word, in section order
    /// (GitHub, Linear projects, Linear teams; each section keeps its own order). `selected`
    /// indexes this list: headers and notes are drawn, never selected.
    pub fn visible(&self) -> Vec<&ProjectSummary> {
        let words: Vec<String> = self
            .input
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut visible: Vec<&ProjectSummary> = self
            .candidates
            .iter()
            .filter(|p| {
                let hay = format!("{} {}", p.board, p.title).to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        visible.sort_by_key(|p| p.section());
        visible
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

fn section_title(section: PickerSection) -> &'static str {
    match section {
        PickerSection::Github => "GitHub",
        PickerSection::LinearProjects => "Linear projects",
        PickerSection::LinearTeams => "Linear teams",
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
    // Section headers and candidates scroll; the notes stay below them.
    let width = w.saturating_sub(2) as usize;
    let mut rows = Vec::new();
    let mut selected_row = 0;
    let mut section = None;
    // A GitHub-only list draws no header: a GitHub user's picker reads as it always has.
    let headers = state
        .candidates
        .iter()
        .any(|p| p.section() != PickerSection::Github);
    for (i, p) in state.visible().into_iter().enumerate() {
        if headers && section != Some(p.section()) {
            section = Some(p.section());
            rows.push(Line::styled(section_title(p.section()), theme.heading()));
        }
        let style = if i == state.selected {
            selected_row = rows.len();
            theme.selected()
        } else {
            ratatui::style::Style::default()
        };
        let text = match p.section() {
            PickerSection::Github => format!("{}  {}", p.board, p.title),
            PickerSection::LinearProjects | PickerSection::LinearTeams => p.title.clone(),
        };
        rows.push(Line::styled(truncate_to_width(&text, width), style));
    }
    // The window follows the selection, so the selected row is always drawn.
    let window = (h.saturating_sub(4) as usize)
        .saturating_sub(state.notes.len())
        .max(1);
    let offset = selected_row.saturating_sub(window - 1);
    lines.extend(rows.into_iter().skip(offset).take(window));
    lines.extend(
        state
            .notes
            .iter()
            .map(|note| Line::styled(truncate_to_width(note, width), theme.dim())),
    );
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
            vec![],
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
        let mut p = PickerState::with(vec![summary("tviles/1", "a")], vec![], true);
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
        let mut p = PickerState::with(list, vec![], false);
        p.selected = 25;
        assert!(render(&p, 60, 10).contains("Board number 25"));
        p.selected = 0;
        assert!(render(&p, 60, 10).contains("Board number 0"));
    }

    #[test]
    fn renders_rows_title_and_survives_tiny_areas() {
        let p = PickerState::with(vec![summary("acme/7", "Sprint")], vec![], true);
        let s = render(&p, 60, 12);
        assert!(s.contains("acme/7  Sprint") && s.contains("ctrl+c quits"));
        assert!(render(&PickerState::loading(false), 60, 12).contains("Loading boards"));
        render(&p, 0, 0);
        render(&p, 40, 8);
    }

    /// Each drawn row inside the box: its text, and the style of its first character.
    fn rows(p: &PickerState, w: u16, h: u16) -> Vec<(String, ratatui::style::Style)> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let buf = terminal
            .draw(|f| render_picker(f, f.area(), p, &Theme::plain()))
            .unwrap()
            .buffer
            .clone();
        // The box's interior, as `render_picker` places it.
        let left = (w - w.saturating_sub(4).min(70)) / 2 + 1;
        let box_h = h.saturating_sub(2).min(20);
        let top = (h - box_h) / 2 + 1;
        (top..top + box_h.saturating_sub(2))
            .map(|y| {
                let text: String = (left..w).map(|x| buf[(x, y)].symbol()).collect();
                let text = text.trim_end_matches([' ', '│']).trim().to_string();
                (text, buf[(left, y)].style())
            })
            .filter(|(text, _)| !text.is_empty())
            .collect()
    }

    fn texts(p: &PickerState, w: u16, h: u16) -> Vec<String> {
        rows(p, w, h).into_iter().map(|(t, _)| t).collect()
    }

    /// GitHub, Linear projects and Linear teams, out of order.
    fn mixed() -> Vec<ProjectSummary> {
        vec![
            summary("linear:team:t1", "Testbed A (TBA)"),
            summary("acme/7", "Sprint"),
            summary("linear:project:p1", "Launch"),
        ]
    }

    #[test]
    fn a_summary_knows_its_section() {
        assert_eq!(summary("acme/7", "x").section(), PickerSection::Github);
        assert_eq!(
            summary("linear:project:p1", "x").section(),
            PickerSection::LinearProjects
        );
        assert_eq!(
            summary("linear:team:t1", "x").section(),
            PickerSection::LinearTeams
        );
    }

    #[test]
    fn sections_show_github_then_linear_projects_then_teams() {
        let mut p = PickerState::with(mixed(), vec![], false);
        let shown = texts(&p, 60, 14);
        let body: Vec<&str> = shown.iter().skip(1).map(String::as_str).collect();
        assert_eq!(
            body,
            [
                "GitHub",
                "acme/7  Sprint",
                "Linear projects",
                "Launch",
                "Linear teams",
                "Testbed A (TBA)",
            ],
            "{shown:?}"
        );
        // A section with no visible candidate has no header; words still match the board key.
        for c in "launch".chars() {
            p.handle(&key(c));
        }
        let shown = texts(&p, 60, 14);
        assert_eq!(shown[1..], ["Linear projects", "Launch"], "{shown:?}");
        p.input = "linear:team".into();
        let shown = texts(&p, 60, 14);
        assert_eq!(shown[1..], ["Linear teams", "Testbed A (TBA)"], "{shown:?}");
    }

    #[test]
    fn selection_skips_headers_and_notes() {
        let note = "GitHub boards could not be loaded (Network)".to_string();
        let mut p = PickerState::with(mixed(), vec![note.clone()], false);
        let selected = |p: &PickerState| -> Vec<String> {
            rows(p, 60, 14)
                .into_iter()
                .filter(|(_, style)| {
                    style
                        .add_modifier
                        .contains(ratatui::style::Modifier::REVERSED)
                })
                .map(|(t, _)| t)
                .collect()
        };
        assert_eq!(selected(&p), ["acme/7  Sprint"]);
        p.handle(&code(KeyCode::Down));
        assert_eq!(
            selected(&p),
            ["Launch"],
            "the Linear projects header is skipped"
        );
        p.handle(&code(KeyCode::Down));
        assert_eq!(selected(&p), ["Testbed A (TBA)"]);
        p.handle(&code(KeyCode::Down));
        assert_eq!(
            selected(&p),
            ["Testbed A (TBA)"],
            "the note is not selectable"
        );
        assert_eq!(
            p.handle(&code(KeyCode::Enter)),
            PickerOutcome::Chosen("linear:team:t1".parse().unwrap())
        );
        p.handle(&code(KeyCode::Up));
        assert_eq!(
            p.handle(&code(KeyCode::Enter)),
            PickerOutcome::Chosen("linear:project:p1".parse().unwrap())
        );
        assert_eq!(texts(&p, 60, 14).last(), Some(&note), "notes come last");
    }

    #[test]
    fn a_missing_linear_key_is_one_dim_line() {
        let note = "Linear boards: set LINEAR_API_KEY or linear_api_key_command";
        let p = PickerState::with(
            vec![summary("tviles/1", "Roadmap"), summary("acme/7", "Sprint")],
            vec![note.into()],
            true,
        );
        let shown = rows(&p, 80, 12);
        let drawn: Vec<&str> = shown.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            drawn[1..],
            ["tviles/1  Roadmap", "acme/7  Sprint", note],
            "a GitHub-only list draws no header: {drawn:?}"
        );
        let (_, style) = shown.last().unwrap();
        assert!(style.add_modifier.contains(ratatui::style::Modifier::DIM));
        // A long list scrolls; the note stays on screen.
        let list = (0..30)
            .map(|i| summary(&format!("acme/{}", i + 1), &format!("Board number {i}")))
            .collect();
        let mut p = PickerState::with(list, vec![note.into()], false);
        for selected in [0, 15, 29] {
            p.selected = selected;
            let shown = texts(&p, 80, 12);
            assert!(
                shown
                    .iter()
                    .any(|t| t.ends_with(&format!("Board number {selected}")))
            );
            assert_eq!(shown.last().map(String::as_str), Some(note), "{shown:?}");
        }
    }

    #[test]
    fn selection_stays_in_range() {
        let mut p = PickerState::with(vec![summary("tviles/1", "a")], vec![], false);
        p.handle(&code(KeyCode::Down));
        p.handle(&code(KeyCode::Down));
        assert_eq!(p.selected, 0);
    }
}
