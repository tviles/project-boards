//! A Linear board's tabs: the built-in ones and the workspace's custom views.

use crate::linear::ids;
use crate::model::*;
use serde_json::{Value, json};

// Built-in tabs.
pub const BOARD_VIEW: &str = "linear:view:board";
pub const ALL_VIEW: &str = "linear:view:all";
pub const MINE_VIEW: &str = "linear:view:mine";

// `viewPreferencesValues` strings this build recognises (finding L3, Decision 20). Task 13
// confirms the ones phase 0 did not measure.
const LAYOUT_BOARD: &str = "board";
const GROUP_STATUS: &str = "workflowState";
const GROUP_PRIORITY: &str = "priority";
const GROUP_PROJECT: &str = "project";
const GROUP_CYCLE: &str = "cycle";
const GROUP_LABEL: &str = "label";
const ORDER_MANUAL: &str = "manual";
const ORDER_PRIORITY: &str = "priority";
const ORDER_CREATED: &str = "createdAt";
const ORDER_UPDATED: &str = "updatedAt";
const ORDER_DUE: &str = "dueDate";
const DIRECTION_ASC: &str = "asc";
const DIRECTION_DESC: &str = "desc";
const SHOW_COMPLETED_NONE: &str = "none";

/// The `field*` toggles and the field each shows, in column order.
const VISIBLE_TOGGLES: [(&str, &str); 10] = [
    ("fieldStatus", ids::STATUS),
    ("fieldPriority", ids::PRIORITY),
    ("fieldAssignee", ids::ASSIGNEES),
    ("fieldLabels", ids::LABELS),
    ("fieldCycle", ids::CYCLE),
    ("fieldEstimate", ids::ESTIMATE),
    ("fieldDueDate", ids::DUE),
    ("fieldProject", ids::PROJECT),
    ("fieldDateCreated", ids::CREATED),
    ("fieldDateUpdated", ids::UPDATED),
];

fn field_ids(list: &[&str]) -> Vec<FieldId> {
    list.iter().map(|id| FieldId::new(*id)).collect()
}

/// The All issues and My issues tabs' columns.
fn all_issues_fields() -> Vec<FieldId> {
    field_ids(&[
        ids::STATUS,
        ids::PRIORITY,
        ids::ASSIGNEES,
        ids::LABELS,
        ids::CYCLE,
        ids::ESTIMATE,
        ids::DUE,
        ids::UPDATED,
    ])
}

/// The tabs every Linear board has, before its custom views (spec section 6).
pub fn builtin_views() -> Vec<View> {
    let table = |id: &str, number: u32, name: &str, filter: Value| View {
        id: ViewId::new(id),
        number,
        name: name.into(),
        layout: Layout::Table,
        filter: ViewFilter::Linear(filter),
        visible_fields: all_issues_fields(),
        group_by: Vec::new(),
        vertical_group_by: Vec::new(),
        sort_by: Vec::new(),
    };
    vec![
        View {
            id: ViewId::new(BOARD_VIEW),
            number: 1,
            name: "Board".into(),
            layout: Layout::Board,
            filter: ViewFilter::Linear(board_tab_filter()),
            visible_fields: field_ids(&[ids::PRIORITY, ids::LABELS, ids::ASSIGNEES, ids::ESTIMATE]),
            group_by: Vec::new(),
            vertical_group_by: field_ids(&[ids::STATUS]),
            sort_by: Vec::new(),
        },
        table(ALL_VIEW, 2, "All issues", json!({})),
        table(
            MINE_VIEW,
            3,
            "My issues",
            json!({"assignee": {"isMe": {"eq": true}}}),
        ),
    ]
}

/// The Board tab's filter: no canceled or duplicate issues, and no issue completed more than
/// 14 days ago.
pub fn board_tab_filter() -> Value {
    json!({
        "state": {"type": {"nin": ["canceled", "duplicate"]}},
        "or": [{"completedAt": {"null": true}}, {"completedAt": {"gt": "-P14D"}}],
    })
}

/// ANDed into a "hide completed" view's list (Decision 19): Linear does not apply that
/// setting to `customView.issues` (finding L5).
pub fn hide_closed_filter() -> Value {
    json!({"state": {"type": {"nin": ["completed", "canceled", "duplicate"]}}})
}

