use crate::model::*;
use crate::ui::keymap::Action;
use crate::ui::labels::{label_spans, spans_width};
use crate::ui::markdown::{Target, render_markdown_from};
use crate::ui::text::{display_width, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub struct DetailState {
    pub item: ItemId,
    pub detail: Option<ItemDetail>,
    pub error: Option<String>,
    pub raw: bool,
    pub scroll: usize,
    pub target: Option<usize>,
    /// The last useful scroll offset; the chrome sets it from the doc before rendering.
    pub max_scroll: usize,
    /// An older-comments request is in flight; stops `P` duplicating a page.
    pub loading_older: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetailOutcome {
    None,
    Close,
    Quit,
    /// Follow the target at this index of `DetailDoc::targets`.
    Follow(usize),
    LoadOlder(String),
    OpenInBrowser,
}

impl DetailState {
    pub fn new(item: ItemId) -> Self {
        Self {
            item,
            detail: None,
            error: None,
            raw: false,
            scroll: 0,
            target: None,
            max_scroll: usize::MAX,
            loading_older: false,
        }
    }

    /// `older` responses carry only older comments: they are prepended and the rest is kept.
    pub fn set_detail(&mut self, detail: ItemDetail, older: bool) {
        match (&mut self.detail, older) {
            (Some(existing), true) => {
                let mut comments = detail.comments;
                comments.append(&mut existing.comments);
                existing.comments = comments;
                existing.older_cursor = detail.older_cursor;
            }
            _ => self.detail = Some(detail),
        }
        self.error = None;
        if older {
            self.loading_older = false;
        }
    }

    pub fn handle(&mut self, action: Option<Action>, targets: usize) -> DetailOutcome {
        if self.target.is_some_and(|t| t >= targets) {
            self.target = None;
        }
        let outcome = self.act(action, targets);
        self.scroll = self.scroll.min(self.max_scroll);
        outcome
    }

    fn act(&mut self, action: Option<Action>, targets: usize) -> DetailOutcome {
        match action {
            Some(Action::Down) => self.scroll = self.scroll.saturating_add(1),
            Some(Action::Up) => self.scroll = self.scroll.saturating_sub(1),
            Some(Action::Top) => self.scroll = 0,
            Some(Action::Bottom) => self.scroll = self.max_scroll,
            Some(Action::NextView) if targets > 0 => {
                self.target = Some(self.target.map_or(0, |t| (t + 1) % targets))
            }
            Some(Action::PrevView) if targets > 0 => {
                self.target = Some(
                    self.target
                        .map_or(targets - 1, |t| (t + targets - 1) % targets),
                )
            }
            Some(Action::Open) => {
                if let Some(t) = self.target.filter(|t| *t < targets) {
                    return DetailOutcome::Follow(t);
                }
            }
            Some(Action::ToggleRaw) => self.raw = !self.raw,
            Some(Action::LoadOlder) => {
                if self.loading_older {
                    return DetailOutcome::None;
                }
                if let Some(cursor) = self.detail.as_ref().and_then(|d| d.older_cursor.clone()) {
                    self.loading_older = true;
                    return DetailOutcome::LoadOlder(cursor);
                }
            }
            Some(Action::OpenBrowser) => return DetailOutcome::OpenInBrowser,
            Some(Action::Back) => return DetailOutcome::Close,
            Some(Action::Quit) => return DetailOutcome::Quit,
            _ => {}
        }
        DetailOutcome::None
    }
}

pub struct DetailDoc {
    pub lines: Vec<Line<'static>>,
    pub targets: Vec<Target>,
}

fn state_word(content: &ItemContent) -> &'static str {
    match content {
        ItemContent::Issue {
            state: ContentState::Closed,
            ..
        }
        | ItemContent::PullRequest {
            state: ContentState::Closed,
            ..
        } => "closed",
        ItemContent::PullRequest {
            state: ContentState::Merged,
            ..
        } => "merged",
        ItemContent::PullRequest { is_draft: true, .. } => "draft PR",
        ItemContent::Issue { .. } | ItemContent::PullRequest { .. } => "open",
        ItemContent::Draft { .. } => "draft",
        ItemContent::Redacted => "private",
        ItemContent::Unknown { .. } => "unsupported",
    }
}

/// `Name: value · Name: value` within `width` cells; labels are coloured pills.
fn field_line(item: &Item, project: &Project, width: usize, theme: &Theme) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left = width;
    for f in project.fields.iter().filter(|f| f.kind != FieldKind::Title) {
        let Some(value) = item.value(&f.id) else {
            continue;
        };
        let sep = if spans.is_empty() { "" } else { " · " };
        let prefix = format!("{sep}{}: ", f.name);
        if display_width(&prefix) >= left {
            if left > 0 {
                spans.push(Span::raw(truncate_to_width(&prefix, left)));
            }
            break;
        }
        left -= display_width(&prefix);
        spans.push(Span::raw(prefix));
        let value_spans = match (theme.color, value) {
            (true, FieldValue::Labels(labels)) => label_spans(labels, left, theme),
            _ => vec![Span::raw(truncate_to_width(&value.display(), left))],
        };
        left -= spans_width(&value_spans);
        spans.extend(value_spans);
    }
    Line::from(spans)
}

