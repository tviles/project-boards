use crate::model::*;
use crate::ui::text::{pad_to_width, truncate_to_width};
use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Row<'a> {
    Group {
        key: Option<String>,
        title: String,
        color: OptionColor,
        count: usize,
        collapsed: bool,
    },
    Item(&'a Item),
}

/// The key a collapsed group is remembered by.
pub fn collapse_key(key: &Option<String>) -> String {
    key.clone().unwrap_or_else(|| "\u{0}none".to_string())
}

/// Buckets for any field: option/iteration buckets when it has them, otherwise one bucket
/// per distinct display value (sorted), then "No <field>".
pub(crate) fn buckets_for(items: &[&Item], field: &Field) -> Vec<Bucket> {
    if let Some(b) = field.buckets() {
        return b;
    }
    let mut values: Vec<String> = items
        .iter()
        .filter_map(|i| i.value(&field.id).map(|v| v.display()))
        .filter(|s| !s.is_empty())
        .collect();
    values.sort();
    values.dedup();
    let mut out: Vec<Bucket> = values
        .into_iter()
        .map(|v| Bucket {
            key: Some(v.clone()),
            title: v,
            color: OptionColor::Gray,
            completed: false,
        })
        .collect();
    out.push(Bucket {
        key: None,
        title: format!("No {}", field.name),
        color: OptionColor::Gray,
        completed: false,
    });
    out
}

/// The bucket key of `item` for `field`, mapped to `None` when it names no bucket
/// (for example an option deleted on GitHub).
pub(crate) fn bucket_of(item: &Item, field: &Field, buckets: &[Bucket]) -> Option<String> {
    let raw = item
        .value(&field.id)
        .map(|v| v.bucket_key().unwrap_or_else(|| v.display()));
    raw.filter(|k| buckets.iter().any(|b| b.key.as_deref() == Some(k.as_str())))
}

pub fn build_rows<'a>(
    items: &[&'a Item],
    group: Option<&Field>,
    collapsed: &HashSet<String>,
) -> Vec<Row<'a>> {
    let Some(field) = group else {
        return items.iter().copied().map(Row::Item).collect();
    };
    let buckets = buckets_for(items, field);
    let mut rows = Vec::new();
    for bucket in &buckets {
        let members: Vec<&'a Item> = items
            .iter()
            .copied()
            .filter(|i| bucket_of(i, field, &buckets) == bucket.key)
            .collect();
        if members.is_empty() {
            continue;
        }
        let is_collapsed = collapsed.contains(&collapse_key(&bucket.key));
        rows.push(Row::Group {
            key: bucket.key.clone(),
            title: bucket.title.clone(),
            color: bucket.color,
            count: members.len(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            rows.extend(members.into_iter().map(Row::Item));
        }
    }
    rows
}

/// The view's visible fields with Title first. A view with none shows Title and Status.
pub fn table_columns<'a>(view: &View, project: &'a Project) -> Vec<&'a Field> {
    let mut cols: Vec<&Field> = view
        .visible_fields
        .iter()
        .filter_map(|id| project.field(id))
        .collect();
    if cols.is_empty() {
        cols.extend(
            project
                .fields
                .iter()
                .filter(|f| f.name == "Status" && f.kind.can_be_column()),
        );
    }
    cols.retain(|f| f.kind != FieldKind::Title);
    if let Some(title) = project.title_field() {
        cols.insert(0, title);
    }
    cols
}

pub fn cell_text(item: &Item, field: &Field) -> String {
    if field.kind == FieldKind::Title {
        return match item.number() {
            Some(n) => format!("#{n} {}", item.title()),
            None => item.title().to_string(),
        };
    }
    item.value(&field.id)
        .map(|v| v.display())
        .unwrap_or_default()
}

/// Column widths: every non-title column gets 10–20 cells by its header; the title takes
/// the rest. Columns are dropped from the right until the title has at least 20 cells.
fn widths(columns: &[&Field], total: usize) -> Vec<usize> {
    let mut others: Vec<usize> = columns
        .iter()
        .skip(1)
        .map(|f| (f.name.chars().count() + 2).clamp(10, 20))
        .collect();
    loop {
        // One separator cell after every column, the title's included.
        let used: usize = others.iter().sum::<usize>() + others.len() + 1;
        if total >= used + 20 || others.is_empty() {
            let mut out = vec![total.saturating_sub(used)];
            out.extend(others);
            return out;
        }
        others.pop();
    }
}

