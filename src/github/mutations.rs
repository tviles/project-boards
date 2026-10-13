//! Every mutation project-boards sends, as `Github` methods returning model types.

use crate::github::Github;
use crate::github::GithubError;
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
}
