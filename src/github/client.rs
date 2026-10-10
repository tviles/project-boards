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

/// One page of a connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub nodes: Vec<T>,
    pub total: usize,
    /// Cursor for the next page, `None` on the last page.
    pub next: Option<String>,
}

fn page_meta(conn: &serde_json::Value) -> (usize, Option<String>) {
    let total = conn["totalCount"].as_u64().unwrap_or(0) as usize;
    let next = (conn["pageInfo"]["hasNextPage"].as_bool() == Some(true))
        .then(|| conn["pageInfo"]["endCursor"].as_str().map(String::from))
        .flatten();
    (total, next)
}

fn items_conn<'a>(
    data: &'a serde_json::Value,
    id: &ProjectId,
) -> Result<&'a serde_json::Value, GithubError> {
    let conn = &data["node"]["items"];
    if conn.is_object() {
        Ok(conn)
    } else {
        Err(GithubError::Decode(format!(
            "project {} not found or not visible to this token",
            id.0
        )))
    }
}

/// Items per page for full loads and view-id fetches.
pub const FULL_PAGE: u32 = 100;
/// Items per page for incremental polls, which usually match a handful of items. GitHub scores
/// a query by its `first:` arguments, so a smaller page makes each poll cheaper.
pub const POLL_PAGE: u32 = 20;