/// The field a grouping names, when it is recognised and the board has that field.
fn grouping(prefs: &Value, key: &str, label_key: &str, fields: &[Field]) -> Option<FieldId> {
    let id = match prefs[key].as_str()? {
        GROUP_STATUS => FieldId::new(ids::STATUS),
        GROUP_PRIORITY => FieldId::new(ids::PRIORITY),
        GROUP_PROJECT => FieldId::new(ids::PROJECT),
        GROUP_CYCLE => FieldId::new(ids::CYCLE),
        GROUP_LABEL => ids::group(prefs[label_key].as_str()?),
        _ => return None,
    };
    fields.iter().any(|f| f.id == id).then_some(id)
}

fn ordering(prefs: &Value) -> Option<SortSpec> {
    let (field, direction) = match prefs["viewOrdering"].as_str()? {
        ORDER_PRIORITY => (ids::PRIORITY, SortDirection::Asc),
        ORDER_CREATED => (ids::CREATED, SortDirection::Desc),
        ORDER_UPDATED => (ids::UPDATED, SortDirection::Desc),
        ORDER_DUE => (ids::DUE, SortDirection::Asc),
        // Manual order is `sortOrder`, the tie-break every sort ends with (Decision 13).
        ORDER_MANUAL => return None,
        _ => return None,
    };
    let direction = match prefs["viewOrderingDirection"].as_str() {
        Some(DIRECTION_ASC) => SortDirection::Asc,
        Some(DIRECTION_DESC) => SortDirection::Desc,
        _ => direction,
    };
    Some(SortSpec {
        field: FieldId::new(field),
        direction,
    })
}

/// Title, then each field a `field*` toggle shows and the board has; All issues' columns
/// when the view sets no toggle at all.
fn visible_fields(prefs: &Value, fields: &[Field]) -> Vec<FieldId> {
    if !VISIBLE_TOGGLES
        .iter()
        .any(|(key, _)| prefs[key].is_boolean())
    {
        return all_issues_fields();
    }
    let shown = VISIBLE_TOGGLES
        .iter()
        .filter(|(key, _)| prefs[key].as_bool() == Some(true))
        .map(|(_, id)| FieldId::new(*id))
        .filter(|id| fields.iter().any(|f| &f.id == id));
    std::iter::once(FieldId::new(ids::TITLE))
        .chain(shown)
        .collect()
}

