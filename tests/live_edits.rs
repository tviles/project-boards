//! Live writes against the testbed in tests/testbed.toml, and nothing else. Each test makes
//! its own throwaway draft, edits it, checks the result with a direct node lookup (which the
//! item index lag does not affect), and deletes the draft even when an assertion fails.
//!   cargo test --features live --test live_edits -- --test-threads=1 --nocapture
//!   PB_RECORD=1 cargo test --features live --test live_edits -- --test-threads=1
//! Each test prints the rate-limit points it used.
#![cfg(feature = "live")]

mod common;

use common::*;
use futures::FutureExt;
use project_boards::github::{Github, GithubError};
use project_boards::model::*;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

/// Runs `body` on a fresh throwaway draft, then deletes the draft whatever happened.
async fn with_draft<F, Fut>(name: &str, body: F)
where
    F: FnOnce(Arc<Github>, Project, Item) -> Fut,
    Fut: Future<Output = ()>,
{
    let gh = Arc::new(github(name));
    let project = gh
        .resolve_project(&testbed().0)
        .await
        .expect("the testbed resolves");
    let before = gh.rate().remaining;
    let draft = gh
        .add_draft_issue(
            &project.id,
            &format!("pb live: {name}"),
            "Throwaway item made by the live suite, which deletes it.",
        )
        .await
        .expect("the throwaway draft is created");
    let outcome = AssertUnwindSafe(body(gh.clone(), project.clone(), draft.clone()))
        .catch_unwind()
        .await;
    // A separate client, so cleanup is never recorded and runs even if `gh` is in a bad state.
    if let Err(e) = Github::new(http())
        .delete_item(&project.id, &draft.id)
        .await
    {
        eprintln!(
            "CLEANUP FAILED: delete the draft '{}' ({}) from the testbed by hand: {e}",
            draft.title(),
            draft.id
        );
    }
    report_points(name, before, gh.rate().remaining);
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn field<'a>(p: &'a Project, name: &str) -> &'a Field {
    p.fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("testbed field {name}"))
}

async fn reload(gh: &Github, item: &ItemId) -> Item {
    gh.hydrate_items(std::slice::from_ref(item))
        .await
        .unwrap()
        .remove(0)
}

/// Values compare by what they show; multi-selects by their option ids, in any order.
fn same(a: Option<&FieldValue>, b: &FieldValue) -> bool {
    match (a, b) {
        (
            Some(FieldValue::MultiSelect { option_ids: x, .. }),
            FieldValue::MultiSelect { option_ids: y, .. },
        ) => {
            let (mut x, mut y) = (x.clone(), y.clone());
            x.sort();
            y.sort();
            x == y
        }
        (Some(a), b) => a.display() == b.display(),
        (None, _) => false,
    }
}

#[tokio::test]
async fn set_and_clear_every_field_type() {
    with_draft("edits_fields", |gh, p, draft| async move {
        let options = |name: &str| match &field(&p, name).kind {
            FieldKind::SingleSelect { options } | FieldKind::MultiSelect { options } => {
                options.clone()
            }
            other => panic!("{name} is {other:?}"),
        };
        let sprint = match &field(&p, "Sprint").kind {
            FieldKind::Iteration { iterations, .. } => iterations[0].clone(),
            other => panic!("Sprint is {other:?}"),
        };
        let p0 = options("Priority")[0].clone();
        let areas = options("Areas");
        let cases = vec![
            ("Notes", FieldValue::Text("live note".into())),
            ("Size", FieldValue::Number(5.0)),
            ("Due", FieldValue::Date("2026-12-24".into())),
            (
                "Priority",
                FieldValue::SingleSelect {
                    option_id: p0.id.clone(),
                    name: p0.name.clone(),
                },
            ),
            (
                "Areas",
                FieldValue::MultiSelect {
                    option_ids: vec![areas[0].id.clone(), areas[1].id.clone()],
                    names: vec![areas[0].name.clone(), areas[1].name.clone()],
                },
            ),
            (
                "Sprint",
                FieldValue::Iteration {
                    iteration_id: sprint.id.clone(),
                    title: sprint.title.clone(),
                    start_date: sprint.start_date.clone(),
                },
            ),
        ];
        for (name, value) in &cases {
            let f = field(&p, name);
            let returned = gh
                .set_field_value(&p.id, &draft.id, &f.id, value)
                .await
                .unwrap();
            assert!(
                same(returned.value(&f.id), value),
                "{name}: returned {:?}",
                returned.value(&f.id)
            );
            let reloaded = reload(&gh, &draft.id).await;
            assert!(
                same(reloaded.value(&f.id), value),
                "{name}: reloaded {:?}",
                reloaded.value(&f.id)
            );
        }
        for (name, _) in &cases {
            let f = field(&p, name);
            let returned = gh.clear_field_value(&p.id, &draft.id, &f.id).await.unwrap();
            assert!(returned.value(&f.id).is_none(), "{name} cleared");
        }
        let reloaded = reload(&gh, &draft.id).await;
        for (name, _) in &cases {
            assert!(
                reloaded.value(&field(&p, name).id).is_none(),
                "{name} stays cleared"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn reorder_to_the_top_and_after_another_item() {
    with_draft("edits_position", |gh, p, draft| async move {
        // The payload lists the project's order after the move (pre-flight ruling 14). If
        // GitHub ever lists something else there, report it rather than weaken the checks.
        let order = gh
            .update_item_position(&p.id, &draft.id, None)
            .await
            .unwrap();
        assert_eq!(
            order.first(),
            Some(&draft.id),
            "first after moving to the top: {order:?}"
        );
        let other = order
            .iter()
            .find(|i| **i != draft.id)
            .expect("a seeded item")
            .clone();
        let order = gh
            .update_item_position(&p.id, &draft.id, Some(&other))
            .await
            .unwrap();
        let at = |id: &ItemId| order.iter().position(|i| i == id).expect("listed");
        assert_eq!(
            at(&draft.id),
            at(&other) + 1,
            "right after {other}: {order:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn a_refused_edit_carries_githubs_message() {
    with_draft("edits_refused", |gh, p, draft| async move {
        let f = field(&p, "Priority");
        let bogus = FieldValue::SingleSelect {
            option_id: OptionId::new("not-an-option"),
            name: "x".into(),
        };
        let err = gh
            .set_field_value(&p.id, &draft.id, &f.id, &bogus)
            .await
            .unwrap_err();
        println!("GitHub said: {err}");
        assert!(matches!(err, GithubError::Graphql(_)), "{err:?}");
    })
    .await;
}
