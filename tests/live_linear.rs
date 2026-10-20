//! Live Linear suite against the testbed workspace (tests/linear-testbed.toml). The key comes
//! only from PB_LINEAR_TESTBED_KEY or the Keychain item `project-boards-linear-testbed`, and
//! `guarded()` checks the workspace before any other request. Record the fixtures with:
//!   PB_RECORD=1 cargo test --features live --test live_linear record_board_fixtures -- --ignored --nocapture
#![cfg(feature = "live")]

#[path = "common/linear_testbed.rs"]
mod linear_testbed;

use graphql_client::GraphQLQuery;
use linear_testbed::{Raw, check_workspace, resolve_key, testbed};
use project_boards::error::BackendError;
use project_boards::linear::Linear;
use project_boards::linear::client::MAX_PAGE;
use project_boards::linear::record::scrubbing_recorder;
use project_boards::model::*;
use project_boards::transport::{HttpTransport, Transport};
use project_boards_queries::linear as ops;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// The views Tyler set up on team TBA for the vocabulary check (Decision 20).
const FIXTURE_VIEWS: [&str; 4] = [
    "Fixture by project",
    "Fixture by cycle",
    "Fixture by type",
    "Fixture by due",
];

static TRANSPORT: OnceLock<Arc<dyn Transport>> = OnceLock::new();
static GUARDED: AtomicBool = AtomicBool::new(false);

/// One transport over the testbed key per run, so the key is resolved once.
fn transport() -> Arc<dyn Transport> {
    TRANSPORT
        .get_or_init(|| {
            let key = resolve_key().unwrap_or_else(|e| panic!("{e}"));
            Arc::new(HttpTransport::linear(key))
        })
        .clone()
}

/// The testbed transport, for clients built after `guarded()` has checked the workspace.
fn http() -> Arc<dyn Transport> {
    assert!(
        GUARDED.load(Ordering::SeqCst),
        "call guarded() before any other request"
    );
    transport()
}

/// The app's Linear client over the testbed key. Its first request is the viewer check, and
/// it panics before any other unless the key reaches the testbed; the other workspace is
/// not named.
async fn guarded() -> Linear {
    let linear = Linear::new(transport());
    let viewer = linear
        .viewer()
        .await
        .unwrap_or_else(|e| panic!("the workspace check failed: {}", e.kind()));
    check_workspace(&viewer.url_key, &testbed()).unwrap_or_else(|e| panic!("{e}"));
    GUARDED.store(true, Ordering::SeqCst);
    linear
}

fn recording() -> bool {
    std::env::var("PB_RECORD").as_deref() == Ok("1")
}

/// A client for one scenario; with PB_RECORD=1 it records scrubbed responses into
/// tests/fixtures/recorded/linear/<name>/, cleared first.
fn recording_linear(name: &str, inner: Arc<dyn Transport>) -> Linear {
    if recording() {
        let dir = std::path::Path::new("tests/fixtures/recorded/linear").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Linear::new(Arc::new(scrubbing_recorder(inner, dir, &testbed().url_key)))
    } else {
        Linear::new(inner)
    }
}

/// An error as the kind, the server's error types and messages; never a key or a person.
fn describe(e: &BackendError) -> String {
    match e {
        BackendError::Api(entries) => {
            let entries: Vec<String> = entries
                .iter()
                .map(|e| format!("{}: {}", e.kind.as_deref().unwrap_or("-"), e.message))
                .collect();
            format!("Api [{}]", entries.join("; "))
        }
        other => other.kind().to_string(),
    }
}

fn outcome<T>(r: &Result<T, BackendError>, ok: impl Fn(&T) -> String) -> String {
    match r {
        Ok(v) => format!("ok ({})", ok(v)),
        Err(e) => format!("error: {}", describe(e)),
    }
}

/// Panics with the error's description (no key, no person) when a recording step fails.
fn must<T>(r: Result<T, BackendError>, what: &str) -> T {
    r.unwrap_or_else(|e| panic!("{what}: {}", describe(&e)))
}

async fn raw_post(raw: &linear_testbed::Linear, body: &impl serde::Serialize, what: &str) -> Raw {
    let r = raw
        .post(body)
        .await
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    if r.status != 200 || r.body["errors"].is_array() {
        panic!("{what}: HTTP {} errors {:?}", r.status, r.error_codes());
    }
    r
}

/// The All issues tab's list: `{}` ANDed with the board filter.
fn list_all() -> ListQuery {
    ListQuery {
        filter: ViewFilter::Linear(json!({})),
        include_archived: false,
    }
}

