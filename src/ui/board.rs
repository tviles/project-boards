//! The board layout: one column per option or iteration, optional swimlanes, bordered cards.

use crate::model::*;
use crate::ui::labels::{label_spans, spans_width};
use crate::ui::pills::{MAX_PILL_LINES, field_pills, pill_lines};
use crate::ui::table::{bucket_of, buckets_for};
use crate::ui::text::{display_width, pad_to_width, sanitize, truncate_to_width, wrap_to_width};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

#[derive(Debug, Clone, PartialEq)]
pub struct Lane<'a> {
    /// `None` when the view has no swimlanes.
    pub title: Option<String>,
    pub items: Vec<&'a Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column<'a> {
    pub bucket: Bucket,
    pub lanes: Vec<Lane<'a>>,
}

impl<'a> Column<'a> {
    pub fn items(&self) -> Vec<&'a Item> {
        self.lanes
            .iter()
            .flat_map(|l| l.items.iter().copied())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.lanes.iter().map(|l| l.items.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoardSelection {
    pub column: usize,
    /// Index into the column's items across all lanes.
    pub index: usize,
}

/// True when the item has a value for `field` that names no current option or iteration.
pub fn has_unknown_option(item: &Item, field: &Field) -> bool {
    match (
        item.value(&field.id).and_then(|v| v.bucket_key()),
        field.buckets(),
    ) {
        (Some(key), Some(buckets)) => !buckets.iter().any(|b| b.matches(&key)),
        _ => false,
    }
}

/// Columns in bucket order; empty completed-iteration columns are dropped. With a lane
/// field, each column's items are split into lanes in lane order (empty lanes omitted).
pub fn build_columns<'a>(
    items: &[&'a Item],
    column_field: &Field,
    lane_field: Option<&Field>,
) -> Vec<Column<'a>> {
    let buckets = column_field.buckets().unwrap_or_default();
    let lanes = lane_field.map(|f| (f, buckets_for(items, f)));
    buckets
        .iter()
        .filter_map(|bucket| {
            let members: Vec<&'a Item> = items
                .iter()
                .copied()
                .filter(|i| bucket_of(i, column_field, &buckets) == bucket.key)
                .collect();
            if bucket.completed && members.is_empty() {
                return None;
            }
            let column_lanes = match &lanes {
                None => vec![Lane {
                    title: None,
                    items: members,
                }],
                Some((field, lane_list)) => lane_list
                    .iter()
                    .map(|lane| Lane {
                        title: Some(lane.title.clone()),
                        items: members
                            .iter()
                            .copied()
                            .filter(|i| bucket_of(i, field, lane_list) == lane.key)
                            .collect(),
                    })
                    .filter(|l| !l.items.is_empty())
                    .collect(),
            };
            Some(Column {
                bucket: bucket.clone(),
                lanes: column_lanes,
            })
        })
        .collect()
}

/// The columns `constraint` allows, in their order. When it would leave none, all of them:
/// a filter that hides every column is more likely misread than meant.
pub fn constrain_columns<'a>(
    columns: Vec<Column<'a>>,
    constraint: &ColumnConstraint,
) -> Vec<Column<'a>> {
    let kept: Vec<Column<'a>> = columns
        .iter()
        .filter(|c| {
            // GitHub already ran the filter, so a column it really excludes is empty. One
            // with items means the parser misread the filter: keep it rather than hide items.
            constraint.allows(&c.bucket.title, c.bucket.key.is_none())
                || c.lanes.iter().any(|l| !l.items.is_empty())
        })
        .cloned()
        .collect();
    if kept.is_empty() { columns } else { kept }
}

/// The layout actually drawn, and a note when it differs from the one asked for.
pub fn resolve_layout(
    requested: Layout,
    column_field: Option<&Field>,
) -> (Layout, Option<&'static str>) {
    match (requested, column_field) {
        (Layout::Roadmap, _) => (Layout::Table, Some("roadmap shown as table")),
        (Layout::Board, None) => (
            Layout::Table,
            Some("no single-select or iteration field for columns; showing table"),
        ),
        (layout, _) => (layout, None),
    }
}

const MIN_COLUMN: usize = 34;
/// Columns narrower than this draw borderless two-line cards.
const MIN_BORDERED: usize = 10;
/// Most lines a bordered card's title wraps to.
const MAX_TITLE_LINES: usize = 3;

enum Entry<'a> {
    Lane(String),
    Card { item: &'a Item, index: usize },
}

fn entries<'a>(column: &Column<'a>) -> Vec<Entry<'a>> {
    let mut out = Vec::new();
    let mut index = 0;
    for lane in &column.lanes {
        if let Some(t) = &lane.title {
            out.push(Entry::Lane(t.clone()));
        }
        for item in lane.items.iter().copied() {
            out.push(Entry::Card { item, index });
            index += 1;
        }
    }
    out
}

fn card_lines(
    item: &Item,
    field: &Field,
    width: usize,
    selected: bool,
    theme: &Theme,
) -> [Line<'static>; 2] {
    let marker = if has_unknown_option(item, field) {
        "? "
    } else {
        ""
    };
    let title = match item.number() {
        Some(n) => format!("{marker}#{n} {}", item.title()),
        None => format!("{marker}{}", item.title()),
    };
    let assignees: Vec<String> = item.assignees().iter().map(|a| format!("@{a}")).collect();
    let style = if selected {
        theme.selected()
    } else {
        Style::default()
    };
    let meta_style = if selected { style } else { theme.dim() };
    let labels = item.labels();
    let meta = if theme.color && !labels.is_empty() {
        // Assignees as dim text, then the pills in whatever room is left. On the selected
        // card everything but the pills takes the selected style, so the line is one bar.
        let mut text = truncate_to_width(&assignees.join(" · "), width);
        let mut room = width.saturating_sub(display_width(&text));
        if !text.is_empty() {
            // The separator is only worth drawing with at least two cells of pill after it.
            if room >= 5 {
                text.push_str(" · ");
                room -= 3;
            } else {
                room = 0;
            }
        }
        let pills = label_spans(&labels, room, theme, selected);
        let pad = width.saturating_sub(display_width(&text) + spans_width(&pills));
        let mut spans = vec![Span::styled(text, meta_style)];
        spans.extend(pills);
        spans.push(Span::styled(" ".repeat(pad), meta_style));
        Line::from(spans)
    } else {
        let mut meta = assignees;
        meta.extend(item.label_names().iter().map(|l| l.to_string()));
        Line::from(Span::styled(
            pad_to_width(&truncate_to_width(&meta.join(" · "), width), width),
            meta_style,
        ))
    };
    [
        Line::from(Span::styled(
            pad_to_width(&truncate_to_width(&title, width), width),
            style.patch(theme.bold()),
        )),
        meta,
    ]
}

