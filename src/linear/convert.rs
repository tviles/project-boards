//! Decodes Linear response JSON into model types, by hand from `serde_json::Value` as the
//! GitHub client does: generated types never leave `crates/queries`.

use crate::github::convert::{nodes, str_of};
use crate::linear::ids;
use crate::model::*;
use serde_json::Value;

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
    [
        (1, "Urgent", OptionColor::Red),
        (2, "High", OptionColor::Orange),
        (3, "Medium", OptionColor::Yellow),
        (4, "Low", OptionColor::Blue),
    ]
    .into_iter()
    .map(|(n, name, color)| SelectOption {
        id: ids::priority_option(n),
        name: name.into(),
        color,
        kind: None,
    })
    .collect()
}

const SINGLE_SELECT_GROUP: &str = "singleSelect";

/// One field per single-select label group, by group name, whose options are its labels.
/// Other labels, multi-select groups' included, stay in Labels.
fn label_groups(schema: &Value) -> Vec<Field> {
    let labels = nodes(schema, "issueLabels");
    let mut groups: Vec<Field> = labels
        .iter()
        .filter(|l| l["isGroup"].as_bool() == Some(true) && l["groupType"] == SINGLE_SELECT_GROUP)
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
            let title = match c["name"].as_str() {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => format!("Cycle {}", c["number"].as_u64().unwrap_or(0)),
            };
            let iteration = Iteration {
                id: IterationId::new(str_of(&c["id"])),
                title,
                start_date: starts.get(..10).unwrap_or(starts).to_string(),
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) const TYPE_GROUP: &str = "lg-type";

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
                state("s-a-prog", "In Progress", "started", 1.0, "team-a"),
                state("s-a-done", "Done", "completed", 2.0, "team-a"),
                state("s-a-triage", "Triage", "triage", 0.0, "team-a"),
                state("s-b-todo", "Todo", "unstarted", 0.0, "team-b"),
                state("s-b-review", "Review", "started", 2.0, "team-b"),
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
        assert!(field(&fields, ids::group("lg-area").as_str()).is_none());
        assert!(!fields.iter().any(|f| f.name == "Area"));
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
}