/// Everything the controller checks against Decision 20 and the open live questions.
/// Read-only; nothing here is recorded.
async fn print_vocabulary(app: &Linear, raw: &linear_testbed::Linear) -> BoardKey {
    let bed = testbed();

    // Rate limits: the reset header's magnitude and the limit types.
    let viewer = raw_post(
        raw,
        &ops::LinearViewer::build_query(ops::linear_viewer::Variables),
        "LinearViewer",
    )
    .await;
    let names: Vec<&str> = viewer.headers.iter().map(|(n, _)| n.as_str()).collect();
    println!("rate headers present: {names:?}");
    match viewer.header("x-ratelimit-requests-reset") {
        Some(v) => {
            let digits = v.trim().chars().filter(char::is_ascii_digit).count();
            let unit = match digits {
                13 => "milliseconds",
                10 => "seconds",
                _ => "unknown",
            };
            println!("x-ratelimit-requests-reset: {digits} digits ({unit})");
        }
        None => println!("x-ratelimit-requests-reset: absent"),
    }
    let types: Vec<&str> = viewer.data()["rateLimitStatus"]["limits"]
        .as_array()
        .map(|l| l.iter().filter_map(|l| l["type"].as_str()).collect())
        .unwrap_or_default();
    println!("rateLimitStatus.limits[].type: {types:?}");
    let v = app.viewer().await;
    println!(
        "app viewer(): {}",
        outcome(&v, |v| format!(
            "requests remaining {:?} of {:?}",
            v.requests_remaining, v.requests_limit
        ))
    );

    // Team TBA's id, then its shared views' raw preferences.
    let teams = raw_post(
        raw,
        &ops::TeamByKey::build_query(ops::team_by_key::Variables {
            key: bed.team_a.clone(),
        }),
        "TeamByKey",
    )
    .await;
    let team_id = teams.data()["teams"]["nodes"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("team {} not found", bed.team_a))
        .to_string();
    let team = BoardKey::LinearTeam {
        id: team_id.clone(),
    };
    let schema = raw_post(
        raw,
        &ops::BoardSchema::build_query(ops::board_schema::Variables {
            team_ids: vec![team_id],
        }),
        "BoardSchema",
    )
    .await;
    let views: Vec<&Value> = schema.data()["customViews"]["nodes"]
        .as_array()
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    for name in FIXTURE_VIEWS {
        let Some(view) = views.iter().find(|v| v["name"] == name) else {
            println!("{name}: not among team {}'s shared issue views", bed.team_a);
            continue;
        };
        let p = &view["viewPreferencesValues"];
        let s = |key: &str| p[key].as_str().map(String::from);
        println!(
            "{name}: modelName {:?} layout {:?} issueGrouping {:?} issueGroupingLabelGroupId {} \
             issueSubGrouping {:?} viewOrdering {:?} viewOrderingDirection {:?} \
             showCompletedIssues {:?}",
            view["modelName"].as_str(),
            s("layout"),
            s("issueGrouping"),
            if p["issueGroupingLabelGroupId"].is_string() {
                "present"
            } else {
                "absent"
            },
            s("issueSubGrouping"),
            s("viewOrdering"),
            s("viewOrderingDirection"),
            s("showCompletedIssues"),
        );
    }

    // Is `{"and": [board_filter, {}]}` (the All issues tab) accepted?
    let all = app.view_ids_page(&team, &list_all(), None).await;
    println!(
        "IssueIds with {{\"and\": [board filter, {{}}]}}: {}",
        outcome(&all, |p| format!("{} ids", p.nodes.len()))
    );

    // Does `project(id: <slugId>)` resolve?
    let boards = must(app.list_boards().await, "ListBoards");
    let project = boards
        .iter()
        .find(|b| b.title == bed.project && matches!(b.board, BoardKey::LinearProject { .. }))
        .unwrap_or_else(|| panic!("project `{}` not in ListBoards", bed.project));
    let BoardKey::LinearProject { id: project_id } = &project.board else {
        unreachable!()
    };
    let resolved = raw_post(
        raw,
        &ops::ResolveProjectBoard::build_query(ops::resolve_project_board::Variables {
            id: project_id.clone(),
        }),
        "ResolveProjectBoard",
    )
    .await;
    match resolved.data()["project"]["slugId"].as_str() {
        Some(slug) => {
            let by_slug = format!("linear:project:{slug}")
                .parse::<BoardKey>()
                .expect("a slug makes a board key");
            let r = app.resolve(&by_slug).await;
            println!(
                "resolve linear:project:<slugId>: {}",
                outcome(&r, |p| format!(
                    "canonical board is the UUID: {}",
                    p.board == project.board
                ))
            );
        }
        None => println!("resolve linear:project:<slugId>: the project has no slugId"),
    }

    team
}

