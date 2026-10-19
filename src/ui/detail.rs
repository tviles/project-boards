use crate::linear::ids;
use crate::model::*;
use crate::ui::keymap::Action;
use crate::ui::labels::{label_spans, spans_width};
use crate::ui::markdown::{RefStyle, Target, render_markdown_from};
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

/// The rows a Linear item's detail shows under its meta line, when set: field id and name.
const LINEAR_ROWS: [(&str, &str); 5] = [
    (ids::PROJECT, "Project"),
    (ids::CYCLE, "Cycle"),
    (ids::PRIORITY, "Priority"),
    (ids::ESTIMATE, "Estimate"),
    (ids::DUE, "Due date"),
];

/// `Name: value · Name: value` within `width` cells; labels are coloured pills. Fields in
/// `skip` are left out.
fn field_line(
    item: &Item,
    project: &Project,
    skip: &[&str],
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut left = width;
    let mut overflow = false;
    for f in project
        .fields
        .iter()
        .filter(|f| f.kind != FieldKind::Title && !skip.contains(&f.id.as_str()))
    {
        let Some(value) = item.value(&f.id) else {
            continue;
        };
        let sep = if spans.is_empty() { "" } else { " · " };
        let prefix = format!("{sep}{}: ", f.name);
        if display_width(&prefix) >= left {
            overflow = true;
            break;
        }
        left -= display_width(&prefix);
        spans.push(Span::raw(prefix));
        let value_spans = match (theme.color, value) {
            (true, FieldValue::Labels(labels)) => {
                let labels: Vec<&Label> = labels.iter().collect();
                label_spans(&labels, left, theme, false)
            }
            _ => vec![Span::raw(truncate_to_width(&value.display(), left))],
        };
        left -= spans_width(&value_spans);
        spans.extend(value_spans);
    }
    if overflow {
        ellipsize(&mut spans, left);
    }
    Line::from(spans)
}

