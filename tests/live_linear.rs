//! Live Linear suite against the testbed workspace (tests/linear-testbed.toml). The key comes
//! only from PB_LINEAR_TESTBED_KEY or the Keychain item `project-boards-linear-testbed`, and
//! `guarded()` checks the workspace before any other request. Every test here is ignored, so
//! the full check never reaches Linear. Run the suite with:
//!   cargo test --features live --test live_linear -- --ignored --nocapture --skip record_board_fixtures
//! and record the fixtures with:
//!   PB_RECORD=1 cargo test --features live --test live_linear record_board_fixtures -- --ignored --nocapture
#![cfg(feature = "live")]

#[path = "common/linear_testbed.rs"]
mod linear_testbed;

use futures::FutureExt;
use graphql_client::GraphQLQuery;
use linear_testbed::{Raw, check_workspace, resolve_key, testbed};
use project_boards::error::BackendError;
use project_boards::linear::Linear;
use project_boards::linear::client::MAX_PAGE;
use project_boards::linear::convert::PolledIssue;
use project_boards::linear::ids;
use project_boards::linear::record::scrubbing_recorder;
use project_boards::model::*;
use project_boards::transport::{HttpTransport, Transport};
use project_boards_queries::linear as ops;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The views Tyler set up on team TBA for the vocabulary check (Decision 20).
const FIXTURE_VIEWS: [&str; 4] = [
    "Fixture by project",
    "Fixture by cycle",
    "Fixture by type",
    "Fixture by due",
];

static KEY: OnceLock<String> = OnceLock::new();
static GUARDED: AtomicBool = AtomicBool::new(false);

/// A new transport over the testbed key, resolved once per run. Each call builds its own HTTP
/// client: every `#[tokio::test]` has its own runtime, and a connection pooled on a runtime
/// that has since been dropped fails with `Network`.
fn transport() -> Arc<dyn Transport> {
    let key = KEY.get_or_init(|| resolve_key().unwrap_or_else(|e| panic!("{e}")));
    Arc::new(HttpTransport::linear(key.clone()))
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

// The live suite. Each test starts with `guarded()`; the raw testbed client is connected only
// after it, and checks the workspace again itself.

/// Throwaway issues carry this title prefix, so leftovers are easy to find and trash.
const THROWAWAY_PREFIX: &str = "pb-live throwaway";
/// Team TBA's custom view of its issues labelled Bug (seeded by examples/seed_linear_testbed.rs).
const BUGS_VIEW: &str = "Testbed A bugs";
const BUG_LABEL: &str = "Bug";

/// The raw testbed client, for ground truth and throwaway issues.
async fn raw_client() -> linear_testbed::Linear {
    linear_testbed::Linear::connect(&testbed())
        .await
        .unwrap_or_else(|e| panic!("{e}"))
}

/// The team `key` names, resolved by the app.
async fn team_board(app: &Linear, key: &str) -> Project {
    let board = format!("linear:team:{key}")
        .parse::<BoardKey>()
        .expect("a team key makes a board key");
    must(app.resolve(&board).await, "resolve team")
}

/// Every issue on `board`, page by page.
async fn all_issues(app: &Linear, board: &BoardKey) -> Vec<Item> {
    let mut items = Vec::new();
    let mut after = None;
    loop {
        let page = must(app.issues_page(board, MAX_PAGE, after).await, "issues page");
        items.extend(page.nodes);
        match page.next {
            Some(next) => after = Some(next),
            None => return items,
        }
    }
}

/// Every id a list holds, page by page.
async fn all_view_ids(app: &Linear, board: &BoardKey, query: &ListQuery) -> Vec<ItemId> {
    let mut ids = Vec::new();
    let mut after = None;
    loop {
        let page = must(
            app.view_ids_page(board, query, after).await,
            "view ids page",
        );
        ids.extend(page.nodes);
        match page.next {
            Some(next) => after = Some(next),
            None => return ids,
        }
    }
}

/// Every issue on `board` updated after `since`, page by page.
async fn all_polled(app: &Linear, board: &BoardKey, since: &str) -> Vec<PolledIssue> {
    let mut polled = Vec::new();
    let mut after = None;
    loop {
        let page = must(
            app.poll_page(board, since, MAX_PAGE, after).await,
            "poll page",
        );
        polled.extend(page.nodes);
        match page.next {
            Some(next) => after = Some(next),
            None => return polled,
        }
    }
}

/// The issue's key (`TBA-12`); never its title or people.
fn issue_key(item: &Item) -> String {
    match &item.content {
        ItemContent::Issue { reference, .. } => reference.key.clone().unwrap_or_default(),
        _ => format!("non-issue {}", item.id),
    }
}

/// The id of team `key`, as Linear lists it.
async fn team_uuid(raw: &linear_testbed::Linear, key: &str) -> String {
    let teams = raw_post(
        raw,
        &ops::TeamByKey::build_query(ops::team_by_key::Variables {
            key: key.to_string(),
        }),
        "TeamByKey",
    )
    .await;
    teams.data()["teams"]["nodes"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("team {key} not found"))
        .to_string()
}

/// A workflow state as Linear lists it.
struct State {
    team: String,
    id: String,
    name: String,
}

/// The workflow states of `team_ids`, read raw: the ground truth for the Status columns.
async fn states_of(raw: &linear_testbed::Linear, team_ids: Vec<String>) -> Vec<State> {
    let schema = raw_post(
        raw,
        &ops::BoardSchema::build_query(ops::board_schema::Variables { team_ids }),
        "BoardSchema",
    )
    .await;
    schema.data()["workflowStates"]["nodes"]
        .as_array()
        .expect("workflowStates nodes")
        .iter()
        .map(|s| State {
            team: s["team"]["id"].as_str().unwrap_or_default().to_string(),
            id: s["id"].as_str().expect("a state id").to_string(),
            name: s["name"].as_str().expect("a state name").to_string(),
        })
        .collect()
}

/// The board's Status columns, ending with "No Status".
fn status_columns(project: &Project) -> Vec<Bucket> {
    project
        .field(&FieldId::new(ids::STATUS))
        .and_then(Field::buckets)
        .expect("the board has Status columns")
}

/// Asserts the Status columns are `states` merged by name: one column per distinct name, each
/// state's id in its name's column, then "No Status".
fn assert_merged_columns(project: &Project, states: &[State]) {
    let buckets = status_columns(project);
    let (none, columns) = buckets.split_last().expect("a No Status column");
    assert_eq!(none.key, None, "the last column is the no-status one");
    let titles: Vec<&str> = columns.iter().map(|b| b.title.as_str()).collect();
    let distinct: BTreeSet<&str> = titles.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        titles.len(),
        "one column per state name: {titles:?}"
    );
    let names: BTreeSet<&str> = states.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(distinct, names, "the columns are the states' names");
    for s in states {
        let column = columns
            .iter()
            .find(|b| b.title == s.name)
            .unwrap_or_else(|| panic!("no column `{}`", s.name));
        assert!(
            column.matches(&s.id),
            "state `{}` of team {} is not in its name's column",
            s.name,
            s.team
        );
    }
}

