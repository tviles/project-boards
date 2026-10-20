//! Offline replays of the Linear fixtures recorded from the testbed (tests/live_linear.rs,
//! `record_board_fixtures`): each scenario's files are queued on a `FixtureTransport` and
//! read back through the app's `Linear` client.

#[path = "common/linear_testbed.rs"]
mod linear_testbed;

use project_boards::linear::Linear;
use project_boards::linear::client::MAX_PAGE;
use project_boards::linear::convert::PolledIssue;
use project_boards::linear::ids;
use project_boards::linear::record::INVENTED_PEOPLE;
use project_boards::model::*;
use project_boards::transport::fixture::FixtureTransport;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const RECORDED: &str = "tests/fixtures/recorded/linear";
/// Team TBA's id and its Type label group's id in the testbed.
const TBA_ID: &str = "152b24bd-5efb-4bf0-8d81-021700a5a8f0";
const TYPE_GROUP: &str = "5649c6a3-6c17-4e06-a924-e776eb41d42b";
const COMMENTED_ISSUE: &str = "ea7a48d9-a787-45f0-83cf-ee292ba99925";

/// The scenario's files as (operation, number, path).
fn files(scenario: &str) -> Vec<(String, usize, PathBuf)> {
    let dir = Path::new(RECORDED).join(scenario);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.to_string();
            let (operation, n) = stem.split_once("__")?;
            Some((operation.to_string(), n.parse().ok()?, path))
        })
        .collect();
    files.sort();
    files
}

/// A client that answers each operation with the scenario's recorded responses, in order.
fn replay(scenario: &str) -> Linear {
    let transport = Arc::new(FixtureTransport::new());
    for (operation, _, path) in files(scenario) {
        transport.push_file(&operation, path);
    }
    Linear::new(transport)
}

fn fid(id: &str) -> FieldId {
    FieldId::new(id)
}

fn key(item: &Item) -> String {
    match &item.content {
        ItemContent::Issue { reference, .. } => reference.key.clone().unwrap_or_default(),
        other => panic!("not an issue: {other:?}"),
    }
}

fn field<'a>(project: &'a Project, id: &str) -> &'a Field {
    project
        .fields
        .iter()
        .find(|f| f.id.as_str() == id)
        .unwrap_or_else(|| panic!("no field {id}"))
}

fn options(f: &Field) -> &[SelectOption] {
    match &f.kind {
        FieldKind::SingleSelect { options } => options,
        other => panic!("{} is {other:?}", f.name),
    }
}

fn tab_names(project: &Project) -> Vec<&str> {
    project.views.iter().map(|v| v.name.as_str()).collect()
}

fn view<'a>(project: &'a Project, name: &str) -> &'a View {
    project
        .views
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("no tab {name}"))
}

/// Layout, columns, lanes and sort of a tab.
fn shape(v: &View) -> (Layout, Vec<FieldId>, Vec<FieldId>, Vec<SortSpec>) {
    (
        v.layout,
        v.vertical_group_by.clone(),
        v.group_by.clone(),
        v.sort_by.clone(),
    )
}

fn sort(id: &str, direction: SortDirection) -> Vec<SortSpec> {
    vec![SortSpec {
        field: fid(id),
        direction,
    }]
}

/// The id count of every tab, in tab order, read back as the recording listed them.
async fn tab_counts(linear: &Linear, project: &Project) -> Vec<usize> {
    let mut counts = Vec::new();
    for v in &project.views {
        let query = ListQuery {
            filter: v.filter.clone(),
            include_archived: false,
        };
        let page = linear
            .view_ids_page(&project.board, &query, None)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", v.name));
        counts.push(page.nodes.len());
    }
    counts
}

fn every_string(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|i| every_string(i, out)),
        Value::Object(map) => map.values().for_each(|c| every_string(c, out)),
        _ => {}
    }
}

/// Every user-like object (one with `displayName`, `email` or `avatarUrl`), as the scrubber
/// finds them.
fn people<'a>(v: &'a Value, out: &mut Vec<&'a serde_json::Map<String, Value>>) {
    match v {
        Value::Object(map) => {
            if ["displayName", "email", "avatarUrl"]
                .iter()
                .any(|k| map.contains_key(*k))
            {
                out.push(map);
            }
            map.values().for_each(|c| people(c, out));
        }
        Value::Array(items) => items.iter().for_each(|i| people(i, out)),
        _ => {}
    }
}

fn is_invented(name: &str) -> bool {
    INVENTED_PEOPLE.iter().any(|base| {
        name == *base
            || name
                .strip_prefix(base)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|n| n.parse::<u32>().is_ok())
    })
}

