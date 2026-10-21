//! What each op sends to Linear (spec section 5): the issue id and `IssueUpdateInput` of
//! every edit that is one `issueUpdate`, and the `IssueCreateInput` of a new issue. Values
//! are sent as the op carries them; the UI has already resolved team-owned ones for the
//! issue's team (`model::teams`).

use crate::linear::client::board_of;
use crate::linear::ids;
use crate::model::teams::GROUP_FIELD_PREFIX;
use crate::model::{BoardKey, FieldId, FieldValue, Item, ProjectId};
use crate::ops::{NewItem, Op};
use serde_json::{Map, Value, json};

/// `n` as Linear's whole-number estimate; `None` for a fraction (spec section 13).
fn whole(n: f64) -> Option<i64> {
    (n.fract() == 0.0).then_some(n as i64)
}

/// The input key and value that set (`Some`) or clear (`None`) `field`, for each field an
/// `issueUpdate` or `issueCreate` takes as one value. `None` for any other field (label
/// groups add and remove labels instead), for a value of the wrong shape, and for Status,
/// which has no clear.
fn field_input(field: &FieldId, value: Option<&FieldValue>) -> Option<(&'static str, Value)> {
    use FieldValue::{Date, Iteration, Number, SingleSelect};
    Some(match (field.as_str(), value) {
        (ids::STATUS, Some(SingleSelect { option_id, .. })) => {
            ("stateId", json!(option_id.as_str()))
        }
        (ids::PRIORITY, Some(SingleSelect { option_id, .. })) => {
            ("priority", json!(ids::priority_of(option_id)?))
        }
        // Linear's 0 is "No priority".
        (ids::PRIORITY, None) => ("priority", json!(0)),
        (ids::ESTIMATE, Some(Number(n))) => ("estimate", json!(whole(*n)?)),
        (ids::ESTIMATE, None) => ("estimate", Value::Null),
        (ids::DUE, Some(Date(day))) => ("dueDate", json!(day)),
        (ids::DUE, None) => ("dueDate", Value::Null),
        (ids::CYCLE, Some(Iteration { iteration_id, .. })) => {
            ("cycleId", json!(iteration_id.as_str()))
        }
        (ids::CYCLE, None) => ("cycleId", Value::Null),
        (ids::PROJECT, Some(SingleSelect { option_id, .. })) => {
            ("projectId", json!(option_id.as_str()))
        }
        (ids::PROJECT, None) => ("projectId", Value::Null),
        _ => return None,
    })
}

fn is_group(field: &FieldId) -> bool {
    field.as_str().starts_with(GROUP_FIELD_PREFIX)
}

/// The label a label-group value names (its options are the group's labels).
fn group_label(value: &FieldValue) -> Option<&str> {
    match value {
        FieldValue::SingleSelect { option_id, .. } => Some(option_id.as_str()),
        _ => None,
    }
}

