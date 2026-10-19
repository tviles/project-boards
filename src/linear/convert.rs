//! Decodes Linear response JSON into model types, by hand from `serde_json::Value` as the
//! GitHub client does: generated types never leave `crates/queries`.

use crate::github::convert::{nodes, str_of};
use crate::linear::ids;
use crate::model::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// A team on the board, as the resolve query reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamInfo {
    pub id: String,
    pub key: String,
    pub cycles_enabled: bool,
}

/// The board's fields from `schema` (the `BoardSchema` response: `workflowStates`,
/// `issueLabels`, `cycles`) for `teams`. `team_projects` is the `TeamProjects` response,
/// given on team boards only; it adds the Project field.
pub fn fields_from_wire(
    schema: &Value,
    teams: &[TeamInfo],
    team_projects: Option<&Value>,
) -> Vec<Field> {
    let field = |id: &str, name: &str, kind: FieldKind| Field {
        id: FieldId::new(id),
        name: name.into(),
        kind,
    };
    let mut fields = vec![
        field(ids::TITLE, "Title", FieldKind::Title),
        field(
            ids::STATUS,
            "Status",
            FieldKind::SingleSelect {
                options: states(schema, teams),
            },
        ),
        field(
            ids::PRIORITY,
            "Priority",
            FieldKind::SingleSelect {
                options: priorities(),
            },
        ),
        field(ids::ASSIGNEES, "Assignees", FieldKind::Assignees),
        field(ids::LABELS, "Labels", FieldKind::Labels),
    ];
    fields.extend(label_groups(schema));
    if teams.iter().any(|t| t.cycles_enabled) {
        fields.push(field(ids::CYCLE, "Cycle", cycles(schema, teams)));
    }
    fields.push(field(ids::ESTIMATE, "Estimate", FieldKind::Number));
    fields.push(field(ids::DUE, "Due date", FieldKind::Date));
    if let Some(projects) = team_projects {
        let options = nodes(&projects["team"], "projects")
            .into_iter()
            .map(|p| SelectOption {
                id: OptionId::new(str_of(&p["id"])),
                name: str_of(&p["name"]),
                color: OptionColor::Gray,
                kind: None,
            })
            .collect();
        fields.push(field(
            ids::PROJECT,
            "Project",
            FieldKind::SingleSelect { options },
        ));
    }
    fields.extend([
        field(ids::CREATED, "Created", FieldKind::Created),
        field(ids::UPDATED, "Updated", FieldKind::Updated),
        field(ids::CLOSED, "Closed", FieldKind::Closed),
        field(ids::PARENT, "Parent issue", FieldKind::ParentIssue),
        field(
            ids::SUB_ISSUES,
            "Sub-issues progress",
            FieldKind::SubIssuesProgress,
        ),
    ]);
    fields
}

/// Whether `node`'s team is one of `teams`.
fn on_board(node: &Value, teams: &[TeamInfo]) -> bool {
    let team = node["team"]["id"].as_str();
    teams.iter().any(|t| Some(t.id.as_str()) == team)
}

/// Every workflow state of the board's teams, by kind and then position. States sharing a
/// name become one column through `Field::buckets`.
fn states(schema: &Value, teams: &[TeamInfo]) -> Vec<SelectOption> {
    let mut states: Vec<(f64, SelectOption)> = nodes(schema, "workflowStates")
        .into_iter()
        .filter(|s| on_board(s, teams))
        .map(|s| {
            let option = SelectOption {
                id: OptionId::new(str_of(&s["id"])),
                name: str_of(&s["name"]),
                color: OptionColor::from_hex(s["color"].as_str().unwrap_or("")),
                kind: s["type"].as_str().and_then(OptionKind::from_linear),
            };
            (s["position"].as_f64().unwrap_or(0.0), option)
        })
        .collect();
    states.sort_by(|(pa, a), (pb, b)| {
        OptionKind::rank(a.kind)
            .cmp(&OptionKind::rank(b.kind))
            .then(pa.total_cmp(pb))
    });
    states.into_iter().map(|(_, o)| o).collect()
}

/// Priorities 1 to 4 (Decision 12). Priority 0, "No priority", is no value: the field's own
/// "No Priority" bucket.
fn priorities() -> Vec<SelectOption> {
    PRIORITIES
        .into_iter()
        .map(|(n, name, color)| SelectOption {
            id: ids::priority_option(n),
            name: name.into(),
            color,
            kind: None,
        })
        .collect()
}

/// Linear's priorities 1 to 4 with their option names and colours.
const PRIORITIES: [(u8, &str, OptionColor); 4] = [
    (1, "Urgent", OptionColor::Red),
    (2, "High", OptionColor::Orange),
    (3, "Medium", OptionColor::Yellow),
    (4, "Low", OptionColor::Blue),
];

const MULTI_SELECT_GROUP: &str = "multiSelect";

/// Whether a label group with `group_type` (`IssueLabel.groupType`) is single-select: every
/// group is, except a `multiSelect` one ("Groups without a type use single-select
/// behavior").
pub(crate) fn is_single_select_group(group_type: Option<&str>) -> bool {
    group_type != Some(MULTI_SELECT_GROUP)
}

