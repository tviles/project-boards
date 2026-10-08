//! A small board for UI tests: Status (3 options), Priority (3 options), five items covering
//! an emoji/CJK title, a draft, and a value pointing at a deleted option.

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
                &[],
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
    let mut draft = issue("d", 0, "", vec![select("F_status", "o_done", "Done")]);
    draft.content = ItemContent::Draft {
        title: "Draft idea".into(),
    };
    vec![
        issue(
            "a",
            1,
            "Fix crash",
            vec![
                select("F_status", "o_todo", "Todo"),
                select("F_prio", "p0", "P0"),
                users("tviles"),
                labels("bug"),
            ],
        ),
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
pub fn render_to_string(w: u16, h: u16, draw: impl FnOnce(&mut Frame)) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(draw).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
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
