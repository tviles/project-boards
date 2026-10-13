//! The whole screen: header, view tabs, body (table, board, detail), status or input line,
//! and the help, picker and setup overlays.

use crate::model::Layout;
use crate::ui::app::{App, LineKind, Mode, Popup};
use crate::ui::board::render_board_with;
use crate::ui::detail::{build_doc, render_detail};
use crate::ui::keymap::Action;
use crate::ui::marks::ItemMarks;
use crate::ui::multiselect::render_multiselect;
use crate::ui::picker::render_picker;
use crate::ui::popup::{render_line_popup, render_list_picker};
use crate::ui::table::{render_table_with, table_columns};
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
    let marks = app.marks();
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
    } else if app.detail.is_some() {
        if body.width >= SPLIT_DETAIL_AT {
            let [left, right] =
                Split::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(body);
            draw_body(frame, left, app, &marks);
            draw_detail(frame, right, app);
        } else {
            draw_detail(frame, body, app);
        }
    } else {
        draw_body(frame, body, app, &marks);
    }
    draw_status(frame, status, app, &marks);
    if app.mode == Mode::Help {
        draw_help(frame, body, app);
    }
    if app.mode == Mode::Picker
        && let Some(p) = &app.picker
    {
        render_picker(frame, area, p, &app.theme);
    }
    if app.mode == Mode::Popup {
        draw_popup(frame, body, app);
    }
}