/// Marks a line that dropped later fields: `…` in the free cell, or in place of the last
/// cell when the line is full. Nothing changes if the line already ends with one.
fn ellipsize(spans: &mut Vec<Span<'static>>, left: usize) {
    if spans.last().is_some_and(|s| s.content.ends_with('…')) {
        return;
    }
    if left > 0 {
        spans.push(Span::raw("…"));
        return;
    }
    if let Some(last) = spans.last_mut() {
        let keep = display_width(&last.content).saturating_sub(1);
        let text = truncate_to_width(&format!("{}…", last.content), keep + 1);
        let text = if text.ends_with('…') {
            text
        } else {
            format!("{}…", truncate_to_width(&last.content, keep))
        };
        *last = Span::styled(text, last.style);
    }
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
    let linear = project.board.provider() == Provider::Linear;
    let mut lines: Vec<Line<'static>> = vec![Line::styled(
        truncate_to_width(item.title(), w),
        theme.heading(),
    )];
    let mut meta = vec![item.reference_label().unwrap_or_default()];
    // A key names its container already: `TBA-7` is in team TBA.
    if let Some(r) = item.reference().filter(|r| r.key.is_none()) {
        meta.push(r.container.clone());
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
    let skip: &[&str] = if linear {
        &LINEAR_ROWS.map(|(id, _)| id)
    } else {
        &[]
    };
    if linear {
        for (id, name) in LINEAR_ROWS {
            if let Some(value) = item.value(&FieldId::new(id)) {
                lines.push(Line::from(truncate_to_width(
                    &format!("{name}: {}", value.display()),
                    w,
                )));
            }
        }
    }
    if let Some(parent) = state.detail.as_ref().and_then(|d| d.parent.as_ref()) {
        lines.push(Line::from(truncate_to_width(
            &format!(
                "Parent: {} {}{}",
                parent.label(),
                parent.title,
                state_suffix(parent, linear)
            ),
            w,
        )));
    }
    let fields = field_line(item, project, skip, w, theme);
    if !fields.spans.is_empty() {
        lines.push(fields);
    }
    lines.push(Line::from(""));

    let mut targets = Vec::new();
    let Some(detail) = &state.detail else {
        let msg = state.error.clone().unwrap_or_else(|| {
            if crate::model::is_placeholder(&state.item) {
                "Creating on GitHub…".into()
            } else {
                "Loading…".into()
            }
        });
        let style = if state.error.is_some() {
            theme.error()
        } else {
            theme.dim()
        };
        lines.push(Line::styled(msg, style));
        return DetailDoc { lines, targets };
    };

    let refs = match &project.linear {
        _ if !linear => RefStyle::Github,
        Some(meta) => RefStyle::Keys(meta.team_keys.clone()),
        None => RefStyle::Keys(Vec::new()),
    };
    // `indent` cells of space go before every line (a reply's), and the text wraps in the rest.
    let body =
        |src: &str, indent: usize, lines: &mut Vec<Line<'static>>, targets: &mut Vec<Target>| {
            let w = w.saturating_sub(indent);
            let mut out = Vec::new();
            if src.trim().is_empty() {
                out.push(Line::styled("No description.", theme.dim()));
            } else if state.raw {
                for l in src.lines() {
                    for piece in wrap_raw(l, w) {
                        out.push(Line::from(piece));
                    }
                }
            } else {
                // Footnote numbers continue across body, links and comments.
                let r = render_markdown_from(src, w as u16, theme, targets.len(), &refs);
                out.extend(r.lines);
                targets.extend(r.targets);
            }
            if indent > 0 {
                for line in &mut out {
                    line.spans.insert(0, Span::raw(" ".repeat(indent)));
                }
            }
            lines.extend(out);
        };
    body(&detail.body, 0, &mut lines, &mut targets);

    if !detail.sub_issues.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Sub-issues ({})", detail.sub_issues.len()),
            theme.bold(),
        ));
        for s in &detail.sub_issues {
            // Linear names the state; GitHub's sub-issues are open or closed.
            let text = if linear {
                format!("{} {}{}", s.label(), s.title, state_suffix(s, linear))
            } else {
                let mark = if s.state == "CLOSED" { "☑" } else { "☐" };
                format!("{mark} {} {}", s.label(), s.title)
            };
            lines.push(Line::from(truncate_to_width(&text, w)));
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
    if !detail.attachments.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::styled("Links", theme.bold()));
        for a in &detail.attachments {
            targets.push(Target::Link(a.url.clone()));
            let pull = if is_github_pull(&a.url) { "⑂ " } else { "" };
            let title = if a.title.trim().is_empty() {
                &a.url
            } else {
                &a.title
            };
            // The footnote number stays; the title is cut first.
            let number = format!(" [{}]", targets.len());
            let room = w.saturating_sub(display_width(pull) + display_width(&number));
            lines.push(Line::from(vec![
                Span::raw(format!("{pull}{}", truncate_to_width(title, room))),
                Span::styled(number, theme.dim()),
            ]));
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
            // A reply sits one level (two cells) under its thread's first comment.
            let indent = 2 * usize::from(c.depth.min(1));
            // Linear authors are display names, GitHub's are logins.
            let at = if linear { "" } else { "@" };
            lines.push(Line::from(""));
            lines.push(Line::styled(
                truncate_to_width(
                    &format!(
                        "{}{at}{} · {}",
                        " ".repeat(indent),
                        c.author,
                        c.created_at.get(..10).unwrap_or(&c.created_at)
                    ),
                    w,
                ),
                theme.accent(),
            ));
            body(&c.body, indent, &mut lines, &mut targets);
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
        Target::Key(k) => k.clone(),
    }
}

/// ` (state)` after a Linear parent or sub-issue; nothing on GitHub or without a state.
fn state_suffix(r: &LinkedRef, linear: bool) -> String {
    if linear && !r.state.is_empty() {
        format!(" ({})", r.state)
    } else {
        String::new()
    }
}

