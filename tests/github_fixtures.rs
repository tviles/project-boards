//! Offline decoding tests against recorded testbed responses.

use project_boards::github::Github;
use project_boards::github::client::FULL_PAGE;
use project_boards::github::fixture::FixtureTransport;
use project_boards::model::*;
use std::sync::Arc;

const REC: &str = "tests/fixtures/recorded";

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

#[tokio::test]
async fn recorded_project_decodes() {
    let (board, id) = testbed();
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "ResolveProject",
        format!("{REC}/resolve/ResolveProject__1.json"),
    );
    let p = Github::new(t).resolve_project(&board).await.unwrap();
    assert_eq!(p.id, id);
    assert!(
        p.fields
            .iter()
            .all(|f| !matches!(f.kind, FieldKind::Unsupported { .. })),
        "every testbed field type is known"
    );
    assert!(p.views.len() >= 4);
}

#[tokio::test]
async fn recorded_items_decode() {
    let (_, id) = testbed();
    let t = Arc::new(FixtureTransport::new());
    t.push_file("ItemsPage", format!("{REC}/items/ItemsPage__1.json"));
    let page = Github::new(t)
        .fetch_items_page(&id, "", FULL_PAGE, None)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 13);
    assert!(page.nodes.iter().any(|i| i.title().contains('🚀')));
    // Labels, assignees and linked PRs are read from content (R37).
    let issue = |n: u32| page.nodes.iter().find(|i| i.number() == Some(n)).unwrap();
    assert_eq!(issue(1).label_names(), ["bug"]);
    assert_eq!(issue(1).assignees(), ["tviles"]);
    assert!(
        issue(2)
            .values
            .values()
            .any(|v| *v == FieldValue::PullRequests(vec![12]))
    );
}

#[tokio::test]
async fn recorded_hydrate_decodes() {
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "HydrateItems",
        format!("{REC}/view_ids/HydrateItems__1.json"),
    );
    let items = Github::new(t)
        .hydrate_items(&[ItemId::new("x")])
        .await
        .unwrap();
    assert!(items.iter().all(|i| i.label_names().contains(&"bug")));
}

#[tokio::test]
async fn redacted_and_future_values_degrade() {
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "ItemsPage",
        "tests/fixtures/handwritten/redacted_and_future.json",
    );
    let page = Github::new(t)
        .fetch_items_page(&ProjectId::new("P"), "", FULL_PAGE, None)
        .await
        .unwrap();
    assert_eq!(page.nodes[0].content, ItemContent::Redacted);
    assert!(page.nodes[1].values.is_empty());
}
