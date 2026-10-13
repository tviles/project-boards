//! Every mutation project-boards sends, as `Github` methods returning model types.

use crate::github::Github;
use crate::github::GithubError;
use crate::github::GraphqlErrorEntry;
use crate::github::convert;
use crate::github::queries;
use crate::github::transport::{GraphqlRequest, GraphqlResponse};
use crate::model::*;
use graphql_client::GraphQLQuery;
use serde_json::Value;

/// The payload object of mutation `name` (`data[name]`). GitHub answers a refused mutation
/// with a null payload and, usually, a FORBIDDEN or NOT_FOUND error that the transport
/// tolerated; that error is the one reported, so the user sees GitHub's own words.
pub(crate) fn payload<'a>(
    response: &'a GraphqlResponse,
    name: &str,
) -> Result<&'a Value, GithubError> {
    let p = &response.data[name];
    if p.is_object() {
        return Ok(p);
    }
    if !response.errors.is_empty() {
        return Err(GithubError::Graphql(response.errors.clone()));
    }
    Err(GithubError::Decode(format!(
        "GitHub returned no result for {name}"
    )))
}

/// The item under `data[name][key]`.
pub(crate) fn item_payload(
    response: &GraphqlResponse,
    name: &str,
    key: &str,
) -> Result<Item, GithubError> {
    let p = payload(response, name)?;
    convert::item_from_wire(&p[key])
        .ok_or_else(|| GithubError::Decode(format!("{name} returned no item")))
}

impl Github {
    /// Sets `field` on `item` to `value` (text, number, date, single-select, multi-select or
    /// iteration) and returns the item as GitHub now has it.
    pub async fn set_field_value(
        &self,
        project: &ProjectId,
        item: &ItemId,
        field: &FieldId,
        value: &FieldValue,
    ) -> Result<Item, GithubError> {
        let (project, item, field) = (project.0.clone(), item.0.clone(), field.0.clone());
        let request = match value {
            FieldValue::Text(text) => GraphqlRequest::from_body(
                queries::SetTextValue::build_query(queries::set_text_value::Variables {
                    project,
                    item,
                    field,
                    text: text.clone(),
                }),
            ),
            FieldValue::Number(number) => GraphqlRequest::from_body(
                queries::SetNumberValue::build_query(queries::set_number_value::Variables {
                    project,
                    item,
                    field,
                    number: *number,
                }),
            ),
            FieldValue::Date(date) => GraphqlRequest::from_body(
                queries::SetDateValue::build_query(queries::set_date_value::Variables {
                    project,
                    item,
                    field,
                    date: date.clone(),
                }),
            ),
            FieldValue::SingleSelect { option_id, .. } => {
                GraphqlRequest::from_body(queries::SetSingleSelectValue::build_query(
                    queries::set_single_select_value::Variables {
                        project,
                        item,
                        field,
                        option: option_id.0.clone(),
                    },
                ))
            }
            FieldValue::MultiSelect { option_ids, .. } => {
                GraphqlRequest::from_body(queries::SetMultiSelectValue::build_query(
                    queries::set_multi_select_value::Variables {
                        project,
                        item,
                        field,
                        options: option_ids.iter().map(|o| o.0.clone()).collect(),
                    },
                ))
            }
            FieldValue::Iteration { iteration_id, .. } => GraphqlRequest::from_body(
                queries::SetIterationValue::build_query(queries::set_iteration_value::Variables {
                    project,
                    item,
                    field,
                    iteration: iteration_id.0.clone(),
                }),
            ),
            other => {
                return Err(GithubError::Decode(format!(
                    "`{}` is not a project field value",
                    other.display()
                )));
            }
        };
        let response = self.run(request).await?;
        item_payload(&response, "updateProjectV2ItemFieldValue", "projectV2Item")
    }

