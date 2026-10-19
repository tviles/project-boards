//! The whole screen: header, view tabs, body (table, board, detail), status or input line,
//! and the help, picker and setup overlays.

use crate::backend::read_only_reason;
use crate::model::Layout;
use crate::model::Provider;
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
        if let Some(Popup::Text { form, .. }) = &mut app.popup {
            crate::ui::textform::draw(frame, body, form, &app.theme);
        } else {
            draw_popup(frame, body, app);
        }
    }
    if app.mode == Mode::ErrorLog {
        draw_error_log(frame, body, app);
    }
}

/// The failed edits, newest first, over the board (and the detail, if one is open).
fn draw_error_log(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.errors.len().max(1) as u16;
    let rect = crate::ui::popup::centered(
        area,
        area.width.saturating_sub(4).min(100),
        (rows + 2).min(area.height),
    );
    let inner = rect.width.saturating_sub(2) as usize;
    let visible = rect.height.saturating_sub(2) as usize;
    let start = (app.error_sel + 1).saturating_sub(visible.max(1));
    let now = std::time::Instant::now();
    let mut lines: Vec<Line> = Vec::new();
    if app.errors.is_empty() {
        lines.push(Line::styled("No failed edits", app.theme.dim()));
    }
    for (row, failed) in app
        .errors
        .iter()
        .rev()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        let text = format!(
            "{:>7}  Could not {}: {}",
            crate::ui::app::edit_ago(now.saturating_duration_since(failed.at)),
            failed.what,
            failed.message
        );
        let style = if row == app.error_sel {
            app.theme.selected()
        } else {
            ratatui::style::Style::default()
        };
        lines.push(Line::styled(
            pad_to_width(&truncate_to_width(&text, inner), inner),
            style,
        ));
    }
    let retry = app
        .keymap
        .keys_for(Action::Retry)
        .first()
        .map(|k| k.label())
        .unwrap_or_default();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(format!(
            " failed edits · {retry} retry · d dismiss · esc close "
        ))),
        rect,
    );
}

