//! Live writes against the testbed in tests/testbed.toml, and nothing else. Each test makes
//! its own throwaway draft or issue (or, for one test, borrows the seeded "Sample pull
//! request" and restores it), edits it, checks the result with a direct node lookup (which
//! the item index lag does not affect), and cleans up even when an assertion fails; a failed
//! cleanup fails the test and names what is left. Only the testbed's own data goes through the
//! recording client: anything wider (the user's repositories) uses an unrecorded one.
//!   cargo test --features live --test live_edits -- --test-threads=1 --nocapture
//!   GH_TOKEN=$(gh auth token --user tviles) PB_RECORD=1 cargo test --features live --test live_edits -- --test-threads=1 --nocapture
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

/// One writer at a time, whatever `--test-threads` says.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Set by a test that already deleted its draft's project item, so `with_draft` does not try
/// again and report a false cleanup failure. Writers are serialised, so one flag is enough.
static ITEM_DELETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Deletes issues in the testbed repository whose title is exactly `title` (search, so it can
/// lag a fresh issue); best effort for an issue whose id was never seen. Returns what could
/// not be cleaned up, naming it, or an empty list when it found and deleted them or found none.
async fn delete_issues_titled(title: &str) -> Vec<String> {
    let gh = Github::new(http());
    let query = format!("repo:{} is:issue in:title \"{title}\"", testbed_repo());
    let hits = match gh.search_content(&query).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("CLEANUP FAILED: could not search the testbed for the issue '{title}': {e}");
            return vec![format!("issue '{title}' (search failed)")];
        }
    };
    let mut left = Vec::new();
    for hit in hits.iter().filter(|h| h.title == title) {
        match gh.delete_issue(&hit.id).await {
            Ok(()) => eprintln!("deleted the stray issue '{title}' (#{})", hit.number),
            Err(e) => {
                eprintln!(
                    "CLEANUP FAILED: delete issue #{} '{title}' by hand: {e}",
                    hit.number
                );
                left.push(format!("issue #{} '{title}'", hit.number));
            }
        }
    }
    left
}

/// Refuses to go on unless the token is the testbed owner's and the testbed is the project
/// the tests were written for. Runs before anything is written or any recording is cleared.
async fn preflight() {
    let (board, id) = testbed();
    let plain = Github::new(http());
    let who = plain.viewer().await.expect("the token works").login;
    assert_eq!(
        who, board.owner,
        "live edits must run as {0}: GH_TOKEN=$(gh auth token --user {0}) cargo test --features live --test live_edits -- --test-threads=1",
        board.owner
    );
    let project = plain
        .resolve_project(&board)
        .await
        .expect("the testbed resolves");
    assert_eq!(
        project.id, id,
        "tests/testbed.toml names a different project than {}/{}",
        board.owner, board.number
    );
}

/// Deletes drafts titled `title` from the project; best effort, for a draft whose creation
/// reply could not be decoded. The item index can lag, so a fresh draft may not show yet.
async fn delete_drafts_titled(project: &ProjectId, title: &str) {
    let gh = Github::new(http());
    match gh
        .fetch_items_page(project, "", project_boards::github::client::FULL_PAGE, None)
        .await
    {
        Ok(page) => {
            for item in page.nodes.iter().filter(|i| i.title() == title) {
                match gh.delete_item(project, &item.id).await {
                    Ok(()) => eprintln!("deleted the stray draft '{title}' ({})", item.id),
                    Err(e) => eprintln!(
                        "CLEANUP FAILED: delete the stray draft '{title}' ({}) by hand: {e}",
                        item.id
                    ),
                }
            }
        }
        Err(e) => {
            eprintln!("CLEANUP FAILED: could not list the testbed to find the draft '{title}': {e}")
        }
    }
}

