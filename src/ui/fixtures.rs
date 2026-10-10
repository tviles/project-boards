//! A small board for UI tests: Status (3 options), Priority (3 options), five items covering
//! an emoji/CJK title, a draft, a value pointing at a deleted option, and (on #1) a linked
//! pull request and a created date for the board's card pills.

use crate::model::*;
use crate::store::BoardSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::{Frame, Terminal};
use std::collections::BTreeMap;

fn opt(id: &str, name: &str, color: OptionColor) -> SelectOption {
    SelectOption {
        id: OptionId::new(id),
        name: name.into(),
        color,
    }
}

pub fn project() -> Project {
    let f = |id: &str, name: &str, kind: FieldKind| Field {
        id: FieldId::new(id),
        name: name.into(),
        kind,
    };
    let view = |id: &str,
                name: &str,
                layout: Layout,
                filter: &str,
                visible: &[&str],
                vertical: &[&str],
                group: &[&str]| View {
        id: ViewId::new(id),
        number: 1,
        name: name.into(),
        layout,
        filter: filter.into(),
        visible_fields: visible.iter().map(|s| FieldId::new(*s)).collect(),
        group_by: group.iter().map(|s| FieldId::new(*s)).collect(),
        vertical_group_by: vertical.iter().map(|s| FieldId::new(*s)).collect(),
        sort_by: vec![],
    };
    Project {
        id: ProjectId::new("PVT_1"),
        board: "tviles/3".parse().unwrap(),
        owner_kind: OwnerKind::User,
        title: "Testbed".into(),
        url: "https://github.com/users/tviles/projects/3".into(),
        viewer_can_update: true,
        fields: vec![
            f("F_title", "Title", FieldKind::Title),
            f(
                "F_status",
                "Status",
                FieldKind::SingleSelect {
                    options: vec![
                        opt("o_todo", "Todo", OptionColor::Gray),
                        opt("o_prog", "In Progress", OptionColor::Yellow),
                        opt("o_done", "Done", OptionColor::Green),
                    ],
                },
            ),
            f(
                "F_prio",
                "Priority",
                FieldKind::SingleSelect {
                    options: vec![
                        opt("p0", "P0", OptionColor::Red),
                        opt("p1", "P1", OptionColor::Orange),
                        opt("p2", "P2", OptionColor::Gray),
                    ],
                },
            ),
            f("F_assignees", "Assignees", FieldKind::Assignees),
            f("F_labels", "Labels", FieldKind::Labels),
            f("F_notes", "Notes", FieldKind::Text),
            f(
                "F_prs",
                "Linked pull requests",
                FieldKind::LinkedPullRequests,
            ),
            f("F_created", "Created", FieldKind::Created),
        ],
        views: vec![
            view(
                "V_table",
                "Table",
                Layout::Table,
                "",
                &["F_title", "F_status", "F_assignees"],
                &[],
                &[],
            ),
            view(
                "V_board",
                "Board",
                Layout::Board,
                "",
                &[
                    "F_title",
                    "F_status",
                    "F_assignees",
                    "F_prs",
                    "F_created",
                    "F_prio",
                    "F_labels",
                ],
                &["F_status"],
                &["F_prio"],
            ),
            view("V_bugs", "Bugs", Layout::Table, "label:bug", &[], &[], &[]),
        ],
    }
}

fn select(field: &str, id: &str, name: &str) -> (FieldId, FieldValue) {
    (
        FieldId::new(field),
        FieldValue::SingleSelect {
            option_id: OptionId::new(id),
            name: name.into(),
        },
    )
}

fn issue(id: &str, number: u32, title: &str, values: Vec<(FieldId, FieldValue)>) -> Item {
    Item {
        id: ItemId::new(id),
        content: ItemContent::Issue {
            reference: ContentRef {
                repo: "tviles/t".into(),
                number,
                url: format!("https://github.com/tviles/t/issues/{number}"),
            },
            title: title.into(),
            state: ContentState::Open,
        },
        archived: false,
        updated_at: "2026-10-01T00:00:00Z".into(),
        values: values.into_iter().collect::<BTreeMap<_, _>>(),
        content_fields: ContentFields::default(),
    }
}

pub fn items() -> Vec<Item> {
    let users = |u: &str| {
        (
            FieldId::new("F_assignees"),
            FieldValue::Users(vec![u.into()]),
        )
    };
    let labels = |l: &str| {
        (
            FieldId::new("F_labels"),
            FieldValue::Labels(vec![Label {
                name: l.into(),
                color: "d73a4a".into(),
            }]),
        )
    };
    let mut fix_crash = issue(
        "a",
        1,
        "Fix crash",
        vec![
            select("F_status", "o_todo", "Todo"),
            select("F_prio", "p0", "P0"),
            users("tviles"),
            labels("bug"),
        ],
    );
    fix_crash.content_fields = ContentFields {
        created_at: Some("2026-08-19T10:00:00Z".into()),
        linked_prs: vec![LinkedPullRequest {
            number: 12,
            state: ContentState::Open,
            is_draft: false,
        }],
        ..ContentFields::default()
    };
    let mut draft = issue("d", 0, "", vec![select("F_status", "o_done", "Done")]);
    draft.content = ItemContent::Draft {
        title: "Draft idea".into(),
    };
    vec![
        fix_crash,
        issue(
            "b",
            2,
            "Add iteration columns",
            vec![
                select("F_status", "o_prog", "In Progress"),
                select("F_prio", "p1", "P1"),
            ],
        ),
        issue(
            "c",
            3,
            "Emoji 🚀 title 中文 that is quite long",
            vec![
                select("F_status", "o_todo", "Todo"),
                select("F_prio", "p2", "P2"),
                users("tviles"),
                labels("enhancement"),
            ],
        ),
        draft,
        issue(
            "e",
            5,
            "Old option",
            vec![
                select("F_status", "o_deleted", "Blocked"),
                select("F_prio", "p0", "P0"),
            ],
        ),
    ]
}

pub fn snapshot() -> BoardSnapshot {
    let mut s = BoardSnapshot::new(project());
    s.upsert_items(items());
    s
}

/// Renders with `draw` into a `w`x`h` test terminal and returns the screen as text.
/// A wide character occupies two cells, and ratatui's buffer holds a blank symbol in the
/// trailing cell. That cell is skipped here, only when it is blank, so the text reads as
/// drawn ("🚀 hi", not "🚀  hi"). A non-blank trailing cell is a real overlap and is printed.
pub fn render_to_string(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    // The completed frame's buffer, not the backend's: the backend only receives the diff,
    // which drops any cell that follows a wide glyph.
    let buf = terminal.draw(draw).unwrap().buffer.clone();
    (0..h)
        .map(|y| {
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..w {
                let symbol = buf[(x, y)].symbol();
                if skip > 0 {
                    skip -= 1;
                    if symbol == " " {
                        continue;
                    }
                }
                skip = skip.max(symbol.width().saturating_sub(1));
                line.push_str(symbol);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

pub fn code(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Paragraph;

    #[test]
    fn render_to_string_keeps_wide_characters_once() {
        let out = render_to_string(10, 2, |f| {
            f.render_widget(Paragraph::new("🚀 hi\n中文"), f.area())
        });
        assert_eq!(out, "🚀 hi\n中文");
    }

    #[test]
    fn render_to_string_prints_a_real_overlap_after_a_wide_glyph() {
        let out = render_to_string(6, 1, |f| {
            let buf = f.buffer_mut();
            buf[(0, 0)].set_symbol("🚀");
            buf[(1, 0)].set_symbol("X");
        });
        assert_eq!(out, "🚀X");
    }
}