/// A custom view (`ViewFields`) as tab `number`; `None` without an id. Only recognised
/// settings map (Decisions 3, 19, 20): an unrecognised grouping gives Status columns on a
/// board and no grouping in a table, an unrecognised ordering gives manual order.
pub fn custom_view(v: &Value, fields: &[Field], number: u32) -> Option<View> {
    let id = v["id"].as_str()?;
    let prefs = &v["viewPreferencesValues"];
    let group = grouping(prefs, "issueGrouping", "issueGroupingLabelGroupId", fields);
    let (layout, vertical_group_by, group_by) = if prefs["layout"] == LAYOUT_BOARD {
        let lane = grouping(
            prefs,
            "issueSubGrouping",
            "issueSubGroupingLabelGroupId",
            fields,
        );
        (
            Layout::Board,
            vec![group.unwrap_or_else(|| FieldId::new(ids::STATUS))],
            lane.into_iter().collect(),
        )
    } else {
        (Layout::Table, Vec::new(), group.into_iter().collect())
    };
    Some(View {
        id: ViewId::new(id),
        number,
        name: v["name"].as_str().unwrap_or_default().to_string(),
        layout,
        filter: ViewFilter::LinearView {
            id: id.to_string(),
            hide_closed: prefs["showCompletedIssues"] == SHOW_COMPLETED_NONE,
        },
        visible_fields: visible_fields(prefs, fields),
        group_by,
        vertical_group_by,
        sort_by: ordering(prefs).into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::convert::tests::{TYPE_GROUP, project_board_fields, team_board_fields};
    use crate::linear::ids;
    use serde_json::json;

    fn fid(id: &str) -> FieldId {
        FieldId::new(id)
    }

    fn fids(ids: &[&str]) -> Vec<FieldId> {
        ids.iter().map(|id| fid(id)).collect()
    }

    fn all_issues_fields() -> Vec<FieldId> {
        fids(&[
            ids::STATUS,
            ids::PRIORITY,
            ids::ASSIGNEES,
            ids::LABELS,
            ids::CYCLE,
            ids::ESTIMATE,
            ids::DUE,
            ids::UPDATED,
        ])
    }

    fn view_json(prefs: Value) -> Value {
        json!({"id": "cv-1", "name": "Mine", "modelName": "Issue", "team": null,
               "viewPreferencesValues": prefs})
    }

    type Shape = (
        Layout,
        Vec<FieldId>,
        Vec<FieldId>,
        Vec<SortSpec>,
        Vec<FieldId>,
    );

    fn shape(prefs: Value, fields: &[Field]) -> Shape {
        let v = custom_view(&view_json(prefs), fields, 4).expect("a view");
        (
            v.layout,
            v.vertical_group_by,
            v.group_by,
            v.sort_by,
            v.visible_fields,
        )
    }

    fn sort(id: &str, direction: SortDirection) -> Vec<SortSpec> {
        vec![SortSpec {
            field: fid(id),
            direction,
        }]
    }

    #[test]
    fn builtin_tabs_match_the_spec() {
        let views = builtin_views();
        let got: Vec<_> = views
            .iter()
            .map(|v| (v.id.as_str(), v.number, v.name.as_str(), v.layout))
            .collect();
        assert_eq!(
            got,
            [
                ("linear:view:board", 1, "Board", Layout::Board),
                ("linear:view:all", 2, "All issues", Layout::Table),
                ("linear:view:mine", 3, "My issues", Layout::Table),
            ]
        );
        let [board, all, mine] = &views[..] else {
            unreachable!()
        };
        assert_eq!(board.vertical_group_by, fids(&[ids::STATUS]));
        assert!(board.group_by.is_empty() && board.sort_by.is_empty());
        assert_eq!(
            board.visible_fields,
            fids(&[ids::PRIORITY, ids::LABELS, ids::ASSIGNEES, ids::ESTIMATE])
        );
        assert_eq!(
            board_tab_filter(),
            json!({"state":{"type":{"nin":["canceled","duplicate"]}},
                   "or":[{"completedAt":{"null":true}},{"completedAt":{"gt":"-P14D"}}]})
        );
        assert_eq!(board.filter, ViewFilter::Linear(board_tab_filter()));
        assert_eq!(all.visible_fields, all_issues_fields());
        assert_eq!(all.filter, ViewFilter::Linear(json!({})));
        assert!(all.vertical_group_by.is_empty() && all.group_by.is_empty());
        assert_eq!(mine.visible_fields, all_issues_fields());
        assert_eq!(
            mine.filter,
            ViewFilter::Linear(json!({"assignee": {"isMe": {"eq": true}}}))
        );
        assert_eq!(
            hide_closed_filter(),
            json!({"state": {"type": {"nin": ["completed", "canceled", "duplicate"]}}})
        );
    }

    #[test]
    fn custom_views_map_recognised_settings() {
        use Layout::{Board, Table};
        use SortDirection::{Asc, Desc};
        let project = project_board_fields();
        let team = team_board_fields();
        let group = ids::group(TYPE_GROUP);
        let cases: Vec<(Value, &[Field], Shape)> = vec![
            (
                json!({"layout": "board", "issueGrouping": "priority",
                       "issueSubGrouping": "workflowState"}),
                &project,
                (
                    Board,
                    fids(&[ids::PRIORITY]),
                    fids(&[ids::STATUS]),
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "issueGrouping": "label",
                       "issueGroupingLabelGroupId": TYPE_GROUP}),
                &project,
                (
                    Table,
                    vec![],
                    vec![group.clone()],
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "board", "issueGrouping": "cycle", "issueSubGrouping": "label",
                       "issueSubGroupingLabelGroupId": TYPE_GROUP}),
                &project,
                (
                    Board,
                    fids(&[ids::CYCLE]),
                    vec![group.clone()],
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "board", "issueGrouping": "label",
                       "issueGroupingLabelGroupId": TYPE_GROUP, "issueSubGrouping": "project"}),
                &team,
                (
                    Board,
                    vec![group.clone()],
                    fids(&[ids::PROJECT]),
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "issueGrouping": "workflowState",
                       "issueSubGrouping": "priority", "viewOrdering": "priority",
                       "viewOrderingDirection": null}),
                &project,
                (
                    Table,
                    vec![],
                    fids(&[ids::STATUS]),
                    sort(ids::PRIORITY, Asc),
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "issueGrouping": "project", "viewOrdering": "createdAt"}),
                &team,
                (
                    Table,
                    vec![],
                    fids(&[ids::PROJECT]),
                    sort(ids::CREATED, Desc),
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "viewOrdering": "updatedAt",
                       "viewOrderingDirection": "asc"}),
                &project,
                (
                    Table,
                    vec![],
                    vec![],
                    sort(ids::UPDATED, Asc),
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "viewOrdering": "dueDate",
                       "viewOrderingDirection": "desc"}),
                &project,
                (
                    Table,
                    vec![],
                    vec![],
                    sort(ids::DUE, Desc),
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "viewOrdering": "manual",
                       "viewOrderingDirection": "asc"}),
                &project,
                (Table, vec![], vec![], vec![], all_issues_fields()),
            ),
            (
                json!({"layout": "list", "fieldStatus": true, "fieldPriority": false,
                       "fieldAssignee": null, "fieldDueDate": true, "fieldProject": true,
                       "fieldDateCreated": true}),
                &project,
                (
                    Table,
                    vec![],
                    vec![],
                    vec![],
                    fids(&[ids::TITLE, ids::STATUS, ids::DUE, ids::CREATED]),
                ),
            ),
            (
                json!({"layout": "list", "fieldStatus": true, "fieldPriority": true,
                       "fieldAssignee": true, "fieldLabels": true, "fieldCycle": true,
                       "fieldEstimate": true, "fieldDueDate": true, "fieldProject": true,
                       "fieldDateCreated": true, "fieldDateUpdated": true}),
                &team,
                (
                    Table,
                    vec![],
                    vec![],
                    vec![],
                    fids(&[
                        ids::TITLE,
                        ids::STATUS,
                        ids::PRIORITY,
                        ids::ASSIGNEES,
                        ids::LABELS,
                        ids::CYCLE,
                        ids::ESTIMATE,
                        ids::DUE,
                        ids::PROJECT,
                        ids::CREATED,
                        ids::UPDATED,
                    ]),
                ),
            ),
            (
                json!({"layout": "list", "fieldStatus": false}),
                &project,
                (Table, vec![], vec![], vec![], fids(&[ids::TITLE])),
            ),
        ];
        for (prefs, fields, want) in cases {
            assert_eq!(shape(prefs.clone(), fields), want, "{prefs}");
        }
        let v = custom_view(&view_json(json!({"layout": "list"})), &project, 7).unwrap();
        assert_eq!(v.id, ViewId::new("cv-1"));
        assert_eq!(v.number, 7);
        assert_eq!(v.name, "Mine");
    }

    #[test]
    fn unrecognised_settings_fall_back() {
        use Layout::{Board, Table};
        let project = project_board_fields();
        let cases: Vec<(Value, Shape)> = vec![
            (
                json!({"layout": "board", "issueGrouping": "assignee",
                       "issueSubGrouping": "assignee"}),
                (
                    Board,
                    fids(&[ids::STATUS]),
                    vec![],
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "issueGrouping": "assignee"}),
                (Table, vec![], vec![], vec![], all_issues_fields()),
            ),
            (
                json!({"layout": "list", "viewOrdering": "title",
                       "viewOrderingDirection": "desc"}),
                (Table, vec![], vec![], vec![], all_issues_fields()),
            ),
            (
                json!({"layout": "timeline"}),
                (Table, vec![], vec![], vec![], all_issues_fields()),
            ),
            (
                // A project board has no Project field; Area is a multi-select group.
                json!({"layout": "board", "issueGrouping": "project", "issueSubGrouping": "label",
                       "issueSubGroupingLabelGroupId": "lg-area"}),
                (
                    Board,
                    fids(&[ids::STATUS]),
                    vec![],
                    vec![],
                    all_issues_fields(),
                ),
            ),
            (
                json!({"layout": "list", "issueGrouping": "label"}),
                (Table, vec![], vec![], vec![], all_issues_fields()),
            ),
        ];
        for (prefs, want) in cases {
            assert_eq!(shape(prefs.clone(), &project), want, "{prefs}");
        }
        let no_prefs = json!({"id": "cv-2", "name": "Bare", "viewPreferencesValues": null});
        let v = custom_view(&no_prefs, &project, 4).unwrap();
        assert_eq!(v.layout, Table);
        assert_eq!(v.visible_fields, all_issues_fields());
    }

    #[test]
    fn hide_completed_views_are_marked() {
        let fields = project_board_fields();
        let hide = |value: Value| {
            let v = custom_view(
                &view_json(json!({"showCompletedIssues": value})),
                &fields,
                4,
            )
            .unwrap();
            match v.filter {
                ViewFilter::LinearView { id, hide_closed } => {
                    assert_eq!(id, "cv-1");
                    hide_closed
                }
                other => panic!("{other:?}"),
            }
        };
        assert!(hide(json!("none")));
        assert!(!hide(json!("all")));
        assert!(!hide(Value::Null));
        assert!(!hide(json!("week")));
        assert!(custom_view(&json!({"name": "No id"}), &fields, 4).is_none());
    }
}
