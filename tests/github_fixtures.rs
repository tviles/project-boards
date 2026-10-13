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

fn values(item: &Item) -> Vec<&FieldValue> {
    item.values.values().collect()
}

#[tokio::test]
async fn recorded_field_edits_decode() {
    let (_, id) = testbed();
    let dir = format!("{REC}/edits_fields");
    let t = Arc::new(FixtureTransport::new());
    t.push_file("AddDraftIssue", format!("{dir}/AddDraftIssue__1.json"));
    for op in [
        "SetTextValue",
        "SetNumberValue",
        "SetDateValue",
        "SetSingleSelectValue",
        "SetMultiSelectValue",
        "SetIterationValue",
    ] {
        t.push_file(op, format!("{dir}/{op}__1.json"));
    }
    for n in 1..=6 {
        t.push_file(
            "ClearFieldValue",
            format!("{dir}/ClearFieldValue__{n}.json"),
        );
    }
    let gh = Github::new(t);
    let f = FieldId::new("x");
    let draft = gh.add_draft_issue(&id, "x", "y").await.unwrap();
    assert!(matches!(draft.content, ItemContent::Draft { .. }));
    assert!(draft.title().starts_with("pb live: edits_fields"));

    // Each set returns the item with the value just written (and the earlier ones).
    let set = |v: FieldValue| {
        let (gh, id, item, f) = (&gh, &id, &draft.id, &f);
        async move { gh.set_field_value(id, item, f, &v).await }
    };
    let text = set(FieldValue::Text("x".into())).await.unwrap();
    assert!(values(&text).contains(&&FieldValue::Text("live note".into())));
    let number = set(FieldValue::Number(0.0)).await.unwrap();
    assert!(values(&number).contains(&&FieldValue::Number(5.0)));
    let date = set(FieldValue::Date("x".into())).await.unwrap();
    assert!(
        values(&date)
            .iter()
            .any(|v| matches!(v, FieldValue::Date(d) if d == "2026-12-24"))
    );
    let single = set(FieldValue::SingleSelect {
        option_id: OptionId::new("o"),
        name: "o".into(),
    })
    .await
    .unwrap();
    assert!(
        values(&single)
            .iter()
            .any(|v| matches!(v, FieldValue::SingleSelect { .. }))
    );
    let multi = set(FieldValue::MultiSelect {
        option_ids: vec![OptionId::new("o")],
        names: vec![],
    })
    .await
    .unwrap();
    assert!(
        values(&multi)
            .iter()
            .any(|v| matches!(v, FieldValue::MultiSelect { names, .. } if names.len() == 2))
    );
    let iteration = set(FieldValue::Iteration {
        iteration_id: IterationId::new("i"),
        title: "i".into(),
        start_date: "d".into(),
    })
    .await
    .unwrap();
    assert!(
        values(&iteration)
            .iter()
            .any(|v| matches!(v, FieldValue::Iteration { title, .. } if !title.is_empty()))
    );

    // Clearing six fields in turn leaves the title and whatever GitHub defaults (Status).
    let mut last = None;
    let mut counts = vec![];
    for _ in 0..6 {
        let cleared = gh.clear_field_value(&id, &draft.id, &f).await.unwrap();
        counts.push(cleared.values.len());
        last = Some(cleared);
    }
    assert!(
        counts.windows(2).all(|w| w[1] < w[0]),
        "each clear removes one value: {counts:?}"
    );
    let last = last.unwrap();
    assert_eq!(
        last.values.len(),
        2,
        "only the title and the default Status remain"
    );
    assert!(!values(&last).contains(&&FieldValue::Text("live note".into())));
}

#[tokio::test]
async fn recorded_position_edits_decode() {
    let (_, id) = testbed();
    let dir = format!("{REC}/edits_position");
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "UpdateItemPosition",
        format!("{dir}/UpdateItemPosition__1.json"),
    );
    t.push_file(
        "UpdateItemPosition",
        format!("{dir}/UpdateItemPosition__2.json"),
    );
    let gh = Github::new(t);
    let (draft, other) = (ItemId::new("d"), ItemId::new("o"));
    let top = gh.update_item_position(&id, &draft, None).await.unwrap();
    let after = gh
        .update_item_position(&id, &draft, Some(&other))
        .await
        .unwrap();
    assert!(top.len() > 2 && top.len() == after.len());
    // The draft went to the top, then right after the item it had displaced.
    assert_eq!((&after[0], &after[1]), (&top[1], &top[0]));
    assert_eq!(&after[2..], &top[2..]);
}

#[tokio::test]
async fn recorded_refused_edit_run_added_a_titled_draft() {
    let (_, id) = testbed();
    let t = Arc::new(FixtureTransport::new());
    t.push_file(
        "AddDraftIssue",
        format!("{REC}/edits_refused/AddDraftIssue__1.json"),
    );
    let draft = Github::new(t).add_draft_issue(&id, "x", "y").await.unwrap();
    assert!(draft.title().starts_with("pb live: edits_refused"));
}
