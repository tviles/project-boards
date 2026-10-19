//! Carries an op to GitHub: which mutation, with which ids, and what came back.

use crate::backend::Backend;
use crate::error::{BackendError, GraphqlErrorEntry};
use crate::github::Github;
use crate::model::{ContentId, FieldId, FieldValue, ItemId, ProjectId};
use crate::ops::{Confirmed, NewItem, Op};

fn ids<T>(list: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
    list.iter().map(|x| id(x).to_string()).collect()
}

/// Creates the item, then sets each preset on it. Every preset is tried; those GitHub refuses
/// are reported with the item: it exists, and the user sees what is missing.
///
/// A new issue takes two calls. Once `createIssue` has succeeded, no failure may come back
/// as an error: the queue sends a rate-limited op again, which would create a second issue.
/// A failed add is `Confirmed::NotAdded` with the issue's id, so a retry only adds it.
async fn create(
    gh: &Github,
    project: &ProjectId,
    temp: &ItemId,
    new: &NewItem,
    presets: &[(FieldId, FieldValue)],
) -> Result<Confirmed, BackendError> {
    let mut item = match new {
        NewItem::Draft { title, body } => gh.add_draft_issue(project, title, body).await?,
        NewItem::Issue { repo, title, body } => {
            let issue = gh.create_issue(&repo.id, title, body).await?;
            match gh.add_item_by_id(project, &issue).await {
                Ok(item) => item,
                Err(error) => return Ok(Confirmed::NotAdded { issue, error }),
            }
        }
        NewItem::Existing { hit } => gh.add_item_by_id(project, &hit.id).await?,
    };
    let mut refused = Vec::new();
    for (field, value) in presets {
        match gh.set_field_value(project, &item.id, field, value).await {
            Ok(updated) => item = updated,
            Err(e) => refused.push((field.clone(), e)),
        }
    }
    Ok(Confirmed::Created {
        temp: temp.clone(),
        item,
        refused,
    })
}

/// Sends `op` to the board's provider and reports what it confirmed. Ops on the same item
/// must not overlap; the queue (ops::queue) makes sure of that.
pub async fn send(
    backend: &Backend,
    project: &ProjectId,
    op: &Op,
) -> Result<Confirmed, BackendError> {
    match backend {
        Backend::Github(gh) => send_github(gh, project, op).await,
        // The controller refuses every edit on a Linear board before it gets here (spec
        // section 5); this keeps any that slips through from being sent.
        Backend::Linear(_) => Err(BackendError::Api(vec![GraphqlErrorEntry {
            message: "not supported".into(),
            kind: None,
        }])),
    }
}

