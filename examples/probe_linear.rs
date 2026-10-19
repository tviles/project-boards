//! Phase 0 probes L1-L9 against the Linear testbed (tests/linear-testbed.toml). Prints a
//! Markdown report for docs/phase0-findings.md. Refuses to run against any other workspace.
//! Run: cargo run --example probe_linear
//!
//! The report holds identifiers (TBA-1), counts, seconds, HTTP statuses, error codes, header
//! values, view names and view settings, and the first error message Linear returns for a
//! request. It never holds a person's name, an email, an issue title or the API key: the
//! operations that read people or titles are only counted, never printed.

#[path = "../tests/common/linear_testbed.rs"]
mod linear_testbed;

use graphql_client::GraphQLQuery;
use linear_testbed::{Linear, Raw, testbed};
use project_boards_queries::linear::*;
use serde_json::Value;
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[tokio::main]
async fn main() {
    let tb = testbed();
    let linear = Linear::connect(&tb).await.unwrap_or_else(|e| panic!("{e}"));
    let team_a = team_id(&linear, &tb.team_a).await;
    let team_b = team_id(&linear, &tb.team_b).await;
    println!("# Linear phase 0 report\n");
    l2_page_cost(&linear, &team_a).await;
    l3_l4_l5_views(&linear, &team_b, &tb.project).await;
    l6_errors(&linear).await;
    l1_read_after_write(&linear, &team_a).await;
    l7_archive_and_trash(&linear, &team_a).await;
    l8_project_teams(&linear, &tb.project).await;
    l9_identifier_and_search(&linear, &tb.team_a).await;
}

async fn post(linear: &Linear, body: &impl serde::Serialize) -> Raw {
    linear
        .post(body)
        .await
        .unwrap_or_else(|e| panic!("request failed: {e}"))
}

/// Like `post`, but a transport failure becomes an empty status-0 response instead of a panic:
/// used wherever L1's restore must still run afterwards.
async fn post_soft(linear: &Linear, body: &impl serde::Serialize) -> Raw {
    linear.post(body).await.unwrap_or_else(|e| Raw {
        status: 0,
        headers: vec![],
        body: Value::Null,
        text: Some(format!("transport error: {e}")),
    })
}

async fn team_id(linear: &Linear, key: &str) -> String {
    let raw = post(linear, &SeedTeams::build_query(seed_teams::Variables {})).await;
    raw.data()["teams"]["nodes"]
        .as_array()
        .and_then(|teams| teams.iter().find(|t| t["key"] == key))
        .and_then(|t| t["id"].as_str())
        .unwrap_or_else(|| panic!("team {key} not found; run the seed first"))
        .to_string()
}

// ---- small helpers -------------------------------------------------------------------------

fn nodes(v: &Value) -> Vec<Value> {
    v["nodes"].as_array().cloned().unwrap_or_default()
}

fn codes(raw: &Raw) -> String {
    let c = raw.error_codes();
    if c.is_empty() {
        "none".to_string()
    } else {
        c.join(", ")
    }
}

fn header(raw: &Raw, name: &str) -> String {
    raw.header(name).unwrap_or("-").to_string()
}

fn secs(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

/// One line of text, cut to 200 characters (an error message or the start of an error page),
/// with anything between `<` and `>` removed so it cannot inject Markdown or HTML.
fn short(text: &str) -> String {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let one: String = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(200).collect()
}

/// What came back, for the error probes: the status, the error codes and the first message
/// (or, when the body was not JSON, the start of the text).
fn describe(raw: &Raw) -> String {
    let message = match &raw.text {
        Some(text) => format!("non-JSON body: {}", short(text)),
        None => short(raw.body["errors"][0]["message"].as_str().unwrap_or("")),
    };
    format!(
        "HTTP {}; codes: {}; first message: {}",
        raw.status,
        codes(raw),
        if message.is_empty() { "-" } else { &message }
    )
}

/// `Issue.priority` is a Float in the schema.
fn priority_of(issue: &Value) -> Option<i64> {
    issue["priority"].as_f64().map(|p| p.round() as i64)
}

/// The node with `id` in `issues.nodes`, if listed.
fn find_listed(raw: &Raw, id: &str) -> Option<Value> {
    nodes(&raw.data()["issues"])
        .into_iter()
        .find(|n| n["id"] == id)
}

async fn view_id(linear: &Linear, name: &str) -> Option<String> {
    let raw = post_soft(linear, &ProbeViews::build_query(probe_views::Variables {})).await;
    nodes(&raw.data()["customViews"])
        .iter()
        .find(|v| v["name"] == name)
        .and_then(|v| v["id"].as_str())
        .map(String::from)
}

async fn issue_by(linear: &Linear, id: &str) -> Raw {
    post_soft(
        linear,
        &ProbeIssue::build_query(probe_issue::Variables { id: id.into() }),
    )
    .await
}

async fn updated_since(linear: &Linear, team: &str, since: &str) -> Raw {
    post_soft(
        linear,
        &ProbeUpdatedSince::build_query(probe_updated_since::Variables {
            team_id: team.into(),
            since: since.into(),
        }),
    )
    .await
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).expect("RFC 3339 formats")
}