/// The issue id and `IssueUpdateInput` of an op that is one `issueUpdate` (spec section 5).
/// `None` for every other op, and for a value Linear cannot take, which is then not sent.
pub fn input_for(op: &Op) -> Option<(String, Value)> {
    let (id, input) = match op {
        Op::SetField { item, field, value } => {
            let (key, value) = field_input(field, Some(value))?;
            (item.as_str(), json!({ key: value }))
        }
        Op::ClearField { item, field } => {
            let (key, value) = field_input(field, None)?;
            (item.as_str(), json!({ key: value }))
        }
        // The old label is removed explicitly; Linear is never relied on to swap it.
        Op::GroupLabel {
            item,
            field,
            value,
            replaces,
        } => {
            if !is_group(field) {
                return None;
            }
            let mut input = Map::new();
            if let Some(value) = value {
                input.insert("addedLabelIds".into(), json!([group_label(value)?]));
            }
            if let Some(old) = replaces {
                input.insert("removedLabelIds".into(), json!([old]));
            }
            if input.is_empty() {
                return None;
            }
            (item.as_str(), Value::Object(input))
        }
        Op::Reorder {
            item,
            sort_order: Some(sort_order),
            ..
        } => (item.as_str(), json!({ "sortOrder": sort_order })),
        // A Linear issue has at most one assignee: the one left in `all`.
        Op::Assignees { content, all, .. } => {
            let assignee = match all.as_slice() {
                [] => Value::Null,
                [one] => json!(one.id),
                _ => return None,
            };
            (content.node_id(), json!({ "assigneeId": assignee }))
        }
        Op::Labels {
            content,
            add,
            remove,
            ..
        } => {
            let label_ids = |labels: &[crate::model::LabelRef]| {
                labels.iter().map(|l| l.id.clone()).collect::<Vec<_>>()
            };
            (
                content.node_id(),
                json!({ "addedLabelIds": label_ids(add), "removedLabelIds": label_ids(remove) }),
            )
        }
        Op::Title { content, title, .. } => (content.node_id(), json!({ "title": title })),
        // Markdown, as Linear stores it.
        Op::Body { content, body, .. } => (content.node_id(), json!({ "description": body })),
        // Leaving a project board is leaving its project.
        Op::Remove { item } => (item.as_str(), json!({ "projectId": null })),
        Op::Reorder {
            sort_order: None, ..
        }
        | Op::Comment { .. }
        | Op::Milestone { .. }
        | Op::Close { .. }
        | Op::Reopen { .. }
        | Op::Convert { .. }
        | Op::Archive { .. }
        | Op::Unarchive { .. }
        | Op::Create { .. } => return None,
    };
    Some((id.to_string(), input))
}

/// Sets each preset on `input`, failing on one Linear cannot take. Label-group values are
/// returned, for the caller to add as it must.
fn presets_into<'p>(
    input: &mut Map<String, Value>,
    presets: &'p [(FieldId, FieldValue)],
) -> Option<Vec<(&'p FieldId, &'p str)>> {
    let mut labels = Vec::new();
    for (field, value) in presets {
        if is_group(field) {
            labels.push((field, group_label(value)?));
        } else {
            let (key, value) = field_input(field, Some(value))?;
            input.insert(key.into(), value);
        }
    }
    Some(labels)
}

/// The `IssueCreateInput` of a new Linear issue: its client id, team, title, description,
/// the board's project on a project board, and every preset (spec section 9). `None` for
/// any other new item, a board that is not Linear's, or a preset Linear cannot take: the
/// issue is created with all its values or not at all.
pub fn create_input(
    project: &ProjectId,
    new: &NewItem,
    presets: &[(FieldId, FieldValue)],
) -> Option<Value> {
    let NewItem::LinearIssue {
        id,
        team,
        title,
        body,
    } = new
    else {
        return None;
    };
    let board = board_of(project).ok()?;
    let mut input = Map::new();
    input.insert("id".into(), json!(id));
    input.insert("teamId".into(), json!(team.id));
    input.insert("title".into(), json!(title));
    input.insert("description".into(), json!(body));
    if let BoardKey::LinearProject { id } = &board {
        input.insert("projectId".into(), json!(id));
    }
    let labels = presets_into(&mut input, presets)?;
    if !labels.is_empty() {
        let ids: Vec<&str> = labels.into_iter().map(|(_, label)| label).collect();
        input.insert("labelIds".into(), json!(ids));
    }
    Some(Value::Object(input))
}