    pub async fn clear_field_value(
        &self,
        project: &ProjectId,
        item: &ItemId,
        field: &FieldId,
    ) -> Result<Item, GithubError> {
        let body = queries::ClearFieldValue::build_query(queries::clear_field_value::Variables {
            project: project.0.clone(),
            item: item.0.clone(),
            field: field.0.clone(),
        });
        let response = self.run(GraphqlRequest::from_body(body)).await?;
        item_payload(&response, "clearProjectV2ItemFieldValue", "projectV2Item")
    }

    /// Adds a draft and returns its item.
    pub async fn add_draft_issue(
        &self,
        project: &ProjectId,
        title: &str,
        body: &str,
    ) -> Result<Item, GithubError> {
        let request = GraphqlRequest::from_body(queries::AddDraftIssue::build_query(
            queries::add_draft_issue::Variables {
                project: project.0.clone(),
                title: title.to_string(),
                body: body.to_string(),
            },
        ));
        let response = self.run(request).await?;
        item_payload(&response, "addProjectV2DraftIssue", "projectItem")
    }

    /// Removes the item from the project. A draft is deleted with it; an issue or pull
    /// request stays in its repository.
    pub async fn delete_item(&self, project: &ProjectId, item: &ItemId) -> Result<(), GithubError> {
        let request = GraphqlRequest::from_body(queries::DeleteItem::build_query(
            queries::delete_item::Variables {
                project: project.0.clone(),
                item: item.0.clone(),
            },
        ));
        let response = self.run(request).await?;
        payload(&response, "deleteProjectV2Item").map(|_| ())
    }

    /// Puts `item` right after `after` in the project's order; `None` puts it first.
    /// Returns the first 100 item ids in the order GitHub now has.
    pub async fn update_item_position(
        &self,
        project: &ProjectId,
        item: &ItemId,
        after: Option<&ItemId>,
    ) -> Result<Vec<ItemId>, GithubError> {
        let body =
            queries::UpdateItemPosition::build_query(queries::update_item_position::Variables {
                project: project.0.clone(),
                item: item.0.clone(),
                after: after.map(|a| a.0.clone()),
            });
        let response = self.run(GraphqlRequest::from_body(body)).await?;
        let p = payload(&response, "updateProjectV2ItemPosition")?;
        Ok(convert::nodes(p, "items")
            .into_iter()
            .filter_map(|n| n["id"].as_str().map(ItemId::new))
            .collect())
    }
}

/// An operation the item type cannot take, refused before any request is sent.
fn refused(message: &str) -> GithubError {
    GithubError::Graphql(vec![GraphqlErrorEntry {
        message: message.into(),
        kind: None,
    }])
}

impl Github {
    /// Runs a mutation whose payload carries nothing the board needs.
    async fn mutate_ok<V: serde::Serialize>(
        &self,
        body: graphql_client::QueryBody<V>,
        name: &str,
    ) -> Result<(), GithubError> {
        let response = self.run(GraphqlRequest::from_body(body)).await?;
        payload(&response, name).map(|_| ())
    }

    /// Adding nobody sends nothing.
    pub async fn add_assignees(&self, content: &str, users: &[String]) -> Result<(), GithubError> {
        if users.is_empty() {
            return Ok(());
        }
        let vars = queries::add_assignees::Variables {
            id: content.into(),
            users: users.to_vec(),
        };
        self.mutate_ok(
            queries::AddAssignees::build_query(vars),
            "addAssigneesToAssignable",
        )
        .await
    }

    pub async fn remove_assignees(
        &self,
        content: &str,
        users: &[String],
    ) -> Result<(), GithubError> {
        if users.is_empty() {
            return Ok(());
        }
        let vars = queries::remove_assignees::Variables {
            id: content.into(),
            users: users.to_vec(),
        };
        self.mutate_ok(
            queries::RemoveAssignees::build_query(vars),
            "removeAssigneesFromAssignable",
        )
        .await
    }

