use crate::ui::text::{display_width, truncate_to_width};
use crate::ui::theme::Theme;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Link(String),
    IssueRef(u32),
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub targets: Vec<Target>,
}

pub fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    lines: Vec<Line<'static>>,
    current: Vec<(String, Style)>,
    styles: Vec<Style>,
    prefixes: Vec<String>,
    lists: Vec<Option<u64>>,
    bullet: Option<String>,
    in_code: bool,
    link: Option<String>,
    targets: Vec<Target>,
    table_row: Vec<String>,
    table_cell: Option<String>,
}

impl<'t> Renderer<'t> {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |acc, s| acc.patch(*s))
    }

    /// Block prefix (quotes and list indentation), cut to at most half the width so deep
    /// nesting never squeezes the text out.
    fn prefix(&self) -> String {
        self.prefix_within(self.width / 2)
    }

    fn prefix_within(&self, limit: usize) -> String {
        let mut out = String::new();
        for ch in self.prefixes.concat().chars() {
            if display_width(&out) + display_width(&ch.to_string()) > limit {
                break;
            }
            out.push(ch);
        }
        out
    }

    /// All inline output goes through here so table cells collect it instead of the line buffer.
    fn inline(&mut self, (text, style): (String, Style)) {
        match self.table_cell.as_mut() {
            Some(cell) => cell.push_str(&text),
            None => self.current.push((text, style)),
        }
    }

    fn push_text(&mut self, text: &str) {
        if let Some(cell) = self.table_cell.as_mut() {
            cell.push_str(text);
            return;
        }
        let style = self.style();
        let mut rest = text;
        // Split out `#123` references that start a word.
        while let Some(pos) = rest.find('#') {
            let starts_word =
                pos == 0 || rest[..pos].ends_with(|c: char| c.is_whitespace() || c == '(');
            let digits: String = rest[pos + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let (before, after) = rest.split_at(pos);
            let number = digits.parse::<u32>().ok();
            if starts_word && number.is_some() && self.link.is_none() {
                self.current.push((before.to_string(), style));
                self.current
                    .push((format!("#{digits}"), style.patch(self.theme.accent())));
                self.targets.push(Target::IssueRef(number.unwrap_or(0)));
                rest = &after[1 + digits.len()..];
            } else {
                self.current.push((rest[..pos + 1].to_string(), style));
                rest = &rest[pos + 1..];
            }
        }
        if !rest.is_empty() {
            self.current.push((rest.to_string(), style));
        }
    }

    fn flush(&mut self) {
        if self.current.is_empty() && self.bullet.is_none() {
            return;
        }
        let mut bullet = self.bullet.take().unwrap_or_default();
        // Keep at least half the width for text: an over-wide bullet (a 9-digit list number)
        // is elided, and the nesting prefix shrinks to fit beside it.
        let bullet_cap = self.width / 4;
        if display_width(&bullet) > bullet_cap {
            bullet = truncate_to_width(&bullet, bullet_cap);
        }
        let base = self.prefix_within((self.width / 2).saturating_sub(display_width(&bullet)));
        let first = format!("{base}{bullet}");
        let rest_prefix = format!("{base}{}", " ".repeat(display_width(&bullet)));
        let tokens = std::mem::take(&mut self.current);
        self.lines
            .extend(wrap(tokens, &first, &rest_prefix, self.width, self.theme));
    }

    fn blank(&mut self) {
        if self
            .lines
            .last()
            .is_some_and(|l| !line_text(l).trim().is_empty())
        {
            self.lines.push(Line::from(""));
        }
    }

    fn code_line(&mut self, text: &str) {
        let line = truncate_to_width(&format!("{}  {text}", self.prefix()), self.width);
        self.lines
            .push(Line::from(Span::styled(line, self.theme.code())));
    }
}