fn draw_popup(frame: &mut Frame, area: Rect, app: &App) {
    let Some(popup) = &app.popup else { return };
    let theme = &app.theme;
    match popup {
        Popup::Actions { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Fields { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Value { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Options { select, .. } => render_multiselect(frame, area, select, theme),
        Popup::Line {
            kind,
            title,
            input,
            error,
            ..
        } => {
            let hint = match kind {
                LineKind::Text => "enter saves · empty clears · esc cancels",
                LineKind::Number => "a number · empty clears · esc cancels",
                LineKind::Date => "2026-10-03, +3d, fri · empty clears · esc cancels",
            };
            render_line_popup(frame, area, title, input, hint, error.as_deref(), theme)
        }
    }
}

/// A header flag in its full and its short form.
struct Flag {
    long: String,
    short: String,
}

impl Flag {
    fn same(text: String) -> Flag {
        Flag {
            long: text.clone(),
            short: text,
        }
    }
}

/// The write flags in priority order (read-only, paused, saving), each in both forms.
fn write_flag_forms(app: &App, now: std::time::Instant) -> Vec<Flag> {
    let mut flags = Vec::new();
    if app.project().is_some_and(|p| !p.viewer_can_update) {
        flags.push(Flag::same("read-only".into()));
    } else if app.project().is_some() && !app.edit_context().has_project_scope() {
        flags.push(Flag {
            long: "read-only: token lacks the project scope".into(),
            short: "read-only (no scope)".into(),
        });
    }
    if let Some(until) = app.status.write_pause {
        let left = until.saturating_duration_since(now);
        // Rounded up, so the last second does not read blank.
        let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        if secs > 0 {
            flags.push(Flag {
                long: format!("writes paused {secs}s"),
                short: format!("paused {secs}s"),
            });
        }
    }
    let pending = app.pending_count();
    if pending > 0 {
        flags.push(Flag {
            long: format!("saving {pending}…"),
            short: format!("⋯{pending}"),
        });
    }
    flags
}

/// The header flags about writing: edits sending, a rate-limit pause, and why the board is
/// read-only.
pub fn write_flags(app: &App, now: std::time::Instant) -> Vec<String> {
    let mut forms = write_flag_forms(app, now);
    // Priority order is read-only, paused, saving; the header reads saving first.
    forms.sort_by_key(|f| !f.long.starts_with("saving"));
    forms.into_iter().map(|f| f.long).collect()
}

/// Cells the header always keeps for the title.
const MIN_TITLE: usize = 12;

/// The header's flags that fit in `room` cells: full forms if they all fit, else short forms,
/// else the most important ones (read-only, paused, saving, stale or loading, rate limit).
fn fit_flags(flags: &[Flag], room: usize) -> String {
    let join = |list: &[String]| list.join(" · ");
    let longs: Vec<String> = flags.iter().map(|f| f.long.clone()).collect();
    if display_width(&join(&longs)) <= room {
        return join(&longs);
    }
    let mut shorts: Vec<String> = flags.iter().map(|f| f.short.clone()).collect();
    while !shorts.is_empty() && display_width(&join(&shorts)) > room {
        shorts.pop();
    }
    join(&shorts)
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let title = app
        .project()
        .map(|p| format!("{} · {}", p.title, p.board))
        .unwrap_or_else(|| "project-boards".into());
    let mut flags = write_flag_forms(app, std::time::Instant::now());
    if let Some((loaded, total)) = app.status.loading {
        flags.push(Flag::same(format!("loading {loaded}/{total}")));
    } else if app.status.stale {
        flags.push(Flag::same("stale".into()));
    }
    if app.status.rate_low {
        flags.push(Flag::same("rate limit low".into()));
    }
    let width = area.width as usize;
    let right = fit_flags(&flags, width.saturating_sub(MIN_TITLE + 1));
    let left_width = width.saturating_sub(display_width(&right) + 1);
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

fn draw_body(frame: &mut Frame, area: Rect, app: &App, marks: &ItemMarks) {
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
                render_board_with(
                    frame,
                    area,
                    &app.board_columns(),
                    &app.board_sel,
                    field,
                    &app.card_fields(),
                    marks,
                    &app.theme,
                );
            }
        }
        _ => {
            let view = app.current_view().expect("a project with views");
            render_table_with(
                frame,
                area,
                &app.table_rows(),
                &table_columns(view, project),
                app.table_selected,
                marks,
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

fn draw_status(frame: &mut Frame, area: Rect, app: &App, marks: &ItemMarks) {
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
        Mode::Confirm => Line::styled(
            truncate_to_width(
                app.confirm.as_ref().map_or("", |c| c.question.as_str()),
                area.width as usize,
            ),
            app.theme.accent().patch(app.theme.bold()),
        ),
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
                let clear = app
                    .extra_filter()
                    .and(app.keymap.keys_for(Action::ClearFilter).first().copied())
                    .map(|k| format!(" · {} clear filter", k.label()))
                    .unwrap_or_default();
                parts.extend(app.edit_hints());
                parts.push(format!("/ search · f filter{clear} · L layout · q quit"));
                // The reminder outranks the hints: they are cut for it, but only when what
                // is left of them is still worth showing.
                let reminder = (!marks.unmatched.is_empty()
                    && area.width as usize
                        >= display_width(crate::ui::marks::UNMATCHED_TEXT)
                            + display_width(" · ? help")
                            + MIN_HINT_ROOM)
                    .then_some(crate::ui::marks::UNMATCHED_TEXT);
                Line::styled(
                    status_line_with(&parts.join(" · "), reminder, area.width as usize),
                    app.theme.dim(),
                )
            }
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The least room the hints keep when the unmatched reminder is shown.
const MIN_HINT_ROOM: usize = 12;

/// The bottom hint line: `left` cut with `…` to fit, then `? help`, which always shows
/// (pre-flight ruling 4).
pub fn status_line(left: &str, width: usize) -> String {
    status_line_with(left, None, width)
}

/// `status_line` with `keep` (never cut) between `left` and `? help`.
fn status_line_with(left: &str, keep: Option<&str>, width: usize) -> String {
    const HELP: &str = "? help";
    let mut tail = String::new();
    if let Some(keep) = keep {
        tail.push_str(" · ");
        tail.push_str(keep);
    }
    tail.push_str(" · ");
    tail.push_str(HELP);
    if left.is_empty() {
        let tail = tail.trim_start_matches(" · ");
        return truncate_to_width(tail, width);
    }
    let room = width.saturating_sub(display_width(&tail));
    format!("{}{tail}", truncate_to_width(left, room))
}

/// The help overlay: every action with its keys. When the pane is too short for all of them
/// it scrolls (`j`/`k`/arrows; any other key closes).
fn draw_help(frame: &mut Frame, area: Rect, app: &mut App) {
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
    let total = lines.len();
    let h = (total as u16 + 2).min(area.height);
    let visible = (h as usize).saturating_sub(2);
    app.help_visible = visible;
    let scrolls = total > visible;
    app.help_scroll = app.help_scroll.min(total.saturating_sub(visible));
    let title = if scrolls {
        format!(
            " keys {}-{} of {total} · j/k scroll · other key closes ",
            app.help_scroll + 1,
            app.help_scroll + visible
        )
    } else {
        " keys · any key closes ".to_string()
    };
    let w = 64.min(area.width);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y,
        width: w,
        height: h,
    };
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(app.help_scroll)
        .take(visible)
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(shown).block(Block::default().borders(Borders::ALL).title(title)),
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
    fn the_header_says_saving_paused_and_read_only() {
        use crate::model::{FieldId, FieldValue, ItemId, OpKind, OptionId};
        use crate::ops::Op;
        let now = std::time::Instant::now();
        let mut a = app();
        assert!(write_flags(&a, now).is_empty());
        a.submit(
            OpKind::Move,
            Op::SetField {
                item: ItemId::new("a"),
                field: FieldId::new("F_status"),
                value: FieldValue::SingleSelect {
                    option_id: OptionId::new("o_done"),
                    name: "Done".into(),
                },
            },
        );
        a.status.write_pause = Some(now + std::time::Duration::from_secs(30));
        assert_eq!(write_flags(&a, now), ["saving 1…", "writes paused 30s"]);
        a.on_sync(crate::sync::SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        assert!(
            write_flags(&a, now).contains(&"read-only: token lacks the project scope".to_string())
        );
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        assert!(
            screen.lines().next().unwrap().contains("saving 1…"),
            "{screen}"
        );
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
        assert!(screen.contains("x clear filter"), "{screen}");
        a.handle_key(key('x'));
        let screen = render_to_string(80, 10, |f| draw(f, &mut a));
        let hints = screen.lines().last().unwrap();
        assert!(
            !hints.contains("clear filter") && !hints.contains("filter: +"),
            "{hints}"
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
        assert!(
            screen
                .lines()
                .any(|l| l.contains("x ") && l.contains("clear the local filter")),
            "{screen}"
        );
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
                .ends_with("read-only · stale")
        );
    }

    fn flagged_app() -> App {
        use crate::model::{FieldId, FieldValue, ItemId, OpKind, OptionId};
        use crate::ops::Op;
        let mut a = app();
        a.submit(
            OpKind::Move,
            Op::SetField {
                item: ItemId::new("a"),
                field: FieldId::new("F_status"),
                value: FieldValue::SingleSelect {
                    option_id: OptionId::new("o_done"),
                    name: "Done".into(),
                },
            },
        );
        a.status.write_pause = Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        a.on_sync(crate::sync::SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        a.status.loading = Some((1, 2));
        a.status.rate_low = true;
        a
    }

    #[test]
    fn the_title_survives_every_header_flag_at_any_width() {
        for width in [40u16, 60, 80] {
            let mut a = flagged_app();
            let screen = render_to_string(width, 10, |f| draw(f, &mut a));
            let head = screen.lines().next().unwrap();
            assert!(head.starts_with("Testbed · tviles/3"), "{width}: {head}");
            // Flags come whole or not at all: each part between separators is a full flag.
            let right = head.trim_end().split_once("tviles/3").unwrap().1.trim();
            for part in right.split(" · ").filter(|p| !p.is_empty()) {
                let known = [
                    "read-only (no scope)",
                    "read-only: token lacks the project scope",
                    "writes paused",
                    "paused",
                    "saving",
                    "⋯",
                    "loading 1/2",
                    "rate limit low",
                ];
                assert!(
                    known.iter().any(|k| part.starts_with(k)),
                    "{width}: {part:?} in {head}"
                );
            }
            if width == 80 {
                assert!(head.contains("loading 1/2"), "{head}");
            }
            if width == 40 {
                assert!(head.contains("read-only (no scope)"), "{head}");
            }
        }
    }

    #[test]
    fn short_flags_drop_the_least_important_first() {
        let a = flagged_app();
        let flags = {
            let mut f = write_flag_forms(&a, std::time::Instant::now());
            f.push(Flag::same("loading 1/2".into()));
            f.push(Flag::same("rate limit low".into()));
            f
        };
        assert_eq!(fit_flags(&flags, 200).matches(" · ").count(), 4);
        let tight = fit_flags(&flags, 40);
        assert!(
            tight.starts_with("read-only (no scope) · paused"),
            "{tight}"
        );
        assert!(!tight.contains("rate limit"), "{tight}");
        assert_eq!(fit_flags(&flags, 5), "");
    }

    #[test]
    fn the_pause_countdown_rounds_up() {
        let now = std::time::Instant::now();
        let mut a = app();
        a.status.write_pause = Some(now + std::time::Duration::from_millis(400));
        assert_eq!(write_flags(&a, now), ["writes paused 1s"]);
        a.status.write_pause = Some(now + std::time::Duration::from_millis(29_100));
        assert_eq!(write_flags(&a, now), ["writes paused 30s"]);
        a.status.write_pause = Some(now);
        assert!(write_flags(&a, now).is_empty());
    }

    #[test]
    fn a_read_only_project_says_plain_read_only_even_without_the_scope() {
        let now = std::time::Instant::now();
        let mut s = snapshot();
        s.project.viewer_can_update = false;
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(s))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.on_sync(crate::sync::SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        assert_eq!(write_flags(&a, now), ["read-only"]);
    }

    #[test]
    fn a_narrow_status_line_keeps_help_with_unmatched_items() {
        let mut a = app();
        a.status.notes.push("a note".into());
        let mut marks = ItemMarks::default();
        marks.unmatched.insert(crate::model::ItemId::new("a"));
        for width in [40u16, 60, 80, 120] {
            let screen = render_to_string(width, 10, |f| {
                draw_status(f, f.area(), &a, &marks);
            });
            let line = screen.lines().next().unwrap();
            let has_reminder = line.contains("⊘ no longer matches filter");
            // The reminder is only shown when `? help` still fits.
            assert!(!has_reminder || line.contains("? help"), "{width}: {line}");
            if width == 120 {
                assert!(has_reminder, "{line}");
            }
        }
    }

    #[test]
    fn a_popover_opened_from_the_detail_draws_over_it() {
        let mut a = app();
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('e'));
        assert_eq!(a.mode, Mode::Popup);
        let screen = render_to_string(100, 40, |f| draw(f, &mut a));
        let lines: Vec<&str> = screen.lines().collect();
        assert!(
            lines[2].starts_with('│'),
            "the detail's border, not the table header: {screen}"
        );
        assert!(screen.contains("Edit · Fix crash"), "{screen}");
    }

    #[test]
    fn the_hint_line_keeps_help_visible_when_narrow() {
        let mut a = app();
        let screen = render_to_string(50, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap();
        assert!(last.ends_with(" · ? help"), "{last}");
        assert!(
            last.contains('…'),
            "the edit hints are cut, not ? help: {last}"
        );
        assert!(display_width(last) <= 50);
        assert_eq!(status_line("", 10), "? help");
        assert_eq!(status_line("e field", 40), "e field · ? help");
    }

    #[test]
    fn help_scrolls_when_the_pane_is_short() {
        let mut a = app();
        a.handle_key(key('?'));
        let screen = render_to_string(100, 14, |f| draw(f, &mut a));
        assert!(screen.contains("1-"), "{screen}");
        assert!(screen.contains("j/k scroll"), "{screen}");
        let first_key = a.help_scroll;
        assert_eq!(first_key, 0);
        // The last action is not visible until we scroll down.
        let last = Action::ALL[Action::ALL.len() - 1].description();
        assert!(!screen.contains(last), "{screen}");
        for _ in 0..40 {
            assert!(a.handle_key(key('j')).is_empty());
            assert_eq!(a.mode, Mode::Help, "j scrolls, it does not close");
            render_to_string(100, 14, |f| draw(f, &mut a));
        }
        let screen = render_to_string(100, 14, |f| draw(f, &mut a));
        assert!(screen.contains(last), "{screen}");
        assert!(
            !screen.contains(Action::ALL[0].description()),
            "scrolled past the first rows: {screen}"
        );
        a.handle_key(key('k'));
        assert_eq!(a.mode, Mode::Help);
        a.handle_key(key('x'));
        assert_eq!(a.mode, Mode::Normal, "any other key closes");
    }

    #[test]
    fn help_that_fits_closes_on_any_key() {
        let mut a = app();
        a.handle_key(key('?'));
        render_to_string(100, 40, |f| draw(f, &mut a));
        a.handle_key(key('j'));
        assert_eq!(a.mode, Mode::Normal);
    }
}
