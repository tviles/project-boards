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

fn logins(v: &Value, key: &str) -> Vec<String> {
    nodes(v, key)
        .into_iter()
        .filter_map(|n| {
            n["login"]
                .as_str()
                .or_else(|| n["slug"].as_str())
                .map(String::from)
        })
        .collect()
}

pub fn value_from_wire(v: &Value) -> Option<(FieldId, FieldValue)> {
    let field = FieldId::new(v["field"]["id"].as_str()?);
    let value = match typename(v) {
        "ProjectV2ItemFieldTextValue" => FieldValue::Text(v["text"].as_str()?.to_string()),
        "ProjectV2ItemFieldNumberValue" => FieldValue::Number(v["number"].as_f64()?),
        "ProjectV2ItemFieldDateValue" => FieldValue::Date(v["date"].as_str()?.to_string()),
        "ProjectV2ItemFieldSingleSelectValue" => FieldValue::SingleSelect {
            option_id: OptionId::new(v["optionId"].as_str()?),
            name: str_of(&v["name"]),
        },
        "ProjectV2ItemFieldMultiSelectValue" => {
            let opts = v["options"].as_array()?;
            FieldValue::MultiSelect {
                option_ids: opts
                    .iter()
                    .map(|o| OptionId::new(str_of(&o["id"])))
                    .collect(),
                names: opts.iter().map(|o| str_of(&o["name"])).collect(),
            }
        }
        "ProjectV2ItemFieldIterationValue" => FieldValue::Iteration {
            iteration_id: IterationId::new(v["iterationId"].as_str()?),
            title: str_of(&v["title"]),
            start_date: str_of(&v["startDate"]),
        },
        "ProjectV2ItemFieldLabelValue" => FieldValue::Labels(
            nodes(v, "labels")
                .into_iter()
                .map(|l| Label {
                    name: str_of(&l["name"]),
                    color: str_of(&l["color"]),
                })
                .collect(),
        ),
        "ProjectV2ItemFieldUserValue" => FieldValue::Users(logins(v, "users")),
        "ProjectV2ItemFieldReviewerValue" => FieldValue::Reviewers(logins(v, "reviewers")),
        "ProjectV2ItemFieldMilestoneValue" => {
            FieldValue::Milestone(v["milestone"]["title"].as_str()?.to_string())
        }
        "ProjectV2ItemFieldRepositoryValue" => {
            FieldValue::Repository(v["repository"]["nameWithOwner"].as_str()?.to_string())
        }
        "ProjectV2ItemFieldPullRequestValue" => FieldValue::PullRequests(
            nodes(v, "pullRequests")
                .into_iter()
                .filter_map(|p| p["number"].as_u64().map(|n| n as u32))
                .collect(),
        ),
        _ => return None,
    };
    Some((field, value))
}

fn content_from_wire(c: &Value) -> ItemContent {
    if c.is_null() {
        return ItemContent::Redacted;
    }
    let reference = || ContentRef {
        repo: str_of(&c["repository"]["nameWithOwner"]),
        number: c["number"].as_u64().unwrap_or(0) as u32,
        url: str_of(&c["url"]),
    };
    match typename(c) {
        "Issue" => ItemContent::Issue {
            reference: reference(),
            title: str_of(&c["title"]),
            state: ContentState::from_api(c["state"].as_str().unwrap_or("")),
        },
        "PullRequest" => ItemContent::PullRequest {
            reference: reference(),
            title: str_of(&c["title"]),
            state: ContentState::from_api(c["state"].as_str().unwrap_or("")),
            is_draft: c["isDraft"].as_bool().unwrap_or(false),
        },
        "DraftIssue" => ItemContent::Draft {
            title: str_of(&c["title"]),
        },
        other => ItemContent::Unknown {
            type_name: other.to_string(),
        },
    }
}

/// Decodes an `ItemFields` object. `None` for null or non-item nodes.
pub fn item_from_wire(v: &Value) -> Option<Item> {
    let id = v["id"].as_str()?;
    if v.get("fieldValues").is_none() && v.get("content").is_none() {
        return None;
    }
    Some(Item {
        id: ItemId::new(id),
        content: content_from_wire(&v["content"]),
        archived: v["isArchived"].as_bool().unwrap_or(false),
        updated_at: str_of(&v["updatedAt"]),
        values: nodes(v, "fieldValues")
            .into_iter()
            .filter_map(value_from_wire)
            .collect(),
    })
}