/// Everything the detail pane shows, as lines at `width`, with the followable targets.
pub fn build_doc(
    item: &Item,
    project: &Project,
    state: &DetailState,
    width: u16,
    theme: &Theme,
) -> DetailDoc {
    let w = width as usize;
    let mut lines: Vec<Line<'static>> = vec![Line::styled(
        truncate_to_width(item.title(), w),
        theme.heading(),
    )];
    let mut meta = vec![item.number().map(|n| format!("#{n}")).unwrap_or_default()];
    if let Some(r) = item.reference() {
        meta.push(r.repo.clone());
    }
    meta.push(state_word(&item.content).to_string());
    if let Some(t) = state.detail.as_ref().and_then(|d| d.issue_type.clone()) {
        meta.push(t);
    }
    meta.retain(|m| !m.is_empty());
    lines.push(Line::styled(
        truncate_to_width(&meta.join(" · "), w),
        theme.dim(),
    ));
    if let Some(parent) = state.detail.as_ref().and_then(|d| d.parent.as_ref()) {
        lines.push(Line::from(truncate_to_width(
            &format!("Parent: #{} {}", parent.number, parent.title),
            w,
        )));
    }
    let fields = field_line(item, project, w, theme);
    if !fields.spans.is_empty() {
        lines.push(fields);
    }
    lines.push(Line::from(""));

    let mut targets = Vec::new();
    let Some(detail) = &state.detail else {
        let msg = state.error.clone().unwrap_or_else(|| "Loading…".into());
        let style = if state.error.is_some() {
            theme.error()
        } else {
            theme.dim()
        };
        lines.push(Line::styled(msg, style));
        return DetailDoc { lines, targets };
    };

    let body = |src: &str, lines: &mut Vec<Line<'static>>, targets: &mut Vec<Target>| {
        if src.trim().is_empty() {
            lines.push(Line::styled("No description.", theme.dim()));
        } else if state.raw {
            for l in src.lines() {
                for piece in wrap_raw(l, w) {
                    lines.push(Line::from(piece));
                }
            }
        } else {
            // Footnote numbers continue across body and comments.
            let r = render_markdown_from(src, width, theme, targets.len());
            lines.extend(r.lines);
            targets.extend(r.targets);
        }
    };
    body(&detail.body, &mut lines, &mut targets);

    if !detail.sub_issues.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Sub-issues ({})", detail.sub_issues.len()),
            theme.bold(),
        ));
        for s in &detail.sub_issues {
            let mark = if s.state == "CLOSED" { "☑" } else { "☐" };
            lines.push(Line::from(truncate_to_width(
                &format!("{mark} #{} {}", s.number, s.title),
                w,
            )));
        }
    }
    if !detail.linked_prs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled("Linked pull requests", theme.bold()));
        for pr in &detail.linked_prs {
            lines.push(Line::from(truncate_to_width(
                &format!("#{} {} ({})", pr.number, pr.title, pr.state.to_lowercase()),
                w,
            )));
        }
    }
    if detail.comments_total > 0 {
        lines.push(Line::from(""));
        let older = if detail.older_cursor.is_some() {
            " · P: load older"
        } else {
            ""
        };
        lines.push(Line::styled(
            truncate_to_width(
                &format!(
                    "Comments ({} of {}){older}",
                    detail.comments.len(),
                    detail.comments_total
                ),
                w,
            ),
            theme.bold(),
        ));
        for c in &detail.comments {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                truncate_to_width(
                    &format!(
                        "@{} · {}",
                        c.author,
                        c.created_at.get(..10).unwrap_or(&c.created_at)
                    ),
                    w,
                ),
                theme.accent(),
            ));
            body(&c.body, &mut lines, &mut targets);
        }
    }
    DetailDoc { lines, targets }
}

