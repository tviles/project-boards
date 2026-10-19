//! Seeds the Linear testbed workspace (tests/linear-testbed.toml). Re-runnable: run it again
//! and it adds what is missing (a duplicate relation lost to an interrupted run is not re-created). Refuses to run against any other workspace.
//! Run: cargo run --example seed_linear_testbed
//! Exit status 2: Linear has not created the cycles yet (nothing was created after that point).
//! Prints counts and invented names only, never a person's name or email, never the key.

#[path = "../tests/common/linear_testbed.rs"]
mod linear_testbed;

use graphql_client::GraphQLQuery;
use linear_testbed::{Linear, Raw, testbed};
use project_boards_queries::linear::*;
use serde_json::Value;
use std::collections::HashMap;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// Sends one operation. Errors name the operation, the HTTP status, the error codes and the
/// server's first message, never the request body or the key.
async fn call(linear: &Linear, body: &impl serde::Serialize, what: &str) -> Result<Value, String> {
    let raw: Raw = linear
        .post(body)
        .await
        .map_err(|e| format!("{what}: {e}"))?;
    if raw.status != 200 || raw.body.get("errors").is_some() {
        return Err(format!(
            "{what}: HTTP {} {:?} {}",
            raw.status,
            raw.error_codes(),
            raw.body["errors"][0]["message"].as_str().unwrap_or("")
        ));
    }
    Ok(raw.data().clone())
}