/// The `IssueUpdateInput` that adds the existing `issue` to a project board: the board's
/// project and every preset (spec section 9). A label-group preset also removes the group's
/// label the issue has now. `None` on a team board, or for a preset Linear cannot take.
pub fn add_input(
    project: &ProjectId,
    issue: &Item,
    presets: &[(FieldId, FieldValue)],
) -> Option<Value> {
    let BoardKey::LinearProject { id } = board_of(project).ok()? else {
        return None;
    };
    let mut input = Map::new();
    input.insert("projectId".into(), json!(id));
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    for (field, label) in presets_into(&mut input, presets)? {
        if let Some(old) = issue.value(field).and_then(group_label)
            && old != label
        {
            removed.push(old);
        }
        added.push(label);
    }
    if !added.is_empty() {
        input.insert("addedLabelIds".into(), json!(added));
    }
    if !removed.is_empty() {
        input.insert("removedLabelIds".into(), json!(removed));
    }
    Some(Value::Object(input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::convert::item_from_wire;
    use crate::linear::convert::tests::{TYPE_GROUP, issue_json};
    use crate::model::{
        ContentId, FieldId, FieldValue, ItemId, IterationId, LabelRef, MilestoneRef, OptionId,
        ProjectId, RepoRef, TeamMeta, UserRef,
    };
    use crate::ops::{NewItem, Op};
    use serde_json::{Value, json};

    const ISSUE_UUID: &str = "0f8e7d6c-5b4a-4938-8271-605f4e3d2c1b";

    fn issue() -> ItemId {
        ItemId::new("i-7")
    }

    fn content() -> ContentId {
        ContentId::Issue("i-7".into())
    }

    fn select(id: &str, name: &str) -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new(id),
            name: name.into(),
        }
    }

    fn set(field: &str, value: FieldValue) -> Op {
        Op::SetField {
            item: issue(),
            field: FieldId::new(field),
            value,
        }
    }

    fn clear(field: &str) -> Op {
        Op::ClearField {
            item: issue(),
            field: FieldId::new(field),
        }
    }

    fn group(value: Option<FieldValue>, replaces: Option<&str>) -> Op {
        Op::GroupLabel {
            item: issue(),
            field: ids::group(TYPE_GROUP),
            value,
            replaces: replaces.map(String::from),
        }
    }

    fn user(id: &str, name: &str) -> UserRef {
        UserRef {
            id: id.into(),
            login: name.into(),
        }
    }

    fn label(id: &str, name: &str) -> LabelRef {
        LabelRef {
            id: id.into(),
            name: name.into(),
            color: "eb5757".into(),
        }
    }

    fn assignees(all: Vec<UserRef>) -> Op {
        Op::Assignees {
            item: issue(),
            content: content(),
            add: vec![],
            remove: vec![],
            all,
        }
    }

    fn cycle(id: &str) -> FieldValue {
        FieldValue::Iteration {
            iteration_id: IterationId::new(id),
            title: "Sprint Three".into(),
            start_date: "2026-10-26".into(),
        }
    }

    /// One row per op of the spec's section 5 table that is an `issueUpdate`.
    #[test]
    fn inputs_follow_the_spec_table() {
        let rows: Vec<(Op, Value)> = vec![
            (
                set(ids::STATUS, select("s-a-done", "Done")),
                json!({"stateId": "s-a-done"}),
            ),
            (
                set(ids::PRIORITY, select("linear:priority:2", "High")),
                json!({"priority": 2}),
            ),
            (clear(ids::PRIORITY), json!({"priority": 0})),
            (
                set(ids::ESTIMATE, FieldValue::Number(3.0)),
                json!({"estimate": 3}),
            ),
            (clear(ids::ESTIMATE), json!({"estimate": null})),
            (
                set(ids::DUE, FieldValue::Date("2026-11-02".into())),
                json!({"dueDate": "2026-11-02"}),
            ),
            (clear(ids::DUE), json!({"dueDate": null})),
            (
                set(ids::CYCLE, cycle("c-next")),
                json!({"cycleId": "c-next"}),
            ),
            (clear(ids::CYCLE), json!({"cycleId": null})),
            (
                group(Some(select("l-feat", "Feature")), Some("l-bug")),
                json!({"addedLabelIds": ["l-feat"], "removedLabelIds": ["l-bug"]}),
            ),
            (
                group(Some(select("l-feat", "Feature")), None),
                json!({"addedLabelIds": ["l-feat"]}),
            ),
            (
                group(None, Some("l-bug")),
                json!({"removedLabelIds": ["l-bug"]}),
            ),
            (
                set(ids::PROJECT, select("p-2", "Billing")),
                json!({"projectId": "p-2"}),
            ),
            (clear(ids::PROJECT), json!({"projectId": null})),
            (
                Op::Reorder {
                    item: issue(),
                    after: Some(ItemId::new("i-3")),
                    sort_order: Some(-3.25),
                },
                json!({"sortOrder": -3.25}),
            ),
            (
                assignees(vec![user("u-2", "Grace Hopper")]),
                json!({"assigneeId": "u-2"}),
            ),
            (assignees(vec![]), json!({"assigneeId": null})),
            (
                Op::Labels {
                    item: issue(),
                    content: content(),
                    add: vec![label("l-ui", "UI")],
                    remove: vec![label("l-docs", "docs")],
                },
                json!({"addedLabelIds": ["l-ui"], "removedLabelIds": ["l-docs"]}),
            ),
            (
                Op::Title {
                    item: issue(),
                    content: content(),
                    title: "Fix the sign-in page".into(),
                },
                json!({"title": "Fix the sign-in page"}),
            ),
            (
                Op::Body {
                    item: issue(),
                    content: content(),
                    body: "Steps:\n\n- **open** it".into(),
                },
                json!({"description": "Steps:\n\n- **open** it"}),
            ),
            (Op::Remove { item: issue() }, json!({"projectId": null})),
        ];
        for (op, input) in rows {
            assert_eq!(
                input_for(&op),
                Some(("i-7".to_string(), input)),
                "{}: {op:?}",
                op.name()
            );
        }
    }

    /// Ops that are not one `issueUpdate`, and values Linear cannot take, have no input.
    #[test]
    fn other_ops_and_unsendable_values_have_no_input() {
        let rows = vec![
            // Every issue has a state (spec section 5: no clear).
            clear(ids::STATUS),
            // A fraction is refused before sending (section 13).
            set(ids::ESTIMATE, FieldValue::Number(2.5)),
            // A GitHub option is not a Linear priority.
            set(ids::PRIORITY, select("p0", "P0")),
            set(ids::STATUS, FieldValue::Text("Done".into())),
            // A label group is set with `GroupLabel`, which names the label it replaces.
            set(ids::group(TYPE_GROUP).as_str(), select("l-feat", "Feature")),
            set(ids::TITLE, FieldValue::Text("x".into())),
            set("F_status", select("o_done", "Done")),
            group(None, None),
            Op::Reorder {
                item: issue(),
                after: None,
                sort_order: None,
            },
            // At most one assignee.
            assignees(vec![
                user("u-1", "Ada Lovelace"),
                user("u-2", "Grace Hopper"),
            ]),
            Op::Comment {
                item: issue(),
                content: content(),
                body: "hi".into(),
                client_id: Some(ISSUE_UUID.into()),
            },
            Op::Archive { item: issue() },
            Op::Unarchive { item: issue() },
            Op::Close {
                item: issue(),
                content: content(),
                reason: None,
            },
            Op::Reopen {
                item: issue(),
                content: content(),
            },
            Op::Milestone {
                item: issue(),
                content: content(),
                milestone: Some(MilestoneRef {
                    id: "MI_1".into(),
                    title: "v1".into(),
                }),
            },
            Op::Convert {
                item: issue(),
                repo: RepoRef {
                    id: "R_1".into(),
                    slug: "acme/app".parse().unwrap(),
                },
            },
            Op::Create {
                temp: ItemId::new("local:1"),
                new: new_issue(),
                presets: vec![],
            },
        ];
        for op in rows {
            assert_eq!(input_for(&op), None, "{}: {op:?}", op.name());
        }
    }

    fn tba() -> TeamMeta {
        TeamMeta {
            id: "team-a".into(),
            key: "TBA".into(),
        }
    }

    fn new_issue() -> NewItem {
        NewItem::LinearIssue {
            id: ISSUE_UUID.into(),
            team: tba(),
            title: "Write the docs".into(),
            body: "For **everyone**".into(),
        }
    }

    fn project_board() -> ProjectId {
        ProjectId::new("linear:project:p-1")
    }

    fn team_board() -> ProjectId {
        ProjectId::new("linear:team:team-a")
    }

    #[test]
    fn create_input_carries_id_team_project_and_presets() {
        let presets = vec![
            (FieldId::new(ids::STATUS), select("s-a-todo", "Todo")),
            (
                FieldId::new(ids::PRIORITY),
                select("linear:priority:3", "Medium"),
            ),
            (ids::group(TYPE_GROUP), select("l-bug", "Bug")),
            (ids::group("lg-size"), select("l-small", "Small")),
            (FieldId::new(ids::CYCLE), cycle("c-now")),
            (FieldId::new(ids::ESTIMATE), FieldValue::Number(2.0)),
            (
                FieldId::new(ids::DUE),
                FieldValue::Date("2026-11-02".into()),
            ),
        ];
        assert_eq!(
            create_input(&project_board(), &new_issue(), &presets),
            Some(json!({
                "id": ISSUE_UUID,
                "teamId": "team-a",
                "title": "Write the docs",
                "description": "For **everyone**",
                "projectId": "p-1",
                "stateId": "s-a-todo",
                "priority": 3,
                "labelIds": ["l-bug", "l-small"],
                "cycleId": "c-now",
                "estimate": 2,
                "dueDate": "2026-11-02",
            }))
        );
        // A preset Linear cannot take refuses the whole create: one call, all or nothing.
        for preset in [
            (
                FieldId::new(ids::ASSIGNEES),
                FieldValue::Users(vec!["Ada Lovelace".into()]),
            ),
            (FieldId::new(ids::ESTIMATE), FieldValue::Number(0.5)),
        ] {
            assert_eq!(
                create_input(
                    &project_board(),
                    &new_issue(),
                    std::slice::from_ref(&preset)
                ),
                None,
                "{preset:?}"
            );
        }
        // Only a new Linear issue on a Linear board is created this way.
        let draft = NewItem::Draft {
            title: "Idea".into(),
            body: String::new(),
        };
        assert_eq!(create_input(&project_board(), &draft, &[]), None);
        assert_eq!(
            create_input(&ProjectId::new("PVT_1"), &new_issue(), &[]),
            None
        );
    }

    #[test]
    fn a_team_board_create_has_no_project() {
        assert_eq!(
            create_input(&team_board(), &new_issue(), &[]),
            Some(json!({
                "id": ISSUE_UUID,
                "teamId": "team-a",
                "title": "Write the docs",
                "description": "For **everyone**",
            }))
        );
        // A team board's Project swimlane presets one.
        let presets = [(FieldId::new(ids::PROJECT), select("p-2", "Billing"))];
        assert_eq!(
            create_input(&team_board(), &new_issue(), &presets).unwrap()["projectId"],
            "p-2"
        );
    }

    #[test]
    fn an_existing_issue_moves_into_the_project_with_its_presets() {
        // The issue is in project p-1 with Bug, its Type group's label.
        let found = item_from_wire(&issue_json()).unwrap();
        let presets = [
            (FieldId::new(ids::STATUS), select("s-a-todo", "Todo")),
            (ids::group(TYPE_GROUP), select("l-feat", "Feature")),
            (ids::group("lg-size"), select("l-small", "Small")),
        ];
        assert_eq!(
            add_input(&ProjectId::new("linear:project:p-9"), &found, &presets),
            Some(json!({
                "projectId": "p-9",
                "stateId": "s-a-todo",
                "addedLabelIds": ["l-feat", "l-small"],
                "removedLabelIds": ["l-bug"],
            }))
        );
        // A preset of the label it already has removes nothing.
        let same = [(ids::group(TYPE_GROUP), select("l-bug", "Bug"))];
        assert_eq!(
            add_input(&ProjectId::new("linear:project:p-9"), &found, &same),
            Some(json!({"projectId": "p-9", "addedLabelIds": ["l-bug"]}))
        );
        // Adding an existing issue is for project boards only.
        assert_eq!(add_input(&team_board(), &found, &[]), None);
        let assignee = [(
            FieldId::new(ids::ASSIGNEES),
            FieldValue::Users(vec!["Ada Lovelace".into()]),
        )];
        assert_eq!(add_input(&project_board(), &found, &assignee), None);
    }
}
