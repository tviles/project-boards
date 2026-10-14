//! The text form popup: a title and/or body field plus buttons in one focus ring, used for
//! new items, comments and edited titles and bodies (Part C, decisions C1 to C7).
//!
//! Keys: `Tab`/`Shift+Tab` move between fields and buttons, `Enter` in the title moves to the
//! body (or submits a title-only form), `Enter` in the body is a newline, `Enter` on a button
//! runs it, `Alt+Enter` submits from anywhere, `Esc` cancels (asking first when text was
//! typed). Every other key is text for the focused field. `Ctrl+C` is never consumed here.

use crate::ui::editor::{title_and_body, trim_blank_lines};
use crate::ui::popup::centered;
use crate::ui::text::truncate_to_width;
use crate::ui::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui_textarea::{CursorMove, Input, TextArea, WrapMode};

/// Which form: decides the fields and the buttons (decision C3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    /// Title and body; `[ Create ]`.
    NewItem,
    /// Body only; `[ Comment ]`.
    Comment,
    /// Body only, prefilled; `[ Save ]`.
    Body,
    /// Title only, prefilled; `[ Save ]`, no editor button.
    Title,
}

/// What a key did, for the caller to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormEvent {
    None,
    Submit,
    OpenEditor,
    /// The form should close (after the discard rule of C4).
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Title,
    Body,
    Button(usize),
}

#[derive(Debug, Clone)]
pub struct TextForm {
    kind: FormKind,
    heading: String,
    title: TextArea<'static>,
    body: TextArea<'static>,
    initial_title: String,
    initial_body: String,
    focus: Focus,
    confirm_discard: bool,
    message: Option<String>,
}

impl PartialEq for TextForm {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.title_text() == other.title_text()
            && self.body_text() == other.body_text()
            && self.focus == other.focus
            && self.confirm_discard == other.confirm_discard
    }
}

fn area_with(text: &str, wrap: WrapMode) -> TextArea<'static> {
    let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut ta = TextArea::new(lines);
    ta.set_wrap_mode(wrap);
    ta.set_cursor_line_style(Style::default());
    ta.move_cursor(CursorMove::Bottom);
    ta.move_cursor(CursorMove::End);
    ta
}

fn text_of(ta: &TextArea<'_>) -> String {
    ta.lines().join("\n")
}

impl TextForm {
    /// `title` is the popup's heading; `initial_*` prefill the fields (and are what "nothing
    /// typed" is measured against). A kind without a field ignores that argument.
    pub fn new(
        kind: FormKind,
        title: impl Into<String>,
        initial_title: &str,
        initial_body: &str,
    ) -> Self {
        let has_title = matches!(kind, FormKind::NewItem | FormKind::Title);
        let has_body = !matches!(kind, FormKind::Title);
        let t = area_with(if has_title { initial_title } else { "" }, WrapMode::None);
        let b = area_with(if has_body { initial_body } else { "" }, WrapMode::Word);
        let focus = if has_title { Focus::Title } else { Focus::Body };
        Self {
            kind,
            heading: title.into(),
            initial_title: text_of(&t),
            initial_body: text_of(&b),
            title: t,
            body: b,
            focus,
            confirm_discard: false,
            message: None,
        }
    }

    fn has_title(&self) -> bool {
        matches!(self.kind, FormKind::NewItem | FormKind::Title)
    }

    fn has_body(&self) -> bool {
        !matches!(self.kind, FormKind::Title)
    }

    /// Button labels and what each does, in focus order.
    fn buttons(&self) -> Vec<(&'static str, FormEvent)> {
        let go = match self.kind {
            FormKind::NewItem => "Create",
            FormKind::Comment => "Comment",
            FormKind::Body | FormKind::Title => "Save",
        };
        let mut v = vec![(go, FormEvent::Submit)];
        if self.kind != FormKind::Title {
            v.push(("Open in editor", FormEvent::OpenEditor));
        }
        v.push(("Cancel", FormEvent::Cancel));
        v
    }

    /// The focus stops: fields, then buttons.
    fn ring(&self) -> Vec<Focus> {
        let mut r = Vec::new();
        if self.has_title() {
            r.push(Focus::Title);
        }
        if self.has_body() {
            r.push(Focus::Body);
        }
        r.extend((0..self.buttons().len()).map(Focus::Button));
        r
    }