    /// Replaces a draft's assignees (drafts have no add/remove mutation); an empty list
    /// clears them all.
    pub async fn set_draft_assignees(
        &self,
        draft: &str,
        users: &[String],
    ) -> Result<(), GithubError> {
        let vars = queries::set_draft_assignees::Variables {
            id: draft.into(),
            users: users.to_vec(),
        };
        self.mutate_ok(
            queries::SetDraftAssignees::build_query(vars),
            "updateProjectV2DraftIssue",
        )
        .await
    }

    pub async fn add_labels(&self, content: &str, labels: &[String]) -> Result<(), GithubError> {
        if labels.is_empty() {
            return Ok(());
        }
        let vars = queries::add_labels::Variables {
            id: content.into(),
            labels: labels.to_vec(),
        };
        self.mutate_ok(
            queries::AddLabels::build_query(vars),
            "addLabelsToLabelable",
        )
        .await
    }

    pub async fn remove_labels(&self, content: &str, labels: &[String]) -> Result<(), GithubError> {
        if labels.is_empty() {
            return Ok(());
        }
        let vars = queries::remove_labels::Variables {
            id: content.into(),
            labels: labels.to_vec(),
        };
        self.mutate_ok(
            queries::RemoveLabels::build_query(vars),
            "removeLabelsFromLabelable",
        )
        .await
    }

    pub async fn set_title(&self, content: &ContentId, title: &str) -> Result<(), GithubError> {
        let (id, title) = (content.node_id().to_string(), title.to_string());
        match content {
            ContentId::Issue(_) => {
                self.mutate_ok(
                    queries::UpdateIssueTitle::build_query(
                        queries::update_issue_title::Variables { id, title },
                    ),
                    "updateIssue",
                )
                .await
            }
            ContentId::PullRequest(_) => {
                self.mutate_ok(
                    queries::UpdatePullRequestTitle::build_query(
                        queries::update_pull_request_title::Variables { id, title },
                    ),
                    "updatePullRequest",
                )
                .await
            }
            ContentId::Draft(_) => {
                self.mutate_ok(
                    queries::UpdateDraftTitle::build_query(
                        queries::update_draft_title::Variables { id, title },
                    ),
                    "updateProjectV2DraftIssue",
                )
                .await
            }
        }
    }

    pub async fn set_body(&self, content: &ContentId, body: &str) -> Result<(), GithubError> {
        let (id, body) = (content.node_id().to_string(), body.to_string());
        match content {
            ContentId::Issue(_) => {
                self.mutate_ok(
                    queries::UpdateIssueBody::build_query(queries::update_issue_body::Variables {
                        id,
                        body,
                    }),
                    "updateIssue",
                )
                .await
            }
            ContentId::PullRequest(_) => {
                self.mutate_ok(
                    queries::UpdatePullRequestBody::build_query(
                        queries::update_pull_request_body::Variables { id, body },
                    ),
                    "updatePullRequest",
                )
                .await
            }
            ContentId::Draft(_) => {
                self.mutate_ok(
                    queries::UpdateDraftBody::build_query(queries::update_draft_body::Variables {
                        id,
                        body,
                    }),
                    "updateProjectV2DraftIssue",
                )
                .await
            }
        }
    }

    /// Sets (`Some(milestone id)`) or clears (`None`) the milestone. Drafts have none.
    pub async fn set_milestone(
        &self,
        content: &ContentId,
        milestone: Option<&str>,
    ) -> Result<(), GithubError> {
        let (id, milestone) = (content.node_id().to_string(), milestone.map(String::from));
        match content {
            ContentId::Issue(_) => {
                self.mutate_ok(
                    queries::SetIssueMilestone::build_query(
                        queries::set_issue_milestone::Variables { id, milestone },
                    ),
                    "updateIssue",
                )
                .await
            }
            ContentId::PullRequest(_) => {
                self.mutate_ok(
                    queries::SetPullRequestMilestone::build_query(
                        queries::set_pull_request_milestone::Variables { id, milestone },
                    ),
                    "updatePullRequest",
                )
                .await
            }
            ContentId::Draft(_) => Err(refused("Drafts have no milestone")),
        }
    }

