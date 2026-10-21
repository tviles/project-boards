//! Linear's issue mutations, the issue lookup by key, and a team's assignee and label
//! candidates. Every request is a named operation in `linear.graphql`, sent raw with the
//! JSON input the caller built.

use crate::error::BackendError;
use crate::github::convert::{nodes, str_of};
use crate::linear::client::{Linear, is_entity_not_found};
use crate::linear::convert::{is_single_select_group, item_from_wire};
use crate::model::{Item, LabelRef, TeamCandidates, UserRef};
use crate::transport::GraphqlRequest;
use project_boards_queries::linear as ops;
use serde_json::{Value, json};
use std::collections::HashSet;

/// The issue in a mutation payload's `field` (`issue` or `entity`); `None` when it is null.
fn payload_issue(data: &Value, mutation: &str, field: &str) -> Result<Option<Item>, BackendError> {
    let payload = &data[mutation];
    if !payload.is_object() {
        return Err(BackendError::Decode(format!(
            "{mutation} response has no payload"
        )));
    }
    let issue = &payload[field];
    if issue.is_null() {
        return Ok(None);
    }
    item_from_wire(issue).map(Some).ok_or_else(|| {
        BackendError::Decode(format!(
            "{mutation} response has an issue that does not decode"
        ))
    })
}

/// The issue a create or update answered with; a payload without one is a decode error.
fn required_issue(data: &Value, mutation: &str) -> Result<Item, BackendError> {
    payload_issue(data, mutation, "issue")?
        .ok_or_else(|| BackendError::Decode(format!("{mutation} response has no issue")))
}