#[test]
fn recorded_fixtures_hold_only_invented_people() {
    let url_key = linear_testbed::testbed().url_key;
    let mut checked = 0;
    let mut persons = 0;
    for scenario in ["team-board", "project-board", "boards", "poll"] {
        for (_, _, path) in files(scenario) {
            let text = std::fs::read_to_string(&path).unwrap();
            let body: Value = serde_json::from_str(&text).unwrap();
            let mut strings = Vec::new();
            every_string(&body, &mut strings);
            assert!(
                !strings.iter().any(|s| s.contains(&url_key)),
                "{} holds the testbed's URL key",
                path.display()
            );
            let mut found = Vec::new();
            people(&body, &mut found);
            for person in found {
                for key in ["name", "displayName"] {
                    if let Some(name) = person.get(key).and_then(Value::as_str) {
                        assert!(
                            is_invented(name),
                            "{}: a {key} is not invented",
                            path.display()
                        );
                    }
                }
                if let Some(email) = person.get("email").and_then(Value::as_str) {
                    assert!(
                        email.ends_with("@example.com"),
                        "{}: an email is not invented",
                        path.display()
                    );
                }
                assert!(
                    person.get("avatarUrl").is_none_or(Value::is_null),
                    "{}: an avatar URL survived",
                    path.display()
                );
                persons += 1;
            }
            checked += 1;
        }
    }
    assert!(checked >= 30, "only {checked} fixture files");
    assert!(
        persons > 0,
        "no person to check: the fixtures changed shape"
    );
}

#[tokio::test]
async fn team_board_fixture_decodes() {
    use SortDirection::{Asc, Desc};
    let linear = replay("team-board");
    let project = linear
        .resolve(&BoardKey::LinearTeam { id: "TBA".into() })
        .await
        .unwrap();
    assert_eq!(project.board, BoardKey::LinearTeam { id: TBA_ID.into() });
    assert!(project.url.ends_with("/team/TBA/all"), "{}", project.url);

    let status = options(field(&project, ids::STATUS));
    let names: Vec<_> = status.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Backlog",
            "Todo",
            "In Progress",
            "Done",
            "Canceled",
            "Duplicate"
        ]
    );
    assert!(
        status
            .windows(2)
            .all(|w| OptionKind::rank(w[0].kind) <= OptionKind::rank(w[1].kind)),
        "states are in kind order"
    );
    let group = ids::group(TYPE_GROUP);
    assert_eq!(field(&project, group.as_str()).name, "Type");
    assert!(matches!(
        field(&project, ids::CYCLE).kind,
        FieldKind::Iteration { .. }
    ));

    // Every state and cycle is TBA's. The Type group is the workspace's (the seed creates
    // it without a team), so it has no owner; the recordings hold no team-owned group.
    let meta = project.linear.as_ref().unwrap();
    assert_eq!(
        meta.teams,
        [TeamMeta {
            id: TBA_ID.into(),
            key: "TBA".into()
        }]
    );
    let owner = |id: &str| meta.owners.get(id).map(String::as_str);
    assert!(status.iter().all(|o| owner(o.id.as_str()) == Some(TBA_ID)));
    let FieldKind::Iteration {
        iterations,
        completed,
    } = &field(&project, ids::CYCLE).kind
    else {
        unreachable!()
    };
    assert!(
        iterations
            .iter()
            .chain(completed)
            .all(|c| owner(c.id.as_str()) == Some(TBA_ID))
    );
    assert_eq!(owner(TYPE_GROUP), None);
    let projects = options(field(&project, ids::PROJECT));
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Testbed project");

    assert_eq!(
        tab_names(&project),
        [
            "Board",
            "All issues",
            "My issues",
            "Fixture by due",
            "Fixture by type",
            "Fixture by cycle",
            "Fixture by project",
            "Probe board by priority",
            "Probe list by assignee",
            "Testbed open",
            "Testbed A bugs",
        ]
    );
    // "Testbed open" is a workspace view (Decision 1): no team in the recorded schema.
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(RECORDED).join("team-board/BoardSchema__1.json"))
            .unwrap(),
    )
    .unwrap();
    let open = schema["data"]["customViews"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "Testbed open")
        .unwrap();
    assert!(open["team"].is_null());

    // The four views Task 13 set up, mapped as the recording measured them (Decision 20).
    assert_eq!(
        shape(view(&project, "Fixture by project")),
        (
            Layout::Table,
            vec![],
            vec![fid(ids::PROJECT)],
            sort(ids::PRIORITY, Asc)
        )
    );
    assert_eq!(
        shape(view(&project, "Fixture by cycle")),
        (
            Layout::Board,
            vec![fid(ids::CYCLE)],
            vec![],
            sort(ids::PRIORITY, Asc)
        )
    );
    assert_eq!(
        shape(view(&project, "Fixture by type")),
        (
            Layout::Board,
            vec![group.clone()],
            vec![fid(ids::STATUS)],
            sort(ids::CREATED, Desc)
        )
    );
    assert_eq!(
        shape(view(&project, "Fixture by due")),
        (Layout::Table, vec![], vec![], sort(ids::DUE, Desc))
    );

    let page = linear
        .issues_page(&project.board, MAX_PAGE, None)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 19);
    assert_eq!(page.next, None);
    for item in &page.nodes {
        assert!(key(item).starts_with("TBA-"), "{}", key(item));
    }

    assert_eq!(
        tab_counts(&linear, &project).await,
        [17, 19, 3, 19, 19, 19, 19, 19, 19, 14, 6]
    );

    let detail = linear
        .detail(&ItemId::new(COMMENTED_ISSUE), None)
        .await
        .unwrap();
    let thread: Vec<_> = detail
        .comments
        .iter()
        .map(|c| (c.depth, is_invented(&c.author)))
        .collect();
    assert_eq!(thread, [(0, true), (1, true)]);
    assert!(detail.comments[1].body.starts_with("Fixture reply"));
    assert_eq!(detail.older_cursor, None);
}

