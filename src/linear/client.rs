//! The one Linear client the rest of the app uses. Methods return model types only.

use crate::error::{BackendError, GraphqlErrorEntry};
use crate::github::convert::{nodes, str_of};
use crate::linear::convert::{self, PolledIssue, TeamInfo};
use crate::linear::ids::board_filter;
use crate::linear::views::{builtin_views, custom_view, hide_closed_filter};
use crate::model::*;
use crate::transport::{GraphqlRequest, Page, RateInfo, Transport};
use graphql_client::GraphQLQuery;
use project_boards_queries::linear as ops;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The largest `first` Linear accepts (finding L2); larger requests are clamped to it.
pub const MAX_PAGE: u32 = 250;
/// Ids per view-list page.
pub const VIEW_IDS_PAGE: u32 = MAX_PAGE;

/// What `viewer()` reports: the workspace the key reaches and its request budget.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearViewer {
    pub url_key: String,
    pub requests_remaining: Option<u32>,
    pub requests_limit: Option<u32>,
}

pub struct Linear {
    transport: Arc<dyn Transport>,
    rate: Mutex<RateInfo>,
}

/// The not-found error GitHub's resolve also returns, so `is_not_found` reads both.
fn not_found(board: &BoardKey) -> BackendError {
    BackendError::Api(vec![GraphqlErrorEntry {
        message: format!("board {board} was not found, or this API key cannot see it"),
        kind: Some("NOT_FOUND".into()),
    }])
}

/// Linear's answer to `project(id:)` or `team(id:)` for an id it does not know.
fn is_entity_not_found(e: &BackendError) -> bool {
    matches!(e, BackendError::Api(entries) if entries.iter().any(|e| {
        e.kind.as_deref() == Some("INPUT_ERROR") && e.message.starts_with("Entity not found")
    }))
}

/// The board's own `IssueFilter`; a GitHub board has none.
fn linear_filter(board: &BoardKey) -> Result<Value, BackendError> {
    match board_filter(board) {
        Value::Null => Err(BackendError::Decode(format!(
            "{board} is not a Linear board"
        ))),
        filter => Ok(filter),
    }
}

/// True for a Linear UUID (`8-4-4-4-12` hex digits); team keys and project slugs are not.
fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// A connection's page of `decode`d nodes; Linear connections have no count, so `total` is
/// 0 (Decision 8). A connection that is not an object is a decode error naming `what`.
fn page_of<T>(
    conn: &Value,
    what: &str,
    decode: impl Fn(&Value) -> Option<T>,
) -> Result<Page<T>, BackendError> {
    if !conn.is_object() {
        return Err(BackendError::Decode(format!(
            "{what} response has no issue connection"
        )));
    }
    let info = &conn["pageInfo"];
    let next = (info["hasNextPage"].as_bool() == Some(true))
        .then(|| info["endCursor"].as_str().map(String::from))
        .flatten();
    let nodes = conn["nodes"]
        .as_array()
        .map(|a| a.iter().filter_map(&decode).collect())
        .unwrap_or_default();
    Ok(Page {
        nodes,
        total: 0,
        next,
    })
}

fn id_of(v: &Value) -> Option<ItemId> {
    v["id"].as_str().map(ItemId::new)
}

/// The board a Linear `ProjectId` names (Decision 7: the id is the canonical board key).
pub fn board_of(project: &ProjectId) -> Result<BoardKey, BackendError> {
    match project.as_str().parse::<BoardKey>() {
        Ok(board @ (BoardKey::LinearProject { .. } | BoardKey::LinearTeam { .. })) => Ok(board),
        Ok(BoardKey::Github(_)) | Err(_) => Err(BackendError::Decode(format!(
            "{} is not a Linear board",
            project.as_str()
        ))),
    }
}

impl Linear {
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

    /// Sends `request`, remembers the rate it reports, and returns its data.
    async fn data(&self, request: GraphqlRequest) -> Result<Value, BackendError> {
        let response = self.transport.execute(request).await?;
        *self.rate.lock().unwrap() = response.rate;
        Ok(response.data)
    }

    /// The board's project with its fields and tabs. The returned project's board is the
    /// canonical key: a team key or project slug becomes the UUID.
    pub async fn resolve(&self, board: &BoardKey) -> Result<Project, BackendError> {
        match board {
            BoardKey::LinearProject { id } => self.resolve_project(board, id).await,
            BoardKey::LinearTeam { id } => self.resolve_team(board, id).await,
            BoardKey::Github(_) => Err(BackendError::Decode(format!(
                "{board} is not a Linear board"
            ))),
        }
    }

    /// Runs a resolve query, turning "Entity not found" into `board`'s not-found error.
    async fn resolve_data(
        &self,
        board: &BoardKey,
        request: GraphqlRequest,
    ) -> Result<Value, BackendError> {
        self.data(request).await.map_err(|e| {
            if is_entity_not_found(&e) {
                not_found(board)
            } else {
                e
            }
        })
    }

