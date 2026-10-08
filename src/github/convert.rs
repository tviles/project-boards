//! Decodes GraphQL response JSON into model types. Every union match has a fallback arm,
//! so a type GitHub adds later degrades to Unsupported/Unknown instead of failing.

use crate::github::GithubError;
use crate::model::*;
use serde_json::Value;

pub(crate) fn typename(v: &Value) -> &str {
    v.get("__typename").and_then(Value::as_str).unwrap_or("")
}

/// Non-null entries of `v[key].nodes`.
pub(crate) fn nodes<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    v[key]["nodes"]
        .as_array()
        .map(|a| a.iter().filter(|n| !n.is_null()).collect())
        .unwrap_or_default()
}

pub(crate) fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn field_ids(v: &Value, key: &str) -> Vec<FieldId> {
    nodes(v, key)
        .into_iter()
        .filter_map(|n| n["id"].as_str().map(FieldId::new))
        .collect()
}

fn options(v: &Value) -> Vec<SelectOption> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|o| SelectOption {
                    id: OptionId::new(str_of(&o["id"])),
                    name: str_of(&o["name"]),
                    color: OptionColor::from_api(o["color"].as_str().unwrap_or("")),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn iterations(v: &Value) -> Vec<Iteration> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|i| Iteration {
                    id: IterationId::new(str_of(&i["id"])),
                    title: str_of(&i["title"]),
                    start_date: str_of(&i["startDate"]),
                    duration_days: i["duration"].as_u64().unwrap_or(0) as u32,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn field_from_wire(v: &Value) -> Field {
    let kind = match typename(v) {
        "ProjectV2SingleSelectField" => FieldKind::SingleSelect {
            options: options(&v["options"]),
        },
        "ProjectV2MultiSelectField" => FieldKind::MultiSelect {
            options: options(&v["multiSelectOptions"]),
        },
        "ProjectV2IterationField" => FieldKind::Iteration {
            iterations: iterations(&v["configuration"]["iterations"]),
            completed: iterations(&v["configuration"]["completedIterations"]),
        },
        "ProjectV2Field" => FieldKind::from_data_type(v["dataType"].as_str().unwrap_or("")),
        other => FieldKind::Unsupported {
            type_name: other.to_string(),
        },
    };
    Field {
        id: FieldId::new(str_of(&v["id"])),
        name: v["name"]
            .as_str()
            .unwrap_or("(unsupported field)")
            .to_string(),
        kind,
    }
}

pub fn view_from_wire(v: &Value) -> View {
    View {
        id: ViewId::new(str_of(&v["id"])),
        number: v["number"].as_u64().unwrap_or(0) as u32,
        name: str_of(&v["name"]),
        layout: Layout::from_api(v["layout"].as_str().unwrap_or("")),
        filter: str_of(&v["filter"]),
        visible_fields: field_ids(v, "visibleFields"),
        group_by: field_ids(v, "groupByFields"),
        vertical_group_by: field_ids(v, "verticalGroupByFields"),
        sort_by: nodes(v, "sortByFields")
            .into_iter()
            .filter_map(|n| {
                Some(SortSpec {
                    field: FieldId::new(n["field"]["id"].as_str()?),
                    direction: if n["direction"] == "DESC" {
                        SortDirection::Desc
                    } else {
                        SortDirection::Asc
                    },
                })
            })
            .collect(),
    }
}

/// Decodes a `ProjectSchemaFields` object.
pub fn project_from_wire(v: &Value, board: &BoardRef) -> Result<Project, GithubError> {
    let id = v["id"]
        .as_str()
        .ok_or_else(|| GithubError::Decode("project has no id".into()))?;
    Ok(Project {
        id: ProjectId::new(id),
        board: board.clone(),
        owner_kind: if typename(&v["owner"]) == "Organization" {
            OwnerKind::Organization
        } else {
            OwnerKind::User
        },
        title: str_of(&v["title"]),
        url: str_of(&v["url"]),
        viewer_can_update: v["viewerCanUpdate"].as_bool().unwrap_or(false),
        fields: nodes(v, "fields")
            .into_iter()
            .map(field_from_wire)
            .collect(),
        views: nodes(v, "views").into_iter().map(view_from_wire).collect(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A project node shaped like `ProjectSchemaFields`, used by other tests too.
    pub fn project_json() -> serde_json::Value {
        json!({
            "id": "PVT_1", "number": 3, "title": "Testbed", "url": "https://github.com/users/tviles/projects/3",
            "viewerCanUpdate": true,
            "owner": {"__typename": "User", "login": "tviles"},
            "fields": {"nodes": [
                {"__typename": "ProjectV2Field", "id": "F_title", "name": "Title", "dataType": "TITLE"},
                {"__typename": "ProjectV2Field", "id": "F_assignees", "name": "Assignees", "dataType": "ASSIGNEES"},
                {"__typename": "ProjectV2SingleSelectField", "id": "F_status", "name": "Status", "dataType": "SINGLE_SELECT",
                 "options": [{"id": "o_todo", "name": "Todo", "color": "GRAY"}, {"id": "o_done", "name": "Done", "color": "GREEN"}]},
                {"__typename": "ProjectV2MultiSelectField", "id": "F_areas", "name": "Areas", "dataType": "MULTI_SELECT",
                 "multiSelectOptions": [{"id": "m_api", "name": "api", "color": "BLUE"}]},
                {"__typename": "ProjectV2IterationField", "id": "F_sprint", "name": "Sprint", "dataType": "ITERATION",
                 "configuration": {"iterations": [{"id": "it2", "title": "Sprint 2", "startDate": "2026-10-15", "duration": 14}],
                                   "completedIterations": [{"id": "it1", "title": "Sprint 1", "startDate": "2026-10-01", "duration": 14}]}},
                {"__typename": "ProjectV2Field", "id": "F_size", "name": "Size", "dataType": "NUMBER"},
                {"__typename": "ProjectV2BrandNewField"}
            ]},
            "views": {"nodes": [
                {"id": "V1", "number": 1, "name": "View 1", "layout": "TABLE_LAYOUT", "filter": null,
                 "visibleFields": {"nodes": [{"id": "F_title"}, {"id": "F_status"}]},
                 "groupByFields": {"nodes": [{"id": "F_sprint"}]},
                 "verticalGroupByFields": {"nodes": []},
                 "sortByFields": {"nodes": [{"direction": "DESC", "field": {"id": "F_size"}}]}},
                {"id": "V2", "number": 2, "name": "Board", "layout": "BOARD_LAYOUT", "filter": "label:bug",
                 "visibleFields": {"nodes": []}, "groupByFields": {"nodes": []},
                 "verticalGroupByFields": {"nodes": [{"id": "F_status"}]}, "sortByFields": {"nodes": []}}
            ]}
        })
    }

    fn board() -> BoardRef {
        "tviles/3".parse().unwrap()
    }

    #[test]
    fn decodes_project_identity() {
        let p = project_from_wire(&project_json(), &board()).unwrap();
        assert_eq!(p.id.as_str(), "PVT_1");
        assert_eq!(p.title, "Testbed");
        assert_eq!(p.owner_kind, OwnerKind::User);
        assert!(p.viewer_can_update);
    }

    #[test]
    fn decodes_every_field_kind() {
        let p = project_from_wire(&project_json(), &board()).unwrap();
        let kind = |name: &str| {
            p.fields
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .kind
                .clone()
        };
        assert_eq!(kind("Title"), FieldKind::Title);
        assert_eq!(kind("Size"), FieldKind::Number);
        assert!(
            matches!(kind("Status"), FieldKind::SingleSelect { options } if options.len() == 2 && options[1].color == OptionColor::Green)
        );
        assert!(
            matches!(kind("Areas"), FieldKind::MultiSelect { options } if options[0].name == "api")
        );
        assert!(
            matches!(kind("Sprint"), FieldKind::Iteration { iterations, completed } if iterations.len() == 1 && completed.len() == 1)
        );
    }

    #[test]
    fn unknown_field_types_become_unsupported() {
        let p = project_from_wire(&project_json(), &board()).unwrap();
        assert!(p.fields.iter().any(|f| f.kind
            == FieldKind::Unsupported {
                type_name: "ProjectV2BrandNewField".into()
            }));
    }

    #[test]
    fn decodes_views() {
        let p = project_from_wire(&project_json(), &board()).unwrap();
        let v1 = &p.views[0];
        assert_eq!(v1.layout, Layout::Table);
        assert_eq!(v1.filter, "");
        assert_eq!(
            v1.visible_fields,
            vec![FieldId::new("F_title"), FieldId::new("F_status")]
        );
        assert_eq!(v1.group_by, vec![FieldId::new("F_sprint")]);
        assert_eq!(
            v1.sort_by,
            vec![SortSpec {
                field: FieldId::new("F_size"),
                direction: SortDirection::Desc
            }]
        );
        let v2 = &p.views[1];
        assert_eq!(
            (v2.layout, v2.filter.as_str()),
            (Layout::Board, "label:bug")
        );
        assert_eq!(v2.column_field(&p.fields).unwrap().name, "Status");
    }

    #[test]
    fn missing_id_is_a_decode_error() {
        let mut v = project_json();
        v["id"] = serde_json::Value::Null;
        assert!(matches!(
            project_from_wire(&v, &board()),
            Err(GithubError::Decode(_))
        ));
    }
}
