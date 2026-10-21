//! Live Linear suite against the testbed workspace (tests/linear-testbed.toml). The key comes
//! only from PB_LINEAR_TESTBED_KEY or the Keychain item `project-boards-linear-testbed`, and
//! `guarded()` checks the workspace before any other request. Every test here is ignored, so
//! the full check never reaches Linear. Run the suite with:
//!   cargo test --features live --test live_linear -- --ignored --nocapture --skip record_
//! and record the fixtures (the board scenarios and `edits/`) with:
//!   PB_RECORD=1 cargo test --features live --test live_linear record_ -- --ignored --nocapture
//! Tests that write, and the recordings, run one at a time (`serial()`), so no recording
//! catches another test's throwaway issue.
#![cfg(feature = "live")]

#[path = "common/linear_testbed.rs"]
mod linear_testbed;

use futures::FutureExt;
use graphql_client::GraphQLQuery;
use linear_testbed::{Raw, check_workspace, resolve_key, testbed};
use project_boards::backend::Backend;
use project_boards::error::BackendError;
use project_boards::linear::Linear;
use project_boards::linear::client::MAX_PAGE;
use project_boards::linear::convert::PolledIssue;
use project_boards::linear::ids;
use project_boards::linear::record::scrubbing_recorder;
use project_boards::model::teams::{self, StateGoal, TeamRefusal};
use project_boards::model::*;
use project_boards::ops::send::send;
use project_boards::ops::{Confirmed, NewItem, Op};
use project_boards::transport::{
    BoxFuture, GraphqlRequest, GraphqlResponse, HttpTransport, Transport,
};
use project_boards_queries::linear as ops;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
/// Held by every test that writes to the testbed and by both recordings.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One writer or recording at a time, whatever `--test-threads` says: a board recording
/// must not catch a throwaway issue another test has just made.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

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
    let _serial = serial().await;
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

/// A title with the throwaway prefix and a suffix no other run shares.
fn throwaway_title() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos();
    format!("{THROWAWAY_PREFIX} {:x}-{nanos:x}", std::process::id())
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
        let created = raw_post(
            self.raw,
            &ops::ProbeCreateIssue::build_query(ops::probe_create_issue::Variables {
                team_id: self.team_id.clone(),
                title: throwaway_title(),
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

    /// Registers an issue the app itself creates (a new issue, a resent create) to be
    /// trashed too. Called before the create is sent: its client id is known up front, so
    /// an issue a failing test made is still trashed.
    fn adopt(&self, id: &str) {
        let mut pending = self.pending.lock().unwrap();
        if !pending.iter().any(|p| p == id) {
            pending.push(id.to_string());
        }
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
    let _serial = serial().await;
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

// The editing suite (plan 3). Every edit goes through `ops::send` with the op built as the
// UI builds it: values resolved for the issue's team by `model::teams`, a label group as
// `GroupLabel` naming the label it replaces, a reorder as a `sortOrder` midpoint, comments
// and new issues with client UUIDs. Each change is read back with `issue_by_key` (or the
// detail, for a description and a comment) through an unrecorded client.

/// What the edits set; tests/linear_fixtures.rs (`edit_fixtures_decode`) reads them back from
/// the recording.
const EDIT_DUE_DATE: &str = "2026-12-01";
const EDIT_ESTIMATE: f64 = 3.0;
const EDIT_PRIORITY: u8 = 2;
const EDIT_BODY: &str = "Edited by the **live suite**.\n\n- one\n- two";
/// The testbed's workspace-level single-select label group (no team owns it).
const TYPE_GROUP_NAME: &str = "Type";

/// Counts the requests it passes on, to show a refusal sends nothing.
struct Counting {
    inner: Arc<dyn Transport>,
    sent: AtomicUsize,
}

impl Counting {
    fn new(inner: Arc<dyn Transport>) -> Self {
        Self {
            inner,
            sent: AtomicUsize::new(0),
        }
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }
}

impl Transport for Counting {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, BackendError>> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        self.inner.execute(request)
    }
}

/// A fresh client UUID, as the UI makes one for a new issue or a comment.
fn new_uuid() -> String {
    project_boards::uuid::new_v4().unwrap_or_else(|e| panic!("a UUID from /dev/urandom: {e}"))
}

/// The testbed project's board, found by name and resolved.
async fn testbed_project(app: &Linear) -> Project {
    let bed = testbed();
    let boards = must(app.list_boards().await, "ListBoards");
    let summary = boards
        .iter()
        .find(|b| b.title == bed.project && matches!(b.board, BoardKey::LinearProject { .. }))
        .unwrap_or_else(|| panic!("project `{}` not in ListBoards", bed.project));
    must(app.resolve(&summary.board).await, "resolve project")
}

/// The board's team with `key`.
fn team_meta(project: &Project, key: &str) -> TeamMeta {
    project
        .linear
        .as_ref()
        .and_then(|m| m.teams.iter().find(|t| t.key == key))
        .cloned()
        .unwrap_or_else(|| panic!("team {key} is not one of the board's teams"))
}

/// The team id owning a state, cycle or label group, by its id.
fn owner<'a>(project: &'a Project, id: &str) -> Option<&'a str> {
    project.linear.as_ref()?.owners.get(id).map(String::as_str)
}

fn field_of<'a>(project: &'a Project, id: &str) -> &'a Field {
    project
        .field(&FieldId::new(id))
        .unwrap_or_else(|| panic!("the board has no field {id}"))
}