    async fn resolve_project(&self, board: &BoardKey, id: &str) -> Result<Project, BackendError> {
        // `project(id:)` takes a UUID or a slug alike.
        let body = ops::ResolveProjectBoard::build_query(ops::resolve_project_board::Variables {
            id: id.to_string(),
        });
        let data = self
            .resolve_data(board, GraphqlRequest::from_body(body))
            .await?;
        let project = &data["project"];
        let Some(uuid) = project["id"].as_str() else {
            return Err(not_found(board));
        };
        let teams: Vec<TeamInfo> = nodes(project, "teams")
            .into_iter()
            .map(|t| TeamInfo {
                id: str_of(&t["id"]),
                key: str_of(&t["key"]),
                cycles_enabled: t["cyclesEnabled"].as_bool() == Some(true),
            })
            .collect();
        // Facet views are not filtered by model in the query; only issue views are tabs.
        let project_views: Vec<&Value> = project["facets"]
            .as_array()
            .map(|facets| {
                facets
                    .iter()
                    .map(|f| &f["targetCustomView"])
                    .filter(|v| v.is_object() && v["modelName"] == "Issue")
                    .collect()
            })
            .unwrap_or_default();
        let schema = self.schema(&teams).await?;
        let fields = convert::fields_from_wire(&schema, &teams, None);
        let views = tabs(&project_views, &schema, &fields);
        Ok(convert::project_from_wire(
            &BoardKey::LinearProject { id: uuid.into() },
            &str_of(&project["name"]),
            &str_of(&project["url"]),
            meta(&data, &teams),
            fields,
            views,
        ))
    }

    async fn resolve_team(&self, board: &BoardKey, id: &str) -> Result<Project, BackendError> {
        let uuid = if is_uuid(id) {
            id.to_string()
        } else {
            self.team_by_key(board, id).await?
        };
        let body = ops::ResolveTeamBoard::build_query(ops::resolve_team_board::Variables {
            id: uuid.clone(),
        });
        let data = self
            .resolve_data(board, GraphqlRequest::from_body(body))
            .await?;
        let team = &data["team"];
        let Some(uuid) = team["id"].as_str() else {
            return Err(not_found(board));
        };
        let info = TeamInfo {
            id: uuid.to_string(),
            key: str_of(&team["key"]),
            cycles_enabled: team["cyclesEnabled"].as_bool() == Some(true),
        };
        let teams = [info];
        let schema = self.schema(&teams).await?;
        let body = ops::TeamProjects::build_query(ops::team_projects::Variables {
            team_id: uuid.to_string(),
        });
        let projects = self.data(GraphqlRequest::from_body(body)).await?;
        let fields = convert::fields_from_wire(&schema, &teams, Some(&projects));
        let views = tabs(&[], &schema, &fields);
        // Team has no url field; this is the team's All issues page.
        let url = format!(
            "https://linear.app/{}/team/{}/all",
            str_of(&data["organization"]["urlKey"]),
            teams[0].key
        );
        Ok(convert::project_from_wire(
            &BoardKey::LinearTeam { id: uuid.into() },
            &str_of(&team["name"]),
            &url,
            meta(&data, &teams),
            fields,
            views,
        ))
    }

    /// The UUID of the team with `key`. Team keys are uppercase and the lookup is
    /// case-sensitive, so `eng` is looked up as `ENG`.
    async fn team_by_key(&self, board: &BoardKey, key: &str) -> Result<String, BackendError> {
        let body = ops::TeamByKey::build_query(ops::team_by_key::Variables {
            key: key.to_ascii_uppercase(),
        });
        let data = self.data(GraphqlRequest::from_body(body)).await?;
        nodes(&data, "teams")
            .first()
            .and_then(|t| t["id"].as_str())
            .map(String::from)
            .ok_or_else(|| not_found(board))
    }

    /// The `BoardSchema` response for `teams`.
    async fn schema(&self, teams: &[TeamInfo]) -> Result<Value, BackendError> {
        let body = ops::BoardSchema::build_query(ops::board_schema::Variables {
            team_ids: teams.iter().map(|t| t.id.clone()).collect(),
        });
        self.data(GraphqlRequest::from_body(body)).await
    }

    /// One page of the board's issues, `first` at a time (at most 250).
    pub async fn issues_page(
        &self,
        board: &BoardKey,
        first: u32,
        after: Option<String>,
    ) -> Result<Page<Item>, BackendError> {
        let filter = linear_filter(board)?;
        let request = GraphqlRequest::raw(
            ops::board_issues::OPERATION_NAME,
            ops::board_issues::QUERY,
            json!({"filter": filter, "first": first.min(MAX_PAGE), "after": after}),
        );
        let data = self.data(request).await?;
        page_of(&data["issues"], "BoardIssues", convert::item_from_wire)
    }

    /// One page of the board's issues updated after `since` (an ISO 8601 date-time),
    /// archived and trashed ones included.
    pub async fn poll_page(
        &self,
        board: &BoardKey,
        since: &str,
        first: u32,
        after: Option<String>,
    ) -> Result<Page<PolledIssue>, BackendError> {
        let filter = json!({"and": [linear_filter(board)?, {"updatedAt": {"gt": since}}]});
        let request = GraphqlRequest::raw(
            ops::poll_issues::OPERATION_NAME,
            ops::poll_issues::QUERY,
            json!({"filter": filter, "first": first.min(MAX_PAGE), "after": after}),
        );
        let data = self.data(request).await?;
        page_of(&data["issues"], "PollIssues", convert::polled_from_wire)
    }