// ---- L2 ------------------------------------------------------------------------------------

async fn l2_page_cost(linear: &Linear, team: &str) {
    println!("## L2: query cost of the issue page\n");
    println!("| first | status | X-Complexity | complexity remaining | nodes | error codes |");
    println!("|---|---|---|---|---|---|");
    for first in [100, 250, 1000] {
        let raw = post(
            linear,
            &ProbeIssuePage::build_query(probe_issue_page::Variables {
                team_id: team.into(),
                first,
            }),
        )
        .await;
        println!(
            "| {first} | {} | {} | {} | {} | {} |",
            raw.status,
            header(&raw, "x-complexity"),
            header(&raw, "x-ratelimit-complexity-remaining"),
            nodes(&raw.data()["issues"]).len(),
            codes(&raw)
        );
    }
    println!();
}

// ---- L3, L4, L5 ----------------------------------------------------------------------------

async fn completed_in_view(linear: &Linear, id: &str) -> (usize, usize) {
    let raw = post(
        linear,
        &ProbeViewIssues::build_query(probe_view_issues::Variables {
            id: id.into(),
            first: 100,
        }),
    )
    .await;
    let issues = nodes(&raw.data()["customView"]["issues"]);
    let done = issues
        .iter()
        .filter(|i| i["state"]["type"] == "completed")
        .count();
    (issues.len(), done)
}

async fn l3_l4_l5_views(linear: &Linear, team_b: &str, project: &str) {
    println!("## L3/L4/L5: custom views\n");
    let raw = post(linear, &ProbeViews::build_query(probe_views::Variables {})).await;
    let views = nodes(&raw.data()["customViews"]);
    println!(
        "ProbeViews: HTTP {}, {} views, error codes: {}\n",
        raw.status,
        views.len(),
        codes(&raw)
    );
    println!(
        "| view | team | project-scoped | layout | issueGrouping | issueSubGrouping | viewOrdering | viewOrderingDirection | showCompletedIssues |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    let text = |v: &Value| match v {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    for v in &views {
        let team = v["team"]["key"].as_str().unwrap_or("workspace");
        let p = &v["viewPreferencesValues"];
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            v["name"].as_str().unwrap_or(""),
            team,
            if v["facet"]["sourceProject"].is_null() {
                "no"
            } else {
                "yes"
            },
            text(&p["layout"]),
            text(&p["issueGrouping"]),
            text(&p["issueSubGrouping"]),
            text(&p["viewOrdering"]),
            text(&p["viewOrderingDirection"]),
            text(&p["showCompletedIssues"]),
        );
    }
    let find = |name: &str| {
        views
            .iter()
            .find(|v| v["name"] == name)
            .and_then(|v| v["id"].as_str())
            .map(String::from)
    };

    println!();
    let in_list = views.iter().any(|v| v["name"] == "Probe project view");
    println!(
        "L4: \"Probe project view\" is {}in the customViews list.",
        if in_list { "" } else { "NOT " }
    );
    let projects = post_soft(
        linear,
        &ProbeProjectViews::build_query(probe_project_views::Variables {
            name: project.into(),
        }),
    )
    .await;
    let facet_view = nodes(&projects.data()["projects"])
        .first()
        .and_then(|p| p["facets"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|f| f["targetCustomView"].clone())
        .find(|v| v["name"] == "Probe project view");
    match facet_view {
        Some(v) => {
            let p = &v["viewPreferencesValues"];
            let ids = v["id"].as_str().unwrap_or("");
            let (count, _) = completed_in_view(linear, ids).await;
            println!(
                "L4: it IS reachable through Project.facets (team: {}; layout {}, issueGrouping {}, issueSubGrouping {}, viewOrdering {}, viewOrderingDirection {}, showCompletedIssues {}); ProbeViewIssues on it returns {count} issues.",
                v["team"]["key"].as_str().unwrap_or("workspace"),
                text(&p["layout"]),
                text(&p["issueGrouping"]),
                text(&p["issueSubGrouping"]),
                text(&p["viewOrdering"]),
                text(&p["viewOrderingDirection"]),
                text(&p["showCompletedIssues"]),
            );
        }
        None => println!(
            "L4: it is NOT reachable through Project.facets (HTTP {}, codes: {}).",
            projects.status,
            codes(&projects)
        ),
    }

    match find("Probe hide completed") {
        Some(id) => {
            let (in_view, done_in_view) = completed_in_view(linear, &id).await;
            let page = post(
                linear,
                &ProbeIssuePage::build_query(probe_issue_page::Variables {
                    team_id: team_b.into(),
                    first: 100,
                }),
            )
            .await;
            let tbb = nodes(&page.data()["issues"]);
            let tbb_done = tbb
                .iter()
                .filter(|i| i["state"]["type"] == "completed")
                .count();
            println!(
                "L5: \"Probe hide completed\" returns {in_view} issues through ProbeViewIssues (first 100), of which {done_in_view} are completed; TBB has {} issues in its first page, of which {tbb_done} are completed.",
                tbb.len()
            );
        }
        None => println!("L5: \"Probe hide completed\" is not in the list."),
    }
    println!();
}