/// Greedy wrap on grapheme boundaries; leading spaces of the source line are kept.
fn wrap_raw(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut used = 0;
    for g in line.graphemes(true) {
        let gw = display_width(g);
        if used + gw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push_str(g);
        used += gw;
    }
    out.push(cur);
    out
}

fn footer_height(doc: &DetailDoc, area_height: u16) -> u16 {
    if doc.targets.is_empty() {
        0
    } else {
        (doc.targets.len() as u16 + 1).min(area_height / 3)
    }
}

/// Rows the document body gets in an area of `area_height`; `max_scroll` is
/// `doc.lines.len().saturating_sub(body_height(..))`.
pub fn body_height(doc: &DetailDoc, area_height: u16) -> usize {
    (area_height - footer_height(doc, area_height).min(area_height)).max(1) as usize
}

fn target_label(t: &Target) -> String {
    match t {
        Target::Link(url) => url.clone(),
        Target::IssueRef(n) => format!("#{n}"),
    }
}

pub fn render_detail(
    frame: &mut Frame,
    area: Rect,
    doc: &DetailDoc,
    state: &DetailState,
    theme: &Theme,
) {
    let footer_height = footer_height(doc, area.height);
    let [body, footer] =
        Split::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(area);
    let max_scroll = doc.lines.len().saturating_sub(body.height as usize);
    let scroll = state.scroll.min(max_scroll);
    let visible: Vec<Line> = doc
        .lines
        .iter()
        .skip(scroll)
        .take(body.height as usize)
        .cloned()
        .collect();
    frame.render_widget(Paragraph::new(visible), body);
    if footer_height > 0 {
        let lines: Vec<Line> = doc
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let style = if state.target == Some(i) {
                    theme.selected()
                } else {
                    theme.dim()
                };
                Line::from(Span::styled(
                    truncate_to_width(
                        &format!("[{}] {}", i + 1, target_label(t)),
                        footer.width as usize,
                    ),
                    style,
                ))
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(" links: tab · enter "),
            ),
            footer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{items, project, render_to_string};
    use crate::ui::markdown::line_text;
    use crate::ui::text::display_width;

    fn detail() -> ItemDetail {
        ItemDetail {
            body: "Steps: see [docs](https://example.com) and #2.".into(),
            comments: vec![Comment {
                author: "tviles".into(),
                created_at: "2026-10-01T09:00:00Z".into(),
                body: "Looks bad".into(),
            }],
            comments_total: 21,
            older_cursor: Some("cur".into()),
            sub_issues: vec![LinkedRef {
                number: 4,
                title: "Child".into(),
                state: "CLOSED".into(),
            }],
            parent: Some(LinkedRef {
                number: 3,
                title: "Epic".into(),
                state: "OPEN".into(),
            }),
            issue_type: Some("Bug".into()),
            linked_prs: vec![],
        }
    }

    fn loaded() -> DetailState {
        let mut s = DetailState::new(ItemId::new("a"));
        s.set_detail(detail(), false);
        s
    }

    #[test]
    fn doc_has_header_fields_body_subissues_and_comments() {
        let (p, all) = (project(), items());
        let doc = build_doc(&all[0], &p, &loaded(), 80, &Theme::plain());
        let text: Vec<String> = doc.lines.iter().map(line_text).collect();
        assert_eq!(text[0], "Fix crash");
        assert!(text[1].contains("#1") && text[1].contains("tviles/t") && text[1].contains("Bug"));
        assert!(text.iter().any(|l| l == "Parent: #3 Epic"));
        assert!(
            text.iter()
                .any(|l| l.contains("Status: Todo") && l.contains("Priority: P0"))
        );
        assert!(text.iter().any(|l| l == "☑ #4 Child"));
        assert!(
            text.iter()
                .any(|l| l.contains("Comments (1 of 21)") && l.contains("P: load older"))
        );
        assert!(text.iter().any(|l| l.starts_with("@tviles")));
        assert_eq!(
            doc.targets,
            vec![
                Target::Link("https://example.com".into()),
                Target::IssueRef(2)
            ]
        );
    }

    #[test]
    fn labels_in_the_field_line_are_pills_with_colour_and_plain_text_without() {
        use ratatui::style::Color;
        let (p, all) = (project(), items());
        let coloured = Theme {
            color: true,
            truecolor: true,
        };
        let doc = build_doc(&all[0], &p, &loaded(), 80, &coloured);
        let line = doc
            .lines
            .iter()
            .find(|l| line_text(l).contains("Labels:"))
            .unwrap();
        assert!(line_text(line).contains("Labels:  bug "));
        let pill = line.spans.iter().find(|s| s.content == " bug ").unwrap();
        assert_eq!(pill.style.bg, Some(Color::Rgb(0xd7, 0x3a, 0x4a)));
        let doc = build_doc(&all[0], &p, &loaded(), 80, &Theme::plain());
        assert!(
            doc.lines
                .iter()
                .any(|l| line_text(l).contains("Labels: bug"))
        );
        let narrow = build_doc(&all[0], &p, &loaded(), 20, &coloured);
        assert!(
            narrow
                .lines
                .iter()
                .all(|l| display_width(&line_text(l)) <= 20)
        );
    }

    #[test]
    fn raw_mode_shows_the_markdown_source() {
        let (p, all) = (project(), items());
        let mut s = loaded();
        s.raw = true;
        let text: Vec<String> = build_doc(&all[0], &p, &s, 80, &Theme::plain())
            .lines
            .iter()
            .map(line_text)
            .collect();
        assert!(
            text.iter()
                .any(|l| l.contains("[docs](https://example.com)"))
        );
    }

    #[test]
    fn keys_scroll_cycle_targets_follow_and_close() {
        let mut s = loaded();
        assert_eq!(s.handle(Some(Action::Down), 2), DetailOutcome::None);
        assert_eq!(s.scroll, 1);
        assert_eq!(s.handle(Some(Action::NextView), 2), DetailOutcome::None);
        assert_eq!(s.target, Some(0));
        s.handle(Some(Action::NextView), 2);
        s.handle(Some(Action::NextView), 2);
        assert_eq!(s.target, Some(0), "wraps around");
        assert_eq!(s.handle(Some(Action::Open), 2), DetailOutcome::Follow(0));
        assert_eq!(
            s.handle(Some(Action::LoadOlder), 2),
            DetailOutcome::LoadOlder("cur".into())
        );
        assert_eq!(s.handle(Some(Action::ToggleRaw), 2), DetailOutcome::None);
        assert!(s.raw);
        assert_eq!(s.handle(Some(Action::Back), 2), DetailOutcome::Close);
        assert_eq!(s.handle(Some(Action::Quit), 2), DetailOutcome::Quit);
    }

    #[test]
    fn older_comments_load_once_until_they_arrive() {
        let mut s = loaded();
        s.detail.as_mut().unwrap().older_cursor = Some("cur".into());
        assert_eq!(
            s.handle(Some(Action::LoadOlder), 0),
            DetailOutcome::LoadOlder("cur".into())
        );
        assert_eq!(s.handle(Some(Action::LoadOlder), 0), DetailOutcome::None);
        let more = ItemDetail {
            older_cursor: Some("cur2".into()),
            ..Default::default()
        };
        s.set_detail(more, true);
        assert_eq!(
            s.handle(Some(Action::LoadOlder), 0),
            DetailOutcome::LoadOlder("cur2".into())
        );
    }

    #[test]
    fn raw_lines_wrap_instead_of_truncating() {
        let (p, all) = (project(), items());
        let mut s = loaded();
        s.raw = true;
        s.detail.as_mut().unwrap().body = format!("  {}", "word ".repeat(30).trim_end());
        let doc = build_doc(&all[0], &p, &s, 20, &Theme::plain());
        let text: Vec<String> = doc.lines.iter().map(line_text).collect();
        assert!(text.iter().all(|l| display_width(l) <= 20));
        let start = text.iter().position(|l| l.starts_with("  word")).unwrap();
        let joined: String = text[start..].join("");
        assert!(joined.starts_with("  word word"));
        assert_eq!(joined.matches("word").count(), 30, "{joined}");
    }

    #[test]
    fn footnotes_continue_across_comments_and_literals_are_kept() {
        let (p, all) = (project(), items());
        let mut s = DetailState::new(ItemId::new("a"));
        let mut d = detail();
        d.body = "[a](https://a.example)".into();
        d.comments[0].body = "[b](https://b.example) and literal [3]".into();
        s.set_detail(d, false);
        let doc = build_doc(&all[0], &p, &s, 80, &Theme::plain());
        let text: Vec<String> = doc.lines.iter().map(line_text).collect();
        assert!(
            text.iter()
                .any(|l| l.contains("b[2]") && l.contains("literal [3]")),
            "{text:?}"
        );
        assert_eq!(
            doc.targets,
            vec![
                Target::Link("https://a.example".into()),
                Target::Link("https://b.example".into())
            ]
        );
    }

    #[test]
    fn scroll_is_clamped_to_max_scroll_and_stale_targets_cleared() {
        let mut s = loaded();
        s.max_scroll = 5;
        s.handle(Some(Action::Bottom), 2);
        assert_eq!(s.scroll, 5);
        s.handle(Some(Action::Up), 2);
        assert_eq!(s.scroll, 4);
        for _ in 0..10 {
            s.handle(Some(Action::Down), 2);
        }
        assert_eq!(s.scroll, 5);
        s.target = Some(3);
        s.handle(None, 2);
        assert_eq!(s.target, None);
    }

    #[test]
    fn older_comments_are_prepended() {
        let mut s = loaded();
        let older = ItemDetail {
            comments: vec![Comment {
                author: "x".into(),
                created_at: "2026-09-01T00:00:00Z".into(),
                body: "older".into(),
            }],
            comments_total: 21,
            older_cursor: None,
            ..Default::default()
        };
        s.set_detail(older, true);
        let d = s.detail.as_ref().unwrap();
        assert_eq!(d.comments[0].body, "older");
        assert_eq!(
            d.body, "Steps: see [docs](https://example.com) and #2.",
            "body kept"
        );
        assert_eq!(d.older_cursor, None);
    }

    #[test]
    fn renders_loading_then_content_with_a_target_footer() {
        let (p, all) = (project(), items());
        let loading = DetailState::new(ItemId::new("a"));
        let doc = build_doc(&all[0], &p, &loading, 60, &Theme::plain());
        let screen = render_to_string(60, 12, |f| {
            render_detail(f, f.area(), &doc, &loading, &Theme::plain())
        });
        assert!(screen.contains("Loading"));
        let mut s = loaded();
        s.target = Some(1);
        let doc = build_doc(&all[0], &p, &s, 60, &Theme::plain());
        let screen = render_to_string(60, 30, |f| {
            render_detail(f, f.area(), &doc, &s, &Theme::plain())
        });
        assert!(screen.contains("[2] #2"), "{screen}");
    }
}
