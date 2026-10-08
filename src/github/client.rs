//! The one GitHub client the rest of the app uses. Methods return model types only.

use crate::github::convert;
use crate::github::queries;
use crate::github::transport::{GraphqlRequest, GraphqlResponse, RateInfo, Transport};
use crate::github::{GithubError, GraphqlErrorEntry};
use crate::model::*;
use graphql_client::GraphQLQuery;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub struct ViewerCheck {
    pub login: String,
    pub scopes: Option<Vec<String>>,
    pub rate: RateInfo,
}

pub struct Github {
    transport: Arc<dyn Transport>,
    rate: Mutex<RateInfo>,
}

impl Github {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            rate: Mutex::new(RateInfo::default()),
        }
    }

    /// The rate-limit budget reported by the most recent response.
    pub fn rate(&self) -> RateInfo {
        self.rate.lock().unwrap().clone()
    }

    pub(crate) async fn run(
        &self,
        request: GraphqlRequest,
    ) -> Result<GraphqlResponse, GithubError> {
        let response = self.transport.execute(request).await?;
        *self.rate.lock().unwrap() = response.rate.clone();
        Ok(response)
    }

    pub async fn resolve_project(&self, board: &BoardRef) -> Result<Project, GithubError> {
        let body = queries::ResolveProject::build_query(queries::resolve_project::Variables {
            owner: board.owner.clone(),
            number: i64::from(board.number),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        let node = [&data["user"]["projectV2"], &data["organization"]["projectV2"]]
            .into_iter()
            .find(|v| v.is_object())
            .ok_or_else(|| {
                GithubError::Graphql(vec![GraphqlErrorEntry {
                    message: format!("project {board} was not found, or this token cannot see it (fine-grained tokens cannot read boards owned by a user account; use `gh auth login`)"),
                    kind: Some("NOT_FOUND".into()),
                }])
            })?;
        convert::project_from_wire(node, board)
    }

    pub async fn fetch_project(
        &self,
        id: &ProjectId,
        board: &BoardRef,
    ) -> Result<Project, GithubError> {
        let body = queries::ProjectSchema::build_query(queries::project_schema::Variables {
            id: id.0.clone(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        convert::project_from_wire(&data["node"], board)
    }

    pub async fn viewer(&self) -> Result<ViewerCheck, GithubError> {
        let body = queries::Viewer::build_query(queries::viewer::Variables);
        let response = self.run(GraphqlRequest::from_body(body)).await?;
        Ok(ViewerCheck {
            login: convert::str_of(&response.data["viewer"]["login"]),
            scopes: response.scopes,
            rate: response.rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::convert::tests::project_json;
    use crate::github::fixture::FixtureTransport;
    use serde_json::json;

    fn gh(t: Arc<FixtureTransport>) -> Github {
        Github::new(t)
    }

    #[tokio::test]
    async fn resolve_project_finds_user_owned_project() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ResolveProject",
            json!({"user": {"projectV2": project_json()}, "organization": null}),
        );
        let p = gh(t.clone())
            .resolve_project(&"tviles/3".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(p.id.as_str(), "PVT_1");
        let req = &t.requests()[0];
        assert_eq!(req.variables["owner"], "tviles");
        assert_eq!(req.variables["number"], 3);
    }

    #[tokio::test]
    async fn resolve_project_finds_org_owned_project() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ResolveProject",
            json!({"user": null, "organization": {"projectV2": project_json()}}),
        );
        assert!(
            gh(t)
                .resolve_project(&"acme/3".parse().unwrap())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn resolve_project_reports_not_found() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ResolveProject",
            json!({"user": {"projectV2": null}, "organization": null}),
        );
        let err = gh(t)
            .resolve_project(&"tviles/99".parse().unwrap())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("tviles/99"));
    }

    #[tokio::test]
    async fn rate_is_remembered_from_the_last_response() {
        let t = Arc::new(FixtureTransport::new());
        t.set_rate(RateInfo {
            remaining: Some(42),
            limit: Some(5000),
            reset_epoch: None,
        });
        t.push("Viewer", json!({"viewer": {"login": "tviles"}}));
        let g = gh(t);
        let v = g.viewer().await.unwrap();
        assert_eq!(v.login, "tviles");
        assert_eq!(g.rate().remaining, Some(42));
    }
}