    /// One page of the ids a tab lists: a custom view's (Linear applies its stored filter)
    /// or a built-in tab's `IssueFilter`, both kept to the board's issues.
    pub async fn view_ids_page(
        &self,
        board: &BoardKey,
        query: &ListQuery,
        after: Option<String>,
    ) -> Result<Page<ItemId>, BackendError> {
        let board_filter = || linear_filter(board);
        let (request, conn): (GraphqlRequest, fn(&Value) -> &Value) = match &query.filter {
            ViewFilter::LinearView { id, hide_closed } => {
                let filter = if *hide_closed {
                    json!({"and": [board_filter()?, hide_closed_filter()]})
                } else {
                    board_filter()?
                };
                let request = GraphqlRequest::raw(
                    ops::custom_view_issue_ids::OPERATION_NAME,
                    ops::custom_view_issue_ids::QUERY,
                    json!({"id": id, "filter": filter, "first": VIEW_IDS_PAGE, "after": after,
                           "includeArchived": query.include_archived}),
                );
                (request, |data| &data["customView"]["issues"])
            }
            ViewFilter::Linear(f) => {
                let request = GraphqlRequest::raw(
                    ops::issue_ids::OPERATION_NAME,
                    ops::issue_ids::QUERY,
                    json!({"filter": {"and": [board_filter()?, f]}, "first": VIEW_IDS_PAGE,
                           "after": after, "includeArchived": query.include_archived}),
                );
                (request, |data| &data["issues"])
            }
            ViewFilter::Github(_) => {
                return Err(BackendError::Api(vec![GraphqlErrorEntry {
                    message: "this list does not belong to a Linear board".into(),
                    kind: None,
                }]));
            }
        };
        let operation = request.operation_name.clone();
        let data = self.data(request).await?;
        if matches!(query.filter, ViewFilter::LinearView { .. }) && !data["customView"].is_object()
        {
            // The view was deleted after its tab was cached.
            return Err(BackendError::Api(vec![GraphqlErrorEntry {
                message: "this view no longer exists in Linear".into(),
                kind: None,
            }]));
        }
        page_of(conn(&data), &operation, id_of)
    }

    /// Loads issues by id, archived ones included. Pass at most 100 ids; ids that no longer
    /// exist are skipped.
    pub async fn hydrate(&self, ids: &[ItemId]) -> Result<Vec<Item>, BackendError> {
        debug_assert!(ids.len() <= 100, "hydrate takes at most 100 ids");
        let body = ops::IssuesById::build_query(ops::issues_by_id::Variables {
            ids: ids.iter().map(|i| i.0.clone()).collect(),
        });
        let data = self.data(GraphqlRequest::from_body(body)).await?;
        let issues = data["issues"]["nodes"]
            .as_array()
            .ok_or_else(|| BackendError::Decode("IssuesById response has no issues".into()))?;
        Ok(issues.iter().filter_map(convert::item_from_wire).collect())
    }

    /// The issue's detail, with the 20 comments before `before` (the newest without it).
    pub async fn detail(
        &self,
        item: &ItemId,
        before: Option<String>,
    ) -> Result<ItemDetail, BackendError> {
        let body = ops::IssueDetail::build_query(ops::issue_detail::Variables {
            id: item.0.clone(),
            before,
        });
        let data = self.data(GraphqlRequest::from_body(body)).await?;
        if !data["issue"].is_object() {
            return Err(BackendError::Decode(
                "IssueDetail response has no issue".into(),
            ));
        }
        Ok(convert::detail_from_wire(&data["issue"]))
    }

    /// The workspace's open projects, then its teams.
    pub async fn list_boards(&self) -> Result<Vec<ProjectSummary>, BackendError> {
        let body = ops::ListBoards::build_query(ops::list_boards::Variables);
        let data = self.data(GraphqlRequest::from_body(body)).await?;
        let summary = |board: BoardKey, title: String| ProjectSummary {
            id: ProjectId::new(board.to_string()),
            board,
            title,
            closed: false,
        };
        let projects = nodes(&data, "projects").into_iter().filter_map(|p| {
            let id = p["id"].as_str()?;
            Some(summary(
                BoardKey::LinearProject { id: id.into() },
                str_of(&p["name"]),
            ))
        });
        let teams = nodes(&data, "teams").into_iter().filter_map(|t| {
            let id = t["id"].as_str()?;
            Some(summary(
                BoardKey::LinearTeam { id: id.into() },
                format!("{} ({})", str_of(&t["name"]), str_of(&t["key"])),
            ))
        });
        Ok(projects.chain(teams).collect())
    }

    /// The workspace the key reaches, and its request budget from `rateLimitStatus`.
    pub async fn viewer(&self) -> Result<LinearViewer, BackendError> {
        let body = ops::LinearViewer::build_query(ops::linear_viewer::Variables);
        let data = self.data(GraphqlRequest::from_body(body)).await?;
        let requests = data["rateLimitStatus"]["limits"]
            .as_array()
            .and_then(|limits| {
                limits.iter().find(|l| {
                    l["type"]
                        .as_str()
                        .is_some_and(|t| t.to_ascii_lowercase().contains("request"))
                })
            });
        let amount = |key: &str| {
            requests
                .and_then(|l| l[key].as_f64())
                .filter(|n| *n >= 0.0)
                .map(|n| n.min(f64::from(u32::MAX)) as u32)
        };
        Ok(LinearViewer {
            url_key: str_of(&data["organization"]["urlKey"]),
            requests_remaining: amount("remainingAmount"),
            requests_limit: amount("allowedAmount"),
        })
    }
}