/// One field per single-select label group, by group name, whose options are its labels.
/// Other labels, multi-select groups' included, stay in Labels.
fn label_groups(schema: &Value) -> Vec<Field> {
    let labels = nodes(schema, "issueLabels");
    let mut groups: Vec<Field> = labels
        .iter()
        .filter(|l| {
            l["isGroup"].as_bool() == Some(true) && is_single_select_group(l["groupType"].as_str())
        })
        .filter_map(|g| {
            let id = g["id"].as_str()?;
            let options = labels
                .iter()
                .filter(|l| l["parent"]["id"].as_str() == Some(id))
                .map(|l| SelectOption {
                    id: OptionId::new(str_of(&l["id"])),
                    name: str_of(&l["name"]),
                    color: OptionColor::from_hex(l["color"].as_str().unwrap_or("")),
                    kind: None,
                })
                .collect();
            Some(Field {
                id: ids::group(id),
                name: str_of(&g["name"]),
                kind: FieldKind::SingleSelect { options },
            })
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    groups
}

/// The board's cycles: those not past by start, and the past ones as `completed`.
fn cycles(schema: &Value, teams: &[TeamInfo]) -> FieldKind {
    let mut cycles: Vec<(bool, Iteration)> = nodes(schema, "cycles")
        .into_iter()
        .filter(|c| on_board(c, teams))
        .map(|c| {
            let starts = c["startsAt"].as_str().unwrap_or("");
            let iteration = Iteration {
                id: IterationId::new(str_of(&c["id"])),
                title: cycle_title(c),
                start_date: date_part(starts).to_string(),
                duration_days: days_between(starts, c["endsAt"].as_str().unwrap_or("")),
            };
            (c["isPast"].as_bool().unwrap_or(false), iteration)
        })
        .collect();
    cycles.sort_by(|(_, a), (_, b)| a.start_date.cmp(&b.start_date));
    let (completed, iterations): (Vec<_>, Vec<_>) = cycles.into_iter().partition(|(past, _)| *past);
    FieldKind::Iteration {
        iterations: iterations.into_iter().map(|(_, i)| i).collect(),
        completed: completed.into_iter().map(|(_, i)| i).collect(),
    }
}

/// A cycle's name, else `Cycle <number>`.
fn cycle_title(c: &Value) -> String {
    match c["name"].as_str() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!("Cycle {}", c["number"].as_u64().unwrap_or(0)),
    }
}

/// The `YYYY-MM-DD` part of an ISO 8601 time; the whole string when it is shorter.
fn date_part(t: &str) -> &str {
    t.get(..10).unwrap_or(t)
}

/// Whole days from the date part of `start` to the date part of `end` (ISO 8601 times);
/// 0 when either does not parse or `end` comes first.
fn days_between(start: &str, end: &str) -> u32 {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let date = |t: &str| time::Date::parse(t.get(..10)?, &format).ok();
    match (date(start), date(end)) {
        (Some(s), Some(e)) => u32::try_from((e - s).whole_days()).unwrap_or(0),
        _ => 0,
    }
}

/// A Linear board's `Project`. Its id is the board key's string (Decision 7); Linear boards
/// are read-only in 0.4 and link no repositories.
pub fn project_from_wire(
    board: &BoardKey,
    title: &str,
    url: &str,
    meta: LinearMeta,
    fields: Vec<Field>,
    views: Vec<View>,
) -> Project {
    Project {
        id: ProjectId::new(board.to_string()),
        board: board.clone(),
        owner_kind: OwnerKind::Organization,
        title: title.to_string(),
        url: url.to_string(),
        viewer_can_update: false,
        fields,
        views,
        linked_repos: Vec::new(),
        issue_repos: Vec::new(),
        linear: Some(meta),
    }
}

/// One issue a poll returned: changed (an archived one keeps `archived: true`), or moved to
/// the trash and so gone from the board.
#[allow(clippy::large_enum_variant)] // short-lived: one poll page at a time, mostly `Item`s
#[derive(Debug, Clone, PartialEq)]
pub enum Polled {
    Item(Item),
    Trashed(ItemId),
}

/// A non-negative whole number from Linear's `Float` (`number`, `priority`).
fn whole(v: &Value) -> Option<u32> {
    let n = v.as_f64()?;
    (n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&n)).then_some(n as u32)
}

/// Open, or closed and why, from a workflow state kind (spec section 4). An unknown kind is
/// open.
fn state_of(kind: &str) -> (ContentState, Option<StateReason>) {
    match OptionKind::from_linear(kind) {
        Some(OptionKind::Completed) => (ContentState::Closed, Some(StateReason::Completed)),
        Some(OptionKind::Canceled) => (ContentState::Closed, Some(StateReason::NotPlanned)),
        Some(OptionKind::Duplicate) => (ContentState::Closed, Some(StateReason::Duplicate)),
        _ => (ContentState::Open, None),
    }
}

/// The Priority value for Linear's `priority`: 1 to 4 only, as 0 is "No priority"
/// (Decision 12).
fn priority_value(v: &Value) -> Option<FieldValue> {
    let n = whole(v)?;
    let (n, name, _) = PRIORITIES.into_iter().find(|(p, ..)| u32::from(*p) == n)?;
    Some(FieldValue::SingleSelect {
        option_id: ids::priority_option(n),
        name: name.to_string(),
    })
}