/// Asserts every item has a Status that one of the board's columns holds.
fn assert_items_in_columns(project: &Project, items: &[Item]) {
    let columns = status_columns(project);
    for item in items {
        let state = match item.values.get(&FieldId::new(ids::STATUS)) {
            Some(FieldValue::SingleSelect { option_id, .. }) => option_id.as_str(),
            other => panic!("{} has no Status: {other:?}", issue_key(item)),
        };
        assert!(
            columns.iter().any(|c| c.matches(state)),
            "{} is in no Status column",
            issue_key(item)
        );
    }
}

/// Whether the item carries the label `name`, loose or as a label group's value.
fn has_label(item: &Item, name: &str) -> bool {
    item.content_fields.label_ids.contains_key(name)
        || item.values.iter().any(|(field, value)| match value {
            FieldValue::Labels(labels) => labels.iter().any(|l| l.name == name),
            FieldValue::SingleSelect { name: n, .. } => {
                field.as_str().starts_with("linear:group:") && n == name
            }
            _ => false,
        })
}

/// RFC 3339, a minute ago: before anything a test is about to create.
fn a_minute_ago() -> String {
    (time::OffsetDateTime::now_utc() - time::Duration::minutes(1))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("a formattable time")
}

/// Issues a test creates on one team, trashed afterwards by `cleaning_up`.
struct Throwaways<'a> {
    raw: &'a linear_testbed::Linear,
    team_id: String,
    /// Created and not yet trashed.
    pending: Mutex<Vec<String>>,
}

impl<'a> Throwaways<'a> {
    fn new(raw: &'a linear_testbed::Linear, team_id: &str) -> Self {
        Self {
            raw,
            team_id: team_id.to_string(),
            pending: Mutex::new(Vec::new()),
        }
    }

