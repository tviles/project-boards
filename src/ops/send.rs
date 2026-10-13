//! Carries an op to GitHub: which mutation, with which ids, and what came back.

use crate::github::{Github, GithubError};
use crate::model::ProjectId;
use crate::ops::{Confirmed, Op};

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
}