/// Decodes a `ProjectSummaryFields` object.
pub fn summary_from_wire(v: &Value) -> Option<ProjectSummary> {
    let owner = v["owner"]["login"].as_str()?;
    let number = v["number"].as_u64()? as u32;
    Some(ProjectSummary {
        id: ProjectId::new(v["id"].as_str()?),
        board: BoardRef {
            owner: owner.to_string(),
            number,
        },
        title: str_of(&v["title"]),
        closed: v["closed"].as_bool().unwrap_or(false),
    })
}

fn linked(v: &Value) -> Option<LinkedRef> {
    Some(LinkedRef {
        number: v["number"].as_u64()? as u32,
        title: str_of(&v["title"]),
        state: str_of(&v["state"]),
    })
}

/// Decodes the `node` of an `ItemDetail` response. Deleted authors show as "ghost", as on GitHub.
pub fn detail_from_wire(node: &Value) -> ItemDetail {
    let c = &node["content"];
    let comments = &c["comments"];
    let has_older = comments["pageInfo"]["hasPreviousPage"].as_bool() == Some(true);
    let prs_key = if typename(c) == "PullRequest" {
        "closingIssuesReferences"
    } else {
        "closedByPullRequestsReferences"
    };
    ItemDetail {
        body: str_of(&c["body"]),
        comments: nodes(c, "comments")
            .into_iter()
            .map(|n| Comment {
                author: n["author"]["login"].as_str().unwrap_or("ghost").to_string(),
                created_at: str_of(&n["createdAt"]),
                body: str_of(&n["body"]),
            })
            .collect(),
        comments_total: comments["totalCount"].as_u64().unwrap_or(0) as usize,
        older_cursor: if has_older {
            comments["pageInfo"]["startCursor"]
                .as_str()
                .map(String::from)
        } else {
            None
        },
        sub_issues: nodes(c, "subIssues")
            .into_iter()
            .filter_map(linked)
            .collect(),
        parent: linked(&c["parent"]),
        issue_type: c["issueType"]["name"].as_str().map(String::from),
        linked_prs: nodes(c, prs_key).into_iter().filter_map(linked).collect(),
    }
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

    pub fn item_json() -> serde_json::Value {
        json!({
            "id": "PVTI_1", "type": "ISSUE", "isArchived": false, "updatedAt": "2026-10-01T10:00:00Z",
            "content": {"__typename": "Issue", "number": 12, "title": "Crash 🚀", "url": "https://github.com/tviles/t/issues/12",
                        "state": "OPEN", "repository": {"nameWithOwner": "tviles/t"}},
            "fieldValues": {"nodes": [
                {"__typename": "ProjectV2ItemFieldTextValue", "text": "Crash 🚀", "field": {"id": "F_title"}},
                {"__typename": "ProjectV2ItemFieldSingleSelectValue", "optionId": "o_todo", "name": "Todo", "field": {"id": "F_status"}},
                {"__typename": "ProjectV2ItemFieldMultiSelectValue", "options": [{"id": "m_api", "name": "api"}], "field": {"id": "F_areas"}},
                {"__typename": "ProjectV2ItemFieldIterationValue", "iterationId": "it2", "title": "Sprint 2", "startDate": "2026-10-15", "field": {"id": "F_sprint"}},
                {"__typename": "ProjectV2ItemFieldNumberValue", "number": 3.0, "field": {"id": "F_size"}},
                {"__typename": "ProjectV2ItemFieldUserValue", "users": {"nodes": [{"login": "tviles"}]}, "field": {"id": "F_assignees"}},
                {"__typename": "ProjectV2ItemFieldLabelValue", "labels": {"nodes": [{"name": "bug", "color": "d73a4a"}]}, "field": {"id": "F_labels"}},
                {"__typename": "ProjectV2ItemIssueFieldValue"},
                {"__typename": "ProjectV2ItemFieldFromTheFuture", "field": {"id": "F_future"}}
            ]}
        })
    }

    #[test]
    fn decodes_issue_items_and_their_values() {
        let item = item_from_wire(&item_json()).unwrap();
        assert_eq!(item.title(), "Crash 🚀");
        assert_eq!(item.number(), Some(12));
        assert_eq!(item.assignees(), ["tviles"]);
        assert_eq!(item.label_names(), ["bug"]);
        assert_eq!(
            item.value(&FieldId::new("F_status")),
            Some(&FieldValue::SingleSelect {
                option_id: OptionId::new("o_todo"),
                name: "Todo".into()
            })
        );
        assert_eq!(
            item.value(&FieldId::new("F_size")),
            Some(&FieldValue::Number(3.0))
        );
        assert!(
            matches!(item.value(&FieldId::new("F_sprint")), Some(FieldValue::Iteration { title, .. }) if title == "Sprint 2")
        );
    }

    #[test]
    fn values_of_unknown_types_are_dropped() {
        let item = item_from_wire(&item_json()).unwrap();
        assert!(item.value(&FieldId::new("F_future")).is_none());
        assert_eq!(item.values.len(), 7);
    }

    #[test]
    fn null_content_is_redacted_and_unknown_content_is_kept() {
        let mut v = item_json();
        v["content"] = serde_json::Value::Null;
        assert_eq!(item_from_wire(&v).unwrap().content, ItemContent::Redacted);
        v["content"] = json!({"__typename": "Discussion"});
        assert_eq!(
            item_from_wire(&v).unwrap().content,
            ItemContent::Unknown {
                type_name: "Discussion".into()
            }
        );
    }

    #[test]
    fn drafts_and_pull_requests_decode() {
        let mut v = item_json();
        v["content"] = json!({"__typename": "DraftIssue", "title": "Idea"});
        assert_eq!(
            item_from_wire(&v).unwrap().content,
            ItemContent::Draft {
                title: "Idea".into()
            }
        );
        v["content"] = json!({"__typename": "PullRequest", "number": 4, "title": "PR", "url": "u", "state": "MERGED", "isDraft": false,
                              "repository": {"nameWithOwner": "tviles/t"}});
        assert!(matches!(
            item_from_wire(&v).unwrap().content,
            ItemContent::PullRequest {
                state: ContentState::Merged,
                ..
            }
        ));
    }

    #[test]
    fn non_item_nodes_are_skipped() {
        assert!(item_from_wire(&serde_json::Value::Null).is_none());
        assert!(item_from_wire(&json!({"__typename": "Issue"})).is_none());
    }

    #[test]
    fn extra_typename_keys_on_value_fields_are_ignored() {
        let v = json!({"__typename": "ProjectV2ItemFieldTextValue", "text": "x",
                       "field": {"__typename": "ProjectV2Field", "id": "F_title"}});
        assert_eq!(
            value_from_wire(&v),
            Some((FieldId::new("F_title"), FieldValue::Text("x".into())))
        );
    }

    #[test]
    fn decodes_issue_detail_with_comments_oldest_first() {
        let v = json!({"node": {"content": {"__typename": "Issue", "body": "Body **md**",
            "issueType": {"name": "Bug"}, "parent": {"number": 3, "title": "Epic", "state": "OPEN"},
            "subIssues": {"nodes": [{"number": 4, "title": "Child", "state": "CLOSED"}]},
            "closedByPullRequestsReferences": {"nodes": [{"number": 9, "title": "Fix", "state": "MERGED"}]},
            "comments": {"totalCount": 30, "pageInfo": {"hasPreviousPage": true, "startCursor": "cur"},
                         "nodes": [{"author": {"login": "a"}, "createdAt": "2026-10-01T00:00:00Z", "body": "first"},
                                   {"author": null, "createdAt": "2026-10-02T00:00:00Z", "body": "second"}]}}}});
        let d = detail_from_wire(&v["node"]);
        assert_eq!(d.body, "Body **md**");
        assert_eq!(d.issue_type.as_deref(), Some("Bug"));
        assert_eq!(d.parent.unwrap().number, 3);
        assert_eq!(d.sub_issues[0].state, "CLOSED");
        assert_eq!(d.linked_prs[0].number, 9);
        assert_eq!(
            (
                d.comments.len(),
                d.comments_total,
                d.older_cursor.as_deref()
            ),
            (2, 30, Some("cur"))
        );
        assert_eq!(d.comments[1].author, "ghost");
    }

    #[test]
    fn draft_detail_has_only_a_body() {
        let d = detail_from_wire(&json!({"content": {"__typename": "DraftIssue", "body": "idea"}}));
        assert_eq!(d.body, "idea");
        assert!(d.comments.is_empty() && d.older_cursor.is_none());
    }
}