fn options_of(field: &Field) -> &[SelectOption] {
    match &field.kind {
        FieldKind::SingleSelect { options } => options,
        _ => panic!("{} is not a single-select field", field.name),
    }
}

/// The testbed's Type label group, a single-select field on every Linear board.
fn type_group(project: &Project) -> &Field {
    project
        .fields
        .iter()
        .find(|f| f.name == TYPE_GROUP_NAME && f.id.as_str().starts_with(teams::GROUP_FIELD_PREFIX))
        .unwrap_or_else(|| panic!("the board has no {TYPE_GROUP_NAME} label group"))
}

fn select(option: &SelectOption) -> FieldValue {
    FieldValue::SingleSelect {
        option_id: option.id.clone(),
        name: option.name.clone(),
    }
}

/// The option or cycle id `item` has in `field`.
fn chosen<'a>(item: &'a Item, field: &str) -> Option<&'a str> {
    match item.values.get(&FieldId::new(field))? {
        FieldValue::SingleSelect { option_id, .. } => Some(option_id.as_str()),
        FieldValue::Iteration { iteration_id, .. } => Some(iteration_id.as_str()),
        _ => None,
    }
}

/// The variant a send confirmed with; never the item, which may name a person.
fn variant(c: &Confirmed) -> &'static str {
    match c {
        Confirmed::Applied => "Applied",
        Confirmed::Item(_) => "Item",
        Confirmed::Created { .. } => "Created",
        Confirmed::NotAdded { .. } => "NotAdded",
    }
}

/// The issue `id` as Linear has it now, archived or not.
async fn read_back(app: &Linear, id: &str) -> Item {
    must(app.issue_by_key(id).await, "IssueByKey").unwrap_or_else(|| panic!("no issue {id}"))
}

/// The op that sets (`Some`) or clears (`None`) `field` of `item`, built as the UI builds it
/// (`App::field_op`, after `App::resolved`): the value resolved for the issue's team, a clear
/// checked with `teams::may_clear`, and a label group as `GroupLabel` naming the label the
/// issue has in it now.
fn field_op(project: &Project, item: &Item, field: &Field, value: Option<FieldValue>) -> Op {
    let value = value.map(|v| {
        teams::for_issue(project, item, field, v)
            .unwrap_or_else(|r| panic!("{}: {}", field.name, r.message()))
    });
    if value.is_none() {
        teams::may_clear(project, field)
            .unwrap_or_else(|r| panic!("{}: {}", field.name, r.message()));
    }
    let (id, field_id) = (item.id.clone(), field.id.clone());
    if field.id.as_str().starts_with(teams::GROUP_FIELD_PREFIX) {
        let replaces = match item.value(&field.id) {
            Some(FieldValue::SingleSelect { option_id, .. }) => Some(option_id.as_str().into()),
            _ => None,
        };
        return Op::GroupLabel {
            item: id,
            field: field_id,
            value,
            replaces,
        };
    }
    match value {
        Some(value) => Op::SetField {
            item: id,
            field: field_id,
            value,
        },
        None => Op::ClearField {
            item: id,
            field: field_id,
        },
    }
}

