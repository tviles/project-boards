//! Carries an op to GitHub: which mutation, with which ids, and what came back.

use crate::github::{Github, GithubError};
use crate::model::{ContentId, ProjectId};
use crate::ops::{Confirmed, Op};

fn ids<T>(list: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
    list.iter().map(|x| id(x).to_string()).collect()
}

/// Sends `op` and reports what GitHub confirmed. Ops on the same item must not overlap; the
/// queue (ops::queue) makes sure of that.
pub async fn send(gh: &Github, project: &ProjectId, op: &Op) -> Result<Confirmed, GithubError> {
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
            matches!(send(&gh, &p, &set).await, Ok(Confirmed::Item(i)) if i.id.as_str() == "PVTI_1")
        );
        let clear = Op::ClearField {
            item: item.clone(),
            field: FieldId::new("F_notes"),
        };
        assert!(matches!(
            send(&gh, &p, &clear).await,
            Ok(Confirmed::Item(_))
        ));
        let reorder = Op::Reorder { item, after: None };
        assert_eq!(send(&gh, &p, &reorder).await, Ok(Confirmed::Applied));
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
                send(&gh, &p, op).await,
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
}