fn draw_popup(frame: &mut Frame, area: Rect, app: &App) {
    let Some(popup) = &app.popup else { return };
    let theme = &app.theme;
    match popup {
        Popup::NewKind { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Search {
            input,
            searched,
            hits,
            selected,
            loading,
            error,
            ..
        } => {
            let status = match (error, loading, searched) {
                (Some(e), _, _) => e.clone(),
                (None, true, _) => "Searching…".to_string(),
                (None, false, Some(q)) if hits.is_empty() && q == input.trim() => {
                    "Nothing found".to_string()
                }
                _ => "Enter searches (repo:owner/name widens it; a pasted URL works) · Enter again adds"
                    .to_string(),
            };
            let rows: Vec<(String, String)> = hits
                .iter()
                .map(|h| {
                    let kind = if h.pull { "PR" } else { "issue" };
                    let detail = if app.on_board(h).is_some() {
                        format!("{kind} · already on the board")
                    } else {
                        kind.to_string()
                    };
                    (format!("{}#{} {}", h.repo, h.number, h.title), detail)
                })
                .collect();
            crate::ui::popup::render_search(frame, area, input, &status, &rows, *selected, theme)
        }
        Popup::Repo { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Actions { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Fields { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Value { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::Assignees { select, .. } => render_multiselect(frame, area, select, theme),
        Popup::Labels { select, .. } => render_multiselect(frame, area, select, theme),
        Popup::Milestone { picker, .. } => render_list_picker(frame, area, picker, theme),
        Popup::CloseReason { picker, .. } => render_list_picker(frame, area, picker, theme),
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
        // Drawn by `draw`, which has the form mutably.
        Popup::Text { .. } => {}
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
    if app
        .project()
        .is_some_and(|p| !p.viewer_can_update || read_only_reason(p.board.provider()).is_some())
    {
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
        .map(|p| match p.board.provider() {
            // A Linear board's key is an internal id; the badge says where it lives.
            Provider::Linear => format!("{} · {}", p.title, Provider::Linear),
            Provider::Github => format!("{} · {}", p.title, p.board),
        })
        .unwrap_or_else(|| "project-boards".into());
    let mut flags = write_flag_forms(app, std::time::Instant::now());
    if let Some((loaded, total)) = app.status.loading {
        flags.push(Flag::same(if total == 0 {
            format!("loading {loaded}")
        } else {
            format!("loading {loaded}/{total}")
        }));
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
            let github = app
                .current_view()
                .map_or("", |v| v.filter.github_query().trim());
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
            } else if matches!(app.popup, Some(Popup::Text { .. })) {
                // Board keys type text here, and the form draws its own hint in its border.
                Line::from("")
            } else if app.mode == Mode::ErrorLog {
                Line::styled(error_log_hints(app, area.width as usize), app.theme.dim())
            } else if let Some(e) = &app.status.error {
                Line::styled(
                    truncate_keeping_hint(app, e, area.width as usize),
                    app.theme.error(),
                )
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
                    parts.push(if list.total > list.ids.len() {
                        format!("loaded {} of {}", list.ids.len(), list.total)
                    } else {
                        format!("loaded {}+", list.ids.len())
                    });
                }
                let clear = app
                    .extra_filter()
                    .and(app.keymap.keys_for(Action::ClearFilter).first().copied())
                    .map(|k| k.label());
                if !app.errors.is_empty()
                    && let Some(k) = app.keymap.keys_for(Action::ErrorLog).first()
                {
                    parts.push(format!("{} {} failed", k.label(), app.errors.len()));
                }
                let edit = app.edit_hints().unwrap_or_default();
                let mut general: Vec<String> = app.new_hint().into_iter().collect();
                general.extend(["/ search".to_string(), "f filter".to_string()]);
                if let Some(k) = clear {
                    general.push(format!("{k} clear filter"));
                }
                general.extend(["L layout".to_string(), "q quit".to_string()]);
                // The reminder outranks the hints: they are cut for it, but only when what
                // is left of them is still worth showing.
                let actions = app.actions_hint();
                let kept = actions.as_ref().map_or(0, |a| display_width(a) + 3);
                let reminder = (!marks.unmatched.is_empty()
                    && area.width as usize
                        >= display_width(crate::ui::marks::UNMATCHED_TEXT)
                            + display_width(" · ? help")
                            + kept
                            + MIN_HINT_ROOM)
                    .then_some(crate::ui::marks::UNMATCHED_TEXT);
                let hints = Hints {
                    lead: &parts,
                    edit: &edit,
                    general: &general,
                    actions,
                };
                Line::styled(
                    status_hints(&hints, reminder, area.width as usize),
                    app.theme.dim(),
                )
            }
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// The error line cut to `width`, keeping its closing `· E error log` pointer.
fn truncate_keeping_hint(app: &App, error: &str, width: usize) -> String {
    let hint = app
        .keymap
        .keys_for(Action::ErrorLog)
        .first()
        .map(|k| format!(" · {} error log", k.label()));
    match hint {
        Some(hint) if error.ends_with(&hint) && display_width(error) > width => {
            let head = error.trim_end_matches(hint.as_str());
            let room = width.saturating_sub(display_width(&hint));
            format!("{}{hint}", truncate_to_width(head, room))
        }
        _ => truncate_to_width(error, width),
    }
}

/// The hint line while the error log is open.
fn error_log_hints(app: &App, width: usize) -> String {
    let key = |a: Action| {
        app.keymap
            .keys_for(a)
            .first()
            .map(|k| k.label())
            .unwrap_or_default()
    };
    let text = format!(
        "{} {} select · {} retry · d dismiss · esc close",
        key(Action::Down),
        key(Action::Up),
        key(Action::Retry)
    );
    truncate_to_width(&text, width)
}

/// The least room the hints keep when the unmatched reminder is shown.
const MIN_HINT_ROOM: usize = 12;

/// The pieces of the bottom hint line, in display order before `space actions · ? help`.
struct Hints<'a> {
    lead: &'a [String],
    edit: &'a [String],
    general: &'a [String],
    actions: Option<String>,
}

/// The hint line with `space actions` and `? help` always kept (Decision E17): when short of
/// room, the edit hints are dropped from the end first, then the general ones.
fn status_hints(h: &Hints, keep: Option<&str>, width: usize) -> String {
    let keep_all = match (&h.actions, keep) {
        (Some(a), Some(k)) => Some(format!("{a} · {k}")),
        (Some(a), None) => Some(a.clone()),
        (None, k) => k.map(str::to_string),
    };
    let tail = keep_all.as_deref();
    let tail_width = display_width(&tail.map(|t| format!(" · {t}")).unwrap_or_default())
        + display_width(" · ? help");
    let room = width.saturating_sub(tail_width);
    let join = |e: usize, g: usize| {
        h.lead
            .iter()
            .chain(&h.edit[..e])
            .chain(&h.general[..g])
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let fits = |text: &str| !text.is_empty() && display_width(text) <= room;
    let mut left = None;
    // With an item selected its edit keys matter more than the board-wide keys, so the
    // general hints go first (from the end), then the edit hints (E17).
    for g in (0..=h.general.len()).rev() {
        let t = join(h.edit.len(), g);
        if fits(&t) {
            left = Some(t);
            break;
        }
    }
    if left.is_none() {
        for e in (0..h.edit.len()).rev() {
            let t = join(e, 0);
            if fits(&t) {
                left = Some(t);
                break;
            }
        }
    }
    status_line_with(
        &left.unwrap_or_else(|| join(0, h.general.len())),
        tail,
        width,
    )
}

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
    let room = width.saturating_sub(display_width(&tail));
    if left.is_empty() || room == 0 {
        // Nothing of `left` fits: no leading separator, and never wider than the line.
        let bare = tail.trim_start_matches(" · ");
        let bare = if display_width(bare) <= width {
            bare
        } else {
            HELP
        };
        return truncate_to_width(bare, width);
    }
    format!("{}{tail}", fit_entries(left, room))
}

/// `left` cut to `room`: whole ` · ` entries are dropped from the end, never cut through; only
/// when not even the first fits is it cut with `…`.
fn fit_entries(left: &str, room: usize) -> String {
    if display_width(left) <= room {
        return left.to_string();
    }
    let entries: Vec<&str> = left.split(" · ").collect();
    for n in (1..entries.len()).rev() {
        let joined = entries[..n].join(" · ");
        if display_width(&joined) <= room {
            return joined;
        }
    }
    truncate_to_width(left, room)
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
        a.on_sync(crate::sync::SyncEvent::Projects {
            boards: vec![crate::model::ProjectSummary {
                id: crate::model::ProjectId::new("x"),
                board: "acme/7".parse().unwrap(),
                title: "Sprint".into(),
                closed: false,
            }],
            notes: vec![],
        });
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
        snap.project.views[2].filter = format!("label:bug {}", "-label:wontfix ".repeat(10)).into();
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
    fn a_linear_filter_prompt_has_no_github_prefix() {
        let mut snap = snapshot();
        snap.project.board = "linear:team:t1".parse().unwrap();
        snap.project.views[2].filter = crate::model::ViewFilter::LinearView {
            id: "cv1".into(),
            hide_closed: false,
        };
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        a.select_view(&crate::model::ViewId::new("V_bugs"));
        a.handle_key(key('f'));
        a.handle_key(key('z'));
        let screen = render_to_string(60, 10, |f| draw(f, &mut a));
        assert_eq!(screen.lines().last().unwrap(), "filter: z");
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

    #[test]
    fn a_linear_header_shows_the_badge_and_read_only() {
        use crate::model::{LinearMeta, Provider};
        let mut snap = snapshot();
        snap.project.board = "linear:team:t1".parse().unwrap();
        snap.project.viewer_can_update = false;
        snap.project.linear = Some(LinearMeta {
            url_key: "acme".into(),
            viewer_id: "U_tviles".into(),
            team_keys: vec!["TBA".into()],
        });
        assert_eq!(snap.project.board.provider(), Provider::Linear);
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        let first = screen.lines().next().unwrap();
        assert!(first.starts_with("Testbed · Linear"), "{first}");
        assert!(first.ends_with("read-only"), "{first}");
        insta::assert_snapshot!(screen);
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
    fn an_unknown_total_reads_as_a_plus() {
        use crate::model::ItemId;
        use crate::store::{MemoryStore, ViewList};
        let mut a = flagged_app();
        a.status.loading = Some((300, 0));
        let screen = render_to_string(120, 10, |f| draw(f, &mut a));
        assert!(screen.lines().next().unwrap().contains("loading 300"));
        assert!(!screen.contains("loading 300/"), "{screen}");
        a.status.loading = Some((100, 250));
        let screen = render_to_string(120, 10, |f| draw(f, &mut a));
        assert!(screen.lines().next().unwrap().contains("loading 100/250"));

        let mut snap = snapshot();
        let view = snap.project.views[0].id.clone();
        let ids: Vec<ItemId> = (0..2000).map(|i| ItemId::new(format!("i{i}"))).collect();
        snap.set_view_ids(
            view.clone(),
            ViewList {
                ids: ids.clone(),
                total: 0,
                truncated: true,
            },
        );
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap.clone()))),
            Keymap::defaults(),
            Theme::plain(),
        );
        let screen = render_to_string(120, 10, |f| draw(f, &mut a));
        assert!(screen.contains("loaded 2000+"), "{screen}");
        assert!(!screen.contains("loaded 2000 of"), "{screen}");
        snap.set_view_ids(
            view,
            ViewList {
                ids,
                total: 2400,
                truncated: true,
            },
        );
        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snap))),
            Keymap::defaults(),
            Theme::plain(),
        );
        let screen = render_to_string(120, 10, |f| draw(f, &mut a));
        assert!(screen.contains("loaded 2000 of 2400"), "{screen}");

        let mut a = App::new(
            Box::new(MemoryStore::new(Some(snapshot()))),
            Keymap::defaults(),
            Theme::plain(),
        );
        let items: Vec<_> = (0..2000)
            .map(|i| crate::model::item::tests::issue(&format!("n{i}"), i + 1, "T"))
            .collect();
        a.on_sync(crate::sync::SyncEvent::ItemsComplete {
            items: items.clone(),
            fetched_at: "t".into(),
            total: 0,
            truncated: true,
        });
        assert!(
            a.status
                .notes
                .iter()
                .any(|n| n == "loaded 2000+ items (max_items)")
        );
        a.on_sync(crate::sync::SyncEvent::ItemsComplete {
            items,
            fetched_at: "t".into(),
            total: 2400,
            truncated: true,
        });
        assert!(
            a.status
                .notes
                .iter()
                .any(|n| n == "loaded 2000 of 2400 items (max_items)")
        );
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
    fn the_comment_form_draws_over_the_board() {
        let mut a = app();
        a.handle_key(key('c'));
        a.handle_key(key('h'));
        a.handle_key(key('i'));
        let screen = render_to_string(100, 30, |f| draw(f, &mut a));
        for want in [
            "Comment on #1",
            "Body",
            "hi",
            "[ Comment ]",
            "[ Open in editor ]",
            "[ Cancel ]",
            "Tab next · Alt+Enter submit · Esc cancel",
        ] {
            assert!(screen.contains(want), "missing {want:?} in\n{screen}");
        }
    }

    #[test]
    fn the_status_line_hides_board_keys_while_the_text_form_is_open() {
        let mut a = app();
        let marks = ItemMarks::default();
        let status = |a: &App| {
            render_to_string(200, 3, |f| draw_status(f, f.area(), a, &marks))
                .lines()
                .next()
                .unwrap()
                .to_string()
        };
        assert!(status(&a).contains("q quit"), "{}", status(&a));
        a.handle_key(key('c'));
        assert!(matches!(a.popup, Some(Popup::Text { .. })));
        let line = status(&a);
        for board_key in ["q quit", "/ search", "f filter", "L layout"] {
            assert!(!line.contains(board_key), "{line}");
        }
        a.handle_key(code(KeyCode::Esc));
        assert!(status(&a).contains("q quit"), "{}", status(&a));
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
        assert!(screen.contains("Edit field · #1"), "{screen}");
    }

    #[test]
    fn the_hint_line_keeps_help_visible_when_narrow() {
        let mut a = app();
        let screen = render_to_string(50, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap();
        assert!(last.ends_with(" · ? help"), "{last}");
        assert!(
            !last.contains('…') && last.ends_with("space actions · ? help"),
            "whole hints are dropped, none is cut through: {last}"
        );
        assert!(display_width(last) <= 50);
        assert_eq!(status_line("", 10), "? help");
        assert_eq!(status_line("e field", 40), "e field · ? help");
        assert_eq!(
            status_line("a assignees · t labels · e field", 26),
            "a assignees · ? help",
            "entries drop from the end, whole"
        );
        assert_eq!(status_line("a long single entry", 14), "a lo… · ? help");
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
        render_to_string(100, 50, |f| draw(f, &mut a));
        a.handle_key(key('j'));
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn space_actions_and_help_outlast_every_other_hint() {
        let lead: Vec<String> = vec![];
        let edit: Vec<String> = ["a assignees", "c comment", "< > move", "J K reorder"]
            .map(String::from)
            .to_vec();
        let general = vec!["/ search · q quit".to_string()];
        let line = |w| {
            status_hints(
                &Hints {
                    lead: &lead,
                    edit: &edit,
                    general: &general,
                    actions: Some("space actions".into()),
                },
                None,
                w,
            )
        };
        assert_eq!(
            line(200),
            "a assignees · c comment · < > move · J K reorder · / search · q quit · space actions · ? help"
        );
        // Board-wide hints go first: every edit hint outlasts them.
        let l = line(88);
        assert!(l.contains("J K reorder") && !l.contains("/ search"), "{l}");
        assert!(l.ends_with("space actions · ? help"), "{l}");
        // Then edit hints go from the end.
        let l = line(45);
        assert!(
            l.starts_with("a assignees") && !l.contains("J K reorder"),
            "{l}"
        );
        assert!(!l.contains("/ search"), "{l}");
        for w in [30, 22] {
            let l = line(w);
            assert!(l.ends_with("space actions · ? help"), "{w}: {l}");
            assert!(display_width(&l) <= w, "{w}: {l}");
        }
    }

    #[test]
    fn status_line_never_overflows_and_help_stands_alone_when_there_is_no_room() {
        for width in 6..=10 {
            let line = status_line("e field · move", width);
            assert!(display_width(&line) <= width, "{width}: {line}");
            assert!(line.ends_with("? help"), "{width}: {line}");
        }
        assert_eq!(status_line("e field", 6), "? help");
        assert_eq!(status_line("e field", 9), "? help");
        assert_eq!(status_line("e field", 10), "… · ? help");
    }

    #[test]
    fn a_wide_char_hint_cut_stays_within_the_width() {
        for width in 10..30 {
            let line = status_line("中文中文中文中文中文", width);
            assert!(display_width(&line) <= width, "{width}: {line}");
            assert!(line.ends_with(" · ? help"), "{width}: {line}");
        }
    }

    #[test]
    fn the_reminder_needs_twelve_cells_left_for_the_hints() {
        let a = app();
        let mut marks = ItemMarks::default();
        marks.unmatched.insert(crate::model::ItemId::new("a"));
        let min = display_width(crate::ui::marks::UNMATCHED_TEXT)
            + display_width(" · ? help")
            + a.actions_hint().map_or(0, |h| display_width(&h) + 3)
            + MIN_HINT_ROOM;
        for (width, shown) in [(min - 1, false), (min, true)] {
            let screen = render_to_string(width as u16, 3, |f| {
                draw_status(f, f.area(), &a, &marks);
            });
            let line = screen.lines().next().unwrap();
            assert_eq!(
                line.contains("⊘ no longer matches"),
                shown,
                "{width}: {line}"
            );
            assert!(line.ends_with("? help"), "{width}: {line}");
        }
    }

    fn failed_app() -> App {
        use crate::model::{FieldId, FieldValue, ItemId, OpKind, OptionId};
        let mut a = app();
        a.submit(
            OpKind::Move,
            crate::ops::Op::SetField {
                item: ItemId::new("a"),
                field: FieldId::new("F_status"),
                value: FieldValue::SingleSelect {
                    option_id: OptionId::new("o_done"),
                    name: "Done".into(),
                },
            },
        );
        a.on_sync(crate::sync::SyncEvent::OpDone {
            id: 1,
            result: Err(crate::error::BackendError::Network(
                "connection reset".into(),
            )),
        });
        a
    }

    #[test]
    fn the_error_log_lists_failures_with_their_messages() {
        let mut a = failed_app();
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap().to_string();
        assert!(
            last.starts_with("Could not set Status to Done on 'Fix crash'"),
            "{last}"
        );
        assert!(
            last.ends_with("· E error log"),
            "the error line points to the log: {last}"
        );
        a.status.error = None;
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap().to_string();
        assert!(
            last.starts_with("E 1 failed · "),
            "the hint line counts failures: {last}"
        );
        assert!(last.ends_with("· ? help"), "{last}");
        a.handle_key(key('E'));
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        assert!(screen.contains("failed edits"), "{screen}");
        assert!(
            screen.contains("set Status to Done on 'Fix crash'"),
            "{screen}"
        );
        assert!(screen.contains("connection reset"), "{screen}");
        assert!(screen.contains("0s ago"), "{screen}");
        let last = screen.lines().last().unwrap().trim_end().to_string();
        assert_eq!(last, "j k select · R retry · d dismiss · esc close");
    }

    #[test]
    fn the_error_line_keeps_its_pointer_when_the_pane_is_narrow() {
        let mut a = failed_app();
        let screen = render_to_string(50, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap().trim_end().to_string();
        assert!(
            last.ends_with("… · E error log") || last.ends_with("· E error log"),
            "{last}"
        );
        assert!(last.chars().count() <= 50, "{last}");
    }

    #[test]
    fn a_refused_retry_shows_its_reason_in_the_log() {
        let mut a = failed_app();
        a.on_sync(crate::sync::SyncEvent::Access {
            scopes: Some(vec!["repo".into()]),
        });
        a.handle_key(key('E'));
        a.handle_key(key('R'));
        let screen = render_to_string(100, 12, |f| draw(f, &mut a));
        let last = screen.lines().last().unwrap().to_string();
        assert!(last.contains("project scope"), "{last}");
        assert!(
            screen.contains("connection reset"),
            "the entry stays: {screen}"
        );
    }

    #[test]
    fn the_error_log_is_drawn_over_the_open_detail() {
        let mut a = failed_app();
        a.status.error = None;
        a.handle_key(code(KeyCode::Enter));
        a.handle_key(key('E'));
        let screen = render_to_string(100, 20, |f| draw(f, &mut a));
        assert!(screen.contains("failed edits"), "{screen}");
        assert!(screen.contains("connection reset"), "{screen}");
        a.handle_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Detail);
    }
}