/// Sends `op` on `project`'s board, then reads issue `id` back: `check` must hold for the
/// issue the send confirmed with, when it returned one, and for the one read back, which is
/// returned. A create must have set every preset.
async fn edit(
    app: &Linear,
    backend: &Backend,
    project: &Project,
    id: &str,
    op: Op,
    what: &str,
    check: impl Fn(&Item) -> bool,
) -> Item {
    let confirmed = must(send(backend, &project.id, &op).await, what);
    match &confirmed {
        Confirmed::Applied => {}
        Confirmed::Item(item) => assert!(check(item), "{what}: the confirmed issue"),
        Confirmed::Created { item, refused, .. } => {
            assert!(
                refused.is_empty(),
                "{what}: {} presets refused",
                refused.len()
            );
            assert_eq!(item.id.as_str(), id, "{what}: the created issue");
            assert!(check(item), "{what}: the created issue");
        }
        other => panic!("{what}: confirmed with {}", variant(other)),
    }
    let back = read_back(app, id).await;
    assert!(check(&back), "{what}: read back");
    back
}

/// Every edit the app makes on a Linear issue, in this order (tests/linear_fixtures.rs,
/// `EDIT_UPDATES`, names the `issueUpdate`s): on a throwaway TBA issue, a status move, a
/// priority set and clear, an estimate, a due date, a cycle, the Type group set, swapped and
/// cleared, a label added and removed, an assignee set and removed, a title, a description, a
/// reorder, a comment, archive and unarchive; then a new issue with presets on the team
/// board; then the throwaway added to the testbed project (with a status preset) and removed
/// from it; then `issue_by_key` by its key. `team_candidates` is asked once, for the label
/// and the assignee.
///
/// Everything is sent through `backend` (recorded, when it records); every read-back and the
/// boards go through `app`, which is not.
async fn every_edit(app: &Linear, backend: &Backend, made: &Throwaways<'_>) {
    let Backend::Linear(sender) = backend else {
        panic!("the edits go to Linear");
    };
    let bed = testbed();
    let team = team_board(app, &bed.team_a).await;
    let tba = team_meta(&team, &bed.team_a);

    let id = made.create().await;
    let mut issue = read_back(app, &id).await;
    let item = issue.id.clone();
    let content = issue.content_id().expect("an issue's content id");

    // A status move into TBA's started state.
    let status = field_of(&team, ids::STATUS);
    let started = options_of(status)
        .iter()
        .find(|o| {
            o.kind == Some(OptionKind::Started) && owner(&team, o.id.as_str()) == Some(&tba.id)
        })
        .expect("TBA has a started state");
    assert_ne!(
        chosen(&issue, ids::STATUS),
        Some(started.id.as_str()),
        "a new issue is not started"
    );
    let op = field_op(&team, &issue, status, Some(select(started)));
    issue = edit(app, backend, &team, &id, op, "status", |i| {
        chosen(i, ids::STATUS) == Some(started.id.as_str())
    })
    .await;

    // Priority High, then no priority.
    let priority = field_of(&team, ids::PRIORITY);
    let high = ids::priority_option(EDIT_PRIORITY);
    let high_option = options_of(priority)
        .iter()
        .find(|o| o.id == high)
        .expect("a High priority option");
    let op = field_op(&team, &issue, priority, Some(select(high_option)));
    issue = edit(app, backend, &team, &id, op, "priority", |i| {
        chosen(i, ids::PRIORITY) == Some(high.as_str())
    })
    .await;
    let op = field_op(&team, &issue, priority, None);
    issue = edit(app, backend, &team, &id, op, "priority cleared", |i| {
        i.value(&FieldId::new(ids::PRIORITY)).is_none()
    })
    .await;

    // An estimate and a due date.
    let estimate = FieldValue::Number(EDIT_ESTIMATE);
    let op = field_op(
        &team,
        &issue,
        field_of(&team, ids::ESTIMATE),
        Some(estimate.clone()),
    );
    issue = edit(app, backend, &team, &id, op, "estimate", |i| {
        i.value(&FieldId::new(ids::ESTIMATE)) == Some(&estimate)
    })
    .await;
    let due = FieldValue::Date(EDIT_DUE_DATE.into());
    let op = field_op(&team, &issue, field_of(&team, ids::DUE), Some(due.clone()));
    issue = edit(app, backend, &team, &id, op, "due", |i| {
        i.value(&FieldId::new(ids::DUE)) == Some(&due)
    })
    .await;

    // One of TBA's current or upcoming cycles.
    let cycle_field = field_of(&team, ids::CYCLE);
    let FieldKind::Iteration { iterations, .. } = &cycle_field.kind else {
        panic!("Cycle is an iteration field");
    };
    let cycle = iterations
        .iter()
        .find(|c| owner(&team, c.id.as_str()) == Some(&tba.id))
        .expect("TBA has a current or upcoming cycle");
    let value = FieldValue::Iteration {
        iteration_id: cycle.id.clone(),
        title: cycle.title.clone(),
        start_date: cycle.start_date.clone(),
    };
    let op = field_op(&team, &issue, cycle_field, Some(value));
    issue = edit(app, backend, &team, &id, op, "cycle", |i| {
        chosen(i, ids::CYCLE) == Some(cycle.id.as_str())
    })
    .await;

    // The Type group set, swapped and cleared, never to Bug: the Bugs view test lists it.
    let group = type_group(&team);
    let kinds: Vec<&SelectOption> = options_of(group)
        .iter()
        .filter(|o| o.name != BUG_LABEL)
        .collect();
    let [first, second, ..] = kinds.as_slice() else {
        panic!("the Type group has two labels besides {BUG_LABEL}");
    };
    let op = field_op(&team, &issue, group, Some(select(first)));
    issue = edit(app, backend, &team, &id, op, "type set", |i| {
        chosen(i, group.id.as_str()) == Some(first.id.as_str())
    })
    .await;
    let op = field_op(&team, &issue, group, Some(select(second)));
    assert!(
        matches!(&op, Op::GroupLabel { replaces: Some(old), .. } if *old == first.id.as_str()),
        "the swap names the label it replaces"
    );
    issue = edit(app, backend, &team, &id, op, "type swapped", |i| {
        chosen(i, group.id.as_str()) == Some(second.id.as_str())
    })
    .await;
    let op = field_op(&team, &issue, group, None);
    issue = edit(app, backend, &team, &id, op, "type cleared", |i| {
        i.value(&group.id).is_none()
    })
    .await;

    // A label from the team's candidates, added and removed.
    let candidates = must(sender.team_candidates(&tba.id).await, "TeamCandidates");
    assert!(!candidates.users.is_empty(), "team TBA has members");
    let has =
        |i: &Item, label: &LabelRef| i.content_fields.label_ids.values().any(|l| *l == label.id);
    let label = candidates
        .labels
        .iter()
        .find(|l| !has(&issue, l))
        .expect("a label the issue does not have")
        .clone();
    let op = Op::Labels {
        item: item.clone(),
        content: content.clone(),
        add: vec![label.clone()],
        remove: vec![],
    };
    edit(app, backend, &team, &id, op, "label added", |i| {
        has(i, &label)
    })
    .await;
    let op = Op::Labels {
        item: item.clone(),
        content: content.clone(),
        add: vec![],
        remove: vec![label.clone()],
    };
    edit(app, backend, &team, &id, op, "label removed", |i| {
        !has(i, &label)
    })
    .await;

    // One assignee, then none.
    let user = candidates.users[0].clone();
    let op = Op::Assignees {
        item: item.clone(),
        content: content.clone(),
        add: vec![user.clone()],
        remove: vec![],
        all: vec![user.clone()],
    };
    let assigned = |i: &Item| {
        i.content_fields
            .assignee_ids
            .values()
            .any(|u| *u == user.id)
    };
    edit(app, backend, &team, &id, op, "assigned", assigned).await;
    let op = Op::Assignees {
        item: item.clone(),
        content: content.clone(),
        add: vec![],
        remove: vec![user.clone()],
        all: vec![],
    };
    let unassigned = |i: &Item| {
        i.content_fields.assignee_ids.is_empty() && i.value(&FieldId::new(ids::ASSIGNEES)).is_none()
    };
    edit(app, backend, &team, &id, op, "unassigned", unassigned).await;

    // A title (still a throwaway's) and a description, read back from the detail.
    let title = throwaway_title();
    let op = Op::Title {
        item: item.clone(),
        content: content.clone(),
        title: title.clone(),
    };
    edit(app, backend, &team, &id, op, "title", |i| {
        i.title() == title
    })
    .await;
    let op = Op::Body {
        item: item.clone(),
        content: content.clone(),
        body: EDIT_BODY.into(),
    };
    edit(app, backend, &team, &id, op, "description", |_| true).await;
    let detail = must(app.detail(&item, None).await, "detail");
    assert_eq!(detail.body, EDIT_BODY, "the description reads back");

    // A reorder between two of the team's other issues, at their sortOrder midpoint.
    let mut others: Vec<(ItemId, f64)> = all_issues(app, &team.board)
        .await
        .into_iter()
        .filter(|i| i.id != item)
        .filter_map(|i| Some((i.id, i.content_fields.sort_order?)))
        .collect();
    others.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (after, sort_order) = others
        .windows(2)
        .find_map(|w| {
            Some((
                w[0].0.clone(),
                sort_order_between(Some(w[0].1), Some(w[1].1))?,
            ))
        })
        .expect("two of TBA's issues with room between them");
    let op = Op::Reorder {
        item: item.clone(),
        after: Some(after),
        sort_order: Some(sort_order),
    };
    edit(app, backend, &team, &id, op, "reorder", |i| {
        i.content_fields
            .sort_order
            .is_some_and(|s| (s - sort_order).abs() < 1e-6)
    })
    .await;

    // A comment with its client id, read back from the detail.
    let client_id = new_uuid();
    let body = format!("pb-live comment {client_id}");
    let op = Op::Comment {
        item: item.clone(),
        content: content.clone(),
        body: body.clone(),
        client_id: Some(client_id),
    };
    let confirmed = must(send(backend, &team.id, &op).await, "comment");
    assert_eq!(
        variant(&confirmed),
        "Applied",
        "a comment confirms as applied"
    );
    let detail = must(app.detail(&item, None).await, "detail");
    assert_eq!(
        detail.comments.iter().filter(|c| c.body == body).count(),
        1,
        "the comment is on the issue once"
    );

    // Archive and unarchive.
    let op = Op::Archive { item: item.clone() };
    edit(app, backend, &team, &id, op, "archive", |i| i.archived).await;
    let op = Op::Unarchive { item: item.clone() };
    issue = edit(app, backend, &team, &id, op, "unarchive", |i| !i.archived).await;

    // A new issue in TBA with the started state, High and the Type group's first label as
    // presets, each resolved for TBA as the new-issue form does (`presets_for_team`).
    let presets: Vec<(FieldId, FieldValue)> = [
        (status, select(started)),
        (priority, select(high_option)),
        (group, select(first)),
    ]
    .into_iter()
    .map(|(field, value)| {
        let value = teams::for_team(&team, &tba, field, value)
            .unwrap_or_else(|r| panic!("{}: {}", field.name, r.message()));
        (field.id.clone(), value)
    })
    .collect();
    let new_id = new_uuid();
    made.adopt(&new_id);
    let op = Op::Create {
        temp: ItemId::new("local:1"),
        new: NewItem::LinearIssue {
            id: new_id.clone(),
            team: tba.clone(),
            title: throwaway_title(),
            body: "Made by the live suite, with presets".into(),
        },
        presets,
    };
    edit(app, backend, &team, &new_id, op, "new issue", |i| {
        chosen(i, ids::STATUS) == Some(started.id.as_str())
            && chosen(i, ids::PRIORITY) == Some(high.as_str())
            && chosen(i, group.id.as_str()) == Some(first.id.as_str())
    })
    .await;

    // The throwaway added to the testbed project with a status preset (TBA's state for
    // reopening, resolved for the issue as `presets_for_issue` does), then removed from it.
    let project = testbed_project(app).await;
    let BoardKey::LinearProject { id: project_id } = &project.board else {
        panic!("the testbed project is a project board");
    };
    assert!(
        issue.value(&FieldId::new(ids::PROJECT)).is_none(),
        "the throwaway starts in no project"
    );
    let on_project = team_meta(&project, &bed.team_a);
    let project_status = field_of(&project, ids::STATUS);
    let reopen = teams::state_for(&project, &on_project, StateGoal::Reopen)
        .unwrap_or_else(|r| panic!("{}", r.message()));
    let reopen = teams::for_issue(&project, &issue, project_status, reopen)
        .unwrap_or_else(|r| panic!("{}", r.message()));
    let FieldValue::SingleSelect {
        option_id: reopen_id,
        ..
    } = &reopen
    else {
        panic!("a state is a single-select value");
    };
    assert_ne!(chosen(&issue, ids::STATUS), Some(reopen_id.as_str()));
    let in_project = |i: &Item| chosen(i, ids::PROJECT) == Some(project_id.as_str());
    let op = Op::Create {
        temp: ItemId::new("local:2"),
        new: NewItem::LinearExisting {
            issue: Box::new(issue.clone()),
        },
        presets: vec![(project_status.id.clone(), reopen.clone())],
    };
    edit(app, backend, &project, &id, op, "added to project", |i| {
        in_project(i) && chosen(i, ids::STATUS) == Some(reopen_id.as_str())
    })
    .await;
    let op = Op::Remove { item: item.clone() };
    issue = edit(
        app,
        backend,
        &project,
        &id,
        op,
        "removed from project",
        |i| i.value(&FieldId::new(ids::PROJECT)).is_none(),
    )
    .await;

    // The issue by its key.
    let key = issue_key(&issue);
    let by_key = must(sender.issue_by_key(&key).await, "IssueByKey")
        .unwrap_or_else(|| panic!("no issue {key}"));
    assert_eq!(by_key.id, item, "{key} is the throwaway");
}