fn border_line(
    left: &'static str,
    fill: &'static str,
    right: &'static str,
    inner: usize,
    style: Style,
) -> Line<'static> {
    Line::from(Span::styled(
        format!("{left}{}{right}", fill.repeat(inner)),
        style,
    ))
}

/// The state glyph and its style, then the dim reference after it: the repo's short name
/// and `#number` for issues and pull requests, `Draft` for a draft. Redacted and unknown
/// content have neither.
///
/// The glyph means issue state only: `●` green when open, magenta when closed as completed
/// (or for no stated reason), grey when closed as not planned; a draft is a grey `○`. A pull
/// request on the board is drawn like an issue: green open, magenta merged or closed.
fn card_reference(
    item: &Item,
    theme: &Theme,
) -> (&'static str, Style, Option<String>, Option<String>) {
    let grey = theme.option(OptionColor::Gray).patch(theme.dim());
    let short = |r: &ContentRef| {
        let name = r.repo.split_once('/').map_or(r.repo.as_str(), |(_, n)| n);
        (Some(sanitize(name)), Some(format!("#{}", r.number)))
    };
    let not_planned = item.content_fields.state_reason == Some(StateReason::NotPlanned);
    match &item.content {
        ItemContent::Issue {
            reference, state, ..
        }
        | ItemContent::PullRequest {
            reference, state, ..
        } => {
            let style = match state {
                ContentState::Open => theme.option(OptionColor::Green),
                ContentState::Closed if not_planned => grey,
                ContentState::Closed | ContentState::Merged => theme.option(OptionColor::Purple),
                ContentState::Unknown => theme.option(OptionColor::Unknown),
            };
            let (name, number) = short(reference);
            ("●", style, name, number)
        }
        ItemContent::Draft { .. } => ("○", grey, Some("Draft".into()), None),
        ItemContent::Redacted | ItemContent::Unknown { .. } => ("●", Style::default(), None, None),
    }
}

/// `name #number` in at most `room` cells: the name is cut first so the number stays.
fn fit_reference(name: Option<&str>, number: Option<&str>, room: usize) -> String {
    let full = [name, number]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if display_width(&full) <= room {
        return full;
    }
    match (name, number) {
        (Some(name), Some(number)) if display_width(number) <= room => {
            let name_room = room.saturating_sub(display_width(number) + 1);
            // A name cut to fewer than two cells says nothing.
            if name_room >= 2 {
                format!("{} {number}", truncate_to_width(name, name_room))
            } else {
                number.to_string()
            }
        }
        _ => truncate_to_width(&full, room),
    }
}

/// A bordered card's first line, exactly `width` cells: the unknown-option marker, the state
/// dot and the reference on the left; the first assignee (and `+N` more) at the right edge.
/// When both do not fit the reference is cut first, keeping `#number`, then the assignee.
fn card_header(item: &Item, field: &Field, width: usize, theme: &Theme) -> Line<'static> {
    let marker = if has_unknown_option(item, field) {
        "? "
    } else {
        ""
    };
    let (glyph, dot, name, number) = card_reference(item, theme);
    let mut right = match item.assignees().split_first() {
        None => String::new(),
        Some((first, [])) => sanitize(&format!("@{first}")),
        Some((first, rest)) => sanitize(&format!("@{first} +{}", rest.len())),
    };
    // The marker and the dot; a reference follows after a space.
    let lead = display_width(marker) + 1;
    let least = number
        .as_deref()
        .or(name.as_deref())
        .map_or(0, display_width);
    let least_left = if least > 0 { lead + 1 + least } else { lead };
    if !right.is_empty() && least_left + 1 + display_width(&right) > width {
        let room = width.saturating_sub(least_left + 1);
        right = if room >= 2 {
            truncate_to_width(&right, room)
        } else {
            String::new()
        };
    }
    let right_w = display_width(&right);
    let gap = if right_w > 0 { right_w + 1 } else { 0 };
    let body = fit_reference(
        name.as_deref(),
        number.as_deref(),
        width.saturating_sub(lead + 1 + gap),
    );
    let mut spans = vec![Span::raw(marker), Span::styled(glyph, dot)];
    let mut used = lead;
    if !body.is_empty() {
        used += 1 + display_width(&body);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(body, theme.dim()));
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used + right_w))));
    if right_w > 0 {
        spans.push(Span::styled(right, theme.dim()));
    }
    Line::from(spans)
}

