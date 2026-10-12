//! Every mutation project-boards sends, as `Github` methods returning model types.

use crate::github::GithubError;
use crate::github::transport::GraphqlResponse;
use serde_json::Value;

/// The payload object of mutation `name` (`data[name]`). GitHub answers a refused mutation
/// with a null payload and, usually, a FORBIDDEN or NOT_FOUND error that the transport
/// tolerated; that error is the one reported, so the user sees GitHub's own words.
// Used by the mutation methods added in the following tasks; drop the allow with the first.
#[allow(dead_code)]
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
}
