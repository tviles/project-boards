//! Live suite against tviles/project-boards-testbed. Run:
//!   cargo test --features live --test live            (assert only)
//!   PB_RECORD=1 cargo test --features live --test live (also rewrite tests/fixtures/recorded)
//! `cost` and `updated_filter` never record; add `-- --nocapture` to see what they measured.
#![cfg(feature = "live")]

use project_boards::github::Github;
use project_boards::github::client::FULL_PAGE;
use project_boards::github::record::RecordingTransport;
use project_boards::github::token::resolve_token_from_system;
use project_boards::github::transport::{HttpTransport, Transport};
use project_boards::model::*;
use std::sync::Arc;

fn testbed() -> (BoardRef, ProjectId) {
    let t: toml::Table = std::fs::read_to_string("tests/testbed.toml")
        .unwrap()
        .parse()
        .unwrap();
    let board = BoardRef {
        owner: t["owner"].as_str().unwrap().into(),
        number: t["number"].as_integer().unwrap() as u32,
    };
    (board, ProjectId::new(t["project_id"].as_str().unwrap()))
}

fn http() -> Arc<dyn Transport> {
    let token = resolve_token_from_system().expect("a token for the testbed");
    Arc::new(HttpTransport::new(token.value))
}

/// A client for one test; with PB_RECORD=1 it records into tests/fixtures/recorded/<name>/.
fn github(name: &str) -> Github {
    let http = http();
    if std::env::var("PB_RECORD").as_deref() == Ok("1") {
        let dir = std::path::Path::new("tests/fixtures/recorded").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Github::new(Arc::new(RecordingTransport::new(http, dir)))
    } else {
        Github::new(http)
    }
}

#[tokio::test]
async fn resolve() {
    let (board, id) = testbed();
    let p = github("resolve").resolve_project(&board).await.unwrap();
    assert_eq!(p.id, id);
    let kind = |n: &str| {
        p.fields
            .iter()
            .find(|f| f.name == n)
            .unwrap_or_else(|| panic!("field {n}"))
            .kind
            .clone()
    };
    assert!(matches!(kind("Status"), FieldKind::SingleSelect { .. }));
    assert!(matches!(kind("Priority"), FieldKind::SingleSelect { options } if options.len() == 3));
    assert_eq!(kind("Size"), FieldKind::Number);
    assert_eq!(kind("Due"), FieldKind::Date);
    assert_eq!(kind("Notes"), FieldKind::Text);
    assert!(
        matches!(kind("Sprint"), FieldKind::Iteration { iterations, .. } if !iterations.is_empty())
    );
    assert!(matches!(kind("Areas"), FieldKind::MultiSelect { options } if options.len() == 3));
    let board_view = p.views.iter().find(|v| v.name == "Board").unwrap();
    assert_eq!(board_view.layout, Layout::Board);
    let view_1 = p.views.iter().find(|v| v.name == "View 1").unwrap();
    assert!(
        !view_1.group_by.is_empty(),
        "View 1 should report its group-by (Sprint); empty means the API does not expose groupByFields"
    );
    assert_eq!(
        p.views.iter().find(|v| v.name == "Bugs").unwrap().filter,
        "label:bug"
    );
}

#[tokio::test]
async fn items() {
    let (_, id) = testbed();
    let page = github("items")
        .fetch_items_page(&id, "", FULL_PAGE, None)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 13);
    assert!(
        page.nodes
            .iter()
            .any(|i| matches!(i.content, ItemContent::Draft { .. }))
    );
    assert!(
        page.nodes
            .iter()
            .any(|i| matches!(i.content, ItemContent::PullRequest { .. }))
    );
    assert!(page.nodes.iter().any(|i| {
        i.values
            .values()
            .any(|v| matches!(v, FieldValue::MultiSelect { names, .. } if names.len() == 2))
    }));
}

#[tokio::test]
async fn view_ids() {
    let (_, id) = testbed();
    let gh = github("view_ids");
    let ids = gh
        .fetch_view_ids_page(&id, "label:bug", None)
        .await
        .unwrap();
    assert!(!ids.nodes.is_empty());
    let items = gh.hydrate_items(&ids.nodes).await.unwrap();
    assert!(items.iter().all(|i| i.label_names().contains(&"bug")));
}

#[tokio::test]
async fn detail() {
    let (_, id) = testbed();
    let gh = github("detail");
    let page = gh.fetch_items_page(&id, "", FULL_PAGE, None).await.unwrap();
    let crash = page
        .nodes
        .iter()
        .find(|i| i.title().starts_with("Crash when board"))
        .expect("seeded issue");
    let d = gh.fetch_item_detail(&crash.id, None).await.unwrap();
    assert!(d.body.contains("## Steps"));
    assert_eq!(d.comments.len(), 2);
    assert_eq!(d.comments[0].body, "First comment with `code`.");
}

#[tokio::test]
async fn projects() {
    let (board, _) = testbed();
    let gh = github("projects");
    let linked = gh
        .list_repo_projects(&"tviles/project-boards-testbed".parse().unwrap())
        .await
        .unwrap();
    assert!(linked.iter().any(|p| p.board == board));
    let all = gh.list_viewer_projects().await.unwrap();
    assert!(all.iter().any(|p| p.board == board));
}

/// R37: GitHub scores a query by its `first:` arguments. A full page must stay cheap, since
/// polls share the user's 5,000-point hourly budget with `gh`.
#[tokio::test]
async fn cost() {
    let (_, id) = testbed();
    let cost = Github::new(http())
        .items_page_cost(&id, FULL_PAGE)
        .await
        .unwrap();
    println!("ItemsPage cost at first: {FULL_PAGE}: {cost} points");
    assert!(cost <= 10, "ItemsPage costs {cost} points per request");
}

/// R40 (re-checks phase 0 finding 1): `updated:>=<date>` must leave out items updated before
/// that date, or every incremental poll is a full load in disguise.
#[tokio::test]
async fn updated_filter() {
    use time::format_description::well_known::Rfc3339;
    let (_, id) = testbed();
    let gh = Github::new(http());
    let all = gh.fetch_items_page(&id, "", FULL_PAGE, None).await.unwrap();
    assert!(all.next.is_none(), "the testbed fits on one page");
    let oldest = all
        .nodes
        .iter()
        .min_by(|a, b| a.updated_at.cmp(&b.updated_at))
        .expect("testbed items");
    // The day after the oldest item's update: that item is older than the filter date.
    let cutoff = time::OffsetDateTime::parse(&oldest.updated_at, &Rfc3339)
        .expect("updatedAt is RFC 3339")
        .date()
        .next_day()
        .unwrap();
    let date = format!(
        "{:04}-{:02}-{:02}",
        cutoff.year(),
        u8::from(cutoff.month()),
        cutoff.day()
    );
    let floor = format!("{date}T00:00:00Z");
    let older = all.nodes.iter().filter(|i| i.updated_at < floor).count();
    let filtered = gh
        .fetch_items_page(&id, &format!("updated:>={date}"), FULL_PAGE, None)
        .await
        .unwrap();
    println!(
        "updated:>={date}: {} of {} items returned; {older} updated before it",
        filtered.nodes.len(),
        all.nodes.len()
    );
    for item in &filtered.nodes {
        assert!(
            item.updated_at >= floor,
            "{} ({}) was updated before {date}",
            item.title(),
            item.updated_at
        );
    }
    assert!(
        filtered.nodes.iter().all(|i| i.id != oldest.id),
        "{} (updated {}) should be filtered out",
        oldest.title(),
        oldest.updated_at
    );
}