/// Like `call`, but a failure ends the program (status 1).
async fn run(linear: &Linear, body: &impl serde::Serialize, what: &str) -> Value {
    call(linear, body, what).await.unwrap_or_else(|e| fail(&e))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

/// A mutation that reports `success: false` has not done its job.
fn expect_success(data: &Value, field: &str, what: &str) {
    if data[field]["success"].as_bool() != Some(true) {
        fail(&format!("{what}: the server answered success=false"));
    }
}

fn str_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn nodes(data: &Value, path: &[&str]) -> Vec<Value> {
    let mut v = data;
    for p in path {
        v = &v[*p];
    }
    v["nodes"].as_array().cloned().unwrap_or_default()
}

/// `YYYY-MM-DD` for today plus `days`, in UTC.
fn date_in(days: i64) -> String {
    let d = OffsetDateTime::now_utc().date() + Duration::days(days);
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// A cycle as read from the API: id, start and end.
type CycleSpan = (String, OffsetDateTime, OffsetDateTime);

/// The active cycle and the earliest future cycle that starts after `now`. Independent of
/// the order the API lists them in.
fn pick_cycles(
    active: Option<&CycleSpan>,
    future: &[CycleSpan],
    now: OffsetDateTime,
) -> Option<(String, String)> {
    let current = active?;
    let next = future
        .iter()
        .filter(|(id, s, _)| *s > now && *id != current.0)
        .min_by_key(|(_, s, _)| *s)?;
    Some((current.0.clone(), next.0.clone()))
}

/// A cycle object from the API; a date that does not parse is an error, not a missing cycle.
fn parse_span(c: &Value) -> Result<CycleSpan, String> {
    let at = |k: &str| {
        c[k].as_str()
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
            .ok_or_else(|| "cycle dates did not parse".to_string())
    };
    Ok((str_of(c, "id"), at("startsAt")?, at("endsAt")?))
}

/// What to do about one child label of a group.
#[derive(Debug, PartialEq)]
enum LabelPlan {
    /// Already under the group.
    Found(String),
    /// A parentless label of that name exists: move it under the group.
    Move(String),
    Create,
}

fn plan_child_label(existing: &[Value], name: &str, group_id: &str) -> LabelPlan {
    if let Some(id) = find_label(existing, name, Some(group_id), false) {
        LabelPlan::Found(id)
    } else if let Some(id) = find_label(existing, name, None, false) {
        LabelPlan::Move(id)
    } else {
        LabelPlan::Create
    }
}

/// A workflow state of the given type, preferring Linear's own name for it (so "Duplicate"
/// loses to "Canceled" and a renamed "Doing" still counts as started).
fn pick_state(states: &[(String, String, String)], kind: &str) -> Option<String> {
    const CANON: [&str; 6] = [
        "Backlog",
        "Todo",
        "In Progress",
        "Doing",
        "Done",
        "Canceled",
    ];
    let of_kind = || states.iter().filter(|(_, _, t)| t == kind);
    of_kind()
        .find(|(_, n, _)| CANON.contains(&n.as_str()))
        .or_else(|| of_kind().next())
        .map(|(id, _, _)| id.clone())
}

#[derive(Clone, Copy, PartialEq)]
enum Team {
    A,
    B,
}

#[derive(Clone, Copy, PartialEq)]
enum Cycle {
    None,
    Current,
    Next,
}

/// One seeded issue. `state` is a workflow state type (`backlog`, `unstarted`, `started`,
/// `completed`, `canceled`); `due` is days from today.
struct SeedIssue {
    team: Team,
    title: &'static str,
    state: &'static str,
    priority: i64,
    estimate: Option<i64>,
    due: Option<i64>,
    labels: &'static [&'static str],
    mine: bool,
    in_project: bool,
    cycle: Cycle,
    parent: Option<&'static str>,
    archived: bool,
    duplicate_of: Option<&'static str>,
}

const fn issue(team: Team, title: &'static str, state: &'static str, priority: i64) -> SeedIssue {
    SeedIssue {
        team,
        title,
        state,
        priority,
        estimate: None,
        due: None,
        labels: &[],
        mine: false,
        in_project: false,
        cycle: Cycle::None,
        parent: None,
        archived: false,
        duplicate_of: None,
    }
}

async fn create_team(linear: &Linear, name: &str, key: &str, created: &mut u32) -> String {
    let d = run(
        linear,
        &SeedCreateTeam::build_query(seed_create_team::Variables {
            name: name.into(),
            key: key.into(),
        }),
        "create team",
    )
    .await;
    *created += 1;
    str_of(&d["teamCreate"]["team"], "id")
}

/// A fixed invented colour per label: Linear rejects a null colour.
fn label_color(name: &str) -> String {
    match name {
        "Type" => "#5e6ad2",
        "Bug" => "#eb5757",
        "Feature" => "#bb87fc",
        "Chore" => "#95a2b3",
        "UI" => "#4cb782",
        "API" => "#f2c94c",
        "Docs" => "#f2994a",
        _ => "#4ea7fc",
    }
    .to_string()
}

/// How a new label hangs in the tree.
enum NewLabel {
    /// A single-select group (the workspace refuses multi-select ones).
    Group,
    Child {
        parent_id: String,
    },
    Loose,
}

async fn create_label(linear: &Linear, name: &str, kind: NewLabel) -> String {
    let (name, color) = (name.to_string(), label_color(name));
    let d = match kind {
        NewLabel::Group => {
            run(
                linear,
                &SeedCreateLabelGroup::build_query(seed_create_label_group::Variables {
                    name,
                    color,
                }),
                "create label group",
            )
            .await
        }
        NewLabel::Child { parent_id } => {
            run(
                linear,
                &SeedCreateChildLabel::build_query(seed_create_child_label::Variables {
                    name,
                    parent_id,
                    color,
                }),
                "create label",
            )
            .await
        }
        NewLabel::Loose => {
            run(
                linear,
                &SeedCreateLooseLabel::build_query(seed_create_loose_label::Variables {
                    name,
                    color,
                }),
                "create label",
            )
            .await
        }
    };
    str_of(&d["issueLabelCreate"]["issueLabel"], "id")
}

/// An existing label by name: a group (`isGroup`), a child of `parent`, or a loose label.
fn find_label(existing: &[Value], name: &str, parent: Option<&str>, group: bool) -> Option<String> {
    existing
        .iter()
        .find(|l| {
            l["name"].as_str() == Some(name)
                && (l["isGroup"].as_bool() == Some(true)) == group
                && l["parent"]["id"].as_str() == parent
        })
        .map(|l| str_of(l, "id"))
}

fn state_list(states: &[Value]) -> Vec<(String, String, String)> {
    states
        .iter()
        .map(|s| (str_of(s, "id"), str_of(s, "name"), str_of(s, "type")))
        .collect()
}

use Cycle::{Current, Next};
use Team::{A, B};

const ISSUES: &[SeedIssue] = &[
    SeedIssue {
        estimate: Some(3),
        labels: &["Bug", "UI"],
        mine: true,
        in_project: true,
        cycle: Current,
        ..issue(A, "Crash when opening settings", "started", 1)
    },
    SeedIssue {
        estimate: Some(2),
        labels: &["Feature", "API"],
        in_project: true,
        cycle: Current,
        due: Some(7),
        ..issue(A, "Export boards as CSV", "unstarted", 2)
    },
    SeedIssue {
        estimate: Some(1),
        labels: &["Chore", "Docs"],
        cycle: Next,
        ..issue(A, "Update the contributor guide", "backlog", 4)
    },
    SeedIssue {
        estimate: Some(5),
        labels: &["Feature", "UI", "API"],
        mine: true,
        in_project: true,
        cycle: Current,
        due: Some(-2),
        ..issue(A, "Keyboard shortcuts overlay", "started", 2)
    },
    SeedIssue {
        estimate: Some(8),
        labels: &["Feature"],
        in_project: true,
        ..issue(A, "Offline mode", "backlog", 3)
    },
    SeedIssue {
        labels: &["Bug"],
        in_project: true,
        cycle: Current,
        ..issue(A, "Sorting ignores accents", "completed", 3)
    },
    SeedIssue {
        labels: &["Bug", "good first issue"],
        ..issue(A, "Typo on the login page", "completed", 4)
    },
    SeedIssue {
        labels: &["Bug"],
        ..issue(
            A,
            "Login button misaligned on narrow screens",
            "canceled",
            0,
        )
    },
    SeedIssue {
        estimate: Some(2),
        labels: &["Chore"],
        in_project: true,
        ..issue(A, "Upgrade the charting library", "unstarted", 0)
    },
    SeedIssue {
        estimate: Some(3),
        labels: &["Feature", "UI"],
        in_project: true,
        ..issue(A, "Dark theme", "unstarted", 3)
    },
    SeedIssue {
        parent: Some("Dark theme"),
        labels: &["UI"],
        in_project: true,
        ..issue(A, "Dark theme: colour tokens", "completed", 3)
    },
    SeedIssue {
        parent: Some("Dark theme"),
        labels: &["UI"],
        in_project: true,
        ..issue(A, "Dark theme: settings toggle", "unstarted", 3)
    },
    SeedIssue {
        parent: Some("Dark theme"),
        labels: &["Docs"],
        in_project: true,
        ..issue(A, "Dark theme: screenshots in docs", "backlog", 4)
    },
    SeedIssue {
        labels: &["Bug"],
        duplicate_of: Some("Crash when opening settings"),
        ..issue(A, "Settings crash on second launch", "backlog", 1)
    },
    SeedIssue {
        archived: true,
        labels: &["Chore"],
        ..issue(A, "Remove the old beta banner", "completed", 4)
    },
    SeedIssue {
        estimate: Some(1),
        labels: &["Bug", "API"],
        mine: true,
        cycle: Next,
        due: Some(3),
        ..issue(A, "Rate limit errors are swallowed", "unstarted", 1)
    },
    SeedIssue {
        labels: &["Feature", "API"],
        in_project: true,
        mine: true,
        ..issue(B, "Webhook retries", "started", 2)
    },
    SeedIssue {
        labels: &["Bug", "API"],
        in_project: true,
        due: Some(1),
        ..issue(B, "Duplicate events after reconnect", "started", 1)
    },
    SeedIssue {
        labels: &["Feature"],
        in_project: true,
        ..issue(B, "Bulk import from spreadsheets", "backlog", 3)
    },
    SeedIssue {
        labels: &["Chore", "Docs"],
        ..issue(B, "Document the import format", "unstarted", 4)
    },
    SeedIssue {
        labels: &["Bug"],
        in_project: true,
        ..issue(B, "Import fails on empty rows", "completed", 2)
    },
    SeedIssue {
        labels: &["Feature", "UI"],
        ..issue(B, "Drag to reorder columns", "unstarted", 0)
    },
    SeedIssue {
        labels: &["Chore"],
        ..issue(B, "Rotate staging credentials", "canceled", 2)
    },
    SeedIssue {
        parent: Some("Bulk import from spreadsheets"),
        labels: &["API"],
        in_project: true,
        ..issue(B, "Bulk import: parse headers", "started", 3)
    },
    SeedIssue {
        parent: Some("Bulk import from spreadsheets"),
        labels: &["UI"],
        in_project: true,
        ..issue(B, "Bulk import: progress bar", "backlog", 3)
    },
    SeedIssue {
        labels: &["good first issue", "Docs"],
        ..issue(B, "Fix broken links in the FAQ", "backlog", 4)
    },
    SeedIssue {
        archived: true,
        ..issue(B, "Old migration checklist", "completed", 0)
    },
    SeedIssue {
        labels: &["Bug", "UI"],
        in_project: true,
        mine: true,
        ..issue(B, "Tooltip covers the save button", "unstarted", 2)
    },
];

#[tokio::main]
async fn main() {
    let tb = testbed();
    let linear = Linear::connect(&tb).await.unwrap_or_else(|e| fail(&e));
    println!("testbed workspace confirmed");
    // The viewer's id is the assignee of the "mine" issues; nothing about the person is printed.
    let viewer = run(
        &linear,
        &TestbedViewer::build_query(testbed_viewer::Variables {}),
        "viewer",
    )
    .await;
    let viewer_id = str_of(&viewer["viewer"], "id");

    // 1. Teams: rename the sign-up team (PRO) to TBA if needed, create TBB if missing.
    let data = run(
        &linear,
        &SeedTeams::build_query(seed_teams::Variables {}),
        "teams",
    )
    .await;
    let teams = nodes(&data, &["teams"]);
    let find_team = |key: &str| {
        teams
            .iter()
            .find(|t| t["key"].as_str() == Some(key))
            .map(|t| str_of(t, "id"))
    };
    let mut teams_changed = 0;
    let team_a = match (find_team(&tb.team_a), find_team("PRO")) {
        (Some(id), _) => id,
        (None, Some(id)) => {
            let d = run(
                &linear,
                &SeedRenameTeam::build_query(seed_rename_team::Variables {
                    id: id.clone(),
                    name: "Testbed A".into(),
                    key: tb.team_a.clone(),
                }),
                "rename team A",
            )
            .await;
            expect_success(&d, "teamUpdate", "rename team A");
            teams_changed += 1;
            id
        }
        (None, None) => create_team(&linear, "Testbed A", &tb.team_a, &mut teams_changed).await,
    };
    let team_b = match find_team(&tb.team_b) {
        Some(id) => id,
        None => create_team(&linear, "Testbed B", &tb.team_b, &mut teams_changed).await,
    };

    // 2. Team settings. TBA's cycles are enabled with a start date of now the first time only
    // (so the first cycle is current), and never re-sent once cycles are on.
    let a_state = run(
        &linear,
        &SeedTeamCycleState::build_query(seed_team_cycle_state::Variables { id: team_a.clone() }),
        "team A cycle state",
    )
    .await;
    if a_state["team"]["cyclesEnabled"].as_bool() == Some(true) {
        let d = run(
            &linear,
            &SeedTeamSettings::build_query(seed_team_settings::Variables {
                id: team_a.clone(),
                cycles: true,
                estimation: "fibonacci".into(),
            }),
            "team A settings",
        )
        .await;
        expect_success(&d, "teamUpdate", "team A settings");
    } else {
        let start = OffsetDateTime::now_utc()
            .replace_time(time::Time::MIDNIGHT)
            .format(&Rfc3339)
            .unwrap_or_else(|e| fail(&format!("cycle start date: {e}")));
        let d = run(
            &linear,
            &SeedEnableTeamCycles::build_query(seed_enable_team_cycles::Variables {
                id: team_a.clone(),
                start,
                estimation: "fibonacci".into(),
            }),
            "enable team A cycles",
        )
        .await;
        expect_success(&d, "teamUpdate", "enable team A cycles");
    }
    let d = run(
        &linear,
        &SeedTeamSettings::build_query(seed_team_settings::Variables {
            id: team_b.clone(),
            cycles: false,
            estimation: "notUsed".into(),
        }),
        "team B settings",
    )
    .await;
    expect_success(&d, "teamUpdate", "team B settings");

    // 3. TBB's "In Progress" state renamed "Doing" (skipped once renamed).
    let details =
        |id: &str| SeedTeamDetails::build_query(seed_team_details::Variables { id: id.into() });
    let details_b = run(&linear, &details(&team_b), "team B details").await;
    let states_b_raw = nodes(&details_b, &["team", "states"]);
    if !states_b_raw
        .iter()
        .any(|s| s["name"].as_str() == Some("Doing"))
    {
        let started = states_b_raw
            .iter()
            .find(|s| {
                s["name"].as_str() == Some("In Progress") && s["type"].as_str() == Some("started")
            })
            .unwrap_or_else(|| fail("team B has no \"In Progress\" state to rename"));
        let d = run(
            &linear,
            &SeedRenameState::build_query(seed_rename_state::Variables {
                id: str_of(started, "id"),
                name: "Doing".into(),
            }),
            "rename state",
        )
        .await;
        expect_success(&d, "workflowStateUpdate", "rename state");
    }
    let states_b = state_list(&states_b_raw);

    // 4. Labels: the "Type" group first (single-select), its children (matched by group, a
    // parentless default label is moved in), then the loose labels UI, API, Docs, good first issue.
    let data = run(
        &linear,
        &SeedLabels::build_query(seed_labels::Variables {}),
        "labels",
    )
    .await;
    let existing = nodes(&data, &["issueLabels"]);
    let mut labels_created = 0;
    let mut labels_moved = 0;
    let mut label_ids: HashMap<&str, String> = HashMap::new();
    {
        let (group, children) = ("Type", ["Bug", "Feature", "Chore"]);
        let group_id = match find_label(&existing, group, None, true) {
            Some(id) => id,
            None => {
                labels_created += 1;
                create_label(&linear, group, NewLabel::Group).await
            }
        };
        for child in children {
            let id = match plan_child_label(&existing, child, &group_id) {
                LabelPlan::Found(id) => id,
                LabelPlan::Move(id) => {
                    let d = run(
                        &linear,
                        &SeedMoveLabel::build_query(seed_move_label::Variables {
                            id: id.clone(),
                            parent_id: group_id.clone(),
                        }),
                        "move label",
                    )
                    .await;
                    expect_success(&d, "issueLabelUpdate", "move label");
                    labels_moved += 1;
                    id
                }
                LabelPlan::Create => {
                    labels_created += 1;
                    create_label(
                        &linear,
                        child,
                        NewLabel::Child {
                            parent_id: group_id.clone(),
                        },
                    )
                    .await
                }
            };
            label_ids.insert(child, id);
        }
    }
    for loose in ["UI", "API", "Docs", "good first issue"] {
        let id = match find_label(&existing, loose, None, false) {
            Some(id) => id,
            None => {
                labels_created += 1;
                create_label(&linear, loose, NewLabel::Loose).await
            }
        };
        label_ids.insert(loose, id);
    }

    // 5. The project, in both teams.
    let data = run(
        &linear,
        &SeedProjects::build_query(seed_projects::Variables {}),
        "projects",
    )
    .await;
    let mut projects_created = 0;
    let project_id = match nodes(&data, &["projects"])
        .iter()
        .find(|p| p["name"].as_str() == Some(tb.project.as_str()))
    {
        Some(p) => str_of(p, "id"),
        None => {
            projects_created += 1;
            let d = run(
                &linear,
                &SeedCreateProject::build_query(seed_create_project::Variables {
                    name: tb.project.clone(),
                    team_ids: vec![team_a.clone(), team_b.clone()],
                }),
                "create project",
            )
            .await;
            str_of(&d["projectCreate"]["project"], "id")
        }
    };

    // 6. Cycles on TBA (Linear creates them itself): stop before any issue if they are absent.
    let details_a = run(&linear, &details(&team_a), "team A details").await;
    let cycles = run(
        &linear,
        &SeedCycles::build_query(seed_cycles::Variables { id: team_a.clone() }),
        "team A cycles",
    )
    .await;
    let active = match &cycles["team"]["activeCycle"] {
        Value::Null => None,
        c => Some(parse_span(c).unwrap_or_else(|e| fail(&e))),
    };
    let future: Vec<CycleSpan> = nodes(&cycles, &["team", "cycles"])
        .iter()
        .map(|c| parse_span(c).unwrap_or_else(|e| fail(&e)))
        .collect();
    let Some((current_cycle, next_cycle)) =
        pick_cycles(active.as_ref(), &future, OffsetDateTime::now_utc())
    else {
        println!("cycles not created yet; run the seed again in a minute");
        std::process::exit(2);
    };
    let states_a = state_list(&nodes(&details_a, &["team", "states"]));

    // 7. Issues. Existing ones (by exact title in the team, archived included) are kept.
    struct Known {
        id: String,
        archived: bool,
        fresh: bool,
    }
    let mut known: HashMap<(bool, String), Known> = HashMap::new();
    for (is_a, id) in [(true, &team_a), (false, &team_b)] {
        let data = run(
            &linear,
            &SeedIssues::build_query(seed_issues::Variables {
                team_id: id.clone(),
            }),
            "issues",
        )
        .await;
        for i in nodes(&data, &["issues"]) {
            known.insert(
                (is_a, str_of(&i, "title")),
                Known {
                    id: str_of(&i, "id"),
                    archived: !i["archivedAt"].is_null(),
                    fresh: false,
                },
            );
        }
    }
    let mut issues_created = 0;
    for i in ISSUES {
        let is_a = i.team == Team::A;
        if known.contains_key(&(is_a, i.title.to_string())) {
            continue;
        }
        let (team_id, states) = if is_a {
            (&team_a, &states_a)
        } else {
            (&team_b, &states_b)
        };
        let state_id = pick_state(states, i.state)
            .unwrap_or_else(|| fail(&format!("{}: no {} state in the team", i.title, i.state)));
        let cycle_id = match i.cycle {
            Cycle::None => None,
            Cycle::Current => Some(current_cycle.clone()),
            Cycle::Next => Some(next_cycle.clone()),
        };
        let parent_id = i.parent.map(|p| {
            known
                .get(&(is_a, p.to_string()))
                .map(|k| k.id.clone())
                .unwrap_or_else(|| fail(&format!("{}: parent \"{p}\" does not exist", i.title)))
        });
        let created = run(
            &linear,
            &SeedCreateIssue::build_query(seed_create_issue::Variables {
                team_id: team_id.clone(),
                title: i.title.to_string(),
                description: Some(format!("Seeded test issue: {}.", i.title)),
                state_id: Some(state_id),
                priority: Some(i.priority),
                estimate: i.estimate,
                due_date: i.due.map(date_in),
                label_ids: Some(i.labels.iter().map(|l| label_ids[l].clone()).collect()),
                assignee_id: i.mine.then(|| viewer_id.clone()),
                project_id: i.in_project.then(|| project_id.clone()),
                cycle_id,
                parent_id,
            }),
            i.title,
        )
        .await;
        let id = str_of(&created["issueCreate"]["issue"], "id");
        if id.is_empty() {
            fail(&format!("{}: the server returned no issue", i.title));
        }
        issues_created += 1;
        known.insert(
            (is_a, i.title.to_string()),
            Known {
                id,
                archived: false,
                fresh: true,
            },
        );
    }
    // Duplicate relations (only for issues made in this run), then archiving (also for an
    // earlier run that stopped before it got here).
    let mut relations_created = 0;
    let mut issues_archived = 0;
    for i in ISSUES {
        let is_a = i.team == Team::A;
        let this = &known[&(is_a, i.title.to_string())];
        if let (Some(of), true) = (i.duplicate_of, this.fresh) {
            let original = &known[&(is_a, of.to_string())];
            let d = run(
                &linear,
                &SeedMarkDuplicate::build_query(seed_mark_duplicate::Variables {
                    issue_id: this.id.clone(),
                    related_issue_id: original.id.clone(),
                }),
                "mark duplicate",
            )
            .await;
            expect_success(&d, "issueRelationCreate", "mark duplicate");
            relations_created += 1;
        }
    }
    for i in ISSUES {
        let this = &known[&(i.team == Team::A, i.title.to_string())];
        if i.archived && !this.archived {
            let d = run(
                &linear,
                &SeedArchiveIssue::build_query(seed_archive_issue::Variables {
                    id: this.id.clone(),
                }),
                "archive issue",
            )
            .await;
            expect_success(&d, "issueArchive", "archive issue");
            issues_archived += 1;
        }
    }

    // 8. Views. A failure here is an answer for probe L4, not a reason to stop.
    let data = run(
        &linear,
        &SeedViews::build_query(seed_views::Variables {}),
        "views",
    )
    .await;
    let views = nodes(&data, &["customViews"]);
    let has_view = |name: &str| views.iter().any(|v| v["name"].as_str() == Some(name));
    let mut views_created = 0;
    let mut view_results = Vec::new();
    if !has_view("Testbed A bugs") {
        view_results.push(
            call(
                &linear,
                &SeedCreateTeamBugsView::build_query(seed_create_team_bugs_view::Variables {
                    name: "Testbed A bugs".into(),
                    team_id: team_a.clone(),
                }),
                "create team view",
            )
            .await
            .and_then(|d| view_ok(&d)),
        );
    }
    if !has_view("Testbed open") {
        view_results.push(
            call(
                &linear,
                &SeedCreateOpenView::build_query(seed_create_open_view::Variables {
                    name: "Testbed open".into(),
                }),
                "create workspace view",
            )
            .await
            .and_then(|d| view_ok(&d)),
        );
    }
    for r in view_results {
        match r {
            Ok(()) => views_created += 1,
            Err(e) => eprintln!("customViewCreate failed (probe L4): {e}"),
        }
    }

    println!(
        "teams ok ({teams_changed} changed), {labels_created} labels created, {labels_moved} labels moved, {projects_created} projects created, {issues_created} issues created, {relations_created} duplicate relations, {issues_archived} issues archived, {views_created} views created"
    );
}

fn view_ok(d: &Value) -> Result<(), String> {
    if d["customViewCreate"]["success"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err("the server answered success=false".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn span(id: &str, s: OffsetDateTime, e: OffsetDateTime) -> CycleSpan {
        (id.into(), s, e)
    }

    #[test]
    fn picks_the_active_and_the_earliest_future_cycle_in_any_list_order() {
        let now = datetime!(2026-10-16 12:00 UTC);
        let active = span(
            "now",
            datetime!(2026-10-12 0:00 UTC),
            datetime!(2026-10-19 0:00 UTC),
        );
        let future = [
            span(
                "later",
                datetime!(2026-10-26 0:00 UTC),
                datetime!(2026-11-02 0:00 UTC),
            ),
            span(
                "next",
                datetime!(2026-10-19 0:00 UTC),
                datetime!(2026-10-26 0:00 UTC),
            ),
        ];
        assert_eq!(
            pick_cycles(Some(&active), &future, now),
            Some(("now".into(), "next".into()))
        );
        assert_eq!(pick_cycles(None, &future, now), None);
        assert_eq!(pick_cycles(Some(&active), &[], now), None);
    }

    #[test]
    fn cycle_dates_that_do_not_parse_are_an_error() {
        let ok = serde_json::json!({"id": "c", "startsAt": "2026-10-12T00:00:00.000Z", "endsAt": "2026-10-19T00:00:00.000Z"});
        assert!(parse_span(&ok).is_ok());
        let bad = serde_json::json!({"id": "c", "startsAt": "tomorrow", "endsAt": "2026-10-19T00:00:00.000Z"});
        assert_eq!(parse_span(&bad), Err("cycle dates did not parse".into()));
    }

    #[test]
    fn a_parentless_label_is_moved_into_its_group_and_a_moved_one_is_found() {
        let label = |id: &str, name: &str, group: bool, parent: Option<&str>| {
            serde_json::json!({"id": id, "name": name, "isGroup": group,
                "parent": parent.map(|p| serde_json::json!({"id": p}))})
        };
        let mut existing = vec![
            label("g", "Type", true, None),
            label("b", "Bug", false, None),
            label("i", "Improvement", false, None),
        ];
        assert_eq!(
            plan_child_label(&existing, "Bug", "g"),
            LabelPlan::Move("b".into())
        );
        assert_eq!(plan_child_label(&existing, "Chore", "g"), LabelPlan::Create);
        existing[1] = label("b", "Bug", false, Some("g"));
        assert_eq!(
            plan_child_label(&existing, "Bug", "g"),
            LabelPlan::Found("b".into())
        );
    }

    #[test]
    fn picks_a_state_by_type_preferring_linears_own_name() {
        let s = |id: &str, name: &str, t: &str| (id.to_string(), name.to_string(), t.to_string());
        let states = [
            s("1", "Duplicate", "canceled"),
            s("2", "Canceled", "canceled"),
            s("3", "Doing", "started"),
        ];
        assert_eq!(pick_state(&states, "canceled").as_deref(), Some("2"));
        assert_eq!(pick_state(&states, "started").as_deref(), Some("3"));
        assert_eq!(pick_state(&states, "completed"), None);
    }

    #[test]
    fn every_seeded_label_has_a_hex_colour() {
        for n in [
            "Type",
            "Bug",
            "Feature",
            "Chore",
            "UI",
            "API",
            "Docs",
            "good first issue",
        ] {
            let c = label_color(n);
            assert!(
                c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit()),
                "{n}"
            );
        }
    }

    #[test]
    fn dates_are_iso() {
        let d = date_in(0);
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }

    #[test]
    fn every_parent_comes_first_and_exists_in_its_team() {
        for (n, i) in ISSUES.iter().enumerate() {
            if let Some(p) = i.parent {
                assert!(
                    ISSUES[..n].iter().any(|o| o.title == p && o.team == i.team),
                    "{}",
                    i.title
                );
            }
            if let Some(p) = i.duplicate_of {
                assert!(
                    ISSUES.iter().any(|o| o.title == p && o.team == i.team),
                    "{}",
                    i.title
                );
            }
        }
    }
}