    /// A new issue titled with the throwaway prefix and a random suffix; returns its id.
    async fn create(&self) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_nanos();
        let title = format!("{THROWAWAY_PREFIX} {:x}-{nanos:x}", std::process::id());
        let created = raw_post(
            self.raw,
            &ops::ProbeCreateIssue::build_query(ops::probe_create_issue::Variables {
                team_id: self.team_id.clone(),
                title,
            }),
            "ProbeCreateIssue",
        )
        .await;
        let id = created.data()["issueCreate"]["issue"]["id"]
            .as_str()
            .expect("ProbeCreateIssue returned the issue's id")
            .to_string();
        self.pending.lock().unwrap().push(id.clone());
        id
    }

    /// Trashes one of these issues as part of the test.
    async fn trash(&self, id: &str) {
        let r = raw_post(
            self.raw,
            &ops::ProbeTrash::build_query(ops::probe_trash::Variables { id: id.to_string() }),
            "ProbeTrash",
        )
        .await;
        assert_eq!(r.data()["issueDelete"]["success"], true, "ProbeTrash");
        self.pending.lock().unwrap().retain(|p| p != id);
    }

    /// Trashes every issue not trashed yet. Errors are reported, not raised: an issue the
    /// test's own trash reached despite an error refuses a second one.
    async fn trash_all(&self) {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for id in pending {
            let body = ops::ProbeTrash::build_query(ops::probe_trash::Variables { id: id.clone() });
            match self.raw.post(&body).await {
                Ok(r) if r.status == 200 && !r.body["errors"].is_array() => {}
                Ok(r) => eprintln!(
                    "cleanup: trashing throwaway {id}: HTTP {} errors {:?}",
                    r.status,
                    r.error_codes()
                ),
                Err(e) => eprintln!("cleanup: trashing throwaway {id}: {e}"),
            }
        }
    }
}