/// The card inside a rounded border, or a heavy accent one when `selected`. `width` is the
/// whole card, borders included. Inside: the header, the title wrapped to at most
/// `MAX_TITLE_LINES` lines (bold when selected), then, when the card has any pills, a blank
/// line, the pills for `pill_fields` (at most `MAX_PILL_LINES` lines) and the label pills,
/// always last. Selection is marked by the border and the bold title alone.
fn bordered_card(
    item: &Item,
    field: &Field,
    pill_fields: &[&Field],
    width: usize,
    selected: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let inner = width - 2;
    let (set, style) = if selected {
        (border::THICK, theme.accent().add_modifier(Modifier::BOLD))
    } else if theme.color {
        (border::ROUNDED, Style::default().fg(Color::DarkGray))
    } else {
        (border::ROUNDED, theme.dim())
    };
    let wrap = |line: Line<'static>| {
        let mut spans = vec![Span::styled(set.vertical_left, style)];
        spans.extend(line.spans);
        spans.push(Span::styled(set.vertical_right, style));
        Line::from(spans)
    };
    let title_style = if selected {
        theme.bold()
    } else {
        Style::default()
    };
    let mut lines = vec![
        border_line(
            set.top_left,
            set.horizontal_top,
            set.top_right,
            inner,
            style,
        ),
        wrap(card_header(item, field, inner, theme)),
    ];
    for title in wrap_to_width(item.title(), inner, MAX_TITLE_LINES) {
        lines.push(wrap(Line::from(Span::styled(
            pad_to_width(&title, inner),
            title_style,
        ))));
    }
    let padded = |mut spans: Vec<Span<'static>>| {
        let pad = inner.saturating_sub(spans_width(&spans));
        spans.push(Span::raw(" ".repeat(pad)));
        wrap(Line::from(spans))
    };
    let fields = pill_lines(
        &field_pills(item, pill_fields, theme),
        inner,
        MAX_PILL_LINES,
        theme,
    );
    let labels = item.labels();
    if !fields.is_empty() || !labels.is_empty() {
        lines.push(padded(Vec::new()));
    }
    lines.extend(fields.into_iter().map(&padded));
    if !labels.is_empty() {
        lines.push(padded(label_spans(&labels, inner, theme, false)));
    }
    lines.push(border_line(
        set.bottom_left,
        set.horizontal_bottom,
        set.bottom_right,
        inner,
        style,
    ));
    lines
}

/// The first line of a column shown in `rows` rows. The selected card, at `(top, height)`
/// in the column's lines, ends on the last visible row at the latest, but the column never
/// scrolls past its top: a card taller than the area shows its top.
fn column_scroll(selected_span: Option<(usize, usize)>, rows: usize) -> usize {
    selected_span.map_or(0, |(top, height)| {
        (top + height).saturating_sub(rows).min(top)
    })
}