    fn step(&mut self, back: bool) {
        let ring = self.ring();
        let at = ring.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = ring.len();
        self.focus = ring[(at + if back { n - 1 } else { 1 }) % n];
    }

    pub fn title_text(&self) -> String {
        text_of(&self.title)
    }

    pub fn body_text(&self) -> String {
        text_of(&self.body)
    }

    /// What goes to `$EDITOR`: new items as `title`, a blank line, `body` (parsed back with
    /// `title_and_body`); the others as the body.
    pub fn editor_text(&self) -> String {
        match self.kind {
            FormKind::NewItem => format!("{}\n\n{}", self.title_text(), self.body_text()),
            _ => self.body_text(),
        }
    }

    /// Takes the editor's result back into the form and focuses the body (C6). A new item
    /// with no title line empties both fields.
    pub fn set_from_editor(&mut self, text: &str) {
        match self.kind {
            FormKind::NewItem => {
                let (t, b) = title_and_body(text).unwrap_or_default();
                self.title = area_with(&t, WrapMode::None);
                self.body = area_with(&b, WrapMode::Word);
            }
            _ => self.body = area_with(&trim_blank_lines(text), WrapMode::Word),
        }
        if self.has_body() {
            self.focus = Focus::Body;
        }
        self.confirm_discard = false;
    }

    /// A line shown in the form (a refused submit, an editor error); `None` clears it.
    pub fn set_message(&mut self, msg: Option<String>) {
        self.message = msg;
    }

    /// The text differs from what the form opened with.
    pub fn is_dirty(&self) -> bool {
        self.title_text() != self.initial_title || self.body_text() != self.initial_body
    }