/// An `IssueFields` node's field values. A label in a single-select group is that group's
/// value; every other label goes to Labels. Unset fields have no value.
fn values_from_wire(v: &Value) -> BTreeMap<FieldId, FieldValue> {
    let mut values = BTreeMap::new();
    let state = &v["state"];
    if let Some(id) = state["id"].as_str() {
        values.insert(
            FieldId::new(ids::STATUS),
            FieldValue::SingleSelect {
                option_id: OptionId::new(id),
                name: str_of(&state["name"]),
            },
        );
    }
    if let Some(priority) = priority_value(&v["priority"]) {
        values.insert(FieldId::new(ids::PRIORITY), priority);
    }
    if let Some(estimate) = v["estimate"].as_f64() {
        values.insert(FieldId::new(ids::ESTIMATE), FieldValue::Number(estimate));
    }
    if let Some(due) = v["dueDate"].as_str() {
        values.insert(FieldId::new(ids::DUE), FieldValue::Date(due.to_string()));
    }
    let cycle = &v["cycle"];
    if let Some(id) = cycle["id"].as_str() {
        values.insert(
            FieldId::new(ids::CYCLE),
            FieldValue::Iteration {
                iteration_id: IterationId::new(id),
                title: cycle_title(cycle),
                start_date: date_part(cycle["startsAt"].as_str().unwrap_or("")).to_string(),
            },
        );
    }
    let project = &v["project"];
    if let Some(id) = project["id"].as_str() {
        values.insert(
            FieldId::new(ids::PROJECT),
            FieldValue::SingleSelect {
                option_id: OptionId::new(id),
                name: str_of(&project["name"]),
            },
        );
    }
    let mut labels = Vec::new();
    for label in nodes(v, "labels") {
        let parent = &label["parent"];
        match (parent["id"].as_str(), label["id"].as_str()) {
            (Some(group), Some(id)) if is_single_select_group(parent["groupType"].as_str()) => {
                values.insert(
                    ids::group(group),
                    FieldValue::SingleSelect {
                        option_id: OptionId::new(id),
                        name: str_of(&label["name"]),
                    },
                );
            }
            _ => labels.push(Label {
                name: str_of(&label["name"]),
                color: label["color"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches('#')
                    .to_string(),
            }),
        }
    }
    if !labels.is_empty() {
        values.insert(FieldId::new(ids::LABELS), FieldValue::Labels(labels));
    }
    if let Some(name) = v["assignee"]["displayName"].as_str() {
        values.insert(
            FieldId::new(ids::ASSIGNEES),
            FieldValue::Users(vec![name.to_string()]),
        );
    }
    values
}

/// The issue's parent, when it has one.
fn parent_from_wire(p: &Value) -> Option<ParentIssue> {
    Some(ParentIssue {
        number: whole(&p["number"])?,
        title: str_of(&p["title"]),
        state: state_of(p["state"]["type"].as_str().unwrap_or("")).0,
        key: p["identifier"].as_str().map(String::from),
    })
}

/// Decodes an `IssueFields` node. `None` without `id`, `identifier` or `state.type`; such an
/// issue is logged by count only.
pub fn item_from_wire(v: &Value) -> Option<Item> {
    let item = issue_from_wire(v);
    if item.is_none() {
        tracing::debug!(
            skipped = 1,
            "skipped a Linear issue without an id, identifier or state type"
        );
    }
    item
}

fn issue_from_wire(v: &Value) -> Option<Item> {
    let id = v["id"].as_str()?;
    let identifier = v["identifier"].as_str()?;
    let (state, state_reason) = state_of(v["state"]["type"].as_str()?);
    let text = |key: &str| v[key].as_str().map(String::from);
    let assignee = &v["assignee"];
    let labels = nodes(v, "labels");
    Some(Item {
        id: ItemId::new(id),
        content: ItemContent::Issue {
            reference: ContentRef {
                container: str_of(&v["team"]["key"]),
                number: whole(&v["number"]).unwrap_or(0),
                key: Some(identifier.to_string()),
                url: str_of(&v["url"]),
            },
            title: str_of(&v["title"]),
            state,
        },
        archived: !v["archivedAt"].is_null(),
        updated_at: str_of(&v["updatedAt"]),
        values: values_from_wire(v),
        content_fields: ContentFields {
            created_at: text("createdAt"),
            updated_at: text("updatedAt"),
            closed_at: text("completedAt").or_else(|| text("canceledAt")),
            state_reason,
            parent: parent_from_wire(&v["parent"]),
            content_id: Some(id.to_string()),
            assignee_ids: assignee["displayName"]
                .as_str()
                .zip(assignee["id"].as_str())
                .map(|(name, id)| (name.to_string(), id.to_string()))
                .into_iter()
                .collect(),
            label_ids: labels
                .iter()
                .filter_map(|l| Some((l["name"].as_str()?.into(), l["id"].as_str()?.into())))
                .collect(),
            sort_order: v["sortOrder"].as_f64(),
            ..ContentFields::default()
        },
    })
}

/// Decodes an issue a poll returned: `Trashed` when it is in the trash, else as
/// `item_from_wire` decodes it.
pub fn polled_from_wire(v: &Value) -> Option<Polled> {
    if v["trashed"].as_bool() == Some(true) {
        return Some(Polled::Trashed(ItemId::new(v["id"].as_str()?)));
    }
    item_from_wire(v).map(Polled::Item)
}

/// A parent or sub-issue in the detail; its state is the state's name, else its kind.
fn linked(v: &Value) -> Option<LinkedRef> {
    let state = &v["state"];
    Some(LinkedRef {
        number: whole(&v["number"])?,
        title: str_of(&v["title"]),
        state: state["name"]
            .as_str()
            .or_else(|| state["type"].as_str())
            .unwrap_or_default()
            .to_string(),
        key: v["identifier"].as_str().map(String::from),
    })
}

/// The loaded comments oldest first, each top-level comment followed by its thread. A
/// comment whose parent is not on the page is top-level; replies to replies join their
/// top-level comment's thread at depth 1.
fn threaded_comments<'a>(mut comments: Vec<&'a Value>) -> Vec<Comment> {
    comments.sort_by(|a, b| a["createdAt"].as_str().cmp(&b["createdAt"].as_str()));
    let parent_of: HashMap<&'a str, Option<&'a str>> = comments
        .iter()
        .filter_map(|c| Some((c["id"].as_str()?, c["parent"]["id"].as_str())))
        .collect();
    let in_page = |id: &&str| parent_of.contains_key(id);
    // The top-level comment `c` replies to, directly or through other replies; `None` for a
    // top-level comment, and for one in a cycle of replies, which has no top.
    let root = |c: &'a Value| -> Option<&'a str> {
        let mut top = c["parent"]["id"].as_str().filter(in_page)?;
        for _ in 0..comments.len() {
            match parent_of[top].filter(in_page) {
                Some(up) => top = up,
                None => return Some(top),
            }
        }
        None
    };
    let roots: Vec<Option<&str>> = comments.iter().map(|&c| root(c)).collect();
    let comment = |c: &Value, depth: u8| Comment {
        author: c["user"]["displayName"]
            .as_str()
            .unwrap_or("Linear")
            .to_string(),
        created_at: str_of(&c["createdAt"]),
        body: str_of(&c["body"]),
        depth,
    };
    let mut out = Vec::with_capacity(comments.len());
    for (c, _) in comments.iter().zip(&roots).filter(|(_, r)| r.is_none()) {
        out.push(comment(c, 0));
        let Some(id) = c["id"].as_str() else { continue };
        out.extend(
            comments
                .iter()
                .zip(&roots)
                .filter(|(_, r)| **r == Some(id))
                .map(|(reply, _)| comment(reply, 1)),
        );
    }
    out
}