/// The resolve response's workspace, viewer and team keys.
fn meta(data: &Value, teams: &[TeamInfo]) -> LinearMeta {
    LinearMeta {
        url_key: str_of(&data["organization"]["urlKey"]),
        viewer_id: str_of(&data["viewer"]["id"]),
        team_keys: teams.iter().map(|t| t.key.clone()).collect(),
    }
}

/// The board's tabs: the built-in ones, then `project_views`, then the schema's team and
/// workspace views (Decision 2), numbered in that order; a view listed twice keeps its
/// first place.
fn tabs(project_views: &[&Value], schema: &Value, fields: &[Field]) -> Vec<View> {
    let mut views = builtin_views();
    let mut seen: HashSet<ViewId> = views.iter().map(|v| v.id.clone()).collect();
    let custom = project_views
        .iter()
        .copied()
        .chain(nodes(schema, "customViews"));
    for v in custom {
        let number = views.len() as u32 + 1;
        if let Some(view) = custom_view(v, fields, number)
            && seen.insert(view.id.clone())
        {
            views.push(view);
        }
    }
    views
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GraphqlErrorEntry;
    use crate::linear::convert::tests::{issue_json, schema_json, team_projects_json};
    use crate::linear::ids;
    use crate::linear::views::hide_closed_filter;
    use crate::transport::fixture::FixtureTransport;
    use serde_json::{Value, json};

    const TEAM_A: &str = "1b4e28ba-2fa1-4d3b-a3f5-ef19b5a7633b";
    const TEAM_B: &str = "6f1c9a2e-8b3d-4c5e-9f0a-1d2e3f4a5b6c";
    const PROJECT: &str = "9e8d7c6b-5a4f-4e3d-8c2b-1a0f9e8d7c6b";

    fn linear(t: &Arc<FixtureTransport>) -> Linear {
        Linear::new(t.clone())
    }

    fn team_a() -> BoardKey {
        BoardKey::LinearTeam { id: TEAM_A.into() }
    }

    fn project_board() -> BoardKey {
        BoardKey::LinearProject { id: PROJECT.into() }
    }

    /// `schema_json()` with the fixture teams' ids as UUIDs, and `views` as its custom views.
    fn schema_with(views: Vec<Value>) -> Value {
        let text = schema_json()
            .to_string()
            .replace("team-a", TEAM_A)
            .replace("team-b", TEAM_B);
        let mut schema: Value = serde_json::from_str(&text).unwrap();
        schema["customViews"]["nodes"] = Value::Array(views);
        schema
    }

    fn view_json(id: &str, name: &str, model: &str) -> Value {
        json!({"id": id, "name": name, "modelName": model, "team": null,
               "viewPreferencesValues": {"layout": "list"}})
    }

    fn team_board_json() -> Value {
        json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
               "team": {"id": TEAM_A, "key": "TBA", "name": "Testbed A", "cyclesEnabled": true}})
    }

    fn project_board_json(facets: Vec<Value>) -> Value {
        json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
               "project": {"id": PROJECT, "name": "Launch", "slugId": "3f2a",
                           "url": "https://linear.app/acme/project/launch-3f2a",
                           "teams": {"nodes": [
                               {"id": TEAM_A, "key": "TBA", "cyclesEnabled": true},
                               {"id": TEAM_B, "key": "TBB", "cyclesEnabled": false},
                           ]},
                           "facets": facets}})
    }

    fn operations(t: &FixtureTransport) -> Vec<String> {
        t.requests().into_iter().map(|r| r.operation_name).collect()
    }

    fn view_names(p: &Project) -> Vec<(u32, String)> {
        p.views.iter().map(|v| (v.number, v.name.clone())).collect()
    }

    fn has_field(p: &Project, id: &str) -> bool {
        p.fields.iter().any(|f| f.id.as_str() == id)
    }

    fn issues(nodes: Vec<Value>, next: Option<&str>) -> Value {
        json!({"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next.unwrap_or("end")},
               "nodes": nodes})
    }

    #[tokio::test]
    async fn resolve_team_by_key_then_uuid() {
        let t = Arc::new(FixtureTransport::new());
        t.push("TeamByKey", json!({"teams": {"nodes": [{"id": TEAM_A}]}}));
        t.push("ResolveTeamBoard", team_board_json());
        t.push(
            "BoardSchema",
            schema_with(vec![view_json("cv-team", "Team bugs", "Issue")]),
        );
        t.push("TeamProjects", team_projects_json());
        let p = linear(&t)
            .resolve(&BoardKey::LinearTeam { id: "TBA".into() })
            .await
            .unwrap();
        assert_eq!(
            operations(&t),
            [
                "TeamByKey",
                "ResolveTeamBoard",
                "BoardSchema",
                "TeamProjects"
            ]
        );
        let requests = t.requests();
        assert_eq!(requests[0].variables["key"], "TBA");
        assert_eq!(requests[1].variables["id"], TEAM_A);
        assert_eq!(requests[2].variables["teamIds"], json!([TEAM_A]));
        assert_eq!(requests[3].variables["teamId"], TEAM_A);

        assert_eq!(p.board, team_a());
        assert_eq!(p.id, ProjectId::new(format!("linear:team:{TEAM_A}")));
        assert_eq!(p.title, "Testbed A");
        assert_eq!(p.url, "https://linear.app/acme/team/TBA/all");
        assert_eq!(
            p.linear,
            Some(LinearMeta {
                url_key: "acme".into(),
                viewer_id: "u-1".into(),
                team_keys: vec!["TBA".into()],
            })
        );
        assert!(has_field(&p, ids::PROJECT) && has_field(&p, ids::CYCLE));
        let status = p.field(&FieldId::new(ids::STATUS)).unwrap();
        let FieldKind::SingleSelect { options } = &status.kind else {
            panic!("Status is single-select");
        };
        assert!(!options.is_empty(), "the team's states are Status options");
        assert_eq!(
            view_names(&p),
            [
                (1, "Board".to_string()),
                (2, "All issues".into()),
                (3, "My issues".into()),
                (4, "Team bugs".into()),
            ]
        );
    }

    #[tokio::test]
    async fn a_team_uuid_needs_no_key_lookup() {
        let t = Arc::new(FixtureTransport::new());
        t.push("ResolveTeamBoard", team_board_json());
        t.push("BoardSchema", schema_with(vec![]));
        t.push("TeamProjects", team_projects_json());
        let p = linear(&t).resolve(&team_a()).await.unwrap();
        assert_eq!(
            operations(&t),
            ["ResolveTeamBoard", "BoardSchema", "TeamProjects"]
        );
        assert_eq!(p.board, team_a());
    }

    #[tokio::test]
    async fn resolve_project_by_slug_lists_project_views_first() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ResolveProjectBoard",
            project_board_json(vec![
                json!({"targetCustomView": view_json("cv-proj", "Project view", "Issue")}),
                json!({"targetCustomView": null}),
                json!({"targetCustomView": view_json("cv-doc", "Docs", "Document")}),
                json!({"targetCustomView": view_json("cv-shared", "Shared", "Issue")}),
            ]),
        );
        t.push(
            "BoardSchema",
            schema_with(vec![
                view_json("cv-team", "Team view", "Issue"),
                view_json("cv-shared", "Shared", "Issue"),
            ]),
        );
        let p = linear(&t)
            .resolve(&BoardKey::LinearProject {
                id: "launch-3f2a".into(),
            })
            .await
            .unwrap();
        assert_eq!(operations(&t), ["ResolveProjectBoard", "BoardSchema"]);
        let requests = t.requests();
        assert_eq!(requests[0].variables["id"], "launch-3f2a");
        assert_eq!(requests[1].variables["teamIds"], json!([TEAM_A, TEAM_B]));

        assert_eq!(p.board, project_board());
        assert_eq!(p.id, ProjectId::new(format!("linear:project:{PROJECT}")));
        assert_eq!(p.title, "Launch");
        assert_eq!(p.url, "https://linear.app/acme/project/launch-3f2a");
        assert_eq!(
            p.linear.as_ref().map(|m| m.team_keys.clone()),
            Some(vec!["TBA".to_string(), "TBB".into()])
        );
        assert!(
            !has_field(&p, ids::PROJECT),
            "a project board has no Project field"
        );
        assert_eq!(
            view_names(&p),
            [
                (1, "Board".to_string()),
                (2, "All issues".into()),
                (3, "My issues".into()),
                (4, "Project view".into()),
                (5, "Shared".into()),
                (6, "Team view".into()),
            ]
        );
    }

    fn not_found_text(board: &str) -> String {
        format!("board {board} was not found, or this API key cannot see it")
    }

    #[tokio::test]
    async fn a_missing_board_is_not_found() {
        let missing = BoardKey::LinearProject { id: "nope".into() };

        let t = Arc::new(FixtureTransport::new());
        t.push_error(
            "ResolveProjectBoard",
            BackendError::Api(vec![GraphqlErrorEntry {
                message: "Entity not found: Project".into(),
                kind: Some("INPUT_ERROR".into()),
            }]),
        );
        let err = linear(&t).resolve(&missing).await.unwrap_err();
        assert!(err.is_not_found(), "{err:?}");
        assert_eq!(
            err,
            BackendError::Api(vec![GraphqlErrorEntry {
                message: not_found_text("linear:project:nope"),
                kind: Some("NOT_FOUND".into()),
            }])
        );

        let t = Arc::new(FixtureTransport::new());
        let mut body = project_board_json(vec![]);
        body["project"] = Value::Null;
        t.push("ResolveProjectBoard", body);
        let err = linear(&t).resolve(&missing).await.unwrap_err();
        assert!(err.is_not_found(), "{err:?}");

        let t = Arc::new(FixtureTransport::new());
        t.push("TeamByKey", json!({"teams": {"nodes": []}}));
        let err = linear(&t)
            .resolve(&BoardKey::LinearTeam { id: "NOPE".into() })
            .await
            .unwrap_err();
        assert!(err.is_not_found(), "{err:?}");
        assert!(
            err.to_string()
                .contains(&not_found_text("linear:team:NOPE"))
        );
        assert_eq!(operations(&t), ["TeamByKey"]);

        let t = Arc::new(FixtureTransport::new());
        let mut body = team_board_json();
        body["team"] = Value::Null;
        t.push("ResolveTeamBoard", body);
        let err = linear(&t).resolve(&team_a()).await.unwrap_err();
        assert!(err.is_not_found(), "{err:?}");
    }

    #[tokio::test]
    async fn other_resolve_errors_pass_through() {
        let t = Arc::new(FixtureTransport::new());
        let other = BackendError::Api(vec![GraphqlErrorEntry {
            message: "Argument Validation Error".into(),
            kind: Some("INPUT_ERROR".into()),
        }]);
        t.push_error("ResolveProjectBoard", other.clone());
        let err = linear(&t).resolve(&project_board()).await.unwrap_err();
        assert_eq!(err, other);
        assert!(!err.is_not_found());
    }

    #[tokio::test]
    async fn a_github_board_is_refused_before_any_request() {
        let t = Arc::new(FixtureTransport::new());
        let gh = BoardKey::github("acme", 3);
        assert!(matches!(
            linear(&t).resolve(&gh).await,
            Err(BackendError::Decode(_))
        ));
        assert!(matches!(
            linear(&t).issues_page(&gh, 100, None).await,
            Err(BackendError::Decode(_))
        ));
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn issues_page_sends_the_board_filter_and_no_total() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "BoardIssues",
            json!({"issues": issues(vec![issue_json(), json!({"id": "broken"})], Some("c1"))}),
        );
        t.push("BoardIssues", json!({"issues": issues(vec![], None)}));
        let l = linear(&t);
        let page = l.issues_page(&team_a(), 100, None).await.unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0].id, ItemId::new("i-7"));
        assert_eq!(page.total, 0);
        assert_eq!(page.next.as_deref(), Some("c1"));
        let last = l
            .issues_page(&team_a(), 100, Some("c1".into()))
            .await
            .unwrap();
        assert_eq!(last.next, None);

        let requests = t.requests();
        let req = &requests[0];
        assert_eq!(
            req.query,
            project_boards_queries::linear::board_issues::QUERY
        );
        assert_eq!(
            req.variables["filter"],
            json!({"team": {"id": {"eq": TEAM_A}}})
        );
        assert_eq!(req.variables["first"], 100);
        assert_eq!(requests[1].variables["after"], "c1");
    }

    #[tokio::test]
    async fn poll_sends_updated_at_and_reports_trash() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "PollIssues",
            json!({"issues": issues(vec![issue_json(), json!({"id": "i-9", "trashed": true})], None)}),
        );
        let page = linear(&t)
            .poll_page(&project_board(), "2026-10-19T10:00:00.000Z", 50, None)
            .await
            .unwrap();
        assert_eq!(page.nodes.len(), 2);
        assert!(matches!(&page.nodes[0], PolledIssue::Item(i) if i.id == ItemId::new("i-7")));
        assert_eq!(page.nodes[1], PolledIssue::Trashed(ItemId::new("i-9")));
        assert_eq!((page.total, page.next), (0, None));
        let req = &t.requests()[0];
        assert_eq!(req.operation_name, "PollIssues");
        assert_eq!(
            req.variables["filter"],
            json!({"and": [
                {"project": {"id": {"eq": PROJECT}}},
                {"updatedAt": {"gt": "2026-10-19T10:00:00.000Z"}},
            ]})
        );
        assert_eq!(req.variables["first"], 50);
    }

    fn id_page(ids: &[&str]) -> Value {
        issues(ids.iter().map(|id| json!({"id": id})).collect(), None)
    }

    #[tokio::test]
    async fn a_lowercase_team_key_is_looked_up_in_uppercase() {
        let t = Arc::new(FixtureTransport::new());
        t.push("TeamByKey", json!({"teams": {"nodes": [{"id": TEAM_A}]}}));
        t.push("ResolveTeamBoard", team_board_json());
        t.push("BoardSchema", schema_with(vec![]));
        t.push("TeamProjects", team_projects_json());
        let p = linear(&t)
            .resolve(&BoardKey::LinearTeam { id: "tba".into() })
            .await
            .unwrap();
        assert_eq!(t.requests()[0].variables["key"], "TBA");
        assert_eq!(p.board, team_a());
    }

    #[tokio::test]
    async fn a_deleted_custom_view_says_so() {
        let t = Arc::new(FixtureTransport::new());
        t.push("CustomViewIssueIds", json!({"customView": null}));
        let query = ListQuery {
            filter: ViewFilter::LinearView {
                id: "cv-gone".into(),
                hide_closed: false,
            },
            include_archived: false,
        };
        let err = linear(&t)
            .view_ids_page(&team_a(), &query, None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            BackendError::Api(vec![GraphqlErrorEntry {
                message: "this view no longer exists in Linear".into(),
                kind: None,
            }])
        );
    }

    #[tokio::test]
    async fn view_ids_for_a_custom_view_and_for_a_tab() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "CustomViewIssueIds",
            json!({"customView": {"issues": id_page(&["a", "b"])}}),
        );
        t.push(
            "CustomViewIssueIds",
            json!({"customView": {"issues": id_page(&[])}}),
        );
        t.push("IssueIds", json!({"issues": id_page(&["c"])}));
        let l = linear(&t);
        let board = team_a();
        let view = |hide_closed| ListQuery {
            filter: ViewFilter::LinearView {
                id: "cv-1".into(),
                hide_closed,
            },
            include_archived: true,
        };
        let page = l.view_ids_page(&board, &view(false), None).await.unwrap();
        assert_eq!(page.nodes, [ItemId::new("a"), ItemId::new("b")]);
        assert_eq!((page.total, page.next), (0, None));
        l.view_ids_page(&board, &view(true), Some("c0".into()))
            .await
            .unwrap();
        let mine = json!({"assignee": {"isMe": {"eq": true}}});
        let tab = ListQuery {
            filter: ViewFilter::Linear(mine.clone()),
            include_archived: false,
        };
        let page = l.view_ids_page(&board, &tab, None).await.unwrap();
        assert_eq!(page.nodes, [ItemId::new("c")]);

        let bf = ids::board_filter(&board);
        let requests = t.requests();
        assert_eq!(
            operations(&t),
            ["CustomViewIssueIds", "CustomViewIssueIds", "IssueIds"]
        );
        let custom = &requests[0].variables;
        assert_eq!(custom["id"], "cv-1");
        assert_eq!(custom["filter"], bf);
        assert_eq!(custom["includeArchived"], true);
        let hidden = &requests[1].variables;
        assert_eq!(hidden["filter"], json!({"and": [bf, hide_closed_filter()]}));
        assert_eq!(hidden["after"], "c0");
        let ids = &requests[2].variables;
        assert_eq!(ids["filter"], json!({"and": [bf, mine]}));
        assert_eq!(ids["includeArchived"], false);
        for r in &requests {
            assert_eq!(r.variables["first"], 250, "{}", r.operation_name);
        }
    }

    #[tokio::test]
    async fn a_github_list_is_refused() {
        let t = Arc::new(FixtureTransport::new());
        let err = linear(&t)
            .view_ids_page(&team_a(), &ListQuery::github("label:bug"), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            BackendError::Api(vec![GraphqlErrorEntry {
                message: "this list does not belong to a Linear board".into(),
                kind: None,
            }])
        );
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn hydrate_asks_by_id() {
        let t = Arc::new(FixtureTransport::new());
        t.push("IssuesById", json!({"issues": {"nodes": [issue_json()]}}));
        let items = linear(&t)
            .hydrate(&[ItemId::new("i-7"), ItemId::new("i-8")])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, ItemId::new("i-7"));
        assert_eq!(t.requests()[0].variables["ids"], json!(["i-7", "i-8"]));
    }

    #[tokio::test]
    async fn detail_pages_comments_backwards() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "IssueDetail",
            json!({"issue": {
                "id": "i-7", "description": "The body", "parent": null,
                "children": {"nodes": []}, "attachments": {"nodes": []},
                "comments": {
                    "pageInfo": {"hasPreviousPage": true, "startCursor": "c-old"},
                    "nodes": [{"id": "k1", "body": "hi", "createdAt": "2026-10-15T10:00:00.000Z",
                               "user": {"displayName": "Ada Lovelace"}, "parent": null}],
                },
            }}),
        );
        t.push("IssueDetail", json!({"issue": null}));
        let l = linear(&t);
        let detail = l
            .detail(&ItemId::new("i-7"), Some("c-newer".into()))
            .await
            .unwrap();
        assert_eq!(detail.body, "The body");
        assert_eq!(detail.comments.len(), 1);
        assert_eq!(detail.older_cursor.as_deref(), Some("c-old"));
        let req = &t.requests()[0];
        assert_eq!(req.variables["id"], "i-7");
        assert_eq!(req.variables["before"], "c-newer");
        assert!(matches!(
            l.detail(&ItemId::new("gone"), None).await,
            Err(BackendError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn list_boards_puts_open_projects_then_teams() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "ListBoards",
            json!({"projects": {"nodes": [{"id": "p1", "name": "Launch"}]},
                   "teams": {"nodes": [{"id": "t1", "key": "TBA", "name": "Testbed A"}]}}),
        );
        let boards = linear(&t).list_boards().await.unwrap();
        assert_eq!(
            boards,
            [
                ProjectSummary {
                    id: ProjectId::new("linear:project:p1"),
                    board: BoardKey::LinearProject { id: "p1".into() },
                    title: "Launch".into(),
                    closed: false,
                },
                ProjectSummary {
                    id: ProjectId::new("linear:team:t1"),
                    board: BoardKey::LinearTeam { id: "t1".into() },
                    title: "Testbed A (TBA)".into(),
                    closed: false,
                },
            ]
        );
        let query = &t.requests()[0].query;
        assert!(
            query.contains(r#"nin: ["completed", "canceled"]"#),
            "{query}"
        );
    }

    #[tokio::test]
    async fn viewer_reports_the_workspace_and_request_budget() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "LinearViewer",
            json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
            "rateLimitStatus": {"limits": [
                {"type": "complexity", "remainingAmount": 2999000.0,
                 "allowedAmount": 3000000.0, "reset": 1760000000000.0},
                {"type": "requests", "remainingAmount": 1499.0,
                 "allowedAmount": 1500.0, "reset": 1760000000000.0},
            ]}}),
        );
        let v = linear(&t).viewer().await.unwrap();
        assert_eq!(
            v,
            LinearViewer {
                url_key: "acme".into(),
                requests_remaining: Some(1499),
                requests_limit: Some(1500),
            }
        );
    }

    #[tokio::test]
    async fn rate_is_remembered_from_the_last_response() {
        let t = Arc::new(FixtureTransport::new());
        let rate = |remaining| RateInfo {
            remaining: Some(remaining),
            limit: Some(1500),
            reset_epoch: None,
        };
        t.set_rate(rate(42));
        t.push(
            "LinearViewer",
            json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"},
                   "rateLimitStatus": {"limits": []}}),
        );
        t.push("BoardIssues", json!({"issues": issues(vec![], None)}));
        let l = linear(&t);
        assert_eq!(l.rate(), RateInfo::default());
        l.viewer().await.unwrap();
        assert_eq!(l.rate(), rate(42));
        t.set_rate(rate(41));
        l.issues_page(&team_a(), 100, None).await.unwrap();
        assert_eq!(l.rate(), rate(41));
    }

    #[test]
    fn board_of_reads_a_linear_project_id() {
        assert_eq!(
            board_of(&ProjectId::new(format!("linear:team:{TEAM_A}"))).unwrap(),
            team_a()
        );
        assert_eq!(
            board_of(&ProjectId::new(format!("linear:project:{PROJECT}"))).unwrap(),
            project_board()
        );
        for bad in ["acme/3", "PVT_kwHO", "linear:cycle:x"] {
            assert!(
                matches!(board_of(&ProjectId::new(bad)), Err(BackendError::Decode(_))),
                "{bad}"
            );
        }
    }

    /// Every literal `first: N` in a query's text.
    fn literal_firsts(query: &str) -> Vec<u64> {
        query
            .match_indices("first:")
            .filter_map(|(at, _)| {
                let rest = query[at + "first:".len()..].trim_start();
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .collect()
    }

    #[tokio::test]
    async fn no_request_sends_first_above_250() {
        let t = Arc::new(FixtureTransport::new());
        t.push("TeamByKey", json!({"teams": {"nodes": [{"id": TEAM_A}]}}));
        t.push("ResolveTeamBoard", team_board_json());
        t.push("BoardSchema", schema_with(vec![]));
        t.push("TeamProjects", team_projects_json());
        t.push("ResolveProjectBoard", project_board_json(vec![]));
        t.push("BoardSchema", schema_with(vec![]));
        t.push("BoardIssues", json!({"issues": issues(vec![], None)}));
        t.push("PollIssues", json!({"issues": issues(vec![], None)}));
        t.push(
            "CustomViewIssueIds",
            json!({"customView": {"issues": id_page(&[])}}),
        );
        t.push("IssueIds", json!({"issues": id_page(&[])}));
        t.push("IssuesById", json!({"issues": {"nodes": []}}));
        t.push(
            "IssueDetail",
            json!({"issue": {"id": "i-7", "comments": {"nodes": []}}}),
        );
        t.push(
            "ListBoards",
            json!({"projects": {"nodes": []}, "teams": {"nodes": []}}),
        );
        t.push(
            "LinearViewer",
            json!({"viewer": {"id": "u-1"}, "organization": {"urlKey": "acme"}}),
        );
        let l = linear(&t);
        l.resolve(&BoardKey::LinearTeam { id: "TBA".into() })
            .await
            .unwrap();
        l.resolve(&project_board()).await.unwrap();
        // Callers asking for more than Linear allows get 250.
        l.issues_page(&team_a(), 1000, None).await.unwrap();
        l.poll_page(&team_a(), "2026-10-19T00:00:00Z", 1000, None)
            .await
            .unwrap();
        let view = ListQuery {
            filter: ViewFilter::LinearView {
                id: "cv-1".into(),
                hide_closed: false,
            },
            include_archived: false,
        };
        l.view_ids_page(&team_a(), &view, None).await.unwrap();
        let tab = ListQuery {
            filter: ViewFilter::Linear(json!({})),
            include_archived: false,
        };
        l.view_ids_page(&team_a(), &tab, None).await.unwrap();
        l.hydrate(&[ItemId::new("i-7")]).await.unwrap();
        l.detail(&ItemId::new("i-7"), None).await.unwrap();
        l.list_boards().await.unwrap();
        l.viewer().await.unwrap();

        let requests = t.requests();
        assert_eq!(requests.len(), 14, "{:?}", operations(&t));
        for r in &requests {
            if let Some(first) = r.variables.get("first") {
                assert!(
                    first.as_u64().is_some_and(|n| n <= 250),
                    "{} sends first {first}",
                    r.operation_name
                );
            }
            for n in literal_firsts(&r.query) {
                assert!(n <= 250, "{} has first: {n}", r.operation_name);
            }
        }
        assert_eq!(requests[6].variables["first"], 250);
        assert_eq!(requests[7].variables["first"], 250);
    }
}