    fn escape(&mut self) -> FormEvent {
        if self.is_dirty() {
            self.confirm_discard = true;
            FormEvent::None
        } else {
            FormEvent::Cancel
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormEvent {
        if key.kind == KeyEventKind::Release {
            return FormEvent::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return FormEvent::None;
        }
        if self.confirm_discard {
            if key.code == KeyCode::Char('y') && !ctrl {
                return FormEvent::Cancel;
            }
            self.confirm_discard = false;
            return FormEvent::None;
        }
        self.message = None;
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter if alt => return FormEvent::Submit,
            KeyCode::Esc => return self.escape(),
            KeyCode::Tab => {
                self.step(key.modifiers.contains(KeyModifiers::SHIFT));
                return FormEvent::None;
            }
            KeyCode::BackTab => {
                self.step(true);
                return FormEvent::None;
            }
            _ => {}
        }
        match self.focus {
            Focus::Button(i) => match key.code {
                KeyCode::Enter => match self.buttons()[i].1 {
                    FormEvent::Cancel => self.escape(),
                    e => e,
                },
                _ => FormEvent::None,
            },
            Focus::Title => match key.code {
                KeyCode::Enter => {
                    if self.has_body() {
                        self.focus = Focus::Body;
                        FormEvent::None
                    } else {
                        FormEvent::Submit
                    }
                }
                // Ctrl+J and Ctrl+M would break the line.
                KeyCode::Char('j' | 'm') if ctrl => FormEvent::None,
                _ => {
                    self.title.input(Input::from(key));
                    FormEvent::None
                }
            },
            Focus::Body => {
                self.body.input(Input::from(key));
                FormEvent::None
            }
        }
    }
}

/// The hint inside the bottom border.
const HINT: &str = "Tab next · Alt+Enter submit · Esc cancel";

fn field_block(label: &str, focused: bool, theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {label} "))
        .border_style(if focused { theme.accent() } else { theme.dim() })
}

fn draw_field(
    f: &mut Frame,
    rect: Rect,
    label: &str,
    ta: &mut TextArea<'static>,
    focused: bool,
    theme: &Theme,
) {
    let block = field_block(label, focused, theme);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    ta.set_cursor_style(if focused {
        theme.selected()
    } else {
        Style::default()
    });
    f.render_widget(&*ta, inner);
}

/// Draws the form centred in `area`: width `min(area - 4, 80)`, height up to `area - 4`,
/// grown when needed so the body keeps at least 3 rows.
pub fn draw(f: &mut Frame, area: Rect, form: &mut TextForm, theme: &Theme) {
    let width = area.width.saturating_sub(4).min(80);
    let status = form.confirm_discard || form.message.is_some();
    // Borders, button row, status row, then the title box (3) and the body box (3 text rows).
    let min_h = 2
        + 1
        + u16::from(status)
        + if form.has_title() { 3 } else { 0 }
        + if form.has_body() { 5 } else { 0 };
    let height = area.height.saturating_sub(4).max(min_h);
    let rect = centered(area, width, height);
    f.render_widget(Clear, rect);
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " {} ",
            truncate_to_width(&form.heading, rect.width.saturating_sub(4) as usize)
        ))
        .title_bottom(Line::styled(
            format!(
                " {} ",
                truncate_to_width(HINT, rect.width.saturating_sub(4) as usize)
            ),
            theme.dim(),
        ));
    let inner = outer.inner(rect);
    f.render_widget(outer, rect);

    let mut rows = Vec::new();
    if form.has_title() {
        rows.push(Constraint::Length(3));
    }
    if form.has_body() {
        rows.push(Constraint::Min(3));
    } else {
        rows.push(Constraint::Min(0));
    }
    if status {
        rows.push(Constraint::Length(1));
    }
    rows.push(Constraint::Length(1));
    let chunks = Layout::vertical(rows).split(inner);
    let mut i = 0;
    if form.has_title() {
        let focused = form.focus == Focus::Title;
        let (mut t, rect) = (std::mem::take(&mut form.title), chunks[i]);
        draw_field(f, rect, "Title", &mut t, focused, theme);
        form.title = t;
        i += 1;
    }
    if form.has_body() {
        let focused = form.focus == Focus::Body;
        let (mut b, rect) = (std::mem::take(&mut form.body), chunks[i]);
        draw_field(f, rect, "Body", &mut b, focused, theme);
        form.body = b;
    }
    i += 1;
    let w = inner.width as usize;
    if status {
        let line = if form.confirm_discard {
            Line::styled(
                truncate_to_width("Discard what you typed? (y/n)", w),
                theme.error(),
            )
        } else {
            Line::styled(
                truncate_to_width(form.message.as_deref().unwrap_or(""), w),
                theme.error(),
            )
        };
        f.render_widget(Paragraph::new(line), chunks[i]);
        i += 1;
    }
    let mut spans = Vec::new();
    for (n, (label, _)) in form.buttons().into_iter().enumerate() {
        let on = form.focus == Focus::Button(n);
        let text = if on && !theme.color {
            format!("»[ {label} ]«")
        } else {
            format!("[ {label} ]")
        };
        let style = if on && theme.color {
            theme.selected()
        } else {
            Style::default()
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw("  "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), chunks[i]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{code, key, render_to_string};

    fn press(f: &mut TextForm, c: KeyCode) -> FormEvent {
        f.handle_key(code(c))
    }

    fn type_str(f: &mut TextForm, s: &str) {
        for c in s.chars() {
            f.handle_key(key(c));
        }
    }

    fn new_item() -> TextForm {
        TextForm::new(FormKind::NewItem, "New draft", "", "")
    }

    fn alt_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
    }

    fn ring_of(mut f: TextForm) -> Vec<Focus> {
        let mut seen = vec![f.focus];
        for _ in 0..f.ring().len() {
            press(&mut f, KeyCode::Tab);
            seen.push(f.focus);
        }
        seen
    }

    #[test]
    fn focus_ring_runs_fields_then_buttons_and_wraps() {
        use Focus::*;
        assert_eq!(
            ring_of(new_item()),
            [Title, Body, Button(0), Button(1), Button(2), Title]
        );
        let c = TextForm::new(FormKind::Comment, "Comment on #12", "", "");
        assert_eq!(ring_of(c), [Body, Button(0), Button(1), Button(2), Body]);
        let t = TextForm::new(FormKind::Title, "Edit title of #12", "Old", "");
        assert_eq!(ring_of(t), [Title, Button(0), Button(1), Title]);
        let b = TextForm::new(FormKind::Body, "Edit body of #12", "", "text");
        assert_eq!(ring_of(b), [Body, Button(0), Button(1), Button(2), Body]);
    }

    #[test]
    fn shift_tab_reverses() {
        let mut f = new_item();
        press(&mut f, KeyCode::BackTab);
        assert_eq!(f.focus, Focus::Button(2));
        f.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(f.focus, Focus::Button(1));
    }

    #[test]
    fn printable_keys_are_text() {
        let mut f = new_item();
        type_str(&mut f, "qje");
        assert_eq!(f.title_text(), "qje");
        assert_eq!(f.body_text(), "");
    }

    #[test]
    fn enter_in_title_moves_to_body_or_submits_a_title_only_form() {
        let mut f = new_item();
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::None);
        assert_eq!(f.focus, Focus::Body);
        let mut t = TextForm::new(FormKind::Title, "Edit title of #12", "Old", "");
        assert_eq!(press(&mut t, KeyCode::Enter), FormEvent::Submit);
    }

    #[test]
    fn title_stays_one_line() {
        let mut f = new_item();
        type_str(&mut f, "a");
        f.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        f.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL));
        type_str(&mut f, "b");
        assert_eq!(f.title_text(), "ab");
    }

    #[test]
    fn enter_in_body_is_a_newline_and_alt_enter_submits_anywhere() {
        let mut f = new_item();
        press(&mut f, KeyCode::Tab);
        type_str(&mut f, "one");
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::None);
        type_str(&mut f, "two");
        assert_eq!(f.body_text(), "one\ntwo");
        for _ in 0..4 {
            assert_eq!(f.handle_key(alt_enter()), FormEvent::Submit);
            press(&mut f, KeyCode::Tab);
        }
    }

    #[test]
    fn enter_on_each_button_gives_its_event() {
        let mut f = new_item();
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::Submit);
        press(&mut f, KeyCode::Tab);
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::OpenEditor);
        press(&mut f, KeyCode::Tab);
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::Cancel);
    }

    #[test]
    fn typing_on_a_button_does_nothing() {
        let mut f = new_item();
        press(&mut f, KeyCode::BackTab);
        type_str(&mut f, "x");
        assert!(!f.is_dirty());
    }

    #[test]
    fn esc_on_a_clean_form_cancels() {
        let mut f = new_item();
        assert_eq!(press(&mut f, KeyCode::Esc), FormEvent::Cancel);
    }

    #[test]
    fn esc_on_a_dirty_form_asks_first() {
        let mut f = new_item();
        type_str(&mut f, "draft");
        assert_eq!(press(&mut f, KeyCode::Esc), FormEvent::None);
        assert!(f.confirm_discard);
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        assert!(shown.contains("Discard what you typed? (y/n)"), "{shown}");
        // `n` (or any key) returns with the text intact.
        assert_eq!(f.handle_key(key('n')), FormEvent::None);
        assert!(!f.confirm_discard);
        assert_eq!(f.title_text(), "draft");
        press(&mut f, KeyCode::Esc);
        assert_eq!(f.handle_key(key('y')), FormEvent::Cancel);
    }

    #[test]
    fn cancel_button_on_a_dirty_form_asks_first() {
        let mut f = new_item();
        type_str(&mut f, "x");
        press(&mut f, KeyCode::BackTab);
        assert_eq!(press(&mut f, KeyCode::Enter), FormEvent::None);
        assert!(f.confirm_discard);
    }

    #[test]
    fn a_form_opened_with_text_is_clean_until_changed() {
        let mut f = TextForm::new(
            FormKind::Body,
            "Edit body of #12",
            "",
            "line one\nline two\n",
        );
        assert!(!f.is_dirty());
        assert_eq!(press(&mut f, KeyCode::Esc), FormEvent::Cancel);
        type_str(&mut f, "!");
        assert!(f.is_dirty());
        assert_eq!(f.body_text(), "line one\nline two\n!");
    }

    #[test]
    fn ctrl_c_is_not_consumed() {
        let mut f = new_item();
        type_str(&mut f, "x");
        let e = f.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(e, FormEvent::None);
        assert_eq!(f.title_text(), "x");
        assert!(!f.confirm_discard);
    }

    #[test]
    fn editor_round_trip_for_a_new_item() {
        let mut f = TextForm::new(
            FormKind::NewItem,
            "New draft",
            "Fix the thing",
            "Why\n\nand how",
        );
        let text = f.editor_text();
        assert_eq!(text, "Fix the thing\n\nWhy\n\nand how");
        f.set_from_editor("Better title\n\nNew body\nsecond line\n");
        assert_eq!(f.title_text(), "Better title");
        assert_eq!(f.body_text(), "New body\nsecond line");
        assert_eq!(f.focus, Focus::Body);
        f.set_from_editor(&text);
        assert_eq!(f.title_text(), "Fix the thing");
        assert_eq!(f.body_text(), "Why\n\nand how");
    }

    #[test]
    fn editor_with_no_title_line_empties_a_new_item() {
        let mut f = TextForm::new(FormKind::NewItem, "New draft", "Old", "body");
        f.set_from_editor("\n\n");
        assert_eq!(f.title_text(), "");
        assert_eq!(f.body_text(), "");
    }

    #[test]
    fn editor_round_trip_for_a_comment() {
        let mut f = TextForm::new(FormKind::Comment, "Comment on #12", "", "");
        type_str(&mut f, "hello");
        f.handle_key(code(KeyCode::Enter));
        type_str(&mut f, "world");
        let text = f.editor_text();
        assert_eq!(text, "hello\nworld");
        f.set_from_editor("hello\nworld\nmore\n");
        assert_eq!(f.body_text(), "hello\nworld\nmore");
        assert_eq!(f.title_text(), "");
    }

    #[test]
    fn equality_compares_text_focus_and_prompt() {
        let a = new_item();
        let mut b = new_item();
        assert_eq!(a, b);
        type_str(&mut b, "x");
        assert_ne!(a, b);
    }

    #[test]
    fn a_message_renders_and_clears_on_the_next_key() {
        let mut f = new_item();
        f.set_message(Some("Empty title; nothing was created".into()));
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        assert!(
            shown.contains("Empty title; nothing was created"),
            "{shown}"
        );
        type_str(&mut f, "a");
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        assert!(!shown.contains("Empty title"), "{shown}");
    }

    #[test]
    fn new_item_form_shows_heading_fields_buttons_and_hint() {
        let mut f = TextForm::new(
            FormKind::NewItem,
            "New issue in acme/widgets",
            "A title",
            "Some body",
        );
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        for want in [
            "New issue in acme/widgets",
            "Title",
            "A title",
            "Body",
            "Some body",
            "[ Create ]",
            "[ Open in editor ]",
            "[ Cancel ]",
            "Tab next · Alt+Enter submit · Esc cancel",
        ] {
            assert!(shown.contains(want), "missing {want:?} in\n{shown}");
        }
    }

    #[test]
    fn title_form_has_no_editor_button() {
        let mut f = TextForm::new(FormKind::Title, "Edit title of #12", "Old title", "");
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        assert!(
            shown.contains("[ Save ]") && shown.contains("[ Cancel ]"),
            "{shown}"
        );
        assert!(
            !shown.contains("Open in editor") && !shown.contains("Body"),
            "{shown}"
        );
    }

    #[test]
    fn a_small_screen_keeps_three_body_rows() {
        let mut f = TextForm::new(FormKind::NewItem, "New draft", "T", "l1\nl2\nl3");
        let shown = render_to_string(50, 12, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        for want in ["l1", "l2", "l3", "[ Create ]", "Tab next"] {
            assert!(shown.contains(want), "missing {want:?} in\n{shown}");
        }
    }

    #[test]
    fn a_long_body_line_wraps_by_word() {
        let body = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
        let mut f = TextForm::new(FormKind::Body, "Edit body of #12", "", body);
        let shown = render_to_string(40, 14, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        for word in body.split(' ') {
            assert!(
                shown.contains(word),
                "word {word:?} split or lost in\n{shown}"
            );
        }
        assert!(
            shown
                .lines()
                .filter(|l| l.contains("alpha") || l.contains("lima"))
                .count()
                == 2,
            "{shown}"
        );
    }

    #[test]
    fn plain_theme_marks_the_focused_button_without_colour() {
        let mut f = new_item();
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        let shown = render_to_string(100, 30, |fr| draw(fr, fr.area(), &mut f, &Theme::plain()));
        assert!(shown.contains("»[ Create ]«"), "{shown}");
        assert!(!shown.contains("»[ Cancel ]«"), "{shown}");
    }
}