/// The id of an issue on `team` with comments, or `None` when no issue has any.
async fn commented_issue(app: &Linear, team: &BoardKey) -> Option<ItemId> {
    let query = ListQuery {
        filter: ViewFilter::Linear(json!({"comments": {"length": {"gt": 0}}})),
        include_archived: false,
    };
    let r = app.view_ids_page(team, &query, None).await;
    println!(
        "issues with comments on {}: {}",
        testbed().team_a,
        outcome(&r, |p| format!("{} ids", p.nodes.len()))
    );
    r.ok().and_then(|p| p.nodes.into_iter().next())
}

/// The ids of every tab, recorded; prints each tab's count.
async fn record_view_ids(linear: &Linear, project: &Project) {
    for view in &project.views {
        let query = ListQuery {
            filter: view.filter.clone(),
            include_archived: false,
        };
        let page = must(
            linear.view_ids_page(&project.board, &query, None).await,
            &format!("view ids of {}", view.name),
        );
        println!(
            "  tab {} `{}`: {} ids",
            view.number,
            view.name,
            page.nodes.len()
        );
    }
}

#[tokio::test]
#[ignore = "records Linear fixtures from the testbed: PB_RECORD=1 ... -- --ignored --nocapture"]
async fn record_board_fixtures() {
    let app = guarded().await;
    let bed = testbed();
    let raw = linear_testbed::Linear::connect(&bed)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let team = print_vocabulary(&app, &raw).await;
    let commented = commented_issue(&app, &team).await;

    // team-board: resolve by key, the first issues page, every tab's ids, one detail.
    let linear = recording_linear("team-board", http());
    let key = BoardKey::LinearTeam {
        id: bed.team_a.clone(),
    };
    let project = must(linear.resolve(&key).await, "resolve team");
    assert_eq!(
        project.board, team,
        "the team key resolves to the team's id"
    );
    for view in project
        .views
        .iter()
        .filter(|v| v.name.starts_with("Fixture by"))
    {
        println!(
            "app maps {}: layout {:?} group_by {:?} vertical_group_by {:?} sort_by {:?}",
            view.name, view.layout, view.group_by, view.vertical_group_by, view.sort_by
        );
    }
    let page = must(
        linear.issues_page(&project.board, MAX_PAGE, None).await,
        "team issues",
    );
    println!(
        "team-board: {} tabs, {} issues",
        project.views.len(),
        page.nodes.len()
    );
    record_view_ids(&linear, &project).await;
    let detail_of = match commented {
        Some(id) => id,
        None => {
            println!("no issue has comments; recording the first issue's detail");
            page.nodes.first().expect("team issues").id.clone()
        }
    };
    let detail = must(linear.detail(&detail_of, None).await, "detail");
    println!("team-board detail: {} comments", detail.comments.len());

    // project-board: found by name through ListBoards, resolved, first page, every tab's ids.
    let linear = recording_linear("project-board", http());
    let boards = must(linear.list_boards().await, "ListBoards");
    let summary = boards
        .iter()
        .find(|b| b.title == bed.project && matches!(b.board, BoardKey::LinearProject { .. }))
        .unwrap_or_else(|| panic!("project `{}` not in ListBoards", bed.project));
    let project = must(linear.resolve(&summary.board).await, "resolve project");
    let page = must(
        linear.issues_page(&project.board, MAX_PAGE, None).await,
        "project issues",
    );
    println!(
        "project-board: {} tabs, {} issues",
        project.views.len(),
        page.nodes.len()
    );
    record_view_ids(&linear, &project).await;

    // boards: the picker's list.
    let boards = must(
        recording_linear("boards", http()).list_boards().await,
        "ListBoards",
    );
    println!("boards: {}", boards.len());

    // poll: team TBA's issues updated in the last hour.
    let since = (time::OffsetDateTime::now_utc() - time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("a formattable time");
    let polled = must(
        recording_linear("poll", http())
            .poll_page(&team, &since, MAX_PAGE, None)
            .await,
        "poll",
    );
    println!(
        "poll: {} issues updated in the last hour",
        polled.nodes.len()
    );
}