/// Word-wraps styled tokens. Words longer than a line are split by grapheme clusters.
fn wrap(
    tokens: Vec<(String, Style)>,
    first: &str,
    rest: &str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![Span::styled(first.to_string(), theme.dim())];
    let mut used = display_width(first);
    let mut prefix_width = used;
    for (text, style) in tokens {
        for word in text.split_inclusive(' ') {
            let w = display_width(word);
            if used + display_width(word.trim_end()) > width && used > prefix_width {
                lines.push(finish(std::mem::take(&mut spans)));
                spans.push(Span::styled(rest.to_string(), theme.dim()));
                used = display_width(rest);
                prefix_width = used;
                let trimmed = word.trim_start();
                if trimmed.is_empty() {
                    continue;
                }
            }
            if used + display_width(word.trim_end()) > width {
                // A single word wider than the line: cut it into pieces.
                let mut piece = String::new();
                for ch in word.graphemes(true) {
                    let cw = display_width(ch);
                    if used + cw > width {
                        spans.push(Span::styled(std::mem::take(&mut piece), style));
                        lines.push(finish(std::mem::take(&mut spans)));
                        spans.push(Span::styled(rest.to_string(), theme.dim()));
                        used = display_width(rest);
                        prefix_width = used;
                    }
                    piece.push_str(ch);
                    used += cw;
                }
                spans.push(Span::styled(piece, style));
            } else {
                spans.push(Span::styled(word.to_string(), style));
                used += w;
            }
        }
    }
    if used > prefix_width || lines.is_empty() {
        lines.push(finish(spans));
    }
    lines
}