/// A label the label editor offers: not a group, and not a child of a single-select group
/// (those are set through the group's own field). A group with no type is single-select,
/// as the board decodes it.
fn offered_label(label: &Value) -> Option<LabelRef> {
    let parent = &label["parent"];
    if label["isGroup"].as_bool() == Some(true)
        || (parent.is_object() && is_single_select_group(parent["groupType"].as_str()))
    {
        return None;
    }
    Some(LabelRef {
        id: label["id"].as_str()?.to_string(),
        name: str_of(&label["name"]),
        color: label["color"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('#')
            .to_string(),
    })
}

fn has_next_page(conn: &Value) -> bool {
    conn["pageInfo"]["hasNextPage"].as_bool() == Some(true)
}

impl Linear {
    /// Sends `issueUpdate(id, input)` and returns the updated issue. `input` is an
    /// `IssueUpdateInput` the caller built.
    pub async fn update_issue(&self, id: &str, input: Value) -> Result<Item, BackendError> {
        let request = GraphqlRequest::raw(
            ops::issue_update::OPERATION_NAME,
            ops::issue_update::QUERY,
            json!({"id": id, "input": input}),
        );
        let data = self.data(request).await?;
        required_issue(&data, "issueUpdate")
    }

    /// Sends `issueCreate(input)` and returns the new issue. `input` carries the client UUID
    /// as `id`, so a resend names the same issue.
    ///
    /// A create that fails in any way is followed by `issue_by_key(<that id>)`: if the issue
    /// exists (an earlier send of this create reached Linear), it is returned as created;
    /// otherwise, or if the lookup itself fails, the create's own error is returned. This is
    /// the spec's L11 fallback (section 12); probe L11's findings may later narrow it to the
    /// answer Linear gives for an id that already exists.
    pub async fn create_issue(&self, input: Value) -> Result<Item, BackendError> {
        let id = input["id"].as_str().map(String::from);
        let request = GraphqlRequest::raw(
            ops::issue_create::OPERATION_NAME,
            ops::issue_create::QUERY,
            json!({"input": input}),
        );
        let created = self.data(request).await;
        let error = match created.and_then(|data| required_issue(&data, "issueCreate")) {
            Ok(item) => return Ok(item),
            Err(e) => e,
        };
        let Some(id) = id else {
            return Err(error);
        };
        match self.issue_by_key(&id).await {
            Ok(Some(item)) => {
                tracing::debug!(
                    operation = "IssueCreate",
                    error = error.kind(),
                    "a failed create's id exists; treating it as created"
                );
                Ok(item)
            }
            Ok(None) | Err(_) => Err(error),
        }
    }

    /// Sends `issueArchive(id)`; the archived issue, or `None` when the payload has none.
    pub async fn archive(&self, id: &str) -> Result<Option<Item>, BackendError> {
        let request = GraphqlRequest::raw(
            ops::issue_archive::OPERATION_NAME,
            ops::issue_archive::QUERY,
            json!({"id": id}),
        );
        let data = self.data(request).await?;
        payload_issue(&data, "issueArchive", "entity")
    }

    /// Sends `issueUnarchive(id)`; the restored issue, or `None` when the payload has none.
    pub async fn unarchive(&self, id: &str) -> Result<Option<Item>, BackendError> {
        let request = GraphqlRequest::raw(
            ops::issue_unarchive::OPERATION_NAME,
            ops::issue_unarchive::QUERY,
            json!({"id": id}),
        );
        let data = self.data(request).await?;
        payload_issue(&data, "issueUnarchive", "entity")
    }

    /// Sends a top-level `commentCreate { id, issueId, body }`; `id` is the client UUID the
    /// op was made with, so a resend names the same comment.
    ///
    /// A comment that fails in any way is followed by `CommentById(id)`: a hit means an
    /// earlier send reached Linear, and is success; a miss, or a lookup that fails itself,
    /// returns the comment's own error. This is the spec's L11 fallback (section 12); probe
    /// L11's findings may later narrow it.
    pub async fn create_comment(
        &self,
        id: &str,
        issue: &str,
        body: &str,
    ) -> Result<(), BackendError> {
        let request = GraphqlRequest::raw(
            ops::comment_create::OPERATION_NAME,
            ops::comment_create::QUERY,
            json!({"input": {"id": id, "issueId": issue, "body": body}}),
        );
        let error = match self.data(request).await {
            Ok(data) if data["commentCreate"]["comment"]["id"].is_string() => return Ok(()),
            Ok(_) => BackendError::Decode("commentCreate response has no comment".into()),
            Err(e) => e,
        };
        if self.comment_exists(id).await == Some(true) {
            tracing::debug!(
                operation = "CommentCreate",
                error = error.kind(),
                "a failed comment's id exists; treating it as sent"
            );
            return Ok(());
        }
        Err(error)
    }

    /// Whether the comment with `id` exists; `None` when the lookup itself failed.
    async fn comment_exists(&self, id: &str) -> Option<bool> {
        let request = GraphqlRequest::raw(
            ops::comment_by_id::OPERATION_NAME,
            ops::comment_by_id::QUERY,
            json!({"id": id}),
        );
        match self.data(request).await {
            Ok(data) => Some(data["comment"]["id"].as_str() == Some(id)),
            Err(e) if is_entity_not_found(&e) => Some(false),
            Err(_) => None,
        }
    }

    /// The issue `key` names (`TBA-12`, or an issue UUID), archived or not; `None` when
    /// Linear answers "Entity not found" or a null issue. An issue that does not decode is
    /// a decode error, not `None`.
    pub async fn issue_by_key(&self, key: &str) -> Result<Option<Item>, BackendError> {
        let request = GraphqlRequest::raw(
            ops::issue_by_key::OPERATION_NAME,
            ops::issue_by_key::QUERY,
            json!({"id": key}),
        );
        match self.data(request).await {
            Ok(data) => {
                let issue = &data["issue"];
                if issue.is_null() {
                    return Ok(None);
                }
                item_from_wire(issue).map(Some).ok_or_else(|| {
                    BackendError::Decode(
                        "IssueByKey response has an issue that does not decode".into(),
                    )
                })
            }
            Err(e) if is_entity_not_found(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The assignees and labels the editors offer on `team_id`'s issues: its first 250
    /// members, and its first 250 labels then the workspace's, without group labels or the
    /// children of single-select groups. `truncated` when any of the three lists has more.
    pub async fn team_candidates(&self, team_id: &str) -> Result<TeamCandidates, BackendError> {
        let request = GraphqlRequest::raw(
            ops::team_candidates::OPERATION_NAME,
            ops::team_candidates::QUERY,
            json!({"teamId": team_id}),
        );
        let data = self.data(request).await?;
        let team = &data["team"];
        if !team.is_object() {
            return Err(BackendError::Decode(format!(
                "team {team_id} not found or not visible"
            )));
        }
        let users = nodes(team, "members")
            .into_iter()
            .filter_map(|u| {
                Some(UserRef {
                    id: u["id"].as_str()?.to_string(),
                    login: u["displayName"].as_str()?.to_string(),
                })
            })
            .collect();
        let mut seen = HashSet::new();
        let labels = nodes(team, "labels")
            .into_iter()
            .chain(nodes(&data, "issueLabels"))
            .filter_map(offered_label)
            .filter(|l| seen.insert(l.id.clone()))
            .collect();
        let truncated = [&team["members"], &team["labels"], &data["issueLabels"]]
            .into_iter()
            .any(has_next_page);
        Ok(TeamCandidates {
            team: team_id.to_string(),
            users,
            labels,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::error::{BackendError, GraphqlErrorEntry};
    use crate::linear::Linear;
    use crate::linear::client::tests::literal_firsts;
    use crate::linear::convert::item_from_wire;
    use crate::linear::convert::tests::issue_json;
    use crate::model::*;
    use crate::transport::fixture::FixtureTransport;
    use project_boards_queries::linear as ops;
    use serde_json::{Value, json};
    use std::sync::Arc;

    const TEAM: &str = "1b4e28ba-2fa1-4d3b-a3f5-ef19b5a7633b";
    const ISSUE_ID: &str = "0f8e7d6c-5b4a-4938-8271-605f4e3d2c1b";
    const COMMENT_ID: &str = "7a6b5c4d-3e2f-4a1b-9c8d-7e6f5a4b3c2d";

    fn linear(t: &Arc<FixtureTransport>) -> Linear {
        Linear::new(t.clone())
    }

    fn operations(t: &FixtureTransport) -> Vec<String> {
        t.requests().into_iter().map(|r| r.operation_name).collect()
    }

    fn the_issue() -> Item {
        item_from_wire(&issue_json()).unwrap()
    }

    // The success answers are recorded from the testbed and replayed in
    // tests/linear_fixtures.rs (`edits/`). What stays here is hand-written: error answers,
    // in the shape `execute_linear` hands the client, and payloads without their issue.
    fn input_error(message: &str) -> BackendError {
        BackendError::Api(vec![GraphqlErrorEntry {
            message: message.into(),
            kind: Some("INPUT_ERROR".into()),
        }])
    }

    /// Linear's answer to a create whose client id is taken: HTTP 200 with `INPUT_ERROR`,
    /// "conflict on insert of Issue" (or "of Comment"). Measured by probe L11 on 2026-10-21.
    fn conflict_on_insert(entity: &str) -> BackendError {
        input_error(&format!("conflict on insert of {entity}"))
    }

    fn entity_not_found(what: &str) -> BackendError {
        input_error(&format!("Entity not found: {what}"))
    }

    fn create_input() -> Value {
        json!({"id": ISSUE_ID, "teamId": TEAM, "title": "Write the docs"})
    }

    /// Hand-written: an update whose payload has no issue.
    #[tokio::test]
    async fn an_update_without_its_issue_is_a_decode_error() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "IssueUpdate",
            json!({"issueUpdate": {"success": false, "issue": null}}),
        );
        assert!(matches!(
            linear(&t).update_issue("i-7", json!({"priority": 2})).await,
            Err(BackendError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn a_failed_create_whose_id_exists_returns_the_existing_issue() {
        let t = Arc::new(FixtureTransport::new());
        t.push_error("IssueCreate", conflict_on_insert("Issue"));
        t.push("IssueByKey", json!({"issue": issue_json()}));
        let item = linear(&t).create_issue(create_input()).await.unwrap();
        assert_eq!(item, the_issue());
        assert_eq!(operations(&t), ["IssueCreate", "IssueByKey"]);
        assert_eq!(t.requests()[1].variables, json!({"id": ISSUE_ID}));

        // An answer without the issue is a failure too.
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "IssueCreate",
            json!({"issueCreate": {"success": true, "issue": null}}),
        );
        t.push("IssueByKey", json!({"issue": issue_json()}));
        let item = linear(&t).create_issue(create_input()).await.unwrap();
        assert_eq!(item, the_issue());
        assert_eq!(operations(&t), ["IssueCreate", "IssueByKey"]);
    }

    #[tokio::test]
    async fn a_failed_create_whose_id_is_unknown_keeps_its_error() {
        let original = input_error("Argument Validation Error");

        let t = Arc::new(FixtureTransport::new());
        t.push_error("IssueCreate", original.clone());
        t.push_error("IssueByKey", entity_not_found("Issue"));
        let err = linear(&t).create_issue(create_input()).await.unwrap_err();
        assert_eq!(err, original);
        assert_eq!(operations(&t), ["IssueCreate", "IssueByKey"]);

        // A lookup that fails itself still reports the create's error, not its own.
        let t = Arc::new(FixtureTransport::new());
        t.push_error(
            "IssueCreate",
            BackendError::RateLimited {
                retry_after_secs: 30,
            },
        );
        t.push_error("IssueByKey", BackendError::Network("reset".into()));
        let err = linear(&t).create_issue(create_input()).await.unwrap_err();
        assert_eq!(
            err,
            BackendError::RateLimited {
                retry_after_secs: 30
            }
        );
        assert_eq!(operations(&t), ["IssueCreate", "IssueByKey"]);

        // An input without an id has nothing to look up.
        let t = Arc::new(FixtureTransport::new());
        t.push_error("IssueCreate", original.clone());
        let err = linear(&t)
            .create_issue(json!({"teamId": TEAM, "title": "x"}))
            .await
            .unwrap_err();
        assert_eq!(err, original);
        assert_eq!(operations(&t), ["IssueCreate"]);
    }

    /// Hand-written: archive and unarchive payloads without their entity (the recorded ones
    /// have it).
    #[tokio::test]
    async fn an_archive_without_its_entity_is_none() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "IssueArchive",
            json!({"issueArchive": {"success": true, "entity": null}}),
        );
        t.push(
            "IssueUnarchive",
            json!({"issueUnarchive": {"success": true, "entity": null}}),
        );
        let l = linear(&t);
        assert_eq!(l.archive("i-7").await.unwrap(), None);
        assert_eq!(l.unarchive("i-7").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_failed_comment_whose_id_exists_is_success() {
        let t = Arc::new(FixtureTransport::new());
        t.push_error("CommentCreate", conflict_on_insert("Comment"));
        t.push("CommentById", json!({"comment": {"id": COMMENT_ID}}));
        linear(&t)
            .create_comment(COMMENT_ID, "i-7", "Looks good")
            .await
            .unwrap();
        assert_eq!(operations(&t), ["CommentCreate", "CommentById"]);
        let lookup = &t.requests()[1];
        assert_eq!(lookup.query, ops::comment_by_id::QUERY);
        assert_eq!(lookup.variables, json!({"id": COMMENT_ID}));

        // An answer without the comment is a failure too.
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "CommentCreate",
            json!({"commentCreate": {"success": false, "comment": null}}),
        );
        t.push("CommentById", json!({"comment": {"id": COMMENT_ID}}));
        linear(&t)
            .create_comment(COMMENT_ID, "i-7", "Looks good")
            .await
            .unwrap();
        assert_eq!(operations(&t), ["CommentCreate", "CommentById"]);
    }

    #[tokio::test]
    async fn a_failed_comment_whose_id_is_unknown_keeps_its_error() {
        let original = input_error("Argument Validation Error");

        let t = Arc::new(FixtureTransport::new());
        t.push_error("CommentCreate", original.clone());
        t.push_error("CommentById", entity_not_found("Comment"));
        let err = linear(&t)
            .create_comment(COMMENT_ID, "i-7", "Looks good")
            .await
            .unwrap_err();
        assert_eq!(err, original);

        let t = Arc::new(FixtureTransport::new());
        t.push_error("CommentCreate", original.clone());
        t.push_error("CommentById", BackendError::Network("reset".into()));
        let err = linear(&t)
            .create_comment(COMMENT_ID, "i-7", "Looks good")
            .await
            .unwrap_err();
        assert_eq!(err, original);
        assert_eq!(operations(&t), ["CommentCreate", "CommentById"]);
    }

    /// Hand-written error answers; a key that names an issue is replayed from the recording.
    #[tokio::test]
    async fn an_unknown_key_is_none() {
        let t = Arc::new(FixtureTransport::new());
        t.push_error("IssueByKey", entity_not_found("Issue"));
        t.push_error("IssueByKey", input_error("Argument Validation Error"));
        let l = linear(&t);
        assert_eq!(l.issue_by_key("TBA-999").await.unwrap(), None);
        assert_eq!(
            l.issue_by_key("TBA-7").await.unwrap_err(),
            input_error("Argument Validation Error")
        );
        let requests = t.requests();
        assert_eq!(requests[0].query, ops::issue_by_key::QUERY);
        assert_eq!(requests[0].variables, json!({"id": "TBA-999"}));
    }

    #[tokio::test]
    async fn a_key_whose_issue_does_not_decode_is_an_error_not_none() {
        let t = Arc::new(FixtureTransport::new());
        t.push("IssueByKey", json!({"issue": {"id": "i-7"}}));
        t.push("IssueByKey", json!({"issue": null}));
        let l = linear(&t);
        assert!(matches!(
            l.issue_by_key("TBA-7").await,
            Err(BackendError::Decode(_))
        ));
        assert_eq!(l.issue_by_key("TBA-7").await.unwrap(), None);
    }

    fn label(id: &str, name: &str, is_group: bool, parent: Value) -> Value {
        json!({"id": id, "name": name, "color": "#eb5757", "isGroup": is_group,
               "parent": parent})
    }

    fn connection(nodes: Vec<Value>, more: bool) -> Value {
        json!({"nodes": nodes, "pageInfo": {"hasNextPage": more}})
    }

    fn candidates_json(more: [bool; 3]) -> Value {
        let single = json!({"id": "g-type", "groupType": "singleSelect"});
        let multi = json!({"id": "g-area", "groupType": "multiSelect"});
        // A group with no type is single-select too.
        let untyped = json!({"id": "g-sev", "groupType": null});
        json!({
            "team": {
                "members": connection(vec![
                    json!({"id": "u-1", "displayName": "Ada Lovelace"}),
                    json!({"id": "u-2", "displayName": "Grace Hopper"}),
                ], more[0]),
                "labels": connection(vec![
                    label("l-bug", "Bug", false, single),
                    label("l-docs", "docs", false, Value::Null),
                    label("l-ui", "UI", false, multi),
                    label("l-sev", "Sev 1", false, untyped),
                    label("g-type", "Type", true, Value::Null),
                ], more[1]),
            },
            "issueLabels": connection(vec![label("l-perf", "perf", false, Value::Null)], more[2]),
        })
    }

    /// Hand-written: group shapes the testbed does not have (an untyped group, a multi-select
    /// group's child). The recorded answer is replayed in tests/linear_fixtures.rs.
    #[tokio::test]
    async fn team_candidates_leave_out_single_select_groups() {
        let t = Arc::new(FixtureTransport::new());
        t.push("TeamCandidates", candidates_json([false; 3]));
        for i in 0..3 {
            let mut more = [false; 3];
            more[i] = true;
            t.push("TeamCandidates", candidates_json(more));
        }
        let l = linear(&t);
        let c = l.team_candidates(TEAM).await.unwrap();
        let label = |id: &str, name: &str| LabelRef {
            id: id.into(),
            name: name.into(),
            color: "eb5757".into(),
        };
        assert_eq!(
            c,
            TeamCandidates {
                team: TEAM.into(),
                users: vec![
                    UserRef {
                        id: "u-1".into(),
                        login: "Ada Lovelace".into(),
                    },
                    UserRef {
                        id: "u-2".into(),
                        login: "Grace Hopper".into(),
                    },
                ],
                labels: vec![
                    label("l-docs", "docs"),
                    label("l-ui", "UI"),
                    label("l-perf", "perf"),
                ],
                truncated: false,
            }
        );
        for i in 0..3 {
            assert!(l.team_candidates(TEAM).await.unwrap().truncated, "{i}");
        }
        let req = &t.requests()[0];
        assert_eq!(req.query, ops::team_candidates::QUERY);
        assert_eq!(req.variables, json!({"teamId": TEAM}));
    }

    #[tokio::test]
    async fn an_unknown_team_has_no_candidates() {
        let t = Arc::new(FixtureTransport::new());
        t.push("TeamCandidates", json!({"team": null, "issueLabels": null}));
        assert!(matches!(
            linear(&t).team_candidates(TEAM).await,
            Err(BackendError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn no_write_sends_first_above_250() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "IssueUpdate",
            json!({"issueUpdate": {"success": true, "issue": issue_json()}}),
        );
        t.push(
            "IssueCreate",
            json!({"issueCreate": {"success": true, "issue": issue_json()}}),
        );
        t.push(
            "IssueArchive",
            json!({"issueArchive": {"success": true, "entity": null}}),
        );
        t.push(
            "IssueUnarchive",
            json!({"issueUnarchive": {"success": true, "entity": null}}),
        );
        t.push(
            "CommentCreate",
            json!({"commentCreate": {"success": true, "comment": {"id": COMMENT_ID}}}),
        );
        t.push("IssueByKey", json!({"issue": issue_json()}));
        t.push("CommentById", json!({"comment": {"id": COMMENT_ID}}));
        t.push("TeamCandidates", candidates_json([false; 3]));
        let l = linear(&t);
        l.update_issue("i-7", json!({"priority": 1})).await.unwrap();
        l.create_issue(create_input()).await.unwrap();
        l.archive("i-7").await.unwrap();
        l.unarchive("i-7").await.unwrap();
        l.create_comment(COMMENT_ID, "i-7", "hi").await.unwrap();
        l.issue_by_key("TBA-7").await.unwrap();
        l.team_candidates(TEAM).await.unwrap();
        // CommentById is sent only after a failed comment; check its text directly.
        let mut queries: Vec<(String, String)> = t
            .requests()
            .into_iter()
            .map(|r| {
                assert!(
                    r.variables.get("first").is_none(),
                    "{} sends first",
                    r.operation_name
                );
                (r.operation_name, r.query)
            })
            .collect();
        assert_eq!(queries.len(), 7, "{queries:?}");
        queries.push(("CommentById".into(), ops::comment_by_id::QUERY.into()));
        for (name, query) in &queries {
            for n in literal_firsts(query) {
                assert!(n <= 250, "{name} has first: {n}");
            }
        }
    }
}
