//! Offline decoding tests against recorded testbed responses.

use project_boards::github::Github;
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
        .fetch_items_page(&id, "", None)
        .await
        .unwrap();
    assert_eq!(page.nodes.len(), 13);
    assert!(page.nodes.iter().any(|i| i.title().contains('🚀')));
}

#[tokio::test]
async fn redacted_and_future_values_degrade() {
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "ItemsPage",
        "tests/fixtures/handwritten/redacted_and_future.json",
    );
    let page = Github::new(t)
        .fetch_items_page(&ProjectId::new("P"), "", None)
        .await
        .unwrap();
    assert_eq!(page.nodes[0].content, ItemContent::Redacted);
    assert!(page.nodes[1].values.is_empty());
}