/// Decodes the `issue` of an `IssueDetail` response. A comment with no user (an integration)
/// is by "Linear"; `comments_total` is the number loaded, as Linear gives no count.
pub fn detail_from_wire(node: &Value) -> ItemDetail {
    let comments = nodes(node, "comments");
    let page = &node["comments"]["pageInfo"];
    ItemDetail {
        body: str_of(&node["description"]),
        comments_total: comments.len(),
        comments: threaded_comments(comments),
        older_cursor: if page["hasPreviousPage"].as_bool() == Some(true) {
            page["startCursor"].as_str().map(String::from)
        } else {
            None
        },
        sub_issues: nodes(node, "children")
            .into_iter()
            .filter_map(linked)
            .collect(),
        parent: linked(&node["parent"]),
        issue_type: None,
        linked_prs: Vec::new(),
        attachments: nodes(node, "attachments")
            .into_iter()
            .map(|a| Attachment {
                title: str_of(&a["title"]),
                url: str_of(&a["url"]),
            })
            .collect(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    pub(crate) const TYPE_GROUP: &str = "lg-type";
    const SIZE_GROUP: &str = "lg-size";

    pub(crate) fn tba() -> TeamInfo {
        TeamInfo {
            id: "team-a".into(),
            key: "TBA".into(),
            cycles_enabled: true,
        }
    }

    pub(crate) fn tbb() -> TeamInfo {
        TeamInfo {
            id: "team-b".into(),
            key: "TBB".into(),
            cycles_enabled: false,
        }
    }

    fn state(id: &str, name: &str, kind: &str, position: f64, team: &str) -> Value {
        json!({"id": id, "name": name, "type": kind, "color": "#5e6ad2",
               "position": position, "team": {"id": team}})
    }

    fn label(id: &str, name: &str, group: Option<&str>, parent: Option<&str>) -> Value {
        json!({"id": id, "name": name, "color": "#bec2c8", "isGroup": group.is_some(),
               "groupType": group, "parent": parent.map(|p| json!({"id": p}))})
    }

    /// A `BoardSchema` response for TBA (cycles on) and TBB (cycles off).
    pub(crate) fn schema_json() -> Value {
        json!({
            "workflowStates": {"nodes": [
                state("s-a-todo", "Todo", "unstarted", 0.0, "team-a"),
                state("s-a-done", "Done", "completed", 2.0, "team-a"),
                state("s-a-triage", "Triage", "triage", 0.0, "team-a"),
                state("s-b-todo", "Todo", "unstarted", 0.0, "team-b"),
                state("s-b-review", "Review", "started", 2.0, "team-b"),
                // Listed after Review, same kind, earlier position: only position orders them.
                state("s-a-prog", "In Progress", "started", 1.0, "team-a"),
                state("s-b-done", "Done", "completed", 3.0, "team-b"),
                state("s-b-paused", "Paused", "paused", 4.0, "team-b"),
            ]},
            "issueLabels": {"nodes": [
                label(TYPE_GROUP, "Type", Some("singleSelect"), None),
                label("l-bug", "Bug", None, Some(TYPE_GROUP)),
                label("l-feature", "Feature", None, Some(TYPE_GROUP)),
                label("lg-area", "Area", Some("multiSelect"), None),
                label("l-ui", "UI", None, Some("lg-area")),
                label("l-docs", "docs", None, None),
                // A group with no type behaves as single-select.
                {"id": SIZE_GROUP, "name": "Size", "color": "#bec2c8", "isGroup": true,
                 "groupType": null, "parent": null},
                label("l-small", "Small", None, Some(SIZE_GROUP)),
            ]},
            "cycles": {"nodes": [
                {"id": "c-now", "number": 2, "name": "Sprint Two",
                 "startsAt": "2026-10-12T05:00:00.000Z", "endsAt": "2026-10-26T04:00:00.000Z",
                 "isPast": false, "team": {"id": "team-a"}},
                {"id": "c-past", "number": 1, "name": null,
                 "startsAt": "2026-09-28T05:00:00.000Z", "endsAt": "2026-10-12T04:00:00.000Z",
                 "isPast": true, "team": {"id": "team-a"}},
            ]},
            "customViews": {"nodes": []},
        })
    }

    /// A `TeamProjects` response.
    pub(crate) fn team_projects_json() -> Value {
        json!({"team": {"projects": {"nodes": [
            {"id": "p-1", "name": "Launch"},
            {"id": "p-2", "name": "Cleanup"},
        ]}}})
    }

    pub(crate) fn project_board_fields() -> Vec<Field> {
        fields_from_wire(&schema_json(), &[tba(), tbb()], None)
    }

    pub(crate) fn team_board_fields() -> Vec<Field> {
        fields_from_wire(&schema_json(), &[tba()], Some(&team_projects_json()))
    }

    fn field<'a>(fields: &'a [Field], id: &str) -> Option<&'a Field> {
        fields.iter().find(|f| f.id.as_str() == id)
    }

    fn options(f: &Field) -> &[SelectOption] {
        match &f.kind {
            FieldKind::SingleSelect { options } => options,
            other => panic!("{} is {other:?}", f.name),
        }
    }

    #[test]
    fn fields_come_in_the_documented_order() {
        let names: Vec<_> = team_board_fields().into_iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            [
                "Title",
                "Status",
                "Priority",
                "Assignees",
                "Labels",
                "Size",
                "Type",
                "Cycle",
                "Estimate",
                "Due date",
                "Project",
                "Created",
                "Updated",
                "Closed",
                "Parent issue",
                "Sub-issues progress",
            ]
        );
        let fields = team_board_fields();
        let kind = |id: &str| field(&fields, id).map(|f| f.kind.clone());
        assert_eq!(kind(ids::TITLE), Some(FieldKind::Title));
        assert_eq!(kind(ids::ASSIGNEES), Some(FieldKind::Assignees));
        assert_eq!(kind(ids::LABELS), Some(FieldKind::Labels));
        assert_eq!(kind(ids::ESTIMATE), Some(FieldKind::Number));
        assert_eq!(kind(ids::DUE), Some(FieldKind::Date));
        assert_eq!(kind(ids::CREATED), Some(FieldKind::Created));
        assert_eq!(kind(ids::UPDATED), Some(FieldKind::Updated));
        assert_eq!(kind(ids::CLOSED), Some(FieldKind::Closed));
        assert_eq!(kind(ids::PARENT), Some(FieldKind::ParentIssue));
        assert_eq!(kind(ids::SUB_ISSUES), Some(FieldKind::SubIssuesProgress));
    }

    #[test]
    fn states_merge_by_name_and_sort_by_kind_then_position() {
        let fields = project_board_fields();
        let status = field(&fields, ids::STATUS).expect("Status");
        assert_eq!(status.name, "Status");
        let buckets = status.buckets().unwrap();
        let titles: Vec<_> = buckets.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Triage",
                "Todo",
                "In Progress",
                "Review",
                "Done",
                "Paused",
                "No Status"
            ]
        );
        assert_eq!(buckets[1].aliases.len(), 1);
        assert!(buckets[1].matches("s-a-todo") && buckets[1].matches("s-b-todo"));
        let opts = options(status);
        let paused = opts.iter().find(|o| o.name == "Paused").unwrap();
        assert_eq!(paused.kind, None);
        let triage = opts.iter().find(|o| o.name == "Triage").unwrap();
        assert_eq!(triage.kind, Some(OptionKind::Triage));
        assert_eq!(triage.color, OptionColor::Hex(0x5e6ad2));
        assert_eq!(triage.id, OptionId::new("s-a-triage"));
    }

    #[test]
    fn priority_has_four_named_options_and_no_value_is_the_no_column() {
        let fields = project_board_fields();
        let priority = field(&fields, ids::PRIORITY).expect("Priority");
        let buckets = priority.buckets().unwrap();
        let got: Vec<_> = buckets
            .iter()
            .map(|b| (b.key.clone(), b.title.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Some("linear:priority:1".to_string()), "Urgent"),
                (Some("linear:priority:2".to_string()), "High"),
                (Some("linear:priority:3".to_string()), "Medium"),
                (Some("linear:priority:4".to_string()), "Low"),
                (None, "No Priority"),
            ]
        );
        let colors: Vec<_> = options(priority).iter().map(|o| o.color).collect();
        assert_eq!(
            colors,
            [
                OptionColor::Red,
                OptionColor::Orange,
                OptionColor::Yellow,
                OptionColor::Blue
            ]
        );
    }

    #[test]
    fn cycles_only_when_a_team_has_them() {
        let fields = project_board_fields();
        let cycle = field(&fields, ids::CYCLE).expect("Cycle");
        assert_eq!(cycle.name, "Cycle");
        let FieldKind::Iteration {
            iterations,
            completed,
        } = &cycle.kind
        else {
            panic!("{:?}", cycle.kind)
        };
        assert_eq!(
            iterations,
            &[Iteration {
                id: IterationId::new("c-now"),
                title: "Sprint Two".into(),
                start_date: "2026-10-12".into(),
                duration_days: 14,
            }]
        );
        assert_eq!(
            completed,
            &[Iteration {
                id: IterationId::new("c-past"),
                title: "Cycle 1".into(),
                start_date: "2026-09-28".into(),
                duration_days: 14,
            }]
        );
        let without = fields_from_wire(&schema_json(), &[tbb()], None);
        assert!(field(&without, ids::CYCLE).is_none());
        assert!(field(&without, ids::STATUS).is_some());
    }

    #[test]
    fn single_select_groups_become_fields_and_other_labels_stay_labels() {
        let fields = project_board_fields();
        let group = field(&fields, ids::group(TYPE_GROUP).as_str()).expect("Type");
        assert_eq!(group.name, "Type");
        let names: Vec<_> = options(group).iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Bug", "Feature"]);
        assert_eq!(options(group)[0].id, OptionId::new("l-bug"));
        let size = field(&fields, ids::group(SIZE_GROUP).as_str()).expect("untyped group");
        assert_eq!(size.name, "Size");
        let names: Vec<_> = options(size).iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Small"]);
        assert!(field(&fields, ids::group("lg-area").as_str()).is_none());
        assert!(!fields.iter().any(|f| f.name == "Area"));
    }

    #[test]
    fn only_multi_select_groups_are_not_single_select() {
        assert!(is_single_select_group(Some("singleSelect")));
        assert!(is_single_select_group(None));
        assert!(!is_single_select_group(Some("multiSelect")));
    }

    #[test]
    fn team_boards_get_a_project_field() {
        assert!(field(&project_board_fields(), ids::PROJECT).is_none());
        let fields = team_board_fields();
        let project = field(&fields, ids::PROJECT).expect("Project");
        assert_eq!(project.name, "Project");
        let got: Vec<_> = options(project)
            .iter()
            .map(|o| (o.id.as_str(), o.name.as_str()))
            .collect();
        assert_eq!(got, [("p-1", "Launch"), ("p-2", "Cleanup")]);
    }

    #[test]
    fn a_linear_project_is_keyed_by_its_board_and_read_only() {
        let board = BoardKey::LinearTeam {
            id: "team-a".into(),
        };
        let meta = LinearMeta {
            url_key: "acme".into(),
            viewer_id: "u1".into(),
            team_keys: vec!["TBA".into()],
        };
        let fields = team_board_fields();
        let p = project_from_wire(
            &board,
            "Team A",
            "https://linear.app/acme/team/TBA",
            meta.clone(),
            fields.clone(),
            vec![],
        );
        assert_eq!(p.id, ProjectId::new("linear:team:team-a"));
        assert_eq!(p.board, board);
        assert_eq!(p.owner_kind, OwnerKind::Organization);
        assert_eq!(p.title, "Team A");
        assert_eq!(p.url, "https://linear.app/acme/team/TBA");
        assert!(!p.viewer_can_update);
        assert_eq!(p.fields, fields);
        assert!(p.linked_repos.is_empty() && p.issue_repos.is_empty());
        assert_eq!(p.linear, Some(meta));
    }

    /// An `IssueFields` node: TBA-7, In Progress, High, estimate 3, due 2026-11-02, in cycle
    /// Sprint Two and project Launch, labelled Bug (the Type group) and docs, assigned to Ada
    /// Lovelace, under TBA-2.
    pub(crate) fn issue_json() -> Value {
        json!({
            "id": "i-7", "identifier": "TBA-7", "number": 7, "title": "Fix the login page",
            "url": "https://linear.app/acme/issue/TBA-7/fix-the-login-page",
            "priority": 2, "estimate": 3, "dueDate": "2026-11-02", "sortOrder": -12.5,
            "createdAt": "2026-10-01T09:00:00.000Z", "updatedAt": "2026-10-15T10:30:00.000Z",
            "completedAt": null, "canceledAt": null, "archivedAt": null, "trashed": null,
            "state": {"id": "s-a-prog", "name": "In Progress", "type": "started",
                      "color": "#f2c94c", "position": 1},
            "team": {"id": "team-a", "key": "TBA"},
            "assignee": {"id": "u-1", "displayName": "Ada Lovelace"},
            "labels": {"nodes": [
                {"id": "l-bug", "name": "Bug", "color": "#eb5757",
                 "parent": {"id": TYPE_GROUP, "groupType": "singleSelect"}},
                {"id": "l-docs", "name": "docs", "color": "#bec2c8", "parent": null},
            ]},
            "cycle": {"id": "c-now", "number": 2, "name": "Sprint Two",
                      "startsAt": "2026-10-12T00:00:00.000Z", "endsAt": "2026-10-26T00:00:00.000Z"},
            "project": {"id": "p-1", "name": "Launch"},
            "parent": {"id": "i-2", "identifier": "TBA-2", "number": 2, "title": "Login epic",
                       "state": {"type": "started"}},
        })
    }

    fn issue_with(changes: Value) -> Value {
        let mut v = issue_json();
        for (key, value) in changes.as_object().unwrap() {
            v[key] = value.clone();
        }
        v
    }

    fn select(option_id: &str, name: &str) -> FieldValue {
        FieldValue::SingleSelect {
            option_id: OptionId::new(option_id),
            name: name.into(),
        }
    }

    #[test]
    fn an_issue_decodes_with_key_state_and_values() {
        let item = item_from_wire(&issue_json()).expect("decodes");
        assert_eq!(item.id, ItemId::new("i-7"));
        assert_eq!(
            item.content,
            ItemContent::Issue {
                reference: ContentRef {
                    container: "TBA".into(),
                    number: 7,
                    key: Some("TBA-7".into()),
                    url: "https://linear.app/acme/issue/TBA-7/fix-the-login-page".into(),
                },
                title: "Fix the login page".into(),
                state: ContentState::Open,
            }
        );
        assert!(!item.archived);
        assert_eq!(item.updated_at, "2026-10-15T10:30:00.000Z");
        let expected: BTreeMap<FieldId, FieldValue> = [
            (ids::STATUS.to_string(), select("s-a-prog", "In Progress")),
            (
                ids::PRIORITY.to_string(),
                select("linear:priority:2", "High"),
            ),
            (ids::ESTIMATE.to_string(), FieldValue::Number(3.0)),
            (ids::DUE.to_string(), FieldValue::Date("2026-11-02".into())),
            (
                ids::CYCLE.to_string(),
                FieldValue::Iteration {
                    iteration_id: IterationId::new("c-now"),
                    title: "Sprint Two".into(),
                    start_date: "2026-10-12".into(),
                },
            ),
            (ids::PROJECT.to_string(), select("p-1", "Launch")),
            (
                ids::group(TYPE_GROUP).as_str().to_string(),
                select("l-bug", "Bug"),
            ),
            (
                ids::LABELS.to_string(),
                FieldValue::Labels(vec![Label {
                    name: "docs".into(),
                    color: "bec2c8".into(),
                }]),
            ),
            (
                ids::ASSIGNEES.to_string(),
                FieldValue::Users(vec!["Ada Lovelace".into()]),
            ),
        ]
        .into_iter()
        .map(|(id, value)| (FieldId::new(id), value))
        .collect();
        assert_eq!(item.values, expected);
        assert_eq!(
            item.content_fields,
            ContentFields {
                created_at: Some("2026-10-01T09:00:00.000Z".into()),
                updated_at: Some("2026-10-15T10:30:00.000Z".into()),
                closed_at: None,
                state_reason: None,
                parent: Some(ParentIssue {
                    number: 2,
                    title: "Login epic".into(),
                    state: ContentState::Open,
                    key: Some("TBA-2".into()),
                }),
                content_id: Some("i-7".into()),
                assignee_ids: [("Ada Lovelace".to_string(), "u-1".to_string())].into(),
                label_ids: [
                    ("Bug".to_string(), "l-bug".to_string()),
                    ("docs".to_string(), "l-docs".to_string()),
                ]
                .into(),
                sort_order: Some(-12.5),
                ..ContentFields::default()
            }
        );
    }

    #[test]
    fn label_groups_follow_the_group_type() {
        let v = issue_with(json!({"labels": {"nodes": [
            {"id": "l-ui", "name": "UI", "color": "#4ea7fc",
             "parent": {"id": "lg-area", "groupType": "multiSelect"}},
            {"id": "l-small", "name": "Small", "color": "#bec2c8",
             "parent": {"id": "lg-size", "groupType": null}},
        ]}}));
        let item = item_from_wire(&v).unwrap();
        assert_eq!(
            item.values.get(&FieldId::new(ids::LABELS)),
            Some(&FieldValue::Labels(vec![Label {
                name: "UI".into(),
                color: "4ea7fc".into(),
            }]))
        );
        assert_eq!(
            item.values.get(&ids::group("lg-size")),
            Some(&select("l-small", "Small"))
        );
        assert!(!item.values.contains_key(&ids::group("lg-area")));
    }

    #[test]
    fn empty_values_are_absent() {
        let v = issue_with(json!({
            "estimate": null, "dueDate": null, "cycle": null, "project": null,
            "assignee": null, "labels": {"nodes": []}, "parent": null,
        }));
        let item = item_from_wire(&v).unwrap();
        let keys: Vec<_> = item.values.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, [ids::PRIORITY, ids::STATUS]);
        assert_eq!(item.content_fields.parent, None);
        assert!(item.content_fields.assignee_ids.is_empty());
        assert!(item.content_fields.label_ids.is_empty());
    }

    #[test]
    fn a_cycle_without_a_name_is_titled_by_number() {
        let v = issue_with(json!({"cycle": {"id": "c-past", "number": 1, "name": null,
            "startsAt": "2026-09-28T05:00:00.000Z", "endsAt": "2026-10-12T04:00:00.000Z"}}));
        let item = item_from_wire(&v).unwrap();
        assert_eq!(
            item.values.get(&FieldId::new(ids::CYCLE)),
            Some(&FieldValue::Iteration {
                iteration_id: IterationId::new("c-past"),
                title: "Cycle 1".into(),
                start_date: "2026-09-28".into(),
            })
        );
    }

    #[test]
    fn closed_kinds_map_to_state_and_reason() {
        let cases = [
            (
                "completed",
                ContentState::Closed,
                Some(StateReason::Completed),
            ),
            (
                "canceled",
                ContentState::Closed,
                Some(StateReason::NotPlanned),
            ),
            (
                "duplicate",
                ContentState::Closed,
                Some(StateReason::Duplicate),
            ),
            ("started", ContentState::Open, None),
            ("triage", ContentState::Open, None),
            ("backlog", ContentState::Open, None),
            ("unstarted", ContentState::Open, None),
        ];
        for (kind, want_state, want_reason) in cases {
            let v = issue_with(json!({"state": {"id": "s", "name": "S", "type": kind}}));
            let item = item_from_wire(&v).unwrap();
            let ItemContent::Issue { state, .. } = item.content else {
                panic!("{kind}: {:?}", item.content)
            };
            assert_eq!(state, want_state, "{kind}");
            assert_eq!(item.content_fields.state_reason, want_reason, "{kind}");
        }
        let done = issue_with(
            json!({"state": {"id": "s", "name": "Done", "type": "completed"},
                                     "completedAt": "2026-10-16T08:00:00.000Z"}),
        );
        assert_eq!(
            item_from_wire(&done)
                .unwrap()
                .content_fields
                .closed_at
                .as_deref(),
            Some("2026-10-16T08:00:00.000Z")
        );
        let dropped = issue_with(
            json!({"state": {"id": "s", "name": "Canceled", "type": "canceled"},
                                        "canceledAt": "2026-10-17T08:00:00.000Z"}),
        );
        assert_eq!(
            item_from_wire(&dropped)
                .unwrap()
                .content_fields
                .closed_at
                .as_deref(),
            Some("2026-10-17T08:00:00.000Z")
        );
        let parent_done = issue_with(json!({"parent": {"id": "i-2", "identifier": "TBA-2",
            "number": 2, "title": "Login epic", "state": {"type": "completed"}}}));
        assert_eq!(
            item_from_wire(&parent_done)
                .unwrap()
                .content_fields
                .parent
                .map(|p| p.state),
            Some(ContentState::Closed)
        );
    }

    #[test]
    fn unknown_state_kind_is_open() {
        let v =
            issue_with(json!({"state": {"id": "s-b-paused", "name": "Paused", "type": "paused"}}));
        let item = item_from_wire(&v).unwrap();
        let ItemContent::Issue { state, .. } = item.content else {
            panic!("{:?}", item.content)
        };
        assert_eq!(state, ContentState::Open);
        assert_eq!(item.content_fields.state_reason, None);
        assert_eq!(
            item.values.get(&FieldId::new(ids::STATUS)),
            Some(&select("s-b-paused", "Paused"))
        );
    }

    #[test]
    fn priority_zero_has_no_value() {
        let priority = |n: Value| {
            item_from_wire(&issue_with(json!({ "priority": n })))
                .unwrap()
                .values
                .get(&FieldId::new(ids::PRIORITY))
                .cloned()
        };
        assert_eq!(priority(json!(0)), None);
        assert_eq!(priority(json!(5)), None);
        assert_eq!(
            priority(json!(1)),
            Some(select("linear:priority:1", "Urgent"))
        );
        assert_eq!(
            priority(json!(3.0)),
            Some(select("linear:priority:3", "Medium"))
        );
        assert_eq!(priority(json!(4)), Some(select("linear:priority:4", "Low")));
    }

    #[test]
    fn archived_is_kept_and_trashed_is_removal() {
        let archived = issue_with(json!({"archivedAt": "2026-10-18T12:00:00.000Z"}));
        assert!(item_from_wire(&archived).unwrap().archived);
        match polled_from_wire(&archived) {
            Some(Polled::Item(item)) => {
                assert!(item.archived);
                assert_eq!(item.id, ItemId::new("i-7"));
            }
            other => panic!("{other:?}"),
        }
        let trashed = issue_with(json!({"trashed": true}));
        assert_eq!(
            polled_from_wire(&trashed),
            Some(Polled::Trashed(ItemId::new("i-7")))
        );
        let restored = issue_with(json!({"trashed": false}));
        assert!(matches!(
            polled_from_wire(&restored),
            Some(Polled::Item(item)) if !item.archived
        ));
        assert_eq!(
            polled_from_wire(&issue_json()),
            Some(Polled::Item(item_from_wire(&issue_json()).unwrap()))
        );
    }

    #[test]
    fn a_missing_identifier_is_skipped_not_a_panic() {
        let mut no_identifier = issue_json();
        no_identifier.as_object_mut().unwrap().remove("identifier");
        assert_eq!(item_from_wire(&no_identifier), None);
        assert_eq!(polled_from_wire(&no_identifier), None);
        let mut no_id = issue_json();
        no_id.as_object_mut().unwrap().remove("id");
        assert_eq!(item_from_wire(&no_id), None);
        assert_eq!(
            polled_from_wire(&issue_with(json!({"id": null, "trashed": true}))),
            None
        );
        assert_eq!(item_from_wire(&issue_with(json!({"state": null}))), None);
        assert_eq!(
            item_from_wire(&issue_with(json!({"state": {"id": "s", "name": "S"}}))),
            None
        );
        assert_eq!(item_from_wire(&Value::Null), None);
        assert_eq!(polled_from_wire(&Value::Null), None);
    }

    fn comment(id: &str, at: &str, parent: Option<&str>, author: Option<&str>) -> Value {
        json!({"id": id, "body": format!("body {id}"), "createdAt": at,
               "user": author.map(|a| json!({"displayName": a})),
               "parent": parent.map(|p| json!({"id": p}))})
    }

    /// An `IssueDetail` response's `issue`.
    fn detail_json(comments: Vec<Value>, has_older: bool) -> Value {
        json!({
            "id": "i-7", "description": "The **login** page.",
            "parent": {"id": "i-2", "identifier": "TBA-2", "number": 2, "title": "Login epic",
                       "state": {"type": "started"}},
            "children": {"nodes": [
                {"id": "i-8", "identifier": "TBA-8", "number": 8, "title": "Style the button",
                 "state": {"name": "Done", "type": "completed"}},
                {"id": "i-9", "identifier": "TBA-9", "number": 9, "title": "Write the copy",
                 "state": {"name": null, "type": "unstarted"}},
            ]},
            "attachments": {"nodes": [
                {"title": "Fix login", "url": "https://github.com/acme/web/pull/12"},
                {"title": "Design", "url": "https://www.figma.com/file/abc"},
            ]},
            "comments": {
                "pageInfo": {"hasPreviousPage": has_older, "startCursor": "cur-1"},
                "nodes": comments,
            },
        })
    }

    fn thread(detail: &ItemDetail) -> Vec<(String, u8)> {
        detail
            .comments
            .iter()
            .map(|c| (c.body.clone(), c.depth))
            .collect()
    }

    #[test]
    fn detail_threads_replies_under_parents_oldest_first() {
        let comments = vec![
            comment(
                "c3",
                "2026-10-03T09:00:00.000Z",
                Some("c1"),
                Some("Ada Lovelace"),
            ),
            comment("c2", "2026-10-02T09:00:00.000Z", None, Some("Grace Hopper")),
            comment("c1", "2026-10-01T09:00:00.000Z", None, Some("Ada Lovelace")),
        ];
        let detail = detail_from_wire(&detail_json(comments, false));
        assert_eq!(
            thread(&detail),
            [
                ("body c1".to_string(), 0),
                ("body c3".to_string(), 1),
                ("body c2".to_string(), 0)
            ]
        );
        assert_eq!(
            detail.comments[0],
            Comment {
                author: "Ada Lovelace".into(),
                created_at: "2026-10-01T09:00:00.000Z".into(),
                body: "body c1".into(),
                depth: 0,
            }
        );
        assert_eq!(detail.comments[2].author, "Grace Hopper");
        assert_eq!(detail.comments_total, 3);
        assert_eq!(detail.older_cursor, None);
    }

    #[test]
    fn deep_replies_flatten_and_orphans_are_top_level() {
        let comments = vec![
            comment(
                "c4",
                "2026-10-04T09:00:00.000Z",
                Some("c3"),
                Some("Ada Lovelace"),
            ),
            comment(
                "c5",
                "2026-10-02T09:00:00.000Z",
                Some("older"),
                Some("Grace Hopper"),
            ),
            comment(
                "c3",
                "2026-10-03T09:00:00.000Z",
                Some("c1"),
                Some("Grace Hopper"),
            ),
            comment("c1", "2026-10-01T09:00:00.000Z", None, None),
            // A cycle is not lost: both become top-level.
            comment("x1", "2026-10-05T09:00:00.000Z", Some("x2"), None),
            comment("x2", "2026-10-06T09:00:00.000Z", Some("x1"), None),
        ];
        let detail = detail_from_wire(&detail_json(comments, false));
        assert_eq!(
            thread(&detail),
            [
                ("body c1".to_string(), 0),
                ("body c3".to_string(), 1),
                ("body c4".to_string(), 1),
                ("body c5".to_string(), 0),
                ("body x1".to_string(), 0),
                ("body x2".to_string(), 0),
            ]
        );
        assert_eq!(detail.comments[0].author, "Linear");
        assert_eq!(detail.comments_total, 6);
    }

    #[test]
    fn detail_keeps_attachments_children_and_the_older_cursor() {
        let detail = detail_from_wire(&detail_json(vec![], true));
        assert_eq!(detail.body, "The **login** page.");
        assert_eq!(
            detail.parent,
            Some(LinkedRef {
                number: 2,
                title: "Login epic".into(),
                state: "started".into(),
                key: Some("TBA-2".into()),
            })
        );
        assert_eq!(
            detail.sub_issues,
            [
                LinkedRef {
                    number: 8,
                    title: "Style the button".into(),
                    state: "Done".into(),
                    key: Some("TBA-8".into()),
                },
                LinkedRef {
                    number: 9,
                    title: "Write the copy".into(),
                    state: "unstarted".into(),
                    key: Some("TBA-9".into()),
                },
            ]
        );
        assert_eq!(
            detail.attachments,
            [
                Attachment {
                    title: "Fix login".into(),
                    url: "https://github.com/acme/web/pull/12".into(),
                },
                Attachment {
                    title: "Design".into(),
                    url: "https://www.figma.com/file/abc".into(),
                },
            ]
        );
        assert_eq!(detail.older_cursor.as_deref(), Some("cur-1"));
        assert_eq!(detail.comments_total, 0);
        assert_eq!(detail.issue_type, None);
        assert!(detail.linked_prs.is_empty());

        let bare = detail_from_wire(&json!({
            "id": "i-1", "description": null, "parent": null,
            "children": {"nodes": []}, "attachments": {"nodes": []},
            "comments": {"pageInfo": {"hasPreviousPage": false, "startCursor": null}, "nodes": []},
        }));
        assert_eq!(bare, ItemDetail::default());
    }
}