impl Github {
    /// One page of items matching `query`, `first` at a time.
    pub async fn fetch_items_page(
        &self,
        project: &ProjectId,
        query: &str,
        first: u32,
        after: Option<String>,
    ) -> Result<Page<Item>, GithubError> {
        let body = queries::ItemsPage::build_query(queries::items_page::Variables {
            id: project.0.clone(),
            first: i64::from(first),
            after,
            query: query.to_string(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        let (total, next) = page_meta(items_conn(&data, project)?);
        let nodes = convert::nodes(&data["node"], "items")
            .into_iter()
            .filter_map(convert::item_from_wire)
            .collect();
        Ok(Page { nodes, total, next })
    }

    pub async fn fetch_view_ids_page(
        &self,
        project: &ProjectId,
        query: &str,
        after: Option<String>,
    ) -> Result<Page<ItemId>, GithubError> {
        let body = queries::ViewItemIds::build_query(queries::view_item_ids::Variables {
            id: project.0.clone(),
            first: i64::from(FULL_PAGE),
            after,
            query: query.to_string(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        let (total, next) = page_meta(items_conn(&data, project)?);
        let nodes = convert::nodes(&data["node"], "items")
            .into_iter()
            .filter_map(|n| n["id"].as_str().map(ItemId::new))
            .collect();
        Ok(Page { nodes, total, next })
    }

    /// The rate-limit points an `ItemsPage` request of `first` items would cost, from a dry
    /// run that evaluates nothing. Used by the live cost test.
    pub async fn items_page_cost(
        &self,
        project: &ProjectId,
        first: u32,
    ) -> Result<u64, GithubError> {
        let body = queries::ItemsPageCost::build_query(queries::items_page_cost::Variables {
            id: project.0.clone(),
            first: i64::from(first),
            after: None,
            query: String::new(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        data["rateLimit"]["cost"]
            .as_u64()
            .ok_or_else(|| GithubError::Decode("dry run returned no rateLimit.cost".into()))
    }

    /// Loads items by id. Pass at most 100 ids; ids that no longer exist are skipped.
    pub async fn hydrate_items(&self, ids: &[ItemId]) -> Result<Vec<Item>, GithubError> {
        let body = queries::HydrateItems::build_query(queries::hydrate_items::Variables {
            ids: ids.iter().map(|i| i.0.clone()).collect(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        let nodes = data["nodes"]
            .as_array()
            .ok_or_else(|| GithubError::Decode("hydrate response has no nodes array".into()))?;
        Ok(nodes.iter().filter_map(convert::item_from_wire).collect())
    }

    pub async fn fetch_item_detail(
        &self,
        item: &ItemId,
        before: Option<String>,
    ) -> Result<ItemDetail, GithubError> {
        let body = queries::ItemDetail::build_query(queries::item_detail::Variables {
            id: item.0.clone(),
            before,
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        if data["node"].is_null() {
            return Err(GithubError::Decode(
                "item detail response has no node".into(),
            ));
        }
        Ok(convert::detail_from_wire(&data["node"]))
    }

    pub async fn list_repo_projects(
        &self,
        repo: &RepoSlug,
    ) -> Result<Vec<ProjectSummary>, GithubError> {
        let body = queries::RepoProjects::build_query(queries::repo_projects::Variables {
            owner: repo.owner.clone(),
            name: repo.name.clone(),
        });
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        Ok(convert::nodes(&data["repository"], "projectsV2")
            .into_iter()
            .filter_map(convert::summary_from_wire)
            .collect())
    }

    /// The viewer's own boards, then each organisation's.
    pub async fn list_viewer_projects(&self) -> Result<Vec<ProjectSummary>, GithubError> {
        let body = queries::ViewerProjects::build_query(queries::viewer_projects::Variables);
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        let viewer = &data["viewer"];
        let mut out: Vec<ProjectSummary> = convert::nodes(viewer, "projectsV2")
            .into_iter()
            .filter_map(convert::summary_from_wire)
            .collect();
        for org in convert::nodes(viewer, "organizations") {
            out.extend(
                convert::nodes(org, "projectsV2")
                    .into_iter()
                    .filter_map(convert::summary_from_wire),
            );
        }
        Ok(out)
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

    use crate::github::convert::tests::item_json;

    #[tokio::test]
    async fn items_page_decodes_nodes_total_and_cursor() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ItemsPage",
            json!({"node": {"items": {"totalCount": 250,
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"}, "nodes": [item_json(), null]}}}),
        );
        let page = gh(t.clone())
            .fetch_items_page(&ProjectId::new("PVT_1"), "label:bug", POLL_PAGE, None)
            .await
            .unwrap();
        assert_eq!(
            (page.nodes.len(), page.total, page.next.as_deref()),
            (1, 250, Some("c1"))
        );
        let req = &t.requests()[0];
        assert_eq!(
            (
                req.variables["id"].as_str(),
                req.variables["query"].as_str(),
                req.variables["first"].as_u64()
            ),
            (Some("PVT_1"), Some("label:bug"), Some(20))
        );
    }

    #[tokio::test]
    async fn last_page_has_no_cursor() {
        let t = Arc::new(FixtureTransport::new());
        t.push("ViewItemIds", json!({"node": {"items": {"totalCount": 2,
            "pageInfo": {"hasNextPage": false, "endCursor": "c9"}, "nodes": [{"id": "a"}, {"id": "b"}]}}}));
        let page = gh(t.clone())
            .fetch_view_ids_page(&ProjectId::new("P"), "", Some("c8".into()))
            .await
            .unwrap();
        assert_eq!(page.nodes, vec![ItemId::new("a"), ItemId::new("b")]);
        assert_eq!(page.next, None);
        assert_eq!(t.requests()[0].variables["first"], 100);
    }

    #[tokio::test]
    async fn items_page_cost_reads_the_dry_run_cost() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ItemsPageCost",
            json!({"rateLimit": {"cost": 7}, "node": null}),
        );
        let cost = gh(t.clone())
            .items_page_cost(&ProjectId::new("PVT_1"), FULL_PAGE)
            .await
            .unwrap();
        assert_eq!(cost, 7);
        let req = &t.requests()[0];
        assert_eq!(req.variables["first"], 100);
        assert!(req.query.contains("dryRun: true"), "{}", req.query);
    }

    #[tokio::test]
    async fn items_page_cost_without_a_cost_is_a_decode_error() {
        let t = Arc::new(FixtureTransport::new());
        t.push("ItemsPageCost", json!({"rateLimit": null}));
        let r = gh(t).items_page_cost(&ProjectId::new("P"), FULL_PAGE).await;
        assert!(matches!(r, Err(GithubError::Decode(_))), "{r:?}");
    }

    #[tokio::test]
    async fn hydrate_skips_nulls() {
        let t = Arc::new(FixtureTransport::new());
        t.push("HydrateItems", json!({"nodes": [item_json(), null]}));
        let items = gh(t.clone())
            .hydrate_items(&[ItemId::new("PVTI_1"), ItemId::new("gone")])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(t.requests()[0].variables["ids"], json!(["PVTI_1", "gone"]));
    }

    #[tokio::test]
    async fn missing_project_node_is_a_decode_error_not_an_empty_page() {
        let t = Arc::new(FixtureTransport::new());
        t.push("ItemsPage", json!({"node": null}));
        t.push("ViewItemIds", json!({"node": null}));
        let g = gh(t);
        let p = ProjectId::new("PVT_x");
        assert!(matches!(
            g.fetch_items_page(&p, "", FULL_PAGE, None).await,
            Err(GithubError::Decode(_))
        ));
        assert!(matches!(
            g.fetch_view_ids_page(&p, "", None).await,
            Err(GithubError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn hydrate_without_nodes_array_is_a_decode_error() {
        let t = Arc::new(FixtureTransport::new());
        t.push("HydrateItems", json!({"nodes": null}));
        let r = gh(t).hydrate_items(&[ItemId::new("a")]).await;
        assert!(matches!(r, Err(GithubError::Decode(_))), "{r:?}");
    }

    fn summary_json(owner: &str, number: u32, closed: bool) -> serde_json::Value {
        json!({"id": format!("PVT_{number}"), "number": number, "title": format!("Board {number}"), "closed": closed,
               "owner": {"__typename": "User", "login": owner}})
    }

    #[tokio::test]
    async fn lists_repo_projects() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "RepoProjects",
            json!({"repository": {"projectsV2": {"nodes": [summary_json("tviles", 3, false)]}}}),
        );
        let list = gh(t.clone())
            .list_repo_projects(&"tviles/app".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(list[0].board.to_string(), "tviles/3");
        assert_eq!(t.requests()[0].variables["name"], "app");
    }

    #[tokio::test]
    async fn lists_viewer_and_org_projects() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ViewerProjects",
            json!({"viewer": {"login": "tviles",
                "projectsV2": {"nodes": [summary_json("tviles", 1, false)]},
                "organizations": {"nodes": [{"login": "acme", "projectsV2": {"nodes": [summary_json("acme", 7, true)]}}]}}}),
        );
        let list = gh(t).list_viewer_projects().await.unwrap();
        let boards: Vec<_> = list
            .iter()
            .map(|p| (p.board.to_string(), p.closed))
            .collect();
        assert_eq!(
            boards,
            [
                ("tviles/1".to_string(), false),
                ("acme/7".to_string(), true)
            ]
        );
    }

    #[tokio::test]
    async fn item_detail_without_a_node_is_a_decode_error() {
        let t = Arc::new(FixtureTransport::new());
        t.push("ItemDetail", json!({"node": null}));
        let err = Github::new(t)
            .fetch_item_detail(&ItemId::new("x"), None)
            .await
            .unwrap_err();
        assert!(matches!(err, GithubError::Decode(_)));
    }
}
