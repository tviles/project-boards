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
    assert!(issue(2).values.values().any(|v| matches!(
        v,
        FieldValue::PullRequests(prs) if prs.iter().map(|p| p.number).eq([12])
    )));
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

#[tokio::test]
async fn recorded_detail_decodes() {
    let t = Arc::new(FixtureTransport::new());
    t.push_file("ItemDetail", format!("{REC}/detail/ItemDetail__1.json"));
    let d = Github::new(t)
        .fetch_item_detail(&ItemId::new("any"), None)
        .await
        .unwrap();
    assert!(d.body.contains("## Steps") && d.body.contains("<details>"));
    assert_eq!(d.comments.len(), 2);
    assert_eq!(d.comments[0].body, "First comment with `code`.");
    assert_eq!(d.comments[0].author, "tviles");
    assert_eq!(d.comments_total, 2);
    assert_eq!(d.older_cursor, None);
}

#[tokio::test]
async fn recorded_project_lists_decode() {
    let (board, _) = testbed();
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "RepoProjects",
        format!("{REC}/projects/RepoProjects__1.json"),
    );
    t.push_file(
        "ViewerProjects",
        format!("{REC}/projects/ViewerProjects__1.json"),
    );
    let gh = Github::new(t);
    let linked = gh
        .list_repo_projects(&"tviles/project-boards-testbed".parse().unwrap())
        .await
        .unwrap();
    assert!(linked.iter().any(|p| p.board == board));
    let all = gh.list_viewer_projects().await.unwrap();
    assert!(all.iter().any(|p| p.board == board));
}