/// Builds a line, dropping the trailing space left by word splitting.
fn finish(mut spans: Vec<Span<'static>>) -> Line<'static> {
    if spans.len() > 1 {
        if let Some(last) = spans.last_mut() {
            let trimmed = last.content.trim_end_matches(' ').to_string();
            last.content = trimmed.into();
        }
    }
    Line::from(spans)
}

pub fn render_markdown(src: &str, width: u16, theme: &Theme) -> Rendered {
    render_markdown_from(src, width, theme, 0)
}

/// Like `render_markdown`, but footnote markers are numbered `first_target + index + 1`, so
/// several documents can share one list of targets.
pub fn render_markdown_from(src: &str, width: u16, theme: &Theme, first_target: usize) -> Rendered {
    let mut r = Renderer {
        // The screen layout shows "widen the pane" below 40 columns, so widths under 10 never arrive.
        width: (width as usize).max(10),
        theme,
        lines: Vec::new(),
        current: Vec::new(),
        styles: Vec::new(),
        prefixes: Vec::new(),
        lists: Vec::new(),
        bullet: None,
        in_code: false,
        link: None,
        targets: Vec::new(),
        table_row: Vec::new(),
        table_cell: None,
    };
    let options =
        Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(src, options) {
        match event {
            Event::Start(Tag::Heading { .. }) => r.styles.push(theme.heading()),
            Event::End(TagEnd::Heading(_)) => {
                r.styles.pop();
                r.flush();
                r.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                r.flush();
                if r.lists.is_empty() {
                    r.blank();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                r.flush();
                r.prefixes.push("│ ".into());
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                r.flush();
                r.prefixes.pop();
                r.blank();
            }
            Event::Start(Tag::List(start)) => {
                r.flush();
                r.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                r.flush();
                r.lists.pop();
                if r.lists.is_empty() {
                    r.blank();
                }
            }
            Event::Start(Tag::Item) => {
                r.flush();
                let bullet = match r.lists.last_mut() {
                    Some(Some(n)) => {
                        let b = format!("{n}. ");
                        *n += 1;
                        b
                    }
                    _ => "• ".to_string(),
                };
                r.bullet = Some(bullet);
                // Each nesting level below the first indents by two cells.
                let indent = if r.lists.len() > 1 { "  " } else { "" };
                r.prefixes.push(indent.to_string());
            }
            Event::End(TagEnd::Item) => {
                r.flush();
                r.prefixes.pop();
            }
            Event::TaskListMarker(done) => {
                r.bullet = Some(if done { "☑ " } else { "☐ " }.to_string())
            }
            Event::Start(Tag::Emphasis) => r
                .styles
                .push(Style::default().add_modifier(ratatui::style::Modifier::ITALIC)),
            Event::Start(Tag::Strong) => r.styles.push(theme.bold()),
            Event::Start(Tag::Strikethrough) => r
                .styles
                .push(Style::default().add_modifier(ratatui::style::Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Strikethrough) => {
                r.styles.pop();
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                r.styles.push(theme.link());
                r.link = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Link) => {
                r.styles.pop();
                if let Some(dest) = r.link.take() {
                    r.targets.push(Target::Link(dest));
                    let n = first_target + r.targets.len();
                    r.inline((format!("[{n}]"), theme.dim()));
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                r.link = Some(dest_url.to_string());
                r.inline(("[image: ".into(), theme.dim()));
            }
            Event::End(TagEnd::Image) => {
                r.inline(("]".into(), theme.dim()));
                if let Some(dest) = r.link.take() {
                    r.targets.push(Target::Link(dest));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                r.flush();
                r.in_code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                r.in_code = false;
                r.blank();
            }
            Event::Text(t) if r.in_code => {
                for line in t.lines() {
                    r.code_line(line);
                }
            }
            Event::Text(t) => r.push_text(&t),
            Event::Code(t) => {
                let style = r.style().patch(theme.code());
                r.inline((t.to_string(), style));
            }
            Event::Html(t) => {
                r.flush();
                for line in t.lines() {
                    r.inline((line.to_string(), theme.dim()));
                    r.flush();
                }
            }
            Event::InlineHtml(t) => r.inline((t.to_string(), theme.dim())),
            Event::SoftBreak => {
                let style = r.style();
                r.inline((" ".into(), style));
            }
            Event::HardBreak => r.flush(),
            Event::Rule => {
                r.flush();
                let rule = "─".repeat(r.width);
                r.lines.push(Line::from(Span::styled(rule, theme.dim())));
            }
            Event::Start(Tag::Table(_)) => r.flush(),
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => r.table_row.clear(),
            Event::Start(Tag::TableCell) => r.table_cell = Some(String::new()),
            Event::End(TagEnd::TableCell) => {
                let cell = r.table_cell.take().unwrap_or_default();
                r.table_row.push(cell.trim().to_string());
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                let row = format!("{}{}", r.prefix(), r.table_row.join(" │ "));
                r.lines.push(Line::from(truncate_to_width(&row, r.width)));
            }
            Event::End(TagEnd::Table) => r.blank(),
            _ => {}
        }
    }
    r.flush();
    while r
        .lines
        .last()
        .is_some_and(|l| line_text(l).trim().is_empty())
    {
        r.lines.pop();
    }
    Rendered {
        lines: r.lines,
        targets: r.targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(md: &str, width: u16) -> (Vec<String>, Vec<Target>) {
        let r = render_markdown(md, width, &Theme::plain());
        (r.lines.iter().map(line_text).collect(), r.targets)
    }

    #[test]
    fn paragraphs_wrap_at_width() {
        let (lines, _) = text("one two three four five six seven eight nine ten", 20);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|l| display_width(l) <= 20), "{lines:?}");
    }

    #[test]
    fn headings_lists_and_task_lists() {
        let (lines, _) = text(
            "# Title\n\n- one\n- two\n\n1. first\n2. second\n\n- [x] done\n- [ ] open",
            40,
        );
        assert!(lines.contains(&"Title".to_string()));
        assert!(lines.contains(&"• one".to_string()));
        assert!(lines.contains(&"2. second".to_string()));
        assert!(lines.contains(&"☑ done".to_string()));
        assert!(lines.contains(&"☐ open".to_string()));
    }

    #[test]
    fn code_blocks_keep_their_lines() {
        let (lines, _) = text("```rust\nfn main() {\n    run();\n}\n```", 40);
        assert!(lines.iter().any(|l| l.trim_end() == "  fn main() {"));
        assert!(lines.iter().any(|l| l.trim_end() == "      run();"));
    }

    #[test]
    fn links_get_numbered_footnotes_and_issue_refs_are_targets() {
        let (lines, targets) = text("See [docs](https://example.com) and #12, not a#3.", 60);
        assert!(lines[0].contains("docs[1]"), "{lines:?}");
        assert_eq!(
            targets,
            vec![
                Target::Link("https://example.com".into()),
                Target::IssueRef(12)
            ]
        );
    }

    #[test]
    fn images_show_alt_text() {
        let (lines, targets) = text("![screenshot](https://example.com/a.png)", 60);
        assert!(lines[0].contains("[image: screenshot]"));
        assert_eq!(
            targets,
            vec![Target::Link("https://example.com/a.png".into())]
        );
    }

    #[test]
    fn quotes_are_prefixed() {
        let (lines, _) = text("> quoted text", 40);
        assert_eq!(lines[0], "│ quoted text");
    }

    /// Review Focus 5: raw HTML and deep nesting render without panicking or overflowing.
    #[test]
    fn html_and_deep_nesting_stay_inside_the_width() {
        let md = "<details><summary>More</summary>hidden</details>\n\n<img src=\"x.png\">\n\n- a\n  - b\n    - c\n      - d\n        - e very long text that has to wrap somewhere sensible\n";
        for width in [12u16, 20, 40] {
            let (lines, _) = text(md, width);
            assert!(
                lines.iter().any(|l| l.contains("<details>")),
                "html kept as text"
            );
            assert!(
                lines.iter().all(|l| display_width(l) <= width as usize),
                "width {width}: {lines:?}"
            );
        }
    }

    #[test]
    fn tables_render_as_rows() {
        let (lines, _) = text("| a | b |\n|---|---|\n| 1 | 2 |", 40);
        assert!(lines.iter().any(|l| l.contains("a │ b")));
        assert!(lines.iter().any(|l| l.contains("1 │ 2")));
    }

    #[test]
    fn long_emoji_words_are_never_split_mid_grapheme() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let md = family.repeat(8);
        let (lines, _) = text(&md, 12);
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|l| display_width(l) <= 12), "{lines:?}");
        assert!(
            lines
                .iter()
                .all(|l| l.matches(family).count() * family.len() == l.len()),
            "{lines:?}"
        );
    }

    #[test]
    fn block_html_does_not_merge_with_the_next_block() {
        let (lines, _) = text("<img src=\"x.png\">\n\ntext", 40);
        assert_eq!(lines[0], "<img src=\"x.png\">");
        assert!(lines.contains(&"text".to_string()), "{lines:?}");
        let (lines, _) = text("<div>\n<b>x</b>\n</div>\n\n# Head", 40);
        assert!(lines.contains(&"<b>x</b>".to_string()), "{lines:?}");
        assert!(lines.contains(&"Head".to_string()), "{lines:?}");
    }

    #[test]
    fn table_cells_keep_inline_code_and_links() {
        let (lines, targets) = text("| a | b |\n|---|---|\n| `x` | [l](u) |", 40);
        assert!(lines.iter().any(|l| l.contains("x │ l[1]")), "{lines:?}");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(targets, vec![Target::Link("u".into())]);
    }

    #[test]
    fn wide_list_numbers_stay_inside_the_width() {
        let md = format!(
            "123456789. a\n\n{}123456789. b\n\n{}123456789. item text\n",
            " ".repeat(11),
            " ".repeat(22)
        );
        for width in [12u16, 20] {
            let (lines, _) = text(&md, width);
            assert!(
                lines.iter().all(|l| display_width(l) <= width as usize),
                "width {width}: {lines:?}"
            );
            assert!(lines.iter().any(|l| l.contains("item")), "{lines:?}");
        }
    }

    #[test]
    fn exact_fit_words_are_not_cut() {
        let (lines, _) = text("aaaa bbbbb cc dd", 10);
        assert_eq!(lines, vec!["aaaa bbbbb", "cc dd"]);
    }

    #[test]
    fn huge_issue_numbers_are_not_targets() {
        let (_, targets) = text("see #99999999999999999999", 40);
        assert!(targets.is_empty());
    }
}
