//! Live suite against tviles/project-boards-testbed. Run:
//!   cargo test --features live --test live            (assert only)
//!   PB_RECORD=1 cargo test --features live --test live (also rewrite tests/fixtures/recorded)
#![cfg(feature = "live")]

use project_boards::github::Github;
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

/// A client for one test; with PB_RECORD=1 it records into tests/fixtures/recorded/<name>/.
fn github(name: &str) -> Github {
    let token = resolve_token_from_system().expect("a token for the testbed");
    let http: Arc<dyn Transport> = Arc::new(HttpTransport::new(token.value));
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
        .fetch_items_page(&id, "", None)
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
