//! A board card's field values drawn as pills, the way GitHub's cards show them.

use crate::model::*;
use crate::ui::text::{display_width, sanitize, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::style::Style;
use ratatui::text::Span;

/// Most lines a card's field pills take; the last one ends with `+N` when pills are left out.
pub const MAX_PILL_LINES: usize = 4;

/// One pill: its parts, each with its own style over the pill background.
#[derive(Debug, Clone, PartialEq)]
pub struct Pill {
    parts: Vec<(String, Style)>,
}

impl Pill {
    /// A pill of one part. Control characters in `text` become spaces.
    pub fn plain(text: &str, style: Style) -> Self {
        Self {
            parts: vec![(sanitize(text), style)],
        }
    }

    fn width(&self) -> usize {
        self.parts.iter().map(|(t, _)| display_width(t)).sum()
    }

    /// At most `width` cells, cut with `…` in the part where the cut falls.
    fn truncated(&self, width: usize) -> Self {
        let full: String = self.parts.iter().map(|(t, _)| t.as_str()).collect();
        let cut = truncate_to_width(&full, width);
        // The cut is a prefix of `full`, plus `…` when anything was dropped.
        let (mut keep, ellipsis) = match cut.strip_suffix('…') {
            Some(prefix) if prefix.len() < full.len() => (prefix.len(), true),
            _ => (cut.len(), false),
        };
        let mut parts = Vec::new();
        for (text, style) in &self.parts {
            if keep == 0 {
                break;
            }
            let n = keep.min(text.len());
            parts.push((text[..n].to_string(), *style));
            keep -= n;
        }
        if ellipsis {
            match parts.last_mut() {
                Some((text, _)) => text.push('…'),
                None => parts.push(("…".into(), self.parts[0].1)),
            }
        }
        Self { parts }
    }
}

/// The fields a card shows as pills: the view's visible fields in its order, without those
/// shown elsewhere on the card (Title, Assignees, Repository, Labels) or by the board itself
/// (the column field and the lane field).
pub fn card_fields<'a>(
    view: &View,
    project: &'a Project,
    column: &Field,
    lane: Option<&Field>,
) -> Vec<&'a Field> {
    view.visible_fields
        .iter()
        .filter_map(|id| project.field(id))
        .filter(|f| {
            !matches!(
                f.kind,
                FieldKind::Title | FieldKind::Assignees | FieldKind::Repository | FieldKind::Labels
            )
        })
        .filter(|f| f.id != column.id && lane.is_none_or(|l| l.id != f.id))
        .collect()
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `YYYY-MM-DD…` as `Mon D, YYYY`; anything else (a month outside 1–12 or a day outside
/// 1–31 included) as it is.
fn format_date(date: &str) -> String {
    let parsed = (|| {
        let year = date.get(..4)?.parse::<u32>().ok()?;
        let month = date.get(5..7)?.parse::<usize>().ok()?;
        let day = date
            .get(8..10)?
            .parse::<u32>()
            .ok()
            .filter(|d| (1..=31).contains(d))?;
        let name = MONTHS.get(month.checked_sub(1)?)?;
        (date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-')
            .then(|| format!("{name} {day}, {year}"))
    })();
    parsed.unwrap_or_else(|| date.to_string())
}

/// GitHub's colour for an issue or pull request in `state`.
fn state_color(state: ContentState) -> OptionColor {
    match state {
        ContentState::Open => OptionColor::Green,
        ContentState::Closed | ContentState::Merged => OptionColor::Purple,
        ContentState::Unknown => OptionColor::Unknown,
    }
}

fn pr_color(pr: &LinkedPullRequest) -> OptionColor {
    match pr.state {
        ContentState::Open if pr.is_draft => OptionColor::Gray,
        ContentState::Open => OptionColor::Green,
        ContentState::Merged => OptionColor::Purple,
        ContentState::Closed => OptionColor::Red,
        ContentState::Unknown => OptionColor::Unknown,
    }
}

/// The pills for `item`'s values of `fields`, in order. Fields without a value have none, a
/// linked pull request field has one per pull request, and unsupported fields have none.
pub fn field_pills(item: &Item, fields: &[&Field], theme: &Theme) -> Vec<Pill> {
    let style = |s: Style| {
        if theme.color {
            theme.pill().patch(s)
        } else {
            Style::default()
        }
    };
    let plain = |text: &str, s: Style| Pill::plain(text, style(s));
    let mut out = Vec::new();
    for field in fields {
        if matches!(field.kind, FieldKind::Unsupported { .. }) {
            continue;
        }
        let Some(value) = item.field_value(field) else {
            continue;
        };
        match value {
            FieldValue::PullRequests(prs) => out.extend(
                prs.iter()
                    .map(|pr| plain(&format!("⑂ #{}", pr.number), theme.option(pr_color(pr)))),
            ),
            FieldValue::Date(date) => out.push(plain(
                &format!("{}: {}", field.name, format_date(&date)),
                theme.dim(),
            )),
            FieldValue::SingleSelect { option_id, name } => {
                let color = match &field.kind {
                    FieldKind::SingleSelect { options } => options
                        .iter()
                        .find(|o| o.id == option_id)
                        .map_or(OptionColor::Unknown, |o| o.color),
                    _ => OptionColor::Unknown,
                };
                out.push(plain(&name, theme.option(color)));
            }
            FieldValue::Milestone(name) => out.push(plain(&format!("⚑ {name}"), Style::default())),
            FieldValue::Parent(parent) => out.push(Pill {
                parts: vec![
                    ("●".into(), style(theme.option(state_color(parent.state)))),
                    (
                        format!(" {}", sanitize(&parent.title)),
                        style(Style::default()),
                    ),
                ],
            }),
            FieldValue::SubIssues(s) => out.push(plain(
                &format!("◔ {}/{}", s.completed, s.total),
                Style::default(),
            )),
            FieldValue::IssueType(t) => out.push(plain(&t.name, theme.option(t.color))),
            // Shown elsewhere on the card.
            FieldValue::Labels(_) | FieldValue::Users(_) | FieldValue::Repository(_) => {}
            other => {
                let text = other.display();
                if !text.is_empty() {
                    out.push(plain(&text, Style::default()));
                }
            }
        }
    }
    out
}

const GAP: &str = "  ";

/// `pills` laid out left to right in lines of at most `width` cells, two spaces apart. A
/// pill wider than a line gets a line of its own, cut with `…`. With more than `max_lines`
/// lines, the last one shown ends with a dim `+N` for the pills left out, giving up pills to
/// make room for it. Pills sit on the theme's pill background; without colour they are plain.
pub fn pill_lines(
    pills: &[Pill],
    width: usize,
    max_lines: usize,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    if pills.is_empty() || width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<Vec<Pill>> = Vec::new();
    let mut used = 0;
    for pill in pills {
        let w = pill.width();
        if w > width {
            lines.push(vec![pill.truncated(width)]);
            // Nothing joins a cut pill on its line.
            used = width;
            continue;
        }
        match lines.last_mut() {
            Some(line) if used + GAP.len() + w <= width => {
                line.push(pill.clone());
                used += GAP.len() + w;
            }
            _ => {
                lines.push(vec![pill.clone()]);
                used = w;
            }
        }
    }
    let line_width = |line: &[Pill]| {
        line.iter().map(Pill::width).sum::<usize>() + GAP.len() * line.len().saturating_sub(1)
    };
    let mut counter = None;
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let shown_before: usize = lines[..max_lines - 1].iter().map(Vec::len).sum();
        let last = lines.last_mut().expect("max_lines > 0");
        loop {
            let plus = format!("+{}", pills.len() - shown_before - last.len());
            let need = if last.is_empty() {
                display_width(&plus)
            } else {
                line_width(last) + GAP.len() + display_width(&plus)
            };
            if need <= width || last.is_empty() {
                counter = Some(truncate_to_width(&plus, width));
                break;
            }
            last.pop();
        }
    }
    let style = |s: Style| {
        if theme.color {
            theme.pill().patch(s)
        } else {
            Style::default()
        }
    };
    let count = lines.len();
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let mut spans = Vec::new();
            for (j, pill) in line.into_iter().enumerate() {
                if j > 0 {
                    spans.push(Span::raw(GAP));
                }
                spans.extend(
                    pill.parts
                        .into_iter()
                        .map(|(t, s)| Span::styled(t, style(s))),
                );
            }
            if let Some(plus) = counter.as_ref().filter(|_| i + 1 == count) {
                if !spans.is_empty() {
                    spans.push(Span::raw(GAP));
                }
                spans.push(Span::styled(plus.clone(), theme.dim()));
            }
            spans
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{items, project};
    use crate::ui::labels::spans_width;
    use ratatui::style::{Color, Modifier};

    fn colour() -> Theme {
        Theme {
            color: true,
            truecolor: false,
        }
    }

    fn field(id: &str, name: &str, kind: FieldKind) -> Field {
        Field {
            id: FieldId::new(id),
            name: name.into(),
            kind,
        }
    }

    fn pr(number: u32, state: ContentState, is_draft: bool) -> LinkedPullRequest {
        LinkedPullRequest {
            number,
            state,
            is_draft,
        }
    }

    /// Each pill as its parts' text and the style of its first part.
    fn texts(pills: &[Pill]) -> Vec<String> {
        pills
            .iter()
            .map(|p| p.parts.iter().map(|(t, _)| t.as_str()).collect())
            .collect()
    }

    fn pills_of(item: &Item, fields: &[Field], theme: &Theme) -> Vec<Pill> {
        field_pills(item, &fields.iter().collect::<Vec<_>>(), theme)
    }

    fn line_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn linked_pull_requests_are_one_pill_each_coloured_by_state() {
        let mut item = items()[0].clone();
        item.content_fields.linked_prs = vec![
            pr(1, ContentState::Open, false),
            pr(2, ContentState::Open, true),
            pr(3, ContentState::Merged, false),
            pr(4, ContentState::Closed, false),
        ];
        let f = [field(
            "F_prs",
            "Linked pull requests",
            FieldKind::LinkedPullRequests,
        )];
        let pills = pills_of(&item, &f, &colour());
        assert_eq!(texts(&pills), ["⑂ #1", "⑂ #2", "⑂ #3", "⑂ #4"]);
        let fg: Vec<_> = pills.iter().map(|p| p.parts[0].1.fg).collect();
        assert_eq!(
            fg,
            [
                Some(Color::Green),
                Some(Color::Gray),
                Some(Color::Magenta),
                Some(Color::Red)
            ]
        );
    }

    #[test]
    fn dates_read_name_and_month_day_year_dimmed() {
        let mut item = items()[0].clone();
        item.content_fields.created_at = Some("2026-08-19T10:00:00Z".into());
        item.content_fields.closed_at = Some("2026-08-25T10:00:00Z".into());
        item.values
            .insert(FieldId::new("F_due"), FieldValue::Date("2026-12-01".into()));
        let f = [
            field("F_created", "Created", FieldKind::Created),
            field("F_updated", "Updated", FieldKind::Updated),
            field("F_closed", "Closed", FieldKind::Closed),
            field("F_due", "Due", FieldKind::Date),
        ];
        let pills = pills_of(&item, &f, &colour());
        assert_eq!(
            texts(&pills),
            [
                "Created: Aug 19, 2026",
                "Closed: Aug 25, 2026",
                "Due: Dec 1, 2026"
            ]
        );
        assert!(
            pills
                .iter()
                .all(|p| p.parts[0].1.add_modifier.contains(Modifier::DIM))
        );
    }

    #[test]
    fn dates_that_do_not_parse_show_as_they_are() {
        assert_eq!(format_date("2026-08-19"), "Aug 19, 2026");
        assert_eq!(format_date("2026-01-01T00:00:00Z"), "Jan 1, 2026");
        for raw in [
            "2026-08-99",
            "2026-08-00",
            "2026-13-01",
            "2026-00-10",
            "2026/08/19",
            "garbage",
            "",
            "2026-0",
            "日本語の日付です",
        ] {
            assert_eq!(format_date(raw), raw);
        }
    }

    #[test]
    fn a_single_select_pill_takes_its_options_colour() {
        let p = project();
        let prio = p.fields.iter().find(|f| f.name == "Priority").unwrap();
        let pills = field_pills(&items()[0], &[prio], &colour());
        assert_eq!(texts(&pills), ["P0"]);
        assert_eq!(pills[0].parts[0].1.fg, Some(Color::Red));
        // A value naming a deleted option keeps its name, uncoloured.
        let pills = field_pills(
            &items()[4],
            &[p.field(&FieldId::new("F_status")).unwrap()],
            &colour(),
        );
        assert_eq!(texts(&pills), ["Blocked"]);
        assert_eq!(pills[0].parts[0].1.fg, Some(Color::Reset));
    }

    #[test]
    fn plain_values_show_as_display_shows_them() {
        let mut item = items()[0].clone();
        let mut set = |id: &str, v: FieldValue| item.values.insert(FieldId::new(id), v);
        set(
            "F_sprint",
            FieldValue::Iteration {
                iteration_id: IterationId::new("it"),
                title: "Sprint 2".into(),
                start_date: "2026-10-15".into(),
            },
        );
        set("F_points", FieldValue::Number(5.0));
        set("F_notes", FieldValue::Text("needs design".into()));
        set("F_ms", FieldValue::Milestone("v1.0".into()));
        set("F_rev", FieldValue::Reviewers(vec!["a".into(), "b".into()]));
        set(
            "F_areas",
            FieldValue::MultiSelect {
                option_ids: vec![],
                names: vec!["api".into(), "ui".into()],
            },
        );
        set("F_odd", FieldValue::Text("never shown".into()));
        let f = [
            field(
                "F_sprint",
                "Sprint",
                FieldKind::Iteration {
                    iterations: vec![],
                    completed: vec![],
                },
            ),
            field("F_points", "Points", FieldKind::Number),
            field("F_notes", "Notes", FieldKind::Text),
            field("F_ms", "Milestone", FieldKind::Milestone),
            field("F_rev", "Reviewers", FieldKind::Reviewers),
            field(
                "F_areas",
                "Areas",
                FieldKind::MultiSelect { options: vec![] },
            ),
            field(
                "F_odd",
                "Odd",
                FieldKind::Unsupported {
                    type_name: "X".into(),
                },
            ),
            field("F_none", "Empty", FieldKind::Text),
        ];
        let pills = pills_of(&item, &f, &colour());
        assert_eq!(
            texts(&pills),
            [
                "Sprint 2",
                "5",
                "needs design",
                "⚑ v1.0",
                "@a, @b",
                "api, ui"
            ]
        );
        let pill_bg = colour().pill();
        for p in &pills {
            assert_eq!(
                p.parts[0].1, pill_bg,
                "default style on the pill background"
            );
        }
    }

    #[test]
    fn parent_progress_and_issue_type_pills() {
        let mut item = items()[0].clone();
        item.content_fields.parent = Some(ParentIssue {
            number: 3,
            title: "Epic".into(),
            state: ContentState::Closed,
        });
        item.content_fields.sub_issues = Some(SubIssuesSummary {
            total: 4,
            completed: 1,
        });
        item.content_fields.issue_type = Some(IssueType {
            name: "Bug".into(),
            color: OptionColor::Red,
        });
        let f = [
            field("F_parent", "Parent issue", FieldKind::ParentIssue),
            field(
                "F_subs",
                "Sub-issues progress",
                FieldKind::SubIssuesProgress,
            ),
            field("F_type", "Type", FieldKind::IssueType),
        ];
        let t = colour();
        let pills = pills_of(&item, &f, &t);
        assert_eq!(texts(&pills), ["● Epic", "◔ 1/4", "Bug"]);
        assert_eq!(pills[0].parts[0].0, "●");
        assert_eq!(
            pills[0].parts[0].1.fg,
            Some(Color::Magenta),
            "closed parent"
        );
        assert_eq!(pills[0].parts[1].1, t.pill(), "the title is plain");
        assert_eq!(pills[2].parts[0].1.fg, Some(Color::Red));
        item.content_fields.parent.as_mut().unwrap().state = ContentState::Open;
        item.content_fields.sub_issues = Some(SubIssuesSummary {
            total: 0,
            completed: 0,
        });
        let pills = pills_of(&item, &f, &t);
        assert_eq!(
            texts(&pills),
            ["● Epic", "Bug"],
            "no progress without sub-issues"
        );
        assert_eq!(pills[0].parts[0].1.fg, Some(Color::Green), "open parent");
    }

    #[test]
    fn pills_sit_on_a_subtle_background() {
        assert_eq!(colour().pill().bg, Some(Color::Indexed(236)));
        let tc = Theme {
            color: true,
            truecolor: true,
        };
        assert!(matches!(tc.pill().bg, Some(Color::Rgb(..))));
        let pills = vec![
            Pill::plain("P0", colour().option(OptionColor::Red)),
            Pill::plain("5", Style::default()),
        ];
        let line = &pill_lines(&pills, 20, MAX_PILL_LINES, &colour())[0];
        assert_eq!(line_text(line), "P0  5");
        let styles: Vec<_> = line
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect();
        assert_eq!(styles[0].1.bg, Some(Color::Indexed(236)));
        assert_eq!(styles[0].1.fg, Some(Color::Red));
        assert_eq!(styles[1], ("  ".into(), Style::default()), "gaps are bare");
        assert_eq!(styles[2].1.bg, Some(Color::Indexed(236)));
    }

    fn plain_pills(texts: &[&str]) -> Vec<Pill> {
        texts
            .iter()
            .map(|t| Pill::plain(t, Style::default()))
            .collect()
    }

    fn layout(texts: &[&str], width: usize) -> Vec<String> {
        let lines = pill_lines(&plain_pills(texts), width, MAX_PILL_LINES, &colour());
        for line in &lines {
            assert!(spans_width(line) <= width, "{line:?} wider than {width}");
        }
        lines.iter().map(|l| line_text(l)).collect()
    }

    #[test]
    fn pills_wrap_to_new_lines_two_spaces_apart() {
        assert_eq!(
            layout(
                &[
                    "⑂ #77",
                    "Created: Aug 19, 2026",
                    "Closed: Aug 25, 2026",
                    "5"
                ],
                31
            ),
            ["⑂ #77  Created: Aug 19, 2026", "Closed: Aug 25, 2026  5"]
        );
        assert_eq!(layout(&["ab", "cd"], 6), ["ab  cd"]);
        assert_eq!(layout(&["ab", "cd"], 5), ["ab", "cd"]);
        assert!(layout(&[], 10).is_empty());
    }

    #[test]
    fn a_pill_wider_than_the_line_gets_a_line_of_its_own_cut_with_an_ellipsis() {
        let mut item = items()[0].clone();
        item.content_fields.parent = Some(ParentIssue {
            number: 3,
            title: "feat(widgets): redesign the widget list".into(),
            state: ContentState::Open,
        });
        let pills = vec![
            Pill::plain("⑂ #1", Style::default()),
            pills_of(
                &item,
                &[field("F_parent", "Parent issue", FieldKind::ParentIssue)],
                &colour(),
            )
            .remove(0),
            Pill::plain("5", Style::default()),
        ];
        let lines = pill_lines(&pills, 20, MAX_PILL_LINES, &colour());
        let text: Vec<String> = lines.iter().map(|l| line_text(l)).collect();
        assert_eq!(text, ["⑂ #1", "● feat(widgets): re…", "5"]);
        assert_eq!(lines[1][0].content, "●", "the glyph keeps its own span");
        assert_eq!(lines[1][0].style.fg, Some(Color::Green));
        assert_eq!(layout(&["中文字中文字"], 5), ["中文…"]);
    }

    #[test]
    fn more_than_four_lines_end_the_fourth_with_a_dim_count() {
        let lines = pill_lines(
            &plain_pills(&["aaaa", "bbbb", "cccc", "dddd", "eeee", "ffff"]),
            6,
            MAX_PILL_LINES,
            &colour(),
        );
        let text: Vec<String> = lines.iter().map(|l| line_text(l)).collect();
        // "dddd  +3" is 8 cells: "dddd" gives way to the counter.
        assert_eq!(text, ["aaaa", "bbbb", "cccc", "+3"]);
        let counter = lines[3].last().unwrap();
        assert!(counter.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(
            layout(&["a", "b", "c", "d", "e", "f"], 4),
            ["a  b", "c  d", "e  f"]
        );
        assert_eq!(
            layout(
                &["aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh", "ii", "jj"],
                6
            ),
            ["aa  bb", "cc  dd", "ee  ff", "gg  +3"]
        );
    }

    #[test]
    fn without_colour_pills_are_plain_text() {
        let plain = Theme::plain();
        let mut item = items()[0].clone();
        item.content_fields.linked_prs = vec![pr(9, ContentState::Merged, false)];
        item.content_fields.created_at = Some("2026-08-19".into());
        let p = project();
        let fields = [
            field(
                "F_prs",
                "Linked pull requests",
                FieldKind::LinkedPullRequests,
            ),
            field("F_created", "Created", FieldKind::Created),
            p.field(&FieldId::new("F_prio")).unwrap().clone(),
        ];
        let pills = pills_of(&item, &fields, &plain);
        let lines = pill_lines(&pills, 40, MAX_PILL_LINES, &plain);
        assert_eq!(line_text(&lines[0]), "⑂ #9  Created: Aug 19, 2026  P0");
        assert!(
            lines[0].iter().all(|s| s.style == Style::default()),
            "{lines:?}"
        );
    }

    #[test]
    fn cards_skip_fields_shown_elsewhere_and_keep_the_views_order() {
        let mut p = project();
        p.fields.extend([
            field("F_repo", "Repository", FieldKind::Repository),
            field("F_created", "Created", FieldKind::Created),
        ]);
        let view = &mut p.views[1];
        view.visible_fields = [
            "F_created",
            "F_title",
            "F_assignees",
            "F_repo",
            "F_status",
            "F_labels",
            "F_prio",
            "F_notes",
        ]
        .iter()
        .map(|s| FieldId::new(*s))
        .collect();
        let p = p;
        let names = |lane: Option<&Field>| {
            let status = p.field(&FieldId::new("F_status")).unwrap();
            card_fields(&p.views[1], &p, status, lane)
                .iter()
                .map(|f| f.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(None), ["Created", "Priority", "Notes"]);
        let prio = p.field(&FieldId::new("F_prio")).unwrap();
        assert_eq!(names(Some(prio)), ["Created", "Notes"], "the lane field");
    }
}