#[tokio::test]
#[ignore = "records Linear fixtures from the testbed: PB_RECORD=1 ... -- --ignored --nocapture"]
async fn record_edit_fixtures() {
    let _serial = serial().await;
    let app = guarded().await;
    let raw = raw_client().await;
    let team = team_board(&app, &testbed().team_a).await;
    let BoardKey::LinearTeam { id: team_id } = &team.board else {
        panic!("a team board resolves to a team key: {}", team.board);
    };
    let made = Throwaways::new(&raw, team_id);
    let backend = Backend::Linear(recording_linear("edits", http()));
    cleaning_up(&made, every_edit(&app, &backend, &made)).await;
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn every_linear_edit_round_trips() {
    let _serial = serial().await;
    let app = guarded().await;
    let raw = raw_client().await;
    let team = team_board(&app, &testbed().team_a).await;
    let BoardKey::LinearTeam { id: team_id } = &team.board else {
        panic!("a team board resolves to a team key: {}", team.board);
    };
    let made = Throwaways::new(&raw, team_id);
    let backend = Backend::Linear(Linear::new(http()));
    cleaning_up(&made, every_edit(&app, &backend, &made)).await;
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn a_resent_create_makes_one_issue() {
    let _serial = serial().await;
    let app = guarded().await;
    let raw = raw_client().await;
    let team = team_board(&app, &testbed().team_a).await;
    let BoardKey::LinearTeam { id: team_id } = &team.board else {
        panic!("a team board resolves to a team key: {}", team.board);
    };
    let tba = team_meta(&team, &testbed().team_a);
    let made = Throwaways::new(&raw, team_id);
    cleaning_up(&made, async {
        let backend = Backend::Linear(Linear::new(http()));
        let id = new_uuid();
        let title = throwaway_title();
        made.adopt(&id);
        let op = Op::Create {
            temp: ItemId::new("local:1"),
            new: NewItem::LinearIssue {
                id: id.clone(),
                team: tba,
                title: title.clone(),
                body: String::new(),
            },
            presets: vec![],
        };
        // The second send is the queue's resend of the same op: the same client id.
        for send_number in 1..=2 {
            match must(send(&backend, &team.id, &op).await, "create") {
                Confirmed::Created { item, refused, .. } => {
                    assert_eq!(item.id.as_str(), id, "send {send_number} names the issue");
                    assert!(refused.is_empty());
                }
                other => panic!("send {send_number}: confirmed with {}", variant(&other)),
            }
        }
        let query = ListQuery {
            filter: ViewFilter::Linear(json!({"title": {"eq": title}})),
            include_archived: true,
        };
        let ids = all_view_ids(&app, &team.board, &query).await;
        assert_eq!(
            ids,
            [ItemId::new(&id)],
            "one issue has the title, the client id's"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn a_resent_comment_posts_once() {
    let _serial = serial().await;
    let app = guarded().await;
    let raw = raw_client().await;
    let team = team_board(&app, &testbed().team_a).await;
    let BoardKey::LinearTeam { id: team_id } = &team.board else {
        panic!("a team board resolves to a team key: {}", team.board);
    };
    let made = Throwaways::new(&raw, team_id);
    cleaning_up(&made, async {
        let backend = Backend::Linear(Linear::new(http()));
        let id = made.create().await;
        let issue = read_back(&app, &id).await;
        let client_id = new_uuid();
        let body = format!("pb-live comment {client_id}");
        let op = Op::Comment {
            item: issue.id.clone(),
            content: issue.content_id().expect("an issue's content id"),
            body: body.clone(),
            client_id: Some(client_id),
        };
        for send_number in 1..=2 {
            let confirmed = must(send(&backend, &team.id, &op).await, "comment");
            assert_eq!(variant(&confirmed), "Applied", "send {send_number}");
        }
        let detail = must(app.detail(&issue.id, None).await, "detail");
        assert_eq!(
            detail.comments.iter().filter(|c| c.body == body).count(),
            1,
            "the comment is posted once"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "live: the Linear testbed; run with --ignored"]
async fn a_move_into_another_teams_state_is_refused_before_sending() {
    guarded().await;
    let bed = testbed();
    let counting = Arc::new(Counting::new(http()));
    let app = Linear::new(counting.clone());
    let project = testbed_project(&app).await;
    let (tba, tbb) = (
        team_meta(&project, &bed.team_a),
        team_meta(&project, &bed.team_b),
    );
    let status = field_of(&project, ids::STATUS);
    let owned_by =
        |o: &SelectOption, team: &TeamMeta| owner(&project, o.id.as_str()) == Some(&team.id);
    let tba_names: BTreeSet<&str> = options_of(status)
        .iter()
        .filter(|o| owned_by(o, &tba))
        .map(|o| o.name.as_str())
        .collect();
    let theirs = options_of(status)
        .iter()
        .find(|o| {
            owned_by(o, &tbb)
                && o.kind != Some(OptionKind::Duplicate)
                && !tba_names.contains(o.name.as_str())
        })
        .expect("TBB has a state whose name TBA has not");
    let items = all_issues(&app, &project.board).await;
    let prefix = format!("{}-", bed.team_a);
    let issue = items
        .iter()
        .find(|i| issue_key(i).starts_with(&prefix))
        .expect("the project has a TBA issue");

    // Ground truth: Linear lists no state of that name for TBA.
    let raw = raw_client().await;
    let states = states_of(&raw, vec![tba.id.clone()]).await;
    assert!(!states.iter().any(|s| s.name == theirs.name));

    // The UI resolves the move before building the op; the refusal is its message, and
    // nothing reaches Linear.
    let before = counting.sent();
    let refused = teams::for_issue(&project, issue, status, select(theirs));
    assert_eq!(
        refused,
        Err(TeamRefusal::NoState {
            team: tba.key.clone(),
            state: theirs.name.clone(),
        })
    );
    assert_eq!(
        refused.unwrap_err().message(),
        format!("{} has no \"{}\" state", tba.key, theirs.name)
    );
    assert_eq!(counting.sent(), before, "the refused move sent no request");
}