#[cfg(test)]
thread_local! {
    /// Cards built by `render_board` on this thread, so tests can check it skips hidden ones.
    static CARDS_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Draws the board. `pill_fields` are the fields each card shows as pills (see
/// `pills::card_fields`).
pub fn render_board(
    frame: &mut Frame,
    area: Rect,
    columns: &[Column],
    selection: &BoardSelection,
    column_field: &Field,
    pill_fields: &[&Field],
    theme: &Theme,
) {
    if columns.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No items match this view.", theme.dim())),
            area,
        );
        return;
    }
    let total = area.width as usize;
    let visible = (total / MIN_COLUMN).clamp(1, columns.len());
    let start = selection
        .column
        .saturating_sub(visible - 1)
        .min(columns.len() - visible);
    let col_width = total / visible;
    let width = col_width.saturating_sub(1);
    let height = area.height as usize;

    let mut header_spans = Vec::new();
    for column in &columns[start..start + visible] {
        let title = format!("{} {}", column.bucket.title, column.len());
        header_spans.push(Span::styled(
            pad_to_width(&truncate_to_width(&title, width), width),
            theme.option(column.bucket.color).patch(theme.bold()),
        ));
        header_spans.push(Span::raw(" "));
    }
    if visible < columns.len() {
        let indicator = format!("‹ {}–{} of {} ›", start + 1, start + visible, columns.len());
        if let Some(last) = header_spans.iter_mut().rev().nth(1) {
            let room = width.saturating_sub(crate::ui::text::display_width(&indicator) + 1);
            let keep = truncate_to_width(last.content.trim_end(), room);
            *last = Span::styled(
                pad_to_width(
                    &truncate_to_width(&format!("{keep} {indicator}"), width),
                    width,
                ),
                last.style,
            );
        }
    }

    let mut grid: Vec<Vec<Span<'static>>> = vec![Vec::new(); height.saturating_sub(1)];
    for (offset, column) in columns[start..start + visible].iter().enumerate() {
        let col_index = start + offset;
        // Each card is built (and its title wrapped) once; its height is the lines it took.
        // Only the cards the column can show are built: in the selected column everything
        // through the selected card (its place sets the scroll), then until the rows below
        // the scroll are filled.
        let rows = grid.len();
        let wants_selected = col_index == selection.column && selection.index < column.len();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected_span = None;
        for entry in entries(column) {
            let scroll_known = !wants_selected || selected_span.is_some();
            if scroll_known && lines.len() >= column_scroll(selected_span, rows) + rows {
                break;
            }
            match entry {
                Entry::Lane(title) => lines.push(Line::from(Span::styled(
                    pad_to_width(&truncate_to_width(&format!("── {title} "), width), width),
                    theme.dim(),
                ))),
                Entry::Card { item, index } => {
                    let is_sel = col_index == selection.column && index == selection.index;
                    let card = if width >= MIN_BORDERED {
                        bordered_card(item, column_field, pill_fields, width, is_sel, theme)
                    } else {
                        card_lines(item, column_field, width, is_sel, theme).into()
                    };
                    #[cfg(test)]
                    CARDS_BUILT.with(|n| n.set(n.get() + 1));
                    if is_sel {
                        selected_span = Some((lines.len(), card.len()));
                    }
                    lines.extend(card);
                }
            }
        }
        let scroll = column_scroll(selected_span, rows);
        for (row, cells) in grid.iter_mut().enumerate() {
            let line = lines
                .get(row + scroll)
                .cloned()
                .unwrap_or_else(|| Line::from(" ".repeat(width)));
            cells.extend(line.spans);
            cells.push(Span::raw(" "));
        }
    }
    let mut all = vec![Line::from(header_spans)];
    all.extend(grid.into_iter().map(Line::from));
    frame.render_widget(Paragraph::new(all), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{items, project, render_to_string};
    use crate::ui::theme::Theme;

    fn field<'a>(p: &'a Project, name: &str) -> &'a Field {
        p.fields.iter().find(|f| f.name == name).unwrap()
    }

    #[test]
    fn columns_follow_options_and_end_with_no_value() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let cols = build_columns(&refs, field(&p, "Status"), None);
        let summary: Vec<_> = cols
            .iter()
            .map(|c| (c.bucket.title.as_str(), c.len()))
            .collect();
        assert_eq!(
            summary,
            [
                ("Todo", 2),
                ("In Progress", 1),
                ("Done", 1),
                ("No Status", 1)
            ]
        );
    }

    /// Review Focus 3: a value naming a deleted option lands in "No Status", marked.
    #[test]
    fn deleted_options_go_to_no_value_and_are_flagged() {
        let (p, all) = (project(), items());
        let status = field(&p, "Status");
        let refs: Vec<&Item> = all.iter().collect();
        let cols = build_columns(&refs, status, None);
        let no_status = cols.last().unwrap();
        assert_eq!(no_status.items()[0].id.as_str(), "e");
        assert!(has_unknown_option(no_status.items()[0], status));
        assert!(!has_unknown_option(&all[0], status));
    }

    #[test]
    fn swimlanes_split_each_column_in_lane_order() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let cols = build_columns(&refs, field(&p, "Status"), Some(field(&p, "Priority")));
        let todo_lanes: Vec<_> = cols[0]
            .lanes
            .iter()
            .map(|l| (l.title.clone().unwrap(), l.items.len()))
            .collect();
        assert_eq!(todo_lanes, [("P0".to_string(), 1), ("P2".to_string(), 1)]);
        assert_eq!(cols[2].lanes[0].title.as_deref(), Some("No Priority"));
    }

    /// Review Focus 4: no column field means a table, with a note.
    #[test]
    fn layout_falls_back_to_table() {
        let p = project();
        assert_eq!(
            resolve_layout(Layout::Board, Some(field(&p, "Status"))),
            (Layout::Board, None)
        );
        let (layout, note) = resolve_layout(Layout::Board, None);
        assert_eq!(layout, Layout::Table);
        assert!(note.unwrap().contains("single-select or iteration"));
        assert_eq!(
            resolve_layout(Layout::Roadmap, None),
            (Layout::Table, Some("roadmap shown as table"))
        );
    }

    #[test]
    fn renders_columns_cards_and_markers_within_width() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let sel = BoardSelection {
            column: 0,
            index: 1,
        };
        // The board view shows a linked PR, a date, Priority and labels on #1's card.
        let pill_fields = crate::ui::pills::card_fields(&p.views[1], &p, status, None);
        let screen = render_to_string(4 * MIN_COLUMN as u16, 18, |f| {
            render_board(
                f,
                f.area(),
                &cols,
                &sel,
                status,
                &pill_fields,
                &Theme::plain(),
            )
        });
        let first = screen.lines().next().unwrap();
        assert!(
            first.contains("Todo 2") && first.contains("No Status 1"),
            "{screen}"
        );
        assert!(screen.contains("│? ● t #5"), "{screen}");
        assert!(screen.contains("│Old option"), "{screen}");
        assert!(screen.contains("┃Emoji 🚀"), "{screen}");
        assert!(screen.contains("│⑂ #12  Created: Aug 19, 2026"), "{screen}");
        assert!(
            screen
                .lines()
                .all(|l| crate::ui::text::display_width(l) <= 4 * MIN_COLUMN)
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn narrow_panes_scroll_columns_and_say_so() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let sel = BoardSelection {
            column: 3,
            index: 0,
        };
        let screen = render_to_string(2 * MIN_COLUMN as u16 + 2, 8, |f| {
            render_board(f, f.area(), &cols, &sel, status, &[], &Theme::plain())
        });
        assert!(
            screen.lines().next().unwrap().contains("3–4 of 4"),
            "{screen}"
        );
        assert!(screen.contains("No Status"));
        assert!(!screen.contains("Todo 2"));
    }

    #[test]
    fn items_on_either_of_two_same_named_options_share_one_column() {
        let mut p = project();
        let status = p.fields.iter_mut().find(|f| f.name == "Status").unwrap();
        let FieldKind::SingleSelect { options } = &mut status.kind else {
            panic!("Status is single-select")
        };
        let first = options[0].clone();
        options.push(SelectOption {
            id: OptionId::new("o_dup"),
            name: first.name.clone(),
            color: first.color,
        });
        let status = field(&p, "Status").clone();
        let all = items();
        let on_first: Vec<&Item> = all
            .iter()
            .filter(|i| {
                matches!(i.value(&status.id), Some(FieldValue::SingleSelect { option_id, .. }) if *option_id == first.id)
            })
            .collect();
        assert!(
            !on_first.is_empty(),
            "fixture has items on the first option"
        );
        let mut moved = on_first[0].clone();
        moved.values.insert(
            status.id.clone(),
            FieldValue::SingleSelect {
                option_id: OptionId::new("o_dup"),
                name: first.name.clone(),
            },
        );
        let mut refs: Vec<&Item> = all.iter().collect();
        refs.push(&moved);
        let cols = build_columns(&refs, &status, None);
        let named: Vec<&Column> = cols
            .iter()
            .filter(|c| c.bucket.title == first.name)
            .collect();
        assert_eq!(named.len(), 1, "one column per name");
        assert_eq!(named[0].items().len(), on_first.len() + 1);
        assert_eq!(
            cols[cols.len() - 2].bucket.title,
            first.name,
            "at the later option's place, before No Status"
        );
    }

    #[test]
    fn columns_the_filter_excludes_are_hidden_and_not_counted() {
        let mut p = project();
        let status = p.fields.iter_mut().find(|f| f.name == "Status").unwrap();
        if let FieldKind::SingleSelect { options } = &mut status.kind {
            for (i, (id, name)) in [("o_design", "Design"), ("o_review", "Design Review")]
                .into_iter()
                .enumerate()
            {
                options.insert(
                    1 + i,
                    SelectOption {
                        id: OptionId::new(id),
                        name: name.into(),
                        color: OptionColor::Purple,
                    },
                );
            }
        }
        let status = field(&p, "Status");
        let all = items();
        let refs: Vec<&Item> = all.iter().collect();
        let titles = |cols: &[Column]| {
            cols.iter()
                .map(|c| c.bucket.title.clone())
                .collect::<Vec<_>>()
        };
        let unfiltered = build_columns(&refs, status, None);
        assert_eq!(unfiltered.len(), 6);
        let constraint = column_constraint("-status:\"Design Review\",Design", status);
        let cols = constrain_columns(unfiltered.clone(), &constraint);
        assert_eq!(
            titles(&cols),
            ["Todo", "In Progress", "Done", "No Status"],
            "GitHub's option order"
        );
        let sel = BoardSelection::default();
        let header = |cols: &[Column]| {
            let screen = render_to_string(2 * MIN_COLUMN as u16 + 2, 6, |f| {
                render_board(f, f.area(), cols, &sel, status, &[], &Theme::plain())
            });
            screen.lines().next().unwrap().to_string()
        };
        assert!(header(&unfiltered).contains("1–2 of 6"));
        assert!(header(&cols).contains("1–2 of 4"), "{}", header(&cols));

        // A filter that would hide every column is a parser miss: columns holding items stay,
        // and with none left at all every column shows.
        let none = column_constraint("status:Nope -no:status", status);
        let occupied = unfiltered.iter().filter(|c| !c.items().is_empty()).count();
        assert_eq!(constrain_columns(unfiltered.clone(), &none).len(), occupied);
        let empty: Vec<Column> = unfiltered
            .iter()
            .map(|c| Column {
                bucket: c.bucket.clone(),
                lanes: vec![],
            })
            .collect();
        assert_eq!(constrain_columns(empty, &none).len(), 6);
        let unknown = column_constraint("label:bug", status);
        assert_eq!(constrain_columns(unfiltered.clone(), &unknown).len(), 6);

        // A column holding items was not excluded by GitHub, whatever the parser thinks.
        let todo_only = column_constraint("status:Todo", status);
        for c in constrain_columns(unfiltered.clone(), &todo_only) {
            assert!(
                c.bucket.title == "Todo" || !c.items().is_empty(),
                "{}",
                c.bucket.title
            );
        }
        assert!(constrain_columns(unfiltered, &todo_only).len() >= occupied);
    }

    #[test]
    fn pills_keep_their_colours_on_their_own_line() {
        use ratatui::style::{Color, Modifier};
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let theme = Theme {
            color: true,
            truecolor: true,
        };
        let sel = BoardSelection {
            column: 0,
            index: 0,
        };
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
            2 * MIN_COLUMN as u16 + 2,
            12,
        ))
        .unwrap();
        let buf = terminal
            .draw(|f| render_board(f, f.area(), &cols, &sel, status, &[], &theme))
            .unwrap()
            .buffer
            .clone();
        // Card "#1 Fix crash": border, header, title, a blank line, then the pill line
        // inside the border.
        assert_eq!(cell_text(&buf, 4, 5), "┃    ");
        assert_eq!(cell_text(&buf, 5, 5), "┃bug ");
        let pill = &buf[(1, 5)];
        assert_eq!(pill.bg, Color::Rgb(0xd7, 0x3a, 0x4a));
        assert!(!pill.modifier.contains(Modifier::REVERSED));
        assert_eq!(buf[(4, 5)].bg, Color::Reset, "only the pill is coloured");
        // The next card (rows 7..) is not selected: its title (row 9) is not bold.
        assert_eq!(cell_text(&buf, 9, 6), "│Emoji");
        assert!(!buf[(1, 9)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn no_separator_dangles_when_too_little_room_is_left_for_a_pill() {
        let (p, all) = (project(), items());
        let status = field(&p, "Status");
        let theme = Theme {
            color: true,
            truecolor: true,
        };
        // "@tviles" is 7 cells, so widths 10 and 11 leave 3 and 4 cells: under 5.
        for width in [10, 11] {
            let [_, meta] = card_lines(&all[0], status, width, false, &theme);
            let text: String = meta.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, format!("{:<width$}", "@tviles"), "width {width}");
        }
        let [_, meta] = card_lines(&all[0], status, 12, false, &theme);
        let text: String = meta.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "@tviles · b…");
        let [_, meta] = card_lines(&all[0], status, 14, false, &theme);
        let text: String = meta.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "@tviles · bug ");
    }

    #[test]
    fn a_selected_card_is_not_reversed_and_its_pills_keep_their_colours() {
        use ratatui::style::{Color, Modifier};
        let (p, mut all) = (project(), items());
        all[0].values.insert(
            FieldId::new("F_labels"),
            FieldValue::Labels(
                ["bug", "enhancement", "documentation"]
                    .iter()
                    .map(|n| Label {
                        name: (*n).into(),
                        color: "d73a4a".into(),
                    })
                    .collect(),
            ),
        );
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let theme = Theme {
            color: true,
            truecolor: true,
        };
        let sel = BoardSelection::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        let buf = terminal
            .draw(|f| render_board(f, f.area(), &cols, &sel, status, &[], &theme))
            .unwrap()
            .buffer
            .clone();
        for y in 0..8 {
            for x in 0..30 {
                assert!(
                    !buf[(x, y)].modifier.contains(Modifier::REVERSED),
                    "reversed cell at {x},{y}"
                );
            }
        }
        // Column width 30: the card is 29 cells, 27 inside the border.
        assert_eq!(cell_text(&buf, 2, 29), "┃● t #1              @tviles┃");
        assert_eq!(cell_text(&buf, 3, 29), "┃Fix crash                  ┃");
        assert_eq!(cell_text(&buf, 4, 29), "┃                           ┃");
        assert_eq!(cell_text(&buf, 5, 29), "┃bug enhancement +1         ┃");
        for x in [1, 2, 3] {
            assert_eq!(buf[(x, 5)].bg, Color::Rgb(0xd7, 0x3a, 0x4a), "pill at {x}");
        }
        assert!(
            buf[(1, 3)].modifier.contains(Modifier::BOLD),
            "selected title bold"
        );
    }

    fn cell_text(buf: &ratatui::buffer::Buffer, y: u16, w: u16) -> String {
        (0..w).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn selected_card_has_a_heavy_accent_border_and_others_rounded() {
        use ratatui::style::Modifier;
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let theme = Theme {
            color: true,
            truecolor: true,
        };
        let sel = BoardSelection::default();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
            2 * MIN_COLUMN as u16 + 2,
            12,
        ))
        .unwrap();
        let buf = terminal
            .draw(|f| render_board(f, f.area(), &cols, &sel, status, &[], &theme))
            .unwrap()
            .buffer
            .clone();
        // Column width 34: the selected card (header, title, spacer, pills) is rows 1..7,
        // the next one starts at row 7.
        assert_eq!(buf[(0, 1)].symbol(), "┏");
        assert_eq!(buf[(33, 1)].symbol(), "┓");
        assert_eq!(buf[(0, 2)].symbol(), "┃");
        assert_eq!(buf[(33, 5)].symbol(), "┃");
        assert_eq!(buf[(0, 6)].symbol(), "┗");
        assert_eq!(buf[(33, 6)].symbol(), "┛");
        assert_eq!(buf[(5, 1)].symbol(), "━");
        for (x, y) in [(0, 1), (5, 1), (0, 2), (33, 6)] {
            let cell = &buf[(x, y)];
            assert_eq!(cell.fg, theme.accent().fg.unwrap(), "{x},{y}");
            assert!(cell.modifier.contains(Modifier::BOLD), "{x},{y}");
        }
        assert_eq!(cell_text(&buf, 7, 1), "╭");
        assert_eq!(buf[(33, 7)].symbol(), "╮");
        assert_eq!(buf[(0, 8)].symbol(), "│");
        assert_ne!(buf[(0, 7)].fg, theme.accent().fg.unwrap());
        assert!(!buf[(0, 7)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn plain_theme_marks_the_selected_card_by_its_heavy_border() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let sel = BoardSelection::default();
        let screen = render_to_string(60, 12, |f| {
            render_board(f, f.area(), &cols, &sel, status, &[], &Theme::plain())
        });
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[1].starts_with("┏━"), "{screen}");
        assert!(rows[2].starts_with("┃● t #1"), "{screen}");
        assert!(rows[3].starts_with("┃Fix crash"), "{screen}");
        assert!(rows[4].starts_with("┃ "), "{screen}");
        assert!(rows[5].starts_with("┃bug"), "{screen}");
        assert!(rows[6].starts_with("┗━"), "{screen}");
        assert!(rows[7].starts_with("╭─"), "{screen}");
        assert_eq!(screen.matches('┏').count(), 1, "one selected card");
    }

    /// Cards of 4 to 8 rows: titles of one to three lines, with and without labels (which
    /// bring a blank line and a pill line).
    fn mixed_height_cards(all: &[Item]) -> Vec<Item> {
        let long = "word ".repeat(30);
        let titles = ["short", "two lines of title text here", long.as_str()];
        (0..12)
            .map(|n| {
                let mut item = all[0].clone();
                item.id = ItemId::new(format!("card{n}"));
                if n % 2 == 1 {
                    item.values.remove(&FieldId::new("F_labels"));
                }
                if let ItemContent::Issue { title, .. } = &mut item.content {
                    *title = titles[n % 3].into();
                }
                item
            })
            .collect()
    }

    fn one_column<'a>(all: &[Item], status: &Field, cards: &'a [Item]) -> Column<'a> {
        Column {
            bucket: build_columns(&all.iter().collect::<Vec<_>>(), status, None)[0]
                .bucket
                .clone(),
            lanes: vec![
                Lane {
                    title: Some("P0".into()),
                    items: cards[..5].iter().collect(),
                },
                Lane {
                    title: Some("P1".into()),
                    items: cards[5..].iter().collect(),
                },
            ],
        }
    }

    #[test]
    fn scrolling_mixed_height_cards_keeps_the_selected_card_fully_visible() {
        let (p, all) = (project(), items());
        let status = field(&p, "Status");
        let cards = mixed_height_cards(&all);
        let column = one_column(&all, status, &cards);
        let mut heights = std::collections::BTreeSet::new();
        for height in [9u16, 13] {
            for index in 0..cards.len() {
                let sel = BoardSelection { column: 0, index };
                let screen = render_to_string(30, height, |f| {
                    render_board(
                        f,
                        f.area(),
                        std::slice::from_ref(&column),
                        &sel,
                        status,
                        &[],
                        &Theme::plain(),
                    )
                });
                let rows: Vec<&str> = screen.lines().collect();
                let top = rows.iter().position(|r| r.starts_with('┏'));
                let bottom = rows.iter().position(|r| r.starts_with('┗'));
                let (Some(top), Some(bottom)) = (top, bottom) else {
                    panic!("height {height} index {index}: not fully visible\n{screen}")
                };
                assert!(top >= 1 && bottom > top, "index {index}\n{screen}");
                heights.insert(bottom - top + 1);
            }
        }
        assert_eq!(heights.into_iter().collect::<Vec<_>>(), [4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_card_taller_than_the_area_shows_its_top() {
        let (p, all) = (project(), items());
        let status = field(&p, "Status");
        let cards = mixed_height_cards(&all);
        let column = one_column(&all, status, &cards);
        // Card 2 has three title lines and pills: 8 rows, in a 4-row area under the header.
        let sel = BoardSelection {
            column: 0,
            index: 2,
        };
        let screen = render_to_string(30, 5, |f| {
            render_board(
                f,
                f.area(),
                std::slice::from_ref(&column),
                &sel,
                status,
                &[],
                &Theme::plain(),
            )
        });
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[1].starts_with("┏"), "{screen}");
        assert!(rows[2].starts_with("┃● t #1"), "{screen}");
    }

    fn render_one(column: &Column, status: &Field, index: usize, height: u16) -> String {
        let sel = BoardSelection { column: 0, index };
        render_to_string(30, height, |f| {
            render_board(
                f,
                f.area(),
                std::slice::from_ref(column),
                &sel,
                status,
                &[],
                &Theme::plain(),
            )
        })
    }

    #[test]
    fn a_long_column_draws_exactly_as_its_first_cards_and_builds_only_those() {
        let (p, all) = (project(), items());
        let status = field(&p, "Status");
        let cards: Vec<Item> = (0..42)
            .flat_map(|_| mixed_height_cards(&all))
            .take(500)
            .collect();
        let bucket = build_columns(&all.iter().collect::<Vec<_>>(), status, None)[0]
            .bucket
            .clone();
        let column = |n: usize| Column {
            bucket: bucket.clone(),
            lanes: vec![Lane {
                title: None,
                items: cards[..n].iter().collect(),
            }],
        };
        let full = column(500);
        // Without the header, whose count differs.
        let body = |s: String| s.lines().skip(1).collect::<Vec<_>>().join("\n");
        for index in [0, 1, 7, 250, 499] {
            // Twenty more cards are at least 80 rows: more than the area shows.
            let first = column((index + 20).min(500));
            assert_eq!(
                body(render_one(&full, status, index, 13)),
                body(render_one(&first, status, index, 13)),
                "index {index}"
            );
            CARDS_BUILT.with(|n| n.set(0));
            render_one(&full, status, index, 13);
            let built = CARDS_BUILT.with(|n| n.get());
            // Through the selected card, then at most the 12 rows below the header (cards
            // are at least 4 rows): never the whole column.
            assert!(built <= index + 4, "index {index}: built {built}");
        }
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        for width in [0, 1, 5, 9, 12] {
            for height in [0, 1, 2] {
                for index in [0, 1] {
                    let sel = BoardSelection { column: 0, index };
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                            .unwrap();
                    terminal
                        .draw(|f| {
                            render_board(f, f.area(), &cols, &sel, status, &[], &Theme::plain())
                        })
                        .unwrap();
                }
            }
        }
    }

    fn with_content(content: ItemContent) -> Item {
        let mut item = items()[0].clone();
        item.content = content;
        item
    }

    fn reference(repo: &str, number: u32) -> ContentRef {
        ContentRef {
            repo: repo.into(),
            number,
            url: String::new(),
        }
    }

    fn issue_in(state: ContentState) -> Item {
        with_content(ItemContent::Issue {
            reference: reference("tviles/t", 1),
            title: "Fix crash".into(),
            state,
        })
    }

    /// The card's lines as text, borders included.
    fn card_text(item: &Item, width: usize, theme: &Theme) -> Vec<String> {
        card_text_with(item, &[], width, theme)
    }

    /// The card's lines as text with `pill_fields` shown as pills.
    fn card_text_with(
        item: &Item,
        pill_fields: &[&Field],
        width: usize,
        theme: &Theme,
    ) -> Vec<String> {
        let p = project();
        bordered_card(item, field(&p, "Status"), pill_fields, width, false, theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn colour() -> Theme {
        Theme {
            color: true,
            truecolor: true,
        }
    }

    #[test]
    fn header_has_the_reference_left_and_the_assignees_at_the_right_inner_edge() {
        let mut item = items()[0].clone();
        item.values.insert(
            FieldId::new("F_assignees"),
            FieldValue::Users(vec!["octocat".into(), "tviles".into()]),
        );
        let lines = card_text(&item, 34, &Theme::plain());
        assert_eq!(lines[1], "│● t #1              @octocat +1│");
        let lines = card_text(&items()[1], 34, &Theme::plain());
        assert_eq!(
            lines[1], "│● t #2                          │",
            "no assignee"
        );
        // Header text is dim; the dot carries the state colour.
        let p = project();
        let header = &bordered_card(&item, field(&p, "Status"), &[], 34, false, &colour())[1];
        let styled = |text: &str| {
            header
                .spans
                .iter()
                .find(|s| s.content == text)
                .unwrap()
                .style
        };
        assert!(styled("@octocat +1").add_modifier.contains(Modifier::DIM));
        assert!(styled("t #1").add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_header_that_does_not_fit_cuts_the_repo_then_the_assignee_but_keeps_the_number() {
        let mut item = with_content(ItemContent::Issue {
            reference: reference("tviles/a-very-long-repository-name", 4242),
            title: "x".into(),
            state: ContentState::Open,
        });
        item.values.insert(
            FieldId::new("F_assignees"),
            FieldValue::Users(vec!["octocat".into()]),
        );
        let header = |width: usize| card_text(&item, width, &Theme::plain())[1].clone();
        assert_eq!(header(36), "│● a-very-long-rep… #4242 @octocat│");
        assert_eq!(header(30), "│● a-very-lo… #4242 @octocat│");
        assert_eq!(header(26), "│● a-ver… #4242 @octocat│");
        assert_eq!(header(19), "│● #4242 @octocat│");
        assert_eq!(header(14), "│● #4242 @ma…│");
        assert_eq!(header(11), "│● #4242  │");
    }

    #[test]
    fn a_long_title_wraps_to_three_lines_and_ends_with_an_ellipsis() {
        let item = with_content(ItemContent::Issue {
            reference: reference("tviles/t", 1),
            title: "feat(widgets): Widget list shipped coverage gaps — empty, loading, error/retry, delete, offline"
                .into(),
            state: ContentState::Open,
        });
        let lines = card_text(&item, 35, &Theme::plain());
        assert_eq!(
            lines[2..5],
            [
                "│feat(widgets): Widget list       │",
                "│shipped coverage gaps — empty,   │",
                "│loading, error/retry, delete, of…│",
            ]
        );
        assert_eq!(
            lines.len(),
            8,
            "border, header, three title lines, spacer, pills, border"
        );
    }

    #[test]
    fn a_blank_line_separates_the_title_from_pills_only_when_there_are_pills() {
        let blank = format!("│{}│", " ".repeat(32));
        let without = card_text(&items()[1], 34, &Theme::plain());
        assert_eq!(without.len(), 4, "{without:?}");
        assert_eq!(without[2], "│Add iteration columns           │");
        assert!(without[3].starts_with('╰'));
        let with_labels = card_text(&items()[0], 34, &Theme::plain());
        assert_eq!(with_labels.len(), 6);
        assert_eq!(with_labels[2], "│Fix crash                       │");
        assert_eq!(with_labels[3], blank);
        assert_eq!(with_labels[4], "│bug                             │");
        // A field pill alone brings the blank line too.
        let p = project();
        let prio = field(&p, "Priority");
        let with_field = card_text_with(&items()[1], &[prio], 34, &Theme::plain());
        assert_eq!(with_field.len(), 6);
        assert_eq!(with_field[3], blank);
        assert_eq!(with_field[4], "│P1                              │");
    }

    #[test]
    fn field_pills_follow_the_title_and_labels_always_come_last() {
        let p = project();
        // Labels listed first among the visible fields still come after the field pills.
        let fields = crate::ui::pills::card_fields(&p.views[1], &p, field(&p, "Status"), None);
        let mut fields_labels_first: Vec<&Field> = vec![field(&p, "Labels")];
        fields_labels_first.extend(fields.iter().copied());
        for pill_fields in [&fields, &fields_labels_first] {
            let lines = card_text_with(&items()[0], pill_fields, 34, &Theme::plain());
            assert_eq!(
                lines[3..6],
                [
                    format!("│{}│", " ".repeat(32)),
                    "│⑂ #12  Created: Aug 19, 2026  P0│".to_string(),
                    "│bug                             │".to_string(),
                ]
            );
        }
        // Labels show even when the view does not list them.
        let no_labels: Vec<&Field> = fields
            .iter()
            .copied()
            .filter(|f| f.kind != FieldKind::Labels)
            .collect();
        let lines = card_text_with(&items()[0], &no_labels, 34, &Theme::plain());
        assert_eq!(lines[5], "│bug                             │");
    }

    fn glyph(item: &Item) -> Style {
        let p = project();
        let header = &bordered_card(item, field(&p, "Status"), &[], 34, false, &colour())[1];
        header
            .spans
            .iter()
            .find(|s| s.content == "●" || s.content == "○")
            .unwrap()
            .style
    }

    fn closed_issue(reason: Option<StateReason>) -> Item {
        let mut item = issue_in(ContentState::Closed);
        item.content_fields.state_reason = reason;
        item
    }

    #[test]
    fn the_state_dot_means_issue_state() {
        assert_eq!(glyph(&issue_in(ContentState::Open)).fg, Some(Color::Green));
        assert_eq!(
            glyph(&closed_issue(Some(StateReason::Completed))).fg,
            Some(Color::Magenta)
        );
        assert_eq!(
            glyph(&closed_issue(None)).fg,
            Some(Color::Magenta),
            "no reason reads as completed"
        );
        let not_planned = glyph(&closed_issue(Some(StateReason::NotPlanned)));
        assert_eq!(not_planned.fg, Some(Color::Gray));
        assert!(not_planned.add_modifier.contains(Modifier::DIM));
        assert_eq!(
            glyph(&issue_in(ContentState::Unknown)).fg,
            Some(Color::Reset)
        );
        // Without colour the dot is still there, just uncoloured.
        let p = project();
        let plain = bordered_card(
            &closed_issue(Some(StateReason::NotPlanned)),
            field(&p, "Status"),
            &[],
            34,
            false,
            &Theme::plain(),
        );
        let dot = plain[1].spans.iter().find(|s| s.content == "●").unwrap();
        assert_eq!(dot.style.fg, None);
    }

    #[test]
    fn a_draft_item_header_says_draft_with_a_grey_dot() {
        let draft = &items()[3];
        let lines = card_text(draft, 34, &Theme::plain());
        assert_eq!(lines[1], "│○ Draft                         │");
        assert_eq!(lines[2], "│Draft idea                      │");
        assert_eq!(lines.len(), 4);
        let dot = glyph(draft);
        assert_eq!(dot.fg, Some(Color::Gray));
        assert!(dot.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn redacted_items_keep_their_text_and_the_unknown_option_marker() {
        let unknown = &items()[4];
        let lines = card_text(unknown, 34, &Theme::plain());
        assert_eq!(lines[1], "│? ● t #5                        │");
        let mut redacted = unknown.clone();
        redacted.content = ItemContent::Redacted;
        let lines = card_text(&redacted, 34, &Theme::plain());
        assert_eq!(lines[1], "│? ●                             │");
        assert_eq!(lines[2], "│Private item                    │");
    }

    #[test]
    fn columns_narrower_than_ten_fall_back_to_two_line_cards() {
        let (p, all) = (project(), items());
        let refs: Vec<&Item> = all.iter().collect();
        let status = field(&p, "Status");
        let cols = build_columns(&refs, status, None);
        let sel = BoardSelection::default();
        // Area 9 gives one column with 8 cells.
        let screen = render_to_string(9, 8, |f| {
            render_board(f, f.area(), &cols, &sel, status, &[], &Theme::plain())
        });
        assert!(
            !screen.contains(['╭', '┏', '│', '┃']),
            "no borders:\n{screen}"
        );
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[1].starts_with("#1 Fix …"), "{screen}");
        assert!(
            rows[3].starts_with("#3 Emoj"),
            "cards two rows apart: {screen}"
        );
    }
}