/// Runs `body`, then trashes every throwaway it left, then resumes its panic if it panicked.
async fn cleaning_up(made: &Throwaways<'_>, body: impl std::future::Future<Output = ()>) {
    let outcome = AssertUnwindSafe(body).catch_unwind().await;
    made.trash_all().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn team_board_loads_with_merged_columns() {
    let app = guarded().await;
    let bed = testbed();
    let raw = raw_client().await;
    let project = team_board(&app, &bed.team_a).await;
    let team = team_uuid(&raw, &bed.team_a).await;
    assert_eq!(
        project.board,
        BoardKey::LinearTeam { id: team.clone() },
        "the team key resolves to the team's id"
    );

    let states = states_of(&raw, vec![team.clone()]).await;
    assert!(
        !states.is_empty(),
        "team {} has workflow states",
        bed.team_a
    );
    assert!(states.iter().all(|s| s.team == team));
    assert_merged_columns(&project, &states);

    let items = all_issues(&app, &project.board).await;
    assert!(
        items.len() >= 15,
        "team {} has {} issues",
        bed.team_a,
        items.len()
    );
    let prefix = format!("{}-", bed.team_a);
    for item in &items {
        assert!(issue_key(item).starts_with(&prefix), "{}", issue_key(item));
    }
    assert_items_in_columns(&project, &items);
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn project_board_merges_both_teams() {
    let app = guarded().await;
    let bed = testbed();
    let raw = raw_client().await;
    let boards = must(app.list_boards().await, "ListBoards");
    let summary = boards
        .iter()
        .find(|b| b.title == bed.project && matches!(b.board, BoardKey::LinearProject { .. }))
        .unwrap_or_else(|| panic!("project `{}` not in ListBoards", bed.project));
    let project = must(app.resolve(&summary.board).await, "resolve project");
    assert_eq!(project.board, summary.board);
    let meta = project
        .linear
        .as_ref()
        .expect("a Linear board's workspace facts");
    for key in [&bed.team_a, &bed.team_b] {
        assert!(meta.team_keys.contains(key), "team {key} is on the project");
    }

    let teams = [
        team_uuid(&raw, &bed.team_a).await,
        team_uuid(&raw, &bed.team_b).await,
    ];
    let states = states_of(&raw, teams.to_vec()).await;
    for (key, id) in [&bed.team_a, &bed.team_b].into_iter().zip(&teams) {
        assert!(
            states.iter().any(|s| &s.team == id),
            "team {key} has workflow states"
        );
    }
    assert_merged_columns(&project, &states);
    assert!(
        status_columns(&project)
            .iter()
            .any(|c| !c.aliases.is_empty()),
        "a state name both teams use is one column"
    );

    let items = all_issues(&app, &project.board).await;
    for key in [&bed.team_a, &bed.team_b] {
        let prefix = format!("{key}-");
        assert!(
            items.iter().any(|i| issue_key(i).starts_with(&prefix)),
            "the project board has team {key}'s issues"
        );
    }
    assert_items_in_columns(&project, &items);
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn a_poll_sees_an_edit_an_archive_and_a_trash() {
    let app = guarded().await;
    let bed = testbed();
    let raw = raw_client().await;
    let project = team_board(&app, &bed.team_a).await;
    let BoardKey::LinearTeam { id: team_id } = &project.board else {
        panic!("a team board resolves to a team key: {}", project.board);
    };
    let since = a_minute_ago();
    let made = Throwaways::new(&raw, team_id);
    cleaning_up(&made, async {
        let edited = made.create().await;
        let archived = made.create().await;
        let trashed = made.create().await;
        raw_post(
            &raw,
            &ops::ProbeSetPriority::build_query(ops::probe_set_priority::Variables {
                id: edited.clone(),
                priority: 2,
            }),
            "ProbeSetPriority",
        )
        .await;
        let r = raw_post(
            &raw,
            &ops::ProbeArchive::build_query(ops::probe_archive::Variables {
                id: archived.clone(),
            }),
            "ProbeArchive",
        )
        .await;
        assert_eq!(r.data()["issueArchive"]["success"], true, "ProbeArchive");
        made.trash(&trashed).await;

        // Phase 0 (L7): a poll lists archivals and trash immediately.
        let polled = all_polled(&app, &project.board, &since).await;
        let item = |id: &str| {
            polled.iter().find_map(|p| match p {
                PolledIssue::Item(item) if item.id.as_str() == id => Some(item),
                _ => None,
            })
        };
        let edited = item(&edited).expect("the poll lists the edited issue");
        assert!(
            matches!(
                edited.values.get(&FieldId::new(ids::PRIORITY)),
                Some(FieldValue::SingleSelect { option_id, .. })
                    if *option_id == ids::priority_option(2)
            ),
            "the edited issue has priority High: {:?}",
            edited.values.get(&FieldId::new(ids::PRIORITY))
        );
        assert!(!edited.archived, "the edited issue is not archived");
        let archived = item(&archived).expect("the poll lists the archived issue");
        assert!(archived.archived, "the archived issue is archived");
        assert!(
            polled
                .iter()
                .any(|p| matches!(p, PolledIssue::Trashed(id) if id.as_str() == trashed)),
            "the poll lists the trashed issue as trashed"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn custom_view_ids_match_linear() {
    let app = guarded().await;
    let bed = testbed();
    let project = team_board(&app, &bed.team_a).await;
    let view = project
        .views
        .iter()
        .find(|v| v.name == BUGS_VIEW)
        .unwrap_or_else(|| panic!("no tab `{BUGS_VIEW}`"));
    let ViewFilter::LinearView { hide_closed, .. } = view.filter else {
        panic!(
            "`{BUGS_VIEW}` lists through its custom view: {:?}",
            view.filter
        );
    };
    let query = ListQuery {
        filter: view.filter.clone(),
        include_archived: false,
    };
    let ids = all_view_ids(&app, &project.board, &query).await;
    assert!(!ids.is_empty(), "`{BUGS_VIEW}` lists issues");
    let listed: BTreeSet<&ItemId> = ids.iter().collect();
    assert_eq!(listed.len(), ids.len(), "no id is listed twice");

    let team: BTreeSet<ItemId> = all_issues(&app, &project.board)
        .await
        .into_iter()
        .map(|i| i.id)
        .collect();
    for id in &ids {
        assert!(
            team.contains(id),
            "{id} is not one of team {}'s issues",
            bed.team_a
        );
    }

    // Linear's own label filter, on the same board, lists the same issues.
    let label = json!({"labels": {"name": {"eq": BUG_LABEL}}});
    let filter = if hide_closed {
        json!({"and": [label, {"state": {"type": {"nin": ["completed", "canceled", "duplicate"]}}}]})
    } else {
        label
    };
    let by_label_query = ListQuery {
        filter: ViewFilter::Linear(filter),
        include_archived: false,
    };
    let by_label = all_view_ids(&app, &project.board, &by_label_query).await;
    assert_eq!(
        listed,
        by_label.iter().collect::<BTreeSet<_>>(),
        "the view lists the team's {BUG_LABEL} issues"
    );

    let mut items = Vec::new();
    for chunk in ids.chunks(100) {
        items.extend(must(app.hydrate(chunk).await, "hydrate"));
    }
    assert_eq!(items.len(), ids.len(), "every listed id hydrates");
    for item in &items {
        assert!(
            has_label(item, BUG_LABEL),
            "{} does not carry {BUG_LABEL}",
            issue_key(item)
        );
    }
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn a_missing_board_is_not_found() {
    let app = guarded().await;
    let board = "linear:project:00000000-0000-4000-8000-000000000000"
        .parse::<BoardKey>()
        .expect("a UUID makes a board key");
    match app.resolve(&board).await {
        Ok(_) => panic!("{board} resolved"),
        Err(e) => assert!(e.is_not_found(), "{board}: {}", describe(&e)),
    }
}