    pub async fn add_comment(&self, subject: &str, body: &str) -> Result<(), GithubError> {
        let vars = queries::add_comment::Variables {
            subject: subject.into(),
            body: body.into(),
        };
        self.mutate_ok(queries::AddComment::build_query(vars), "addComment")
            .await
    }

    /// Closes an issue (as completed unless `reason` says otherwise) or a pull request.
    pub async fn close(
        &self,
        content: &ContentId,
        reason: Option<CloseReason>,
    ) -> Result<(), GithubError> {
        use queries::close_issue::IssueClosedStateReason as Reason;
        let id = content.node_id().to_string();
        match content {
            ContentId::Issue(_) => {
                let reason = match reason {
                    Some(CloseReason::NotPlanned) => Reason::NOT_PLANNED,
                    Some(CloseReason::Completed) | None => Reason::COMPLETED,
                };
                self.mutate_ok(
                    queries::CloseIssue::build_query(queries::close_issue::Variables {
                        id,
                        reason,
                    }),
                    "closeIssue",
                )
                .await
            }
            ContentId::PullRequest(_) => {
                self.mutate_ok(
                    queries::ClosePullRequest::build_query(
                        queries::close_pull_request::Variables { id },
                    ),
                    "closePullRequest",
                )
                .await
            }
            ContentId::Draft(_) => Err(refused("Drafts cannot be closed")),
        }
    }