#[tokio::test]
async fn project_board_fixture_has_both_teams_and_project_views() {
    let linear = replay("project-board");
    let boards = linear.list_boards().await.unwrap();
    let summary = boards
        .iter()
        .find(|b| b.title == "Testbed project")
        .expect("the testbed project");
    let project = linear.resolve(&summary.board).await.unwrap();
    assert_eq!(project.board, summary.board);
    assert_eq!(project.title, "Testbed project");

    // TBA's "In Progress" and TBB's "Doing" are both columns; shared names merge later.
    let status = options(field(&project, ids::STATUS));
    let names: Vec<_> = status.iter().map(|o| o.name.as_str()).collect();
    assert!(
        names.contains(&"In Progress") && names.contains(&"Doing"),
        "{names:?}"
    );
    assert_eq!(status.len(), 12);

    // Each state is one of the two teams'; the Type group is the workspace's.
    let meta = project.linear.as_ref().unwrap();
    let mut keys: Vec<_> = meta.teams.iter().map(|t| t.key.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["TBA", "TBB"]);
    let tbb = &meta.teams.iter().find(|t| t.key == "TBB").unwrap().id;
    let owner = |id: &str| meta.owners.get(id);
    assert!(status.iter().all(|o| {
        meta.teams
            .iter()
            .any(|t| owner(o.id.as_str()) == Some(&t.id))
    }));
    assert!(status.iter().any(|o| owner(o.id.as_str()) == Some(tbb)));
    assert_eq!(owner(TYPE_GROUP), None);
    assert!(
        project.fields.iter().all(|f| f.id.as_str() != ids::PROJECT),
        "a project board has no Project field"
    );

    // The project's own view comes before the team and workspace views (Decision 2).
    assert_eq!(
        tab_names(&project),
        [
            "Board",
            "All issues",
            "My issues",
            "Probe project view",
            "Fixture by due",
            "Fixture by type",
            "Fixture by cycle",
            "Fixture by project",
            "Probe team board by status",
            "Probe hide completed",
            "Probe board by priority",
            "Probe list by assignee",
            "Testbed open",
            "Testbed A bugs",
        ]
    );
    assert!(matches!(
        view(&project, "Probe hide completed").filter,
        ViewFilter::LinearView {
            hide_closed: true,
            ..
        }
    ));

    let page = linear
        .issues_page(&project.board, MAX_PAGE, None)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 17);
    let keys: Vec<String> = page.nodes.iter().map(key).collect();
    assert!(keys.iter().any(|k| k.starts_with("TBA-")));
    assert!(keys.iter().any(|k| k.starts_with("TBB-")));

    assert_eq!(
        tab_counts(&linear, &project).await,
        [17, 17, 4, 17, 10, 10, 10, 10, 6, 6, 10, 10, 14, 2]
    );
}

#[tokio::test]
async fn boards_fixture_lists_the_project_then_the_teams() {
    let boards = replay("boards").list_boards().await.unwrap();
    let titles: Vec<_> = boards.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Testbed project", "Testbed B (TBB)", "Testbed A (TBA)"]
    );
    assert!(matches!(boards[0].board, BoardKey::LinearProject { .. }));
}

#[tokio::test]
async fn poll_fixture_decodes() {
    let board = BoardKey::LinearTeam { id: TBA_ID.into() };
    let page = replay("poll")
        .poll_page(&board, "2026-10-20T00:00:00Z", MAX_PAGE, None)
        .await
        .unwrap();
    let keys: Vec<String> = page
        .nodes
        .iter()
        .map(|p| match p {
            PolledIssue::Item(item) => key(item),
            PolledIssue::Trashed(id) => panic!("trashed {id:?}"),
        })
        .collect();
    assert_eq!(keys, ["TBA-16", "TBA-20"]);
}