// ---- L6 ------------------------------------------------------------------------------------

async fn l6_errors(linear: &Linear) {
    println!("## L6: errors\n");

    let bad = Linear::for_error_probe("lin_api_invalid0000".into());
    let body = TestbedViewer::build_query(testbed_viewer::Variables {});
    match bad.post(&body).await {
        Ok(raw) => println!("- bad key, TestbedViewer: {}", describe(&raw)),
        Err(e) => println!("- bad key, TestbedViewer: request failed: {}", short(&e)),
    }

    let raw = issue_by(linear, "00000000-0000-4000-8000-000000000000").await;
    println!("- random UUID, ProbeIssue: {}", describe(&raw));

    let raw = issue_by(linear, "ZZZ-99999").await;
    println!(
        "- unknown identifier ZZZ-99999, ProbeIssue: {}",
        describe(&raw)
    );

    match std::env::var("PB_LINEAR_REVOKED_KEY") {
        Ok(key) if !key.trim().is_empty() => {
            let revoked = Linear::for_error_probe(key.trim().to_string());
            match revoked.post(&body).await {
                Ok(raw) if raw.status == 200 && !raw.data().is_null() => println!(
                    "- WARNING: PB_LINEAR_REVOKED_KEY still works \u{2014} it is not revoked (HTTP 200 with data)"
                ),
                Ok(raw) => println!("- revoked key, TestbedViewer: {}", describe(&raw)),
                Err(e) => println!(
                    "- revoked key, TestbedViewer: request failed: {}",
                    short(&e)
                ),
            }
        }
        _ => println!("- revoked key: not run (PB_LINEAR_REVOKED_KEY is not set)"),
    }
    println!();
}

// ---- L1 ------------------------------------------------------------------------------------

async fn set_priority(linear: &Linear, id: &str, priority: i64) -> Raw {
    post_soft(
        linear,
        &ProbeSetPriority::build_query(probe_set_priority::Variables {
            id: id.into(),
            priority,
        }),
    )
    .await
}

fn update_ok(raw: &Raw) -> bool {
    raw.status == 200
        && raw.error_codes().is_empty()
        && !raw.data()["issueUpdate"]["issue"].is_null()
}

/// Everything after the first write goes through paths that cannot panic, so the priority is
/// always put back (three tries, then a loud line).
async fn l1_read_after_write(linear: &Linear, team: &str) {
    println!("## L1: read-after-write delay\n");
    let raw = issue_by(linear, "TBA-1").await;
    let issue = &raw.data()["issue"];
    let (Some(id), Some(original)) = (issue["id"].as_str().map(String::from), priority_of(issue))
    else {
        println!("TBA-1 could not be read: {}\n", describe(&raw));
        return;
    };
    let new = if original == 1 { 2 } else { 1 };

    let raw = set_priority(linear, &id, new).await;
    let t0 = Instant::now();
    let updated = raw.data()["issueUpdate"]["issue"]["updatedAt"]
        .as_str()
        .map(String::from);
    println!(
        "Set TBA-1 priority {original} -> {new}: {}; updatedAt returned: {}",
        describe(&raw),
        updated.is_some()
    );
    if update_ok(&raw) {
        poll_until_visible(linear, team, &id, new, updated.as_deref(), t0).await;
    }

    // Restore, whatever happened above.
    let mut restored = false;
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2 * attempt)).await;
        }
        let raw = set_priority(linear, &id, original).await;
        if update_ok(&raw) {
            restored = true;
            break;
        }
        last = describe(&raw);
    }
    if restored {
        let raw = issue_by(linear, "TBA-1").await;
        println!(
            "Restored TBA-1 priority to {original}; confirmed by a read: {}",
            priority_of(&raw.data()["issue"])
                .map(|p| p.to_string())
                .unwrap_or_else(|| format!("unknown ({})", describe(&raw)))
        );
    } else {
        println!(
            "RESTORE FAILED after 3 tries ({last}): set TBA-1 priority back to {original} by hand"
        );
    }
    println!();
}