/// Runs `body` on a fresh throwaway draft, then deletes the draft whatever happened.
async fn with_draft<F, Fut>(name: &str, body: F)
where
    F: FnOnce(Arc<Github>, Project, Item) -> Fut,
    Fut: Future<Output = ()>,
{
    let _serial = SERIAL.lock().await;
    preflight().await;
    let gh = Arc::new(github(name));
    let project = gh
        .resolve_project(&testbed().0)
        .await
        .expect("the testbed resolves");
    assert_eq!(project.id, testbed().1);
    let before = gh.rate().remaining;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let title = format!("pb live: {name} {nanos}");
    let draft = match gh
        .add_draft_issue(
            &project.id,
            &title,
            "Throwaway item made by the live suite, which deletes it.",
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            delete_drafts_titled(&project.id, &title).await;
            panic!("the throwaway draft '{title}' was not created cleanly: {e}");
        }
    };
    ITEM_DELETED.store(false, std::sync::atomic::Ordering::SeqCst);
    let outcome = AssertUnwindSafe(body(gh.clone(), project.clone(), draft.clone()))
        .catch_unwind()
        .await;
    // A separate client, so cleanup is never recorded and runs even if `gh` is in a bad state.
    let cleanup = if ITEM_DELETED.load(std::sync::atomic::Ordering::SeqCst) {
        Ok(())
    } else {
        Github::new(http())
            .delete_item(&project.id, &draft.id)
            .await
    };
    report_points(name, before, gh.rate().remaining);
    if let Err(e) = &cleanup {
        eprintln!(
            "CLEANUP FAILED: delete the draft '{}' ({}) from the testbed by hand: {e}",
            draft.title(),
            draft.id
        );
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    if let Err(e) = cleanup {
        panic!(
            "cleanup failed: the draft '{}' ({}) is still on the testbed: {e}",
            draft.title(),
            draft.id
        );
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

/// Runs `body` on a fresh throwaway issue in the testbed repository, added to the testbed
/// project; afterwards removes the item and deletes the issue, whatever happened. Once the
/// issue exists everything, adding it to the project included, runs inside the guard, so a
/// failed add still deletes the issue (pre-flight ruling 22).
async fn with_issue<F, Fut>(name: &str, body: F)
where
    F: FnOnce(Arc<Github>, Project, Item) -> Fut,
    Fut: Future<Output = ()>,
{
    let _serial = SERIAL.lock().await;
    preflight().await;
    let gh = Arc::new(github(name));
    let project = gh
        .resolve_project(&testbed().0)
        .await
        .expect("the testbed resolves");
    assert_eq!(project.id, testbed().1);
    let before = gh.rate().remaining;
    let repo = gh
        .lookup_repo(&testbed_repo())
        .await
        .unwrap()
        .expect("the testbed repository is writable");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let title = format!("pb live: {name} {nanos}");
    let issue = match gh
        .create_issue(
            &repo.id,
            &title,
            "Throwaway issue made by the live suite, which deletes it.",
        )
        .await
    {
        Ok(i) => i,
        Err(e) => {
            // The reply may have been lost after GitHub created the issue.
            eprintln!("CLEANUP: creating '{title}' failed ({e}); looking for it to delete");
            let left = delete_issues_titled(&title).await;
            panic!(
                "the throwaway issue '{title}' was not created cleanly: {e}; left over: {left:?}"
            );
        }
    };
    let added = gh.add_item_by_id(&project.id, &issue).await;
    let outcome = match &added {
        Ok(item) => {
            AssertUnwindSafe(body(gh.clone(), project.clone(), item.clone()))
                .catch_unwind()
                .await
        }
        Err(e) => Err(Box::new(format!(
            "adding the throwaway issue to the project failed: {e}"
        )) as Box<dyn std::any::Any + Send>),
    };
    // A separate client, so cleanup is never recorded and runs even if `gh` is in a bad state.
    let cleanup = Github::new(http());
    let mut leftovers = Vec::new();
    if let Ok(item) = &added
        && let Err(e) = cleanup.delete_item(&project.id, &item.id).await
    {
        eprintln!(
            "CLEANUP FAILED: remove item {} from the testbed project by hand: {e}",
            item.id
        );
        leftovers.push(format!("project item {}", item.id));
    }
    if let Err(e) = cleanup.delete_issue(&issue).await {
        eprintln!(
            "CLEANUP FAILED: delete the issue '{title}' from the testbed repository by hand: {e}"
        );
        leftovers.push(format!("issue '{title}'"));
    }
    report_points(name, before, gh.rate().remaining);
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(
        leftovers.is_empty(),
        "cleanup failed, still on the testbed: {}",
        leftovers.join(", ")
    );
}

fn content(item: &Item) -> ContentId {
    item.content_id().expect("content id")
}

#[tokio::test]
async fn repository_lookups() {
    let _serial = SERIAL.lock().await;
    preflight().await;
    let gh = github("edits_lookups");
    let c = gh.repo_candidates(&testbed_repo()).await.unwrap();
    assert!(c.users.iter().any(|u| u.login == "tviles"));
    for name in ["bug", "docs", "good first issue"] {
        assert!(c.labels.iter().any(|l| l.name == name), "label {name}");
    }
    for title in ["v1", "v2"] {
        assert!(
            c.milestones.iter().any(|m| m.title == title),
            "milestone {title}"
        );
    }
    assert!(
        // Unrecorded: the user's repository list must never reach the fixtures.
        Github::new(http())
            .pushable_repos()
            .await
            .unwrap()
            .repos
            .iter()
            .any(|r| r.slug == testbed_repo())
    );
    assert!(gh.lookup_repo(&testbed_repo()).await.unwrap().is_some());
    assert!(
        gh.lookup_repo(&"tviles/no-such-repo-pb".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn assignees_labels_and_milestone_on_an_issue() {
    with_issue("edits_people", |gh, _p, item| async move {
        let c = gh.repo_candidates(&testbed_repo()).await.unwrap();
        let id = content(&item);
        let me = c.users.iter().find(|u| u.login == "tviles").unwrap();
        let label = |n: &str| c.labels.iter().find(|l| l.name == n).unwrap().id.clone();
        gh.add_assignees(id.node_id(), std::slice::from_ref(&me.id))
            .await
            .unwrap();
        gh.add_labels(id.node_id(), &[label("bug"), label("good first issue")])
            .await
            .unwrap();
        gh.remove_labels(id.node_id(), &[label("bug")])
            .await
            .unwrap();
        let v1 = c.milestones.iter().find(|m| m.title == "v1").unwrap();
        gh.set_milestone(&id, Some(&v1.id)).await.unwrap();
        let now = reload(&gh, &item.id).await;
        assert_eq!(now.assignees(), ["tviles"]);
        assert_eq!(now.label_names(), ["good first issue"]);
        assert_eq!(
            now.content_fields
                .milestone
                .as_ref()
                .map(|m| m.title.as_str()),
            Some("v1")
        );
        gh.remove_assignees(id.node_id(), std::slice::from_ref(&me.id))
            .await
            .unwrap();
        gh.set_milestone(&id, None).await.unwrap();
        let now = reload(&gh, &item.id).await;
        assert!(now.assignees().is_empty());
        assert_eq!(now.content_fields.milestone, None);
    })
    .await;
}

#[tokio::test]
async fn title_body_comment_close_and_reopen_an_issue() {
    with_issue("edits_issue", |gh, _p, item| async move {
        let id = content(&item);
        gh.set_title(&id, "pb live: retitled").await.unwrap();
        gh.set_body(&id, "New body").await.unwrap();
        gh.add_comment(id.node_id(), "A live comment")
            .await
            .unwrap();
        let detail = gh.fetch_item_detail(&item.id, None).await.unwrap();
        assert_eq!(detail.body, "New body");
        assert!(detail.comments.iter().any(|c| c.body == "A live comment"));
        assert_eq!(reload(&gh, &item.id).await.title(), "pb live: retitled");
        gh.close(&id, Some(CloseReason::NotPlanned)).await.unwrap();
        let closed = reload(&gh, &item.id).await;
        assert!(matches!(
            closed.content,
            ItemContent::Issue {
                state: ContentState::Closed,
                ..
            }
        ));
        assert_eq!(
            closed.content_fields.state_reason,
            Some(StateReason::NotPlanned)
        );
        gh.reopen(&id).await.unwrap();
        assert!(matches!(
            reload(&gh, &item.id).await.content,
            ItemContent::Issue {
                state: ContentState::Open,
                ..
            }
        ));
    })
    .await;
}

#[tokio::test]
async fn archive_and_unarchive() {
    with_issue("edits_archive", |gh, p, item| async move {
        gh.archive_item(&p.id, &item.id).await.unwrap();
        assert!(reload(&gh, &item.id).await.archived);
        gh.unarchive_item(&p.id, &item.id).await.unwrap();
        assert!(!reload(&gh, &item.id).await.archived);
    })
    .await;
}

#[tokio::test]
async fn adding_existing_content_finds_it_by_number_and_returns_the_same_item() {
    with_issue("edits_existing", |gh, p, item| async move {
        let number = item.number().expect("an issue number");
        let hit = gh
            .content_by_number(&testbed_repo(), number)
            .await
            .unwrap()
            .expect("found by number");
        assert_eq!(Some(hit.id.clone()), item.content_fields.content_id);
        let again = gh.add_item_by_id(&p.id, &hit.id).await.unwrap();
        assert_eq!(
            again.id, item.id,
            "adding content already on the board returns its item"
        );
        // The search index lags writes; only check that searching works.
        gh.search_content(&format!("repo:{} is:open", testbed_repo()))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn draft_title_body_assignees_and_conversion() {
    with_draft("edits_draft", |gh, p, draft| async move {
        let id = content(&draft);
        let c = gh.repo_candidates(&testbed_repo()).await.unwrap();
        let me = c.users.iter().find(|u| u.login == "tviles").unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let title = format!("pb live: draft retitled {nanos}");
        gh.set_title(&id, &title).await.unwrap();
        gh.set_body(&id, "Draft body").await.unwrap();
        gh.set_draft_assignees(id.node_id(), std::slice::from_ref(&me.id))
            .await
            .unwrap();
        let now = reload(&gh, &draft.id).await;
        assert_eq!((now.title(), now.assignees()), (title.as_str(), vec!["tviles"]));
        // From here the draft may become an issue: clean up in order (item, then issue).
        let converted = gh.convert_draft(&draft.id, &c.repo.id).await;
        let (checks, issue) = match &converted {
            Ok(item) => (
                AssertUnwindSafe(async {
                    assert!(matches!(item.content, ItemContent::Issue { .. }));
                    assert_eq!(item.id, draft.id, "the same project item");
                    assert!(item.content_fields.content_id.is_some(), "the new issue's id");
                })
                .catch_unwind()
                .await,
                item.content_fields.content_id.clone(),
            ),
            Err(e) => (
                Err(Box::new(format!("convert_draft failed: {e}")) as Box<dyn std::any::Any + Send>),
                None,
            ),
        };
        let cleanup = Github::new(http());
        let mut left = Vec::new();
        match cleanup.delete_item(&p.id, &draft.id).await {
            Ok(()) => ITEM_DELETED.store(true, std::sync::atomic::Ordering::SeqCst),
            Err(e) => eprintln!("CLEANUP FAILED: remove item {} by hand: {e}", draft.id),
        }
        match issue {
            Some(issue) => {
                if let Err(e) = cleanup.delete_issue(&issue).await {
                    eprintln!("CLEANUP FAILED: delete the converted issue '{title}' by hand: {e}");
                    left.push(format!("issue '{title}'"));
                }
            }
            // Converted on the server, or maybe not: find it by its unique title.
            None => left.extend(delete_issues_titled(&title).await),
        }
        if let Err(panic) = checks {
            std::panic::resume_unwind(panic);
        }
        assert!(left.is_empty(), "cleanup failed, still on the testbed: {}", left.join(", "));
    })
    .await;
}

/// Uses the seeded "Sample pull request", and puts it back as it was even on failure.
#[tokio::test]
async fn pull_request_title_close_and_reopen() {
    let _serial = SERIAL.lock().await;
    preflight().await;
    let gh = github("edits_pull");
    let p = gh.resolve_project(&testbed().0).await.unwrap();
    let page = gh
        .fetch_items_page(&p.id, "", project_boards::github::client::FULL_PAGE, None)
        .await
        .unwrap();
    let pr = page
        .nodes
        .iter()
        .find(|i| i.title() == "Sample pull request")
        .expect(
            "no 'Sample pull request' on the testbed: an earlier run may have left it retitled; \
             restore its title by hand (and reopen it)",
        )
        .clone();
    assert!(
        matches!(
            pr.content,
            ItemContent::PullRequest {
                state: ContentState::Open,
                ..
            }
        ),
        "the 'Sample pull request' is not an open pull request: reopen it by hand before running this test"
    );
    let id = content(&pr);
    let original = pr.title().to_string();
    let closed_by_us = std::sync::atomic::AtomicBool::new(false);
    let outcome = AssertUnwindSafe(async {
        gh.set_title(&id, &format!("{original} (live)"))
            .await
            .unwrap();
        // Set before the call: a close whose reply is lost may still have happened.
        closed_by_us.store(true, std::sync::atomic::Ordering::SeqCst);
        gh.close(&id, None).await.unwrap();
        let closed = reload(&gh, &pr.id).await;
        assert!(matches!(
            closed.content,
            ItemContent::PullRequest {
                state: ContentState::Closed,
                ..
            }
        ));
        assert_eq!(closed.title(), format!("{original} (live)"));
    })
    .catch_unwind()
    .await;
    let restore = Github::new(http());
    let mut failed = Vec::new();
    if closed_by_us.load(std::sync::atomic::Ordering::SeqCst)
        && let Err(e) = restore.reopen(&id).await
    {
        eprintln!("RESTORE FAILED: reopen the sample pull request by hand: {e}");
        failed.push("reopen");
    }
    if let Err(e) = restore.set_title(&id, &original).await {
        eprintln!("RESTORE FAILED: retitle the sample pull request to '{original}' by hand: {e}");
        failed.push("retitle");
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(
        failed.is_empty(),
        "the sample pull request was not restored: {}",
        failed.join(", ")
    );
}