pub(crate) async fn send_github(
    gh: &Github,
    project: &ProjectId,
    op: &Op,
) -> Result<Confirmed, BackendError> {
    match op {
        Op::SetField { item, field, value } => gh
            .set_field_value(project, item, field, value)
            .await
            .map(Confirmed::Item),
        Op::ClearField { item, field } => gh
            .clear_field_value(project, item, field)
            .await
            .map(Confirmed::Item),
        Op::Reorder { item, after } => gh
            .update_item_position(project, item, after.as_ref())
            .await
            .map(|_order| Confirmed::Applied),
        Op::Assignees {
            content,
            add,
            remove,
            all,
            ..
        } => {
            match content {
                ContentId::Draft(id) => gh.set_draft_assignees(id, &ids(all, |u| &u.id)).await?,
                ContentId::Issue(id) | ContentId::PullRequest(id) => {
                    if !add.is_empty() {
                        gh.add_assignees(id, &ids(add, |u| &u.id)).await?;
                    }
                    if !remove.is_empty() {
                        gh.remove_assignees(id, &ids(remove, |u| &u.id)).await?;
                    }
                }
            }
            Ok(Confirmed::Applied)
        }
        Op::Labels {
            content,
            add,
            remove,
            ..
        } => {
            let id = content.node_id();
            if !add.is_empty() {
                gh.add_labels(id, &ids(add, |l| &l.id)).await?;
            }
            if !remove.is_empty() {
                gh.remove_labels(id, &ids(remove, |l| &l.id)).await?;
            }
            Ok(Confirmed::Applied)
        }
        Op::Milestone {
            content, milestone, ..
        } => gh
            .set_milestone(content, milestone.as_ref().map(|m| m.id.as_str()))
            .await
            .map(|()| Confirmed::Applied),
        Op::Title { content, title, .. } => gh
            .set_title(content, title)
            .await
            .map(|()| Confirmed::Applied),
        Op::Body { content, body, .. } => gh
            .set_body(content, body)
            .await
            .map(|()| Confirmed::Applied),
        Op::Comment { content, body, .. } => gh
            .add_comment(content.node_id(), body)
            .await
            .map(|()| Confirmed::Applied),
        Op::Close {
            content, reason, ..
        } => gh
            .close(content, *reason)
            .await
            .map(|()| Confirmed::Applied),
        Op::Reopen { content, .. } => gh.reopen(content).await.map(|()| Confirmed::Applied),
        Op::Convert { item, repo } => gh.convert_draft(item, &repo.id).await.map(Confirmed::Item),
        Op::Archive { item } => gh
            .archive_item(project, item)
            .await
            .map(|()| Confirmed::Applied),
        Op::Unarchive { item } => gh
            .unarchive_item(project, item)
            .await
            .map(|()| Confirmed::Applied),
        Op::Remove { item } => gh
            .delete_item(project, item)
            .await
            .map(|()| Confirmed::Applied),
        Op::Create { temp, new, presets } => create(gh, project, temp, new, presets).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::convert::tests::item_json;
    use crate::github::fixture::FixtureTransport;
    use crate::model::{FieldId, FieldValue, ItemId};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn field_ops_confirm_with_the_returned_item_and_reorder_as_applied() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "SetTextValue",
            json!({"updateProjectV2ItemFieldValue": {"projectV2Item": item_json()}}),
        );
        t.push(
            "ClearFieldValue",
            json!({"clearProjectV2ItemFieldValue": {"projectV2Item": item_json()}}),
        );
        t.push(
            "UpdateItemPosition",
            json!({"updateProjectV2ItemPosition": {"clientMutationId": null}}),
        );
        let gh = Github::new(t.clone());
        let p = ProjectId::new("PVT_1");
        let item = ItemId::new("PVTI_1");
        let set = Op::SetField {
            item: item.clone(),
            field: FieldId::new("F_notes"),
            value: FieldValue::Text("x".into()),
        };
        assert!(
            matches!(send_github(&gh, &p, &set).await, Ok(Confirmed::Item(i)) if i.id.as_str() == "PVTI_1")
        );
        let clear = Op::ClearField {
            item: item.clone(),
            field: FieldId::new("F_notes"),
        };
        assert!(matches!(
            send_github(&gh, &p, &clear).await,
            Ok(Confirmed::Item(_))
        ));
        let reorder = Op::Reorder { item, after: None };
        assert_eq!(send_github(&gh, &p, &reorder).await, Ok(Confirmed::Applied));
        let names: Vec<String> = t
            .requests()
            .iter()
            .map(|r| r.operation_name.clone())
            .collect();
        assert_eq!(
            names,
            ["SetTextValue", "ClearFieldValue", "UpdateItemPosition"]
        );
    }

    #[tokio::test]
    async fn issue_ops_send_their_mutations() {
        use crate::model::{CloseReason, ContentId, LabelRef, UserRef};
        let t = Arc::new(FixtureTransport::new());
        let ok = |op: &str, m: &str| {
            let mut d = serde_json::Map::new();
            d.insert(m.into(), json!({"clientMutationId": null, "issue": {"id": "I_1"}, "draftIssue": {"id": "DI_1"}}));
            t.push(op, serde_json::Value::Object(d));
        };
        ok("AddAssignees", "addAssigneesToAssignable");
        ok("SetDraftAssignees", "updateProjectV2DraftIssue");
        ok("RemoveLabels", "removeLabelsFromLabelable");
        ok("CloseIssue", "closeIssue");
        ok("AddComment", "addComment");
        let gh = Github::new(t.clone());
        let p = ProjectId::new("PVT_1");
        let u = UserRef {
            id: "U_1".into(),
            login: "octo".into(),
        };
        let item = ItemId::new("a");
        let issue = ContentId::Issue("I_1".into());
        let ops = [
            Op::Assignees {
                item: item.clone(),
                content: issue.clone(),
                add: vec![u.clone()],
                remove: vec![],
                all: vec![u.clone()],
            },
            Op::Assignees {
                item: item.clone(),
                content: ContentId::Draft("DI_1".into()),
                add: vec![u.clone()],
                remove: vec![],
                all: vec![u],
            },
            Op::Labels {
                item: item.clone(),
                content: issue.clone(),
                add: vec![],
                remove: vec![LabelRef {
                    id: "L_1".into(),
                    name: "bug".into(),
                    color: "f00".into(),
                }],
            },
            Op::Close {
                item: item.clone(),
                content: issue.clone(),
                reason: Some(CloseReason::Completed),
            },
            Op::Comment {
                item,
                content: issue,
                body: "LGTM".into(),
            },
        ];
        for op in &ops {
            assert_eq!(
                send_github(&gh, &p, op).await,
                Ok(Confirmed::Applied),
                "{}",
                op.name()
            );
        }
        let names: Vec<String> = t
            .requests()
            .iter()
            .map(|r| r.operation_name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "AddAssignees",
                "SetDraftAssignees",
                "RemoveLabels",
                "CloseIssue",
                "AddComment"
            ]
        );
    }

    #[tokio::test]
    async fn two_call_ops_send_both_in_order_and_the_other_arms_send_theirs() {
        use crate::model::{ContentId, LabelRef, MilestoneRef, UserRef};
        let t = Arc::new(FixtureTransport::new());
        for (op, body) in [
            (
                "AddAssignees",
                json!({"addAssigneesToAssignable": {"clientMutationId": null}}),
            ),
            (
                "RemoveAssignees",
                json!({"removeAssigneesFromAssignable": {"clientMutationId": null}}),
            ),
            (
                "AddLabels",
                json!({"addLabelsToLabelable": {"clientMutationId": null}}),
            ),
            (
                "RemoveLabels",
                json!({"removeLabelsFromLabelable": {"clientMutationId": null}}),
            ),
            (
                "SetIssueMilestone",
                json!({"updateIssue": {"issue": {"id": "I_1"}}}),
            ),
            (
                "UpdateIssueTitle",
                json!({"updateIssue": {"issue": {"id": "I_1"}}}),
            ),
            (
                "UpdateIssueBody",
                json!({"updateIssue": {"issue": {"id": "I_1"}}}),
            ),
            (
                "ReopenIssue",
                json!({"reopenIssue": {"issue": {"id": "I_1"}}}),
            ),
            (
                "ClosePullRequest",
                json!({"closePullRequest": {"pullRequest": {"id": "PR_1"}}}),
            ),
        ] {
            t.push(op, body);
        }
        let gh = Github::new(t.clone());
        let p = ProjectId::new("PVT_1");
        let item = ItemId::new("a");
        let issue = ContentId::Issue("I_1".into());
        let u = |n: &str| UserRef {
            id: format!("U_{n}"),
            login: n.into(),
        };
        let l = |n: &str| LabelRef {
            id: format!("L_{n}"),
            name: n.into(),
            color: "f00".into(),
        };
        let ops = [
            Op::Assignees {
                item: item.clone(),
                content: issue.clone(),
                add: vec![u("a")],
                remove: vec![u("b")],
                all: vec![u("a")],
            },
            Op::Labels {
                item: item.clone(),
                content: issue.clone(),
                add: vec![l("x")],
                remove: vec![l("y")],
            },
            Op::Milestone {
                item: item.clone(),
                content: issue.clone(),
                milestone: Some(MilestoneRef {
                    id: "MI_1".into(),
                    title: "v1".into(),
                }),
            },
            Op::Title {
                item: item.clone(),
                content: issue.clone(),
                title: "T".into(),
            },
            Op::Body {
                item: item.clone(),
                content: issue.clone(),
                body: "B".into(),
            },
            Op::Reopen {
                item: item.clone(),
                content: issue,
            },
            Op::Close {
                item,
                content: ContentId::PullRequest("PR_1".into()),
                reason: None,
            },
        ];
        for op in &ops {
            assert_eq!(
                send_github(&gh, &p, op).await,
                Ok(Confirmed::Applied),
                "{}",
                op.name()
            );
        }
        let names: Vec<String> = t
            .requests()
            .iter()
            .map(|r| r.operation_name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "AddAssignees",
                "RemoveAssignees",
                "AddLabels",
                "RemoveLabels",
                "SetIssueMilestone",
                "UpdateIssueTitle",
                "UpdateIssueBody",
                "ReopenIssue",
                "ClosePullRequest"
            ]
        );
    }

    #[tokio::test]
    async fn create_makes_the_item_then_sets_presets_and_reports_a_failed_one() {
        use crate::model::RepoRef;
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "CreateIssue",
            json!({"createIssue": {"issue": {"id": "I_new"}}}),
        );
        t.push(
            "AddItemById",
            json!({"addProjectV2ItemById": {"item": item_json()}}),
        );
        t.push(
            "SetSingleSelectValue",
            json!({"updateProjectV2ItemFieldValue": {"projectV2Item": item_json()}}),
        );
        t.push_error("SetTextValue", BackendError::Network("reset".into()));
        let gh = Github::new(t.clone());
        let op = Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Issue {
                repo: RepoRef {
                    id: "R_1".into(),
                    slug: "acme/app".parse().unwrap(),
                },
                title: "T".into(),
                body: "B".into(),
            },
            presets: vec![
                (
                    FieldId::new("F_status"),
                    FieldValue::SingleSelect {
                        option_id: crate::model::OptionId::new("o"),
                        name: "Todo".into(),
                    },
                ),
                (FieldId::new("F_notes"), FieldValue::Text("x".into())),
            ],
        };
        match send_github(&gh, &ProjectId::new("P"), &op).await {
            Ok(Confirmed::Created {
                temp,
                item,
                refused,
            }) => {
                assert_eq!((temp.as_str(), item.id.as_str()), ("local:1", "PVTI_1"));
                assert!(matches!(
                    &refused[..],
                    [(field, BackendError::Network(_))] if field.as_str() == "F_notes"
                ));
            }
            other => panic!("expected Created, got {other:?}"),
        }
        let names: Vec<String> = t
            .requests()
            .iter()
            .map(|r| r.operation_name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "CreateIssue",
                "AddItemById",
                "SetSingleSelectValue",
                "SetTextValue"
            ]
        );
    }

    #[tokio::test]
    async fn drafts_existing_items_conversion_archive_and_removal_send_theirs() {
        use crate::model::{ContentHit, RepoRef};
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "AddDraftIssue",
            json!({"addProjectV2DraftIssue": {"projectItem": item_json()}}),
        );
        t.push(
            "AddItemById",
            json!({"addProjectV2ItemById": {"item": item_json()}}),
        );
        t.push(
            "ConvertDraft",
            json!({"convertProjectV2DraftIssueItemToIssue": {"item": item_json()}}),
        );
        t.push(
            "ArchiveItem",
            json!({"archiveProjectV2Item": {"item": {"id": "PVTI_1"}}}),
        );
        t.push(
            "DeleteItem",
            json!({"deleteProjectV2Item": {"deletedItemId": "PVTI_1"}}),
        );
        let gh = Github::new(t.clone());
        let p = ProjectId::new("P");
        let item = ItemId::new("PVTI_1");
        let hit = ContentHit {
            id: "I_9".into(),
            pull: false,
            repo: "acme/app".into(),
            number: 9,
            title: "x".into(),
            open: true,
        };
        let draft = Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![],
        };
        assert!(matches!(
            send_github(&gh, &p, &draft).await,
            Ok(Confirmed::Created { refused, .. }) if refused.is_empty()
        ));
        let existing = Op::Create {
            temp: ItemId::new("local:2"),
            new: crate::ops::NewItem::Existing { hit },
            presets: vec![],
        };
        assert!(matches!(
            send_github(&gh, &p, &existing).await,
            Ok(Confirmed::Created { .. })
        ));
        let convert = Op::Convert {
            item: item.clone(),
            repo: RepoRef {
                id: "R_1".into(),
                slug: "acme/app".parse().unwrap(),
            },
        };
        assert!(matches!(
            send_github(&gh, &p, &convert).await,
            Ok(Confirmed::Item(_))
        ));
        assert_eq!(
            send_github(&gh, &p, &Op::Archive { item: item.clone() }).await,
            Ok(Confirmed::Applied)
        );
        assert_eq!(
            send_github(&gh, &p, &Op::Remove { item }).await,
            Ok(Confirmed::Applied)
        );
    }

    #[tokio::test]
    async fn unarchive_sends_its_mutation() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "UnarchiveItem",
            json!({"unarchiveProjectV2Item": {"item": {"id": "PVTI_1"}}}),
        );
        let gh = Github::new(t.clone());
        let op = Op::Unarchive {
            item: ItemId::new("PVTI_1"),
        };
        assert_eq!(
            send_github(&gh, &ProjectId::new("P"), &op).await,
            Ok(Confirmed::Applied)
        );
    }

    fn new_issue_op() -> Op {
        Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Issue {
                repo: crate::model::RepoRef {
                    id: "R_1".into(),
                    slug: "acme/app".parse().unwrap(),
                },
                title: "T".into(),
                body: String::new(),
            },
            presets: vec![(FieldId::new("F_notes"), FieldValue::Text("x".into()))],
        }
    }

    /// Controller ruling E15: once the issue exists, no failure after it may look like one
    /// the queue retries (a rate limit is retried), or the issue would be created twice.
    #[tokio::test]
    async fn an_issue_created_but_not_added_is_reported_with_its_id_whatever_the_error() {
        for error in [
            BackendError::Network("reset".into()),
            BackendError::RateLimited {
                retry_after_secs: 30,
            },
        ] {
            let t = Arc::new(FixtureTransport::new());
            t.push(
                "CreateIssue",
                json!({"createIssue": {"issue": {"id": "I_new"}}}),
            );
            t.push_error("AddItemById", error.clone());
            let gh = Github::new(t.clone());
            assert_eq!(
                send_github(&gh, &ProjectId::new("P"), &new_issue_op()).await,
                Ok(Confirmed::NotAdded {
                    issue: "I_new".into(),
                    error
                })
            );
            let names: Vec<String> = t
                .requests()
                .iter()
                .map(|r| r.operation_name.clone())
                .collect();
            assert_eq!(names, ["CreateIssue", "AddItemById"], "no presets sent");
        }
    }

    #[tokio::test]
    async fn a_failed_create_or_add_is_an_error_with_nothing_created() {
        let t = Arc::new(FixtureTransport::new());
        t.push_error("CreateIssue", BackendError::Network("reset".into()));
        let gh = Github::new(t.clone());
        assert_eq!(
            send_github(&gh, &ProjectId::new("P"), &new_issue_op()).await,
            Err(BackendError::Network("reset".into()))
        );
        let t = Arc::new(FixtureTransport::new());
        t.push_error("AddDraftIssue", BackendError::Network("reset".into()));
        let gh = Github::new(t.clone());
        let draft = Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![(FieldId::new("F_notes"), FieldValue::Text("x".into()))],
        };
        assert!(
            send_github(&gh, &ProjectId::new("P"), &draft)
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1, "no presets for an item never made");
    }

    #[tokio::test]
    async fn every_preset_is_tried_after_one_is_refused() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "AddDraftIssue",
            json!({"addProjectV2DraftIssue": {"projectItem": item_json()}}),
        );
        t.push_error(
            "SetSingleSelectValue",
            BackendError::MissingPermission {
                provider: crate::model::Provider::Github,
                needed: vec!["project".into()],
            },
        );
        t.push(
            "SetTextValue",
            json!({"updateProjectV2ItemFieldValue": {"projectV2Item": item_json()}}),
        );
        t.push_error("SetNumberValue", BackendError::Network("reset".into()));
        let gh = Github::new(t.clone());
        let op = Op::Create {
            temp: ItemId::new("local:1"),
            new: crate::ops::NewItem::Draft {
                title: "Idea".into(),
                body: String::new(),
            },
            presets: vec![
                (
                    FieldId::new("F_status"),
                    FieldValue::SingleSelect {
                        option_id: crate::model::OptionId::new("o"),
                        name: "Todo".into(),
                    },
                ),
                (FieldId::new("F_notes"), FieldValue::Text("x".into())),
                (FieldId::new("F_size"), FieldValue::Number(3.0)),
            ],
        };
        let Ok(Confirmed::Created { refused, .. }) =
            send_github(&gh, &ProjectId::new("P"), &op).await
        else {
            panic!("expected Created")
        };
        let fields: Vec<&str> = refused.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(fields, ["F_status", "F_size"]);
        assert!(matches!(
            refused[0].1,
            BackendError::MissingPermission { .. }
        ));
        assert_eq!(t.requests().len(), 4, "the draft and all three presets");
    }

    #[tokio::test]
    async fn send_dispatches_to_the_boards_backend() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "UpdateItemPosition",
            json!({"updateProjectV2ItemPosition": {"clientMutationId": null}}),
        );
        let backend = crate::backend::Backend::Github(Github::new(t.clone()));
        let op = Op::Reorder {
            item: ItemId::new("PVTI_1"),
            after: None,
        };
        assert_eq!(
            send(&backend, &ProjectId::new("P"), &op).await,
            Ok(Confirmed::Applied)
        );
        assert_eq!(t.requests()[0].operation_name, "UpdateItemPosition");
    }

    #[tokio::test]
    async fn nothing_is_sent_to_linear() {
        let t = Arc::new(FixtureTransport::new());
        let backend = crate::backend::Backend::Linear(crate::linear::Linear::new(t.clone()));
        let op = Op::Reorder {
            item: ItemId::new("i-1"),
            after: None,
        };
        assert_eq!(
            send(&backend, &ProjectId::new("linear:team:t1"), &op).await,
            Err(BackendError::Api(vec![GraphqlErrorEntry {
                message: "not supported".into(),
                kind: None,
            }]))
        );
        assert!(t.requests().is_empty());
    }
}
