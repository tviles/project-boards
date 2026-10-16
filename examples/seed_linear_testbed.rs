//! Seeds the Linear testbed workspace (tests/linear-testbed.toml). Idempotent: run it again
//! and it only adds what is missing. Refuses to run against any other workspace.
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

/// The cycle that contains `now` and the first one that starts after it.
fn pick_cycles(cycles: &[CycleSpan], now: OffsetDateTime) -> Option<(String, String)> {
    let current = cycles.iter().find(|(_, s, e)| *s <= now && now < *e)?;
    let next = cycles
        .iter()
        .filter(|(_, s, _)| *s > now)
        .min_by_key(|(_, s, _)| *s)?;
    Some((current.0.clone(), next.0.clone()))
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

async fn create_label(
    linear: &Linear,
    name: &str,
    is_group: bool,
    single_select: Option<bool>,
    parent_id: Option<String>,
) -> String {
    use seed_create_label::LabelGroupType;
    let d = run(
        linear,
        &SeedCreateLabel::build_query(seed_create_label::Variables {
            name: name.into(),
            is_group,
            group_type: single_select.map(|single| {
                if single {
                    LabelGroupType::singleSelect
                } else {
                    LabelGroupType::multiSelect
                }
            }),
            parent_id,
            color: None,
        }),
        "create label",
    )
    .await;
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

    // 2. Team settings: the same values every run.
    for (id, cycles, estimation, what) in [
        (&team_a, true, "fibonacci", "team A settings"),
        (&team_b, false, "notUsed", "team B settings"),
    ] {
        let d = run(
            &linear,
            &SeedTeamSettings::build_query(seed_team_settings::Variables {
                id: id.clone(),
                cycles,
                estimation: estimation.into(),
            }),
            what,
        )
        .await;
        expect_success(&d, "teamUpdate", what);
    }

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

    // 4. Labels: groups first, then their children (matched by group), then the loose label.
    let data = run(
        &linear,
        &SeedLabels::build_query(seed_labels::Variables {}),
        "labels",
    )
    .await;
    let existing = nodes(&data, &["issueLabels"]);
    let mut labels_created = 0;
    let mut label_ids: HashMap<&str, String> = HashMap::new();
    let groups: [(&str, bool, [&str; 3]); 2] = [
        ("Type", true, ["Bug", "Feature", "Chore"]),
        ("Area", false, ["UI", "API", "Docs"]),
    ];
    for (group, single, children) in groups {
        let group_id = match find_label(&existing, group, None, true) {
            Some(id) => id,
            None => {
                labels_created += 1;
                create_label(&linear, group, true, Some(single), None).await
            }
        };
        for child in children {
            let id = match find_label(&existing, child, Some(&group_id), false) {
                Some(id) => id,
                None => {
                    labels_created += 1;
                    create_label(&linear, child, false, None, Some(group_id.clone())).await
                }
            };
            label_ids.insert(child, id);
        }
    }
    let loose = "good first issue";
    let id = match find_label(&existing, loose, None, false) {
        Some(id) => id,
        None => {
            labels_created += 1;
            create_label(&linear, loose, false, None, None).await
        }
    };
    label_ids.insert(loose, id);

    // 5. The project, in both teams.
    let data = run(
        &linear,
        &SeedProjects::build_query(seed_projects::Variables {}),
        "projects",
    )
    .await;
    let project_id = match nodes(&data, &["projects"])
        .iter()
        .find(|p| p["name"].as_str() == Some(tb.project.as_str()))
    {
        Some(p) => str_of(p, "id"),
        None => {
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
    let spans: Vec<CycleSpan> = nodes(&details_a, &["team", "cycles"])
        .iter()
        .filter_map(|c| {
            let at = |k: &str| OffsetDateTime::parse(c[k].as_str()?, &Rfc3339).ok();
            Some((str_of(c, "id"), at("startsAt")?, at("endsAt")?))
        })
        .collect();
    let Some((current_cycle, next_cycle)) = pick_cycles(&spans, OffsetDateTime::now_utc()) else {
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
        "teams ok ({teams_changed} changed), {labels_created} labels created, {issues_created} issues created, {relations_created} duplicate relations, {issues_archived} issues archived, {views_created} views created"
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
    fn picks_the_current_and_the_next_cycle() {
        let now = datetime!(2026-10-16 12:00 UTC);
        let cycles = [
            span(
                "past",
                datetime!(2026-10-02 0:00 UTC),
                datetime!(2026-10-09 0:00 UTC),
            ),
            span(
                "later",
                datetime!(2026-10-23 0:00 UTC),
                datetime!(2026-10-30 0:00 UTC),
            ),
            span(
                "now",
                datetime!(2026-10-12 0:00 UTC),
                datetime!(2026-10-19 0:00 UTC),
            ),
            span(
                "next",
                datetime!(2026-10-19 0:00 UTC),
                datetime!(2026-10-23 0:00 UTC),
            ),
        ];
        assert_eq!(
            pick_cycles(&cycles, now),
            Some(("now".into(), "next".into()))
        );
        assert_eq!(pick_cycles(&cycles[..2], now), None);
        assert_eq!(pick_cycles(&[], now), None);
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