async fn poll_until_visible(
    linear: &Linear,
    team: &str,
    id: &str,
    new: i64,
    updated_at: Option<&str>,
    t0: Instant,
) {
    // One second before the returned updatedAt; a duration if that did not parse.
    let since = updated_at
        .and_then(|u| OffsetDateTime::parse(u, &Rfc3339).ok())
        .map(|u| rfc3339(u - time::Duration::seconds(1)))
        .unwrap_or_else(|| "-PT1M".to_string());
    let board = view_id(linear, "Probe board by priority").await;

    let (mut a, mut b, mut c): (Option<f64>, Option<f64>, Option<f64>) = (None, None, None);
    let mut absent_from_view = false;
    while t0.elapsed() < Duration::from_secs(120) {
        if a.is_none() {
            let raw = updated_since(linear, team, &since).await;
            if find_listed(&raw, id).and_then(|n| priority_of(&n)) == Some(new) {
                a = Some(secs(t0));
            }
        }
        if b.is_none() {
            let raw = issue_by(linear, id).await;
            if priority_of(&raw.data()["issue"]) == Some(new) {
                b = Some(secs(t0));
            }
        }
        if c.is_none()
            && !absent_from_view
            && let Some(view) = &board
        {
            let raw = post_soft(
                linear,
                &ProbeViewIssues::build_query(probe_view_issues::Variables {
                    id: view.clone(),
                    first: 100,
                }),
            )
            .await;
            let listed = nodes(&raw.data()["customView"]["issues"])
                .into_iter()
                .find(|n| n["id"] == id);
            match listed {
                Some(n) if priority_of(&n) == Some(new) => c = Some(secs(t0)),
                Some(_) => {}
                None if raw.status == 200 && !raw.data()["customView"].is_null() => {
                    absent_from_view = true;
                }
                None => {}
            }
        }
        if a.is_some() && b.is_some() && (c.is_some() || board.is_none() || absent_from_view) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let show = |v: Option<f64>| match v {
        Some(s) => format!("{s:.1} s"),
        None => "not seen within 120 s".to_string(),
    };
    println!("| path | seen with the new priority after |");
    println!("|---|---|");
    println!(
        "| a: updatedAt-filtered list (includeArchived) | {} |",
        show(a)
    );
    println!("| b: single issue by id | {} |", show(b));
    match &board {
        Some(_) if absent_from_view => {
            println!("| c: \"Probe board by priority\" view issues | TBA-1 not in the view |")
        }
        Some(_) => println!(
            "| c: \"Probe board by priority\" view issues | {} |",
            show(c)
        ),
        None => println!("| c: \"Probe board by priority\" view issues | view not found |"),
    }
}

// ---- L7 ------------------------------------------------------------------------------------

async fn l7_archive_and_trash(linear: &Linear, team: &str) {
    println!("## L7: archive and trash in the polled list\n");
    let create = |title: &str| {
        ProbeCreateIssue::build_query(probe_create_issue::Variables {
            team_id: team.into(),
            title: title.into(),
        })
    };
    let archive_raw = post_soft(linear, &create("[probe] L7 archive")).await;
    let trash_raw = post_soft(linear, &create("[probe] L7 trash")).await;
    let id_of = |raw: &Raw| {
        raw.data()["issueCreate"]["issue"]["id"]
            .as_str()
            .map(String::from)
    };
    let (Some(archive_id), Some(trash_id)) = (id_of(&archive_raw), id_of(&trash_raw)) else {
        // Do not leave an active probe issue behind: trash whichever one was created.
        for created in [id_of(&archive_raw), id_of(&trash_raw)]
            .into_iter()
            .flatten()
        {
            let raw = post_soft(
                linear,
                &ProbeTrash::build_query(probe_trash::Variables { id: created }),
            )
            .await;
            println!(
                "Trashed the probe issue that was created: HTTP {}",
                raw.status
            );
        }
        println!(
            "Creating the probe issues failed: archive: {}; trash: {}\n",
            describe(&archive_raw),
            describe(&trash_raw)
        );
        return;
    };

    let t0 = OffsetDateTime::now_utc();
    let archived = post_soft(
        linear,
        &ProbeArchive::build_query(probe_archive::Variables {
            id: archive_id.clone(),
        }),
    )
    .await;
    let trashed = post_soft(
        linear,
        &ProbeTrash::build_query(probe_trash::Variables {
            id: trash_id.clone(),
        }),
    )
    .await;
    println!(
        "issueArchive: HTTP {}, success: {}, codes: {}",
        archived.status,
        archived.data()["issueArchive"]["success"],
        codes(&archived)
    );
    println!(
        "issueDelete: HTTP {}, success: {}, codes: {}\n",
        trashed.status,
        trashed.data()["issueDelete"]["success"],
        codes(&trashed)
    );

    let since = rfc3339(t0 - time::Duration::seconds(5));
    println!("| when | issue | listed | archivedAt set | trashed |");
    println!("|---|---|---|---|---|");
    for (when, wait) in [("immediately", 0), ("after 10 s", 10)] {
        if wait > 0 {
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
        let raw = updated_since(linear, team, &since).await;
        for (name, id) in [("archived one", &archive_id), ("trashed one", &trash_id)] {
            match find_listed(&raw, id) {
                Some(n) => println!(
                    "| {when} | {name} | yes | {} | {} |",
                    !n["archivedAt"].is_null(),
                    n["trashed"]
                ),
                None => println!(
                    "| {when} | {name} | no (HTTP {}, codes: {}) | - | - |",
                    raw.status,
                    codes(&raw)
                ),
            }
        }
    }
    println!();
}

// ---- L8 ------------------------------------------------------------------------------------

async fn l8_project_teams(linear: &Linear, project: &str) {
    println!("## L8: a project's teams\n");
    let raw = post(
        linear,
        &ProbeProjectTeams::build_query(probe_project_teams::Variables {
            name: project.into(),
        }),
    )
    .await;
    let found = nodes(&raw.data()["projects"]);
    let Some(p) = found.first() else {
        println!("The project was not found: {}\n", describe(&raw));
        return;
    };
    let keys = |list: Vec<Value>, path: &dyn Fn(&Value) -> Value| {
        let mut keys: Vec<String> = list
            .iter()
            .filter_map(|n| path(n).as_str().map(String::from))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    };
    let teams = keys(nodes(&p["teams"]), &|n| n["key"].clone());
    let on_issues = keys(nodes(&p["issues"]), &|n| n["team"]["key"].clone());
    println!("- project teams: {}", teams.join(", "));
    println!(
        "- team keys on its issues (first 100, {} issues): {}",
        nodes(&p["issues"]).len(),
        on_issues.join(", ")
    );
    println!("- they match: {}\n", teams == on_issues);
}

// ---- L9 ------------------------------------------------------------------------------------

async fn l9_identifier_and_search(linear: &Linear, team_key: &str) {
    println!("## L9: identifier lookup and search\n");
    let ident = format!("{team_key}-1");
    let raw = issue_by(linear, &ident).await;
    let works = raw.data()["issue"]["identifier"] == ident.as_str();
    println!(
        "- ProbeIssue(\"{ident}\"): {} (HTTP {}, codes: {})",
        if raw.status == 0 {
            "unknown (transport error)"
        } else if works {
            "works"
        } else {
            "does not work"
        },
        raw.status,
        codes(&raw)
    );

    let raw = post_soft(
        linear,
        &ProbeSearch::build_query(probe_search::Variables {
            term: "Dark theme".into(),
        }),
    )
    .await;
    println!(
        "- ProbeSearch(<seeded title>): HTTP {}, {} results, codes: {}",
        raw.status,
        nodes(&raw.data()["searchIssues"]).len(),
        codes(&raw)
    );
    let endpoint: Vec<_> = raw
        .headers
        .iter()
        .filter(|(n, _)| n.contains("endpoint"))
        .collect();
    if endpoint.is_empty() {
        println!("- no header with `endpoint` in its name");
    }
    for (n, v) in endpoint {
        println!("- {n}: {v}");
    }
    println!();
}