    pub async fn reopen(&self, content: &ContentId) -> Result<(), GithubError> {
        let id = content.node_id().to_string();
        match content {
            ContentId::Issue(_) => {
                self.mutate_ok(
                    queries::ReopenIssue::build_query(queries::reopen_issue::Variables { id }),
                    "reopenIssue",
                )
                .await
            }
            ContentId::PullRequest(_) => {
                self.mutate_ok(
                    queries::ReopenPullRequest::build_query(
                        queries::reopen_pull_request::Variables { id },
                    ),
                    "reopenPullRequest",
                )
                .await
            }
            ContentId::Draft(_) => Err(refused("Drafts cannot be reopened")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::FixtureTransport;
    use crate::github::transport::GraphqlRequest;
    use crate::github::{Github, GithubError};
    use serde_json::json;
    use std::sync::Arc;

    async fn response(t: &FixtureTransport) -> GraphqlResponse {
        use crate::github::transport::Transport;
        t.execute(GraphqlRequest::raw("Op", "mutation Op { x }", json!({})))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_payload_object_is_returned() {
        let t = FixtureTransport::new();
        t.push("Op", json!({"addComment": {"clientMutationId": null}}));
        let r = response(&t).await;
        assert!(payload(&r, "addComment").unwrap().is_object());
    }

    #[tokio::test]
    async fn a_null_payload_reports_githubs_own_message() {
        let t = FixtureTransport::new();
        t.push_body(
            "Op",
            json!({"data": {"addComment": null},
                   "errors": [{"type": "FORBIDDEN", "message": "Resource not accessible by integration"}]}),
        );
        let r = response(&t).await;
        let err = payload(&r, "addComment").unwrap_err();
        assert!(
            matches!(&err, GithubError::Graphql(e) if e[0].message == "Resource not accessible by integration")
        );
    }

    #[tokio::test]
    async fn a_null_payload_without_errors_is_a_decode_error() {
        let t = FixtureTransport::new();
        t.push("Op", json!({"addComment": null}));
        let r = response(&t).await;
        assert!(
            matches!(payload(&r, "addComment"), Err(GithubError::Decode(m)) if m.contains("addComment"))
        );
    }

    #[tokio::test]
    async fn the_latest_reported_scopes_are_remembered() {
        let t = Arc::new(FixtureTransport::new());
        let gh = Github::new(t.clone());
        assert_eq!(gh.scopes(), None);
        t.set_scopes(Some(vec!["project".into(), "repo".into()]));
        t.push("Viewer", json!({"viewer": {"login": "octo"}}));
        gh.viewer().await.unwrap();
        assert_eq!(
            gh.scopes(),
            Some(vec!["project".to_string(), "repo".to_string()])
        );
        // A response without the header (fine-grained token, fixture) keeps what was known.
        t.set_scopes(None);
        t.push("Viewer", json!({"viewer": {"login": "octo"}}));
        gh.viewer().await.unwrap();
        assert_eq!(gh.scopes().map(|s| s.len()), Some(2));
    }

    use crate::github::convert::tests::item_json;

    fn returns_item(t: &FixtureTransport, op: &str, mutation: &str) {
        let mut data = serde_json::Map::new();
        data.insert(mutation.into(), json!({"projectV2Item": item_json()}));
        t.push(op, serde_json::Value::Object(data));
    }

    fn ids() -> (ProjectId, ItemId, FieldId) {
        (
            ProjectId::new("PVT_1"),
            ItemId::new("PVTI_1"),
            FieldId::new("F_status"),
        )
    }

    #[tokio::test]
    async fn setting_text_sends_its_variables_and_returns_the_item() {
        let t = Arc::new(FixtureTransport::new());
        returns_item(&t, "SetTextValue", "updateProjectV2ItemFieldValue");
        let (p, i, f) = ids();
        let item = Github::new(t.clone())
            .set_field_value(&p, &i, &f, &FieldValue::Text("Needs a repro".into()))
            .await
            .unwrap();
        assert_eq!(item.id.as_str(), "PVTI_1");
        let v = &t.requests()[0].variables;
        assert_eq!(
            (
                v["project"].as_str(),
                v["item"].as_str(),
                v["field"].as_str(),
                v["text"].as_str()
            ),
            (
                Some("PVT_1"),
                Some("PVTI_1"),
                Some("F_status"),
                Some("Needs a repro")
            )
        );
    }

    #[tokio::test]
    async fn each_value_type_has_its_own_operation() {
        let cases = [
            (
                "SetTextValue",
                FieldValue::Text("x".into()),
                "text",
                json!("x"),
            ),
            (
                "SetNumberValue",
                FieldValue::Number(2.5),
                "number",
                json!(2.5),
            ),
            (
                "SetDateValue",
                FieldValue::Date("2026-10-03".into()),
                "date",
                json!("2026-10-03"),
            ),
            (
                "SetSingleSelectValue",
                FieldValue::SingleSelect {
                    option_id: OptionId::new("o_done"),
                    name: "Done".into(),
                },
                "option",
                json!("o_done"),
            ),
            (
                "SetMultiSelectValue",
                FieldValue::MultiSelect {
                    option_ids: vec![OptionId::new("m1"), OptionId::new("m2")],
                    names: vec![],
                },
                "options",
                json!(["m1", "m2"]),
            ),
            (
                "SetIterationValue",
                FieldValue::Iteration {
                    iteration_id: IterationId::new("it2"),
                    title: "Sprint 2".into(),
                    start_date: "2026-10-15".into(),
                },
                "iteration",
                json!("it2"),
            ),
        ];
        for (op, value, var, expected) in cases {
            let t = Arc::new(FixtureTransport::new());
            returns_item(&t, op, "updateProjectV2ItemFieldValue");
            let (p, i, f) = ids();
            Github::new(t.clone())
                .set_field_value(&p, &i, &f, &value)
                .await
                .unwrap();
            let req = &t.requests()[0];
            assert_eq!(req.operation_name, op);
            assert_eq!(req.variables[var], expected, "{op}");
        }
    }

    #[tokio::test]
    async fn values_that_are_not_project_fields_are_refused_without_a_request() {
        let t = Arc::new(FixtureTransport::new());
        let (p, i, f) = ids();
        let r = Github::new(t.clone())
            .set_field_value(&p, &i, &f, &FieldValue::Users(vec![]))
            .await;
        assert!(matches!(r, Err(GithubError::Decode(_))));
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn clearing_returns_the_item() {
        let t = Arc::new(FixtureTransport::new());
        returns_item(&t, "ClearFieldValue", "clearProjectV2ItemFieldValue");
        let (p, i, f) = ids();
        let item = Github::new(t.clone())
            .clear_field_value(&p, &i, &f)
            .await
            .unwrap();
        assert_eq!(item.id.as_str(), "PVTI_1");
        assert_eq!(t.requests()[0].variables["field"], "F_status");
    }

    #[tokio::test]
    async fn a_position_at_the_top_sends_a_null_after_and_returns_the_new_order() {
        let t = Arc::new(FixtureTransport::new());
        t.push("UpdateItemPosition", json!({"updateProjectV2ItemPosition": {"items": {"nodes": [{"id": "PVTI_1"}, {"id": "PVTI_0"}]}}}));
        t.push("UpdateItemPosition", json!({"updateProjectV2ItemPosition": {"items": {"nodes": [{"id": "PVTI_0"}, {"id": "PVTI_1"}]}}}));
        let gh = Github::new(t.clone());
        let (p, i, _) = ids();
        let order = gh.update_item_position(&p, &i, None).await.unwrap();
        assert_eq!(order, [ItemId::new("PVTI_1"), ItemId::new("PVTI_0")]);
        gh.update_item_position(&p, &i, Some(&ItemId::new("PVTI_0")))
            .await
            .unwrap();
        assert!(t.requests()[0].variables["after"].is_null());
        assert_eq!(t.requests()[1].variables["after"], "PVTI_0");
    }

    #[tokio::test]
    async fn a_refused_field_edit_reports_githubs_message() {
        let t = Arc::new(FixtureTransport::new());
        t.push_body(
            "SetTextValue",
            json!({"data": {"updateProjectV2ItemFieldValue": null},
                   "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node with the global id of 'F_gone'"}]}),
        );
        let (p, i, f) = ids();
        let err = Github::new(t)
            .set_field_value(&p, &i, &f, &FieldValue::Text("x".into()))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Could not resolve to a node"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_draft_is_added_and_an_item_deleted() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "AddDraftIssue",
            json!({"addProjectV2DraftIssue": {"projectItem": item_json()}}),
        );
        t.push(
            "DeleteItem",
            json!({"deleteProjectV2Item": {"deletedItemId": "PVTI_1"}}),
        );
        let gh = Github::new(t.clone());
        let item = gh
            .add_draft_issue(&ProjectId::new("PVT_1"), "Idea", "Body")
            .await
            .unwrap();
        assert_eq!(item.id.as_str(), "PVTI_1");
        gh.delete_item(&ProjectId::new("PVT_1"), &item.id)
            .await
            .unwrap();
        let r = t.requests();
        assert_eq!(
            (
                r[0].variables["title"].as_str(),
                r[0].variables["body"].as_str()
            ),
            (Some("Idea"), Some("Body"))
        );
        assert_eq!(r[1].variables["item"], "PVTI_1");
    }

    fn ok_payload(t: &FixtureTransport, op: &str, mutation: &str) {
        let mut data = serde_json::Map::new();
        data.insert(mutation.into(), json!({"clientMutationId": null}));
        t.push(op, serde_json::Value::Object(data));
    }

    #[tokio::test]
    async fn assignee_and_label_changes_send_node_ids() {
        let t = Arc::new(FixtureTransport::new());
        ok_payload(&t, "AddAssignees", "addAssigneesToAssignable");
        ok_payload(&t, "RemoveAssignees", "removeAssigneesFromAssignable");
        ok_payload(&t, "SetDraftAssignees", "updateProjectV2DraftIssue");
        ok_payload(&t, "AddLabels", "addLabelsToLabelable");
        ok_payload(&t, "RemoveLabels", "removeLabelsFromLabelable");
        let gh = Github::new(t.clone());
        gh.add_assignees("I_1", &["U_1".into()]).await.unwrap();
        gh.remove_assignees("I_1", &["U_2".into()]).await.unwrap();
        gh.set_draft_assignees("DI_1", &["U_1".into(), "U_3".into()])
            .await
            .unwrap();
        gh.add_labels("I_1", &["LA_1".into()]).await.unwrap();
        gh.remove_labels("I_1", &["LA_2".into()]).await.unwrap();
        let r = t.requests();
        assert_eq!(
            (r[0].variables["id"].as_str(), &r[0].variables["users"]),
            (Some("I_1"), &json!(["U_1"]))
        );
        assert_eq!(r[2].variables["users"], json!(["U_1", "U_3"]));
        assert_eq!(r[4].variables["labels"], json!(["LA_2"]));
    }

    #[tokio::test]
    async fn title_body_and_milestone_go_to_the_right_mutation_per_content_type() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "UpdateIssueTitle",
            json!({"updateIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "UpdatePullRequestBody",
            json!({"updatePullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        t.push(
            "UpdateDraftTitle",
            json!({"updateProjectV2DraftIssue": {"draftIssue": {"id": "DI_1"}}}),
        );
        t.push(
            "SetIssueMilestone",
            json!({"updateIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "SetPullRequestMilestone",
            json!({"updatePullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        let gh = Github::new(t.clone());
        gh.set_title(&ContentId::Issue("I_1".into()), "New")
            .await
            .unwrap();
        gh.set_body(&ContentId::PullRequest("PR_1".into()), "Body")
            .await
            .unwrap();
        gh.set_title(&ContentId::Draft("DI_1".into()), "Idea")
            .await
            .unwrap();
        gh.set_milestone(&ContentId::Issue("I_1".into()), None)
            .await
            .unwrap();
        gh.set_milestone(&ContentId::PullRequest("PR_1".into()), Some("MI_1"))
            .await
            .unwrap();
        let r = t.requests();
        let names: Vec<&str> = r.iter().map(|q| q.operation_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "UpdateIssueTitle",
                "UpdatePullRequestBody",
                "UpdateDraftTitle",
                "SetIssueMilestone",
                "SetPullRequestMilestone"
            ]
        );
        assert!(
            r[3].variables["milestone"].is_null(),
            "null clears the milestone"
        );
        assert_eq!(r[4].variables["milestone"], "MI_1");
        let refused = gh
            .set_milestone(&ContentId::Draft("DI_1".into()), Some("MI_1"))
            .await;
        assert!(refused.is_err());
        assert_eq!(t.requests().len(), 5, "no request for a draft milestone");
    }

    #[tokio::test]
    async fn comments_close_and_reopen() {
        let t = Arc::new(FixtureTransport::new());
        ok_payload(&t, "AddComment", "addComment");
        t.push(
            "CloseIssue",
            json!({"closeIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "CloseIssue",
            json!({"closeIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "ReopenIssue",
            json!({"reopenIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "ClosePullRequest",
            json!({"closePullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        t.push(
            "ReopenPullRequest",
            json!({"reopenPullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        let gh = Github::new(t.clone());
        let issue = ContentId::Issue("I_1".into());
        let pr = ContentId::PullRequest("PR_1".into());
        gh.add_comment("I_1", "LGTM").await.unwrap();
        gh.close(&issue, Some(CloseReason::NotPlanned))
            .await
            .unwrap();
        gh.close(&issue, None).await.unwrap();
        gh.reopen(&issue).await.unwrap();
        gh.close(&pr, None).await.unwrap();
        gh.reopen(&pr).await.unwrap();
        let r = t.requests();
        assert_eq!(
            (
                r[0].variables["subject"].as_str(),
                r[0].variables["body"].as_str()
            ),
            (Some("I_1"), Some("LGTM"))
        );
        assert_eq!(r[1].variables["reason"], "NOT_PLANNED");
        assert_eq!(
            r[2].variables["reason"], "COMPLETED",
            "issues default to completed"
        );
        assert_eq!(r[4].operation_name, "ClosePullRequest");
        assert!(
            gh.close(&ContentId::Draft("DI_1".into()), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn empty_lists_send_nothing_except_clearing_draft_assignees() {
        let t = Arc::new(FixtureTransport::new());
        ok_payload(&t, "SetDraftAssignees", "updateProjectV2DraftIssue");
        let gh = Github::new(t.clone());
        gh.add_assignees("I_1", &[]).await.unwrap();
        gh.remove_assignees("I_1", &[]).await.unwrap();
        gh.add_labels("I_1", &[]).await.unwrap();
        gh.remove_labels("I_1", &[]).await.unwrap();
        assert!(t.requests().is_empty());
        gh.set_draft_assignees("DI_1", &[]).await.unwrap();
        assert_eq!(t.requests()[0].variables["users"], json!([]));
    }

    #[tokio::test]
    async fn a_refusal_surfaces_githubs_message() {
        let t = Arc::new(FixtureTransport::new());
        for op in ["AddLabels", "CloseIssue"] {
            t.push_body(
                op,
                json!({"data": {"addLabelsToLabelable": null, "closeIssue": null},
                       "errors": [{"type": "FORBIDDEN", "message": "Resource not accessible"}]}),
            );
        }
        let gh = Github::new(t.clone());
        let e = gh.add_labels("I_1", &["LA_1".into()]).await.unwrap_err();
        assert!(matches!(&e, GithubError::Graphql(m) if m[0].message == "Resource not accessible"));
        let e = gh
            .close(&ContentId::Issue("I_1".into()), None)
            .await
            .unwrap_err();
        assert!(matches!(&e, GithubError::Graphql(m) if m[0].message == "Resource not accessible"));
    }

    #[tokio::test]
    async fn drafts_are_refused_and_remaining_operations_are_named() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "UpdateDraftBody",
            json!({"updateProjectV2DraftIssue": {"draftIssue": {"id": "DI_1"}}}),
        );
        t.push(
            "UpdateIssueBody",
            json!({"updateIssue": {"issue": {"id": "I_1"}}}),
        );
        t.push(
            "UpdatePullRequestTitle",
            json!({"updatePullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        t.push(
            "ReopenPullRequest",
            json!({"reopenPullRequest": {"pullRequest": {"id": "PR_1"}}}),
        );
        let gh = Github::new(t.clone());
        gh.set_body(&ContentId::Draft("DI_1".into()), "b")
            .await
            .unwrap();
        gh.set_body(&ContentId::Issue("I_1".into()), "b")
            .await
            .unwrap();
        gh.set_title(&ContentId::PullRequest("PR_1".into()), "t")
            .await
            .unwrap();
        gh.reopen(&ContentId::PullRequest("PR_1".into()))
            .await
            .unwrap();
        let names: Vec<String> = t
            .requests()
            .iter()
            .map(|q| q.operation_name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "UpdateDraftBody",
                "UpdateIssueBody",
                "UpdatePullRequestTitle",
                "ReopenPullRequest"
            ]
        );
        let draft = ContentId::Draft("DI_1".into());
        let e = gh.reopen(&draft).await.unwrap_err();
        assert!(
            matches!(&e, GithubError::Graphql(m) if m[0].message == "Drafts cannot be reopened")
        );
        assert!(gh.close(&draft, None).await.is_err());
        assert!(gh.set_milestone(&draft, None).await.is_err());
        assert_eq!(
            t.requests().len(),
            4,
            "no request for a refused draft operation"
        );
    }
}
