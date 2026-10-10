//! The whole screen: header, view tabs, body (table, board, detail), status or input line,
//! and the help, picker and setup overlays.

use crate::model::Layout;
use crate::ui::app::{App, Mode};
use crate::ui::board::render_board;
use crate::ui::detail::{build_doc, render_detail};
use crate::ui::picker::render_picker;
use crate::ui::table::{render_table, table_columns};
use crate::ui::text::{display_width, pad_to_width, truncate_to_width};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout as Split, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

pub const MIN_WIDTH: u16 = 40;
pub const MIN_HEIGHT: u16 = 8;
const SPLIT_DETAIL_AT: u16 = 140;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("Widen the pane to see the board.").wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let [header, tabs, body, status] = Split::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, header, app);
    draw_tabs(frame, tabs, app);
    if app.mode == Mode::Setup {
        let text = app.setup.clone().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" setup · r retry · q quit "),
            ),
            body,
        );
    } else if app.mode == Mode::Detail && app.detail.is_some() {
        if body.width >= SPLIT_DETAIL_AT {
            let [left, right] =
                Split::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(body);
            draw_body(frame, left, app);
            draw_detail(frame, right, app);
        } else {
            draw_detail(frame, body, app);
        }
    } else {
        draw_body(frame, body, app);
    }
    draw_status(frame, status, app);
    if app.mode == Mode::Help {
        draw_help(frame, body, app);
    }
    if app.mode == Mode::Picker
        && let Some(p) = &app.picker
    {
        render_picker(frame, area, p, &app.theme);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let title = app
        .project()
        .map(|p| format!("{} · {}", p.title, p.board))
        .unwrap_or_else(|| "project-boards".into());
    let mut flags = Vec::new();
    if let Some((loaded, total)) = app.status.loading {
        flags.push(format!("loading {loaded}/{total}"));
    } else if app.status.stale {
        flags.push("stale".into());
    }
    if app.project().is_some_and(|p| !p.viewer_can_update) {
        flags.push("read-only".into());
    }
    if app.status.rate_low {
        flags.push("rate limit low".into());
    }
    let right = flags.join(" · ");
    let left_width = (area.width as usize).saturating_sub(display_width(&right) + 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(pad_to_width(&title, left_width), app.theme.bold()),
            Span::styled(format!(" {right}"), app.theme.dim()),
        ])),
        area,
    );
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let Some(project) = app.project() else { return };
    let current = app.current_view().map(|v| v.id.clone());
    let marker = if app.layout().0 == Layout::Board {
        "▥ board"
    } else {
        "▤ table"
    };
    // Each tab is " name " plus a separating space. The marker always stays visible, so the
    // tabs get the rest of the row.
    let tab = |name: &str| format!(" {name} ");
    let widths: Vec<usize> = project
        .views
        .iter()
        .map(|v| display_width(&tab(&v.name)) + 1)
        .collect();
    let room = (area.width as usize).saturating_sub(display_width(marker));
    let selected = project
        .views
        .iter()
        .position(|v| Some(&v.id) == current.as_ref())
        .unwrap_or(0);
    // Scroll just far enough that the selected tab fits; with room to spare nothing moves.
    let mut start = 0;
    while start < selected && widths[start..=selected].iter().sum::<usize>() > room {
        start += 1;
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, v) in project.views.iter().enumerate().skip(start) {
        let text = if used + widths[i] <= room {
            tab(&v.name)
        } else if i == selected {
            // Too wide on its own: shorten the name rather than hide the current view.
            tab(&truncate_to_width(&v.name, room.saturating_sub(3)))
        } else {
            break;
        };
        used += display_width(&text) + 1;
        let style = if i == selected {
            app.theme.selected()
        } else {
            app.theme.dim()
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(marker, app.theme.accent()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    let Some(project) = app.project() else {
        frame.render_widget(
            Paragraph::new(Line::styled("Loading the board…", app.theme.dim())),
            area,
        );
        return;
    };
    if app.view_items().is_none() {
        frame.render_widget(
            Paragraph::new(Line::styled("Loading this view…", app.theme.dim())),
            area,
        );
        return;
    }
    match app.layout().0 {
        Layout::Board => {
            if let Some(field) = app.column_field() {
                render_board(
                    frame,
                    area,
                    &app.board_columns(),
                    &app.board_sel,
                    field,
                    &app.theme,
                );
            }
        }
        _ => {
            let view = app.current_view().expect("a project with views");
            render_table(
                frame,
                area,
                &app.table_rows(),
                &table_columns(view, project),
                app.table_selected,
                &app.theme,
            );
        }
    }
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let inner = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    app.detail_width = inner.width;
    let (Some(state), Some(item), Some(project)) =
        (app.detail.as_ref(), app.detail_item(), app.project())
    else {
        return;
    };
    let doc = build_doc(item, project, state, inner.width, &app.theme);
    let max_scroll = doc
        .lines
        .len()
        .saturating_sub(crate::ui::detail::body_height(&doc, inner.height));
    if let Some(state) = app.detail.as_mut() {
        state.max_scroll = max_scroll;
    }
    let Some(state) = app.detail.as_ref() else {
        return;
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().borders(Borders::LEFT), area);
    render_detail(frame, inner, &doc, state, &app.theme);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let line = match app.mode {
        Mode::Search => Line::from(vec![
            Span::styled("/", app.theme.accent()),
            Span::raw(app.input.clone()),
        ]),
        Mode::Filter => {
            let mut spans = vec![Span::styled("filter: ", app.theme.accent())];
            let github = app.current_view().map_or("", |v| v.filter.trim());
            if !github.is_empty() {
                // The GitHub filter still applies: show it dimmed, cut to leave room to type.
                let room = (area.width as usize / 2)
                    .saturating_sub(display_width("filter: GitHub:  + "))
                    .max(8);
                spans.push(Span::styled(
                    format!("GitHub: {} + ", truncate_to_width(github, room)),
                    app.theme.dim(),
                ));
            }
            spans.push(Span::raw(app.input.clone()));
            Line::from(spans)
        }
        _ => {
            if let Some(flash) = &app.status.flash {
                Line::styled(
                    truncate_to_width(flash, area.width as usize),
                    app.theme.accent(),
                )
            } else if let Some(e) = &app.status.error {
                Line::styled(truncate_to_width(e, area.width as usize), app.theme.error())
            } else {
                let mut parts: Vec<String> = Vec::new();
                if let Some(extra) = app.extra_filter() {
                    parts.push(format!("filter: + {extra}"));
                }
                parts.extend(app.status.notes.iter().cloned());
                if let Some(note) = app.layout().1 {
                    parts.push(note.to_string());
                }
                if let Some(list) = app.view_list().filter(|l| l.truncated) {
                    parts.push(format!("loaded {} of {}", list.ids.len(), list.total));
                }
                parts.push("? help · / search · f filter · L layout · q quit".into());
                Line::styled(
                    truncate_to_width(&parts.join(" · "), area.width as usize),
                    app.theme.dim(),
                )
            }
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    use crate::ui::keymap::Action;
    let lines: Vec<Line> = Action::ALL
        .iter()
        .map(|a| {
            let keys: Vec<String> = app.keymap.keys_for(*a).iter().map(|k| k.label()).collect();
            Line::from(vec![
                Span::styled(format!("{:<14}", keys.join(" ")), app.theme.accent()),
                Span::raw(a.description()),
            ])
        })
        .collect();
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 64.min(area.width);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" keys · any key closes "),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use crate::ui::fixtures::{code, key, render_to_string, snapshot};
    use crate::ui::keymap::Keymap;
    use crate::ui::theme::Theme;
    use crossterm::event::KeyCode;

    fn app() -> App {
        App::new(
            Box::new(MemoryStore::new(Some(snapshot()))),
            Keymap::defaults(),
            Theme::plain(),
        )
    }

    #[test]
    fn full_screen_has_header_tabs_body_and_hints() {
        let mut a = app();
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        let lines: Vec<&str> = screen.lines().collect();
        assert!(lines[0].starts_with("Testbed · tviles/3"));
        assert!(
            lines[1].contains("Table")
                && lines[1].contains("Board")
                && lines[1].contains("▤ table")
        );
        assert!(lines[2].starts_with("Title"));
        assert!(lines[11].contains("? help"));
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn a_cjk_title_keeps_the_flags_at_the_right_edge() {
        let mut snap = snapshot();
        snap.project.title = "中文看板中文看板".into();
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.status.stale = true;
        let screen = render_to_string(44, 10, |f| draw(f, &mut a));
        let first = screen.lines().next().unwrap();
        assert!(first.ends_with(" stale"), "{first}");
        assert!(display_width(first) <= 44);
    }

    #[test]
    fn picker_and_setup_overlays_render_and_tiny_sizes_do_not_panic() {
        let mut a = app();
        a.picker = Some(crate::ui::picker::PickerState::loading(false)); // `B` was pressed
        a.on_sync(crate::sync::SyncEvent::Projects(vec![
            crate::model::ProjectSummary {
                id: crate::model::ProjectId::new("x"),
                board: "acme/7".parse().unwrap(),
                title: "Sprint".into(),
                closed: false,
            },
        ]));
        let screen = render_to_string(80, 16, |f| draw(f, &mut a));
        assert!(screen.contains("acme/7  Sprint") && screen.contains("esc cancels"));
        render_to_string(0, 0, |f| draw(f, &mut a));
        render_to_string(40, 8, |f| draw(f, &mut a));

        let mut b = app();
        b.mode = Mode::Setup;
        b.setup = Some("Run gh auth login first.".into());
        let screen = render_to_string(80, 12, |f| draw(f, &mut b));
        assert!(screen.contains("Run gh auth login first.") && screen.contains("setup"));
        render_to_string(40, 8, |f| draw(f, &mut b));
    }

    #[test]
    fn a_flash_replaces_the_status_line_until_the_next_key() {
        let mut a = app();
        a.status.error = Some("refresh failed: down".into());
        a.status.flash = Some("not opened: not a web link".into());
        let screen = render_to_string(80, 10, |f| draw(f, &mut a));
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .starts_with("not opened: not a web link")
        );
        a.handle_key(key('j'));
        let screen = render_to_string(80, 10, |f| draw(f, &mut a));
        assert!(screen.lines().last().unwrap().starts_with("refresh failed"));
    }

    #[test]
    fn the_local_filter_shows_in_the_status_line_and_the_prompt_shows_githubs() {
        let mut a = app();
        a.select_view(&crate::model::ViewId::new("V_bugs"));
        a.handle_key(key('f'));
        let screen = render_to_string(80, 10, |f| draw(f, &mut a));
        assert_eq!(
            screen.lines().last().unwrap(),
            "filter: GitHub: label:bug +"
        );
        for c in "assignee:x".chars() {
            a.handle_key(key(c));
        }
        a.handle_key(code(KeyCode::Enter));
        let screen = render_to_string(80, 10, |f| draw(f, &mut a));
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .starts_with("filter: + assignee:x · "),
            "{screen}"
        );

        // A long GitHub filter is cut so the typed text keeps its room.
        let mut snap = snapshot();
        snap.project.views[2].filter = format!("label:bug {}", "-label:wontfix ".repeat(10));
        let mut b = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        b.select_view(&crate::model::ViewId::new("V_bugs"));
        b.handle_key(key('f'));
        b.handle_key(key('z'));
        let screen = render_to_string(60, 10, |f| draw(f, &mut b));
        let prompt = screen.lines().last().unwrap();
        assert!(
            prompt.starts_with("filter: GitHub: label:bug") && prompt.ends_with("… + z"),
            "{prompt}"
        );
        // Without a GitHub filter there is nothing to show.
        let mut c = app();
        c.handle_key(key('f'));
        let screen = render_to_string(60, 10, |f| draw(f, &mut c));
        assert_eq!(screen.lines().last().unwrap(), "filter:");
    }

    #[test]
    fn overflowing_tabs_scroll_to_keep_the_current_tab_and_layout_visible() {
        let mut snap = snapshot();
        let template = snap.project.views[0].clone();
        snap.project.views = (1..=8)
            .map(|n| crate::model::View {
                id: crate::model::ViewId::new(format!("V{n}")),
                name: format!("Sprint planning view {n}"),
                ..template.clone()
            })
            .collect();
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        for n in 1..=8 {
            a.select_view(&crate::model::ViewId::new(format!("V{n}")));
            let screen = render_to_string(40, 10, |f| draw(f, &mut a));
            let tabs = screen.lines().nth(1).unwrap();
            assert!(
                tabs.contains(&format!("Sprint planning view {n}")),
                "view {n}: {tabs:?}"
            );
            assert!(tabs.contains("▤ table"), "view {n}: {tabs:?}");
        }
    }

    #[test]
    fn a_tab_wider_than_the_pane_is_shortened() {
        let mut snap = snapshot();
        snap.project.views[2].name = "An extremely long view name that cannot fit".into();
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.select_view(&crate::model::ViewId::new("V_bugs"));
        let screen = render_to_string(40, 10, |f| draw(f, &mut a));
        let tabs = screen.lines().nth(1).unwrap();
        assert!(
            tabs.starts_with(" An extremely") && tabs.contains("…"),
            "{tabs:?}"
        );
        assert!(tabs.contains("▤ table"), "{tabs:?}");
    }

    #[test]
    fn tiny_panes_ask_to_be_widened() {
        let mut a = app();
        assert!(render_to_string(30, 6, |f| draw(f, &mut a)).contains("Widen the pane"));
    }

    #[test]
    fn wide_panes_split_board_and_detail() {
        let mut a = app();
        a.handle_key(code(KeyCode::Enter));
        let screen = render_to_string(160, 14, |f| draw(f, &mut a));
        assert!(
            screen.lines().nth(2).unwrap().starts_with("Title"),
            "board still on the left"
        );
        assert!(screen.contains("Loading…"), "detail on the right");
    }

    #[test]
    fn help_lists_every_action() {
        let mut a = app();
        a.handle_key(key('?'));
        let screen = render_to_string(100, 30, |f| draw(f, &mut a));
        assert!(screen.contains("quick search") && screen.contains("switch table / board"));
    }

    #[test]
    fn read_only_and_stale_flags_show_in_the_header() {
        let mut snap = snapshot();
        snap.project.viewer_can_update = false;
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.status.stale = true;
        let screen = render_to_string(100, 10, |f| draw(f, &mut a));
        assert!(
            screen
                .lines()
                .next()
                .unwrap()
                .ends_with("stale · read-only")
        );
    }
}
