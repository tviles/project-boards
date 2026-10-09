//! The board layout: one column per option or iteration, optional swimlanes, cards of two lines.

use crate::model::*;
use crate::ui::table::{bucket_of, buckets_for};
use crate::ui::text::{pad_to_width, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
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
        (Some(key), Some(buckets)) => !buckets
            .iter()
            .any(|b| b.key.as_deref() == Some(key.as_str())),
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

const MIN_COLUMN: usize = 24;

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
    let mut meta: Vec<String> = item.assignees().iter().map(|a| format!("@{a}")).collect();
    meta.extend(item.label_names().iter().map(|l| l.to_string()));
    let style = if selected {
        theme.selected()
    } else {
        Style::default()
    };
    [
        Line::from(Span::styled(
            pad_to_width(&truncate_to_width(&title, width), width),
            style.patch(theme.bold()),
        )),
        Line::from(Span::styled(
            pad_to_width(&truncate_to_width(&meta.join(" · "), width), width),
            if selected { style } else { theme.dim() },
        )),
    ]
}

pub fn render_board(
    frame: &mut Frame,
    area: Rect,
    columns: &[Column],
    selection: &BoardSelection,
    column_field: &Field,
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
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected_line = 0;
        for entry in entries(column) {
            match entry {
                Entry::Lane(title) => lines.push(Line::from(Span::styled(
                    pad_to_width(&truncate_to_width(&format!("── {title} "), width), width),
                    theme.dim(),
                ))),
                Entry::Card { item, index } => {
                    let is_sel = col_index == selection.column && index == selection.index;
                    if is_sel {
                        selected_line = lines.len();
                    }
                    lines.extend(card_lines(item, column_field, width, is_sel, theme));
                }
            }
        }
        let rows = grid.len();
        let scroll = if col_index == selection.column && rows > 1 {
            (selected_line + 2).saturating_sub(rows)
        } else {
            0
        };
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
        let screen = render_to_string(100, 10, |f| {
            render_board(f, f.area(), &cols, &sel, status, &Theme::plain())
        });
        let first = screen.lines().next().unwrap();
        assert!(
            first.contains("Todo 2") && first.contains("No Status 1"),
            "{screen}"
        );
        assert!(screen.contains("? #5 Old option"));
        assert!(screen.contains("#3 Emoji 🚀"));
        assert!(
            screen
                .lines()
                .all(|l| crate::ui::text::display_width(l) <= 100)
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
        let screen = render_to_string(50, 8, |f| {
            render_board(f, f.area(), &cols, &sel, status, &Theme::plain())
        });
        assert!(
            screen.lines().next().unwrap().contains("3–4 of 4"),
            "{screen}"
        );
        assert!(screen.contains("No Status"));
        assert!(!screen.contains("Todo 2"));
    }
}