/// Whether `url` is a GitHub pull request: `github.com/<owner>/<repo>/pull/<number>`.
fn is_github_pull(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let mut parts = path.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("github.com"), Some(owner), Some(repo), Some("pull"), Some(n))
            if !owner.is_empty() && !repo.is_empty()
                && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    )
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
                depth: 0,
            }],
            comments_total: 21,
            older_cursor: Some("cur".into()),
            sub_issues: vec![LinkedRef {
                number: 4,
                title: "Child".into(),
                state: "CLOSED".into(),
                key: None,
            }],
            parent: Some(LinkedRef {
                number: 3,
                title: "Epic".into(),
                state: "OPEN".into(),
                key: None,
            }),
            issue_type: Some("Bug".into()),
            linked_prs: vec![],
            attachments: vec![],
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
        assert!(line_text(line).contains("Labels: bug"));
        let pill = line.spans.iter().find(|s| s.content == "bug").unwrap();
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
    fn a_full_field_line_ends_with_an_ellipsis_plain_and_coloured() {
        let (p, all) = (project(), items());
        let coloured = Theme {
            color: true,
            truecolor: true,
        };
        for theme in [Theme::plain(), coloured] {
            for width in [14u16, 20, 28, 34] {
                let doc = build_doc(&all[0], &p, &loaded(), width, &theme);
                let line = doc
                    .lines
                    .iter()
                    .map(line_text)
                    .find(|l| l.starts_with("Status:"))
                    .unwrap();
                assert!(line.ends_with('…'), "width {width}: {line:?}");
                assert!(display_width(&line) <= width as usize, "{line:?}");
            }
            let wide = build_doc(&all[0], &p, &loaded(), 80, &theme);
            assert!(
                wide.lines
                    .iter()
                    .map(line_text)
                    .any(|l| l.starts_with("Status:") && !l.ends_with('…'))
            );
        }
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
                depth: 0,
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

    #[test]
    fn a_new_items_detail_heading_says_new_not_zero() {
        let (p, all) = (project(), items());
        let mut placeholder = all[0].clone();
        placeholder.id = ItemId::new("local:1");
        if let ItemContent::Issue { reference, .. } = &mut placeholder.content {
            reference.number = 0;
        }
        let state = DetailState::new(ItemId::new("local:1"));
        let doc = build_doc(&placeholder, &p, &state, 60, &Theme::plain());
        let screen = render_to_string(60, 12, |f| {
            render_detail(f, f.area(), &doc, &state, &Theme::plain())
        });
        assert!(screen.contains("new · tviles/t"), "{screen}");
        assert!(!screen.contains("#0"), "{screen}");
    }

    #[test]
    fn a_new_items_detail_says_it_is_being_created() {
        let (p, all) = (project(), items());
        let mut placeholder = all[0].clone();
        placeholder.id = ItemId::new("local:1");
        let state = DetailState::new(ItemId::new("local:1"));
        let doc = build_doc(&placeholder, &p, &state, 60, &Theme::plain());
        let screen = render_to_string(60, 12, |f| {
            render_detail(f, f.area(), &doc, &state, &Theme::plain())
        });
        assert!(screen.contains("Creating on GitHub"), "{screen}");
        assert!(!screen.contains("Loading"));
    }

    #[test]
    fn detail_shows_linear_fields_links_and_threaded_comments() {
        use crate::linear::ids;
        let mut p = project();
        p.board = "linear:team:t1".parse().unwrap();
        p.linear = Some(LinearMeta {
            url_key: "testbed-workspace".into(),
            viewer_id: "u-1".into(),
            team_keys: vec!["TBA".into()],
        });
        let field = |id: &str, name: &str, kind: FieldKind| Field {
            id: FieldId::new(id),
            name: name.into(),
            kind,
        };
        p.fields.extend([
            field(
                ids::PRIORITY,
                "Priority",
                FieldKind::SingleSelect { options: vec![] },
            ),
            field(ids::ESTIMATE, "Estimate", FieldKind::Number),
            field(ids::DUE, "Due date", FieldKind::Date),
            field(
                ids::PROJECT,
                "Project",
                FieldKind::SingleSelect { options: vec![] },
            ),
        ]);
        let mut item = items()[1].clone();
        item.content = ItemContent::Issue {
            reference: ContentRef {
                container: "TBA".into(),
                number: 7,
                key: Some("TBA-7".into()),
                url: "https://linear.app/testbed-workspace/issue/TBA-7".into(),
            },
            title: "Fix login".into(),
            state: ContentState::Open,
        };
        let select = |id: &str, name: &str| FieldValue::SingleSelect {
            option_id: OptionId::new(id),
            name: name.into(),
        };
        // The fixture's own Priority field would read as a repeat.
        item.values.remove(&FieldId::new("F_prio"));
        item.values.extend([
            (FieldId::new(ids::PROJECT), select("p1", "Website")),
            (
                FieldId::new(ids::CYCLE),
                FieldValue::Iteration {
                    iteration_id: IterationId::new("c5"),
                    title: "Cycle 5".into(),
                    start_date: "2026-10-12".into(),
                },
            ),
            (
                FieldId::new(ids::PRIORITY),
                select("linear:priority:2", "High"),
            ),
            (FieldId::new(ids::ESTIMATE), FieldValue::Number(3.0)),
            (
                FieldId::new(ids::DUE),
                FieldValue::Date("2026-10-30".into()),
            ),
        ]);
        let comment = |author: &str, day: &str, body: &str, depth: u8| Comment {
            author: author.into(),
            created_at: format!("2026-10-{day}T09:00:00Z"),
            body: body.into(),
            depth,
        };
        let mut state = DetailState::new(item.id.clone());
        state.set_detail(
            ItemDetail {
                body: "Blocks TBA-8, not XYZ-1 or #12. See [spec](https://example.com/spec)."
                    .into(),
                comments: vec![
                    comment("Ada Lovelace", "02", "Looks good, see TBA-9.", 0),
                    comment("Grace Hopper", "03", "Agreed.", 1),
                ],
                comments_total: 2,
                older_cursor: None,
                sub_issues: vec![
                    LinkedRef {
                        number: 8,
                        title: "Style the button".into(),
                        state: "Done".into(),
                        key: Some("TBA-8".into()),
                    },
                    LinkedRef {
                        number: 9,
                        title: "Write the copy".into(),
                        state: "Todo".into(),
                        key: Some("TBA-9".into()),
                    },
                ],
                parent: Some(LinkedRef {
                    number: 2,
                    title: "Login epic".into(),
                    state: "In Progress".into(),
                    key: Some("TBA-2".into()),
                }),
                issue_type: None,
                linked_prs: vec![],
                attachments: vec![
                    Attachment {
                        title: "Fix login".into(),
                        url: "https://github.com/acme/web/pull/12".into(),
                    },
                    Attachment {
                        title: "Design".into(),
                        url: "https://www.figma.com/file/abc".into(),
                    },
                ],
            },
            false,
        );
        let doc = build_doc(&item, &p, &state, 60, &Theme::plain());
        let text: Vec<String> = doc.lines.iter().map(line_text).collect();
        assert_eq!(text[1], "TBA-7 · open", "the key already names the team");
        assert_eq!(
            text[2..8],
            [
                "Project: Website",
                "Cycle: Cycle 5",
                "Priority: High",
                "Estimate: 3",
                "Due date: 2026-10-30",
                "Parent: TBA-2 Login epic (In Progress)",
            ]
        );
        assert_eq!(text[8], "Status: In Progress", "the rows are not repeated");
        assert!(text.iter().any(|l| l == "TBA-8 Style the button (Done)"));
        assert!(text.iter().any(|l| l == "⑂ Fix login [3]"), "{text:?}");
        assert!(text.iter().any(|l| l == "Design [4]"), "{text:?}");
        assert!(text.iter().any(|l| l == "Ada Lovelace · 2026-10-02"));
        assert!(text.iter().any(|l| l == "  Grace Hopper · 2026-10-03"));
        assert!(text.iter().any(|l| l == "  Agreed."));
        assert_eq!(
            doc.targets,
            vec![
                Target::Key("TBA-8".into()),
                Target::Link("https://example.com/spec".into()),
                Target::Link("https://github.com/acme/web/pull/12".into()),
                Target::Link("https://www.figma.com/file/abc".into()),
                Target::Key("TBA-9".into()),
            ]
        );
        let screen = render_to_string(60, 40, |f| {
            render_detail(f, f.area(), &doc, &state, &Theme::plain())
        });
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn only_github_pull_request_urls_are_pull_requests() {
        assert!(is_github_pull("https://github.com/acme/web/pull/12"));
        assert!(is_github_pull("https://github.com/acme/web/pull/12/files"));
        assert!(is_github_pull("http://www.github.com/acme/web/pull/3#top"));
        assert!(!is_github_pull("https://github.com/acme/web/issues/12"));
        assert!(!is_github_pull("https://github.com/acme/web/pull/x"));
        assert!(!is_github_pull("https://github.com/acme/pull/12"));
        assert!(!is_github_pull("https://gitlab.com/acme/web/pull/12"));
        assert!(!is_github_pull("https://notgithub.com/acme/web/pull/12"));
    }
}