pub fn render_table(
    frame: &mut Frame,
    area: Rect,
    rows: &[Row],
    columns: &[&Field],
    selected: usize,
    theme: &Theme,
) {
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("No items match this view.", theme.dim())),
            area,
        );
        return;
    }
    let widths = widths(columns, area.width as usize);
    let shown: Vec<&Field> = columns.iter().take(widths.len()).copied().collect();
    let mut lines = Vec::new();
    let header: Vec<Span> = shown
        .iter()
        .zip(&widths)
        .flat_map(|(f, w)| {
            [
                Span::styled(pad_to_width(&f.name, *w), theme.bold()),
                Span::raw(" "),
            ]
        })
        .collect();
    lines.push(Line::from(header));

    let body_height = area.height.saturating_sub(1) as usize;
    let offset = if body_height == 0 {
        0
    } else {
        selected.saturating_sub(body_height - 1)
    };
    for (index, row) in rows.iter().enumerate().skip(offset).take(body_height) {
        let style = if index == selected {
            theme.selected()
        } else {
            ratatui::style::Style::default()
        };
        let line = match row {
            Row::Group {
                title,
                count,
                collapsed,
                color,
                ..
            } => {
                let marker = if *collapsed { "▸" } else { "▾" };
                Line::from(vec![
                    Span::styled(format!("{marker} "), style),
                    Span::styled(
                        truncate_to_width(
                            &format!("{title} ({count})"),
                            (area.width as usize).saturating_sub(2),
                        ),
                        theme.option(*color).patch(theme.bold()).patch(style),
                    ),
                ])
            }
            Row::Item(item) => Line::from(
                shown
                    .iter()
                    .zip(&widths)
                    .flat_map(|(f, w)| {
                        [
                            Span::styled(pad_to_width(&cell_text(item, f), *w), style),
                            Span::styled(" ", style),
                        ]
                    })
                    .collect::<Vec<_>>(),
            ),
        };
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{items, project, render_to_string};

    fn refs(items: &[Item]) -> Vec<&Item> {
        items.iter().collect()
    }

    #[test]
    fn ungrouped_rows_are_items_in_order() {
        let all = items();
        let rows = build_rows(&refs(&all), None, &HashSet::new());
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| matches!(r, Row::Item(_))));
    }

    #[test]
    fn grouping_follows_option_order_and_puts_unknown_options_in_no_value() {
        let all = items();
        let p = project();
        let status = p.fields.iter().find(|f| f.name == "Status").unwrap();
        let rows = build_rows(&refs(&all), Some(status), &HashSet::new());
        let groups: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Group { title, count, .. } => Some((title.as_str(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("Todo", 2),
                ("In Progress", 1),
                ("Done", 1),
                ("No Status", 1)
            ]
        );
    }

    #[test]
    fn collapsed_groups_hide_their_items() {
        let all = items();
        let p = project();
        let status = p.fields.iter().find(|f| f.name == "Status").unwrap();
        let collapsed: HashSet<String> = [collapse_key(&Some("o_todo".into()))].into();
        let rows = build_rows(&refs(&all), Some(status), &collapsed);
        assert!(matches!(
            &rows[0],
            Row::Group {
                collapsed: true,
                ..
            }
        ));
        assert!(matches!(&rows[1], Row::Group { title, .. } if title == "In Progress"));
    }

    /// Review Focus 4: a view without visible fields falls back to Title plus Status.
    #[test]
    fn columns_fall_back_to_title_and_status() {
        let p = project();
        let names = |v: &View| {
            table_columns(v, &p)
                .iter()
                .map(|f| f.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&p.views[0]), ["Title", "Status", "Assignees"]);
        assert_eq!(names(&p.views[2]), ["Title", "Status"]);
    }

    #[test]
    fn cells_show_numbers_drafts_and_values() {
        let all = items();
        let p = project();
        let title = p.title_field().unwrap();
        let status = p.fields.iter().find(|f| f.name == "Status").unwrap();
        assert_eq!(cell_text(&all[0], title), "#1 Fix crash");
        assert_eq!(cell_text(&all[3], title), "Draft idea");
        assert_eq!(cell_text(&all[0], status), "Todo");
        assert_eq!(
            cell_text(
                &all[1],
                p.fields.iter().find(|f| f.name == "Notes").unwrap()
            ),
            ""
        );
    }

    #[test]
    fn renders_header_rows_and_selection() {
        let all = items();
        let p = project();
        let rows = build_rows(&refs(&all), None, &HashSet::new());
        let cols = table_columns(&p.views[0], &p);
        let screen = render_to_string(60, 8, |f| {
            render_table(f, f.area(), &rows, &cols, 2, &Theme::plain())
        });
        let lines: Vec<&str> = screen.lines().collect();
        assert!(lines[0].starts_with("Title"));
        assert!(lines[1].starts_with("#1 Fix crash"));
        assert!(
            lines[3].contains("#3 Emoji 🚀"),
            "wide title renders: {screen}"
        );
        assert!(
            lines
                .iter()
                .all(|l| crate::ui::text::display_width(l) <= 60)
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn empty_view_says_so() {
        let p = project();
        let cols = table_columns(&p.views[0], &p);
        let screen = render_to_string(40, 4, |f| {
            render_table(f, f.area(), &[], &cols, 0, &Theme::plain())
        });
        assert!(screen.contains("No items match this view"));
    }
}
