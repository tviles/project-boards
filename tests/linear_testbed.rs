//! Offline checks of the Linear testbed tooling in tests/common/linear_testbed.rs, which the
//! seed program and the probes share.

#[path = "common/linear_testbed.rs"]
mod linear_testbed;

use linear_testbed::*;

fn bed() -> Testbed {
    testbed_from(
        r#"
        url_key = "example-testbed"
        team_a = "TBA"
        team_b = "TBB"
        project = "Testbed project"
        "#,
    )
    .unwrap()
}

#[test]
fn the_testbed_key_never_comes_from_linear_api_key() {
    let env = |k: &str| (k == "LINEAR_API_KEY").then(|| "lin_api_real".to_string());
    assert!(matches!(key_source(env), KeySource::Keychain(_)));
}

#[test]
fn the_testbed_key_comes_from_its_own_variable_then_its_own_keychain_item() {
    let env = |k: &str| (k == "PB_LINEAR_TESTBED_KEY").then(|| " lin_api_test \n".to_string());
    assert_eq!(key_source(env), KeySource::Env("lin_api_test".into()));
    let blank = |k: &str| (k == "PB_LINEAR_TESTBED_KEY").then(|| "  ".to_string());
    match key_source(blank) {
        KeySource::Keychain(argv) => {
            assert_eq!(argv[0], "security");
            assert!(argv.contains(&"project-boards-linear-testbed".to_string()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn another_workspace_is_refused_without_naming_it() {
    let tb = bed();
    assert_eq!(check_workspace("example-testbed", &tb), Ok(()));
    let err = check_workspace("my-real-company", &tb).unwrap_err();
    assert!(!err.contains("my-real-company"), "{err}");
    assert!(err.contains("stopping"), "{err}");
}

#[test]
fn the_testbed_file_needs_every_field() {
    assert_eq!(bed().team_b, "TBB");
    assert!(testbed_from("url_key = \"x\"").is_err());
    assert!(
        testbed_from("url_key = \"\"\nteam_a = \"A\"\nteam_b = \"B\"\nproject = \"P\"").is_err()
    );
}

#[test]
fn raw_responses_expose_error_codes_and_headers() {
    let raw = Raw {
        status: 400,
        headers: vec![("x-complexity".into(), "2301".into())],
        body: serde_json::json!({"errors": [{"message": "m", "extensions": {"code": "RATELIMITED"}}]}),
        text: None,
    };
    assert_eq!(raw.error_codes(), ["RATELIMITED"]);
    assert_eq!(raw.header("X-Complexity"), Some("2301"));
    assert!(raw.data().is_null());
}

#[test]
fn a_non_json_body_is_kept_as_text() {
    let raw = Raw::from_body(502, vec![], b"<html>Bad gateway</html>");
    assert_eq!(raw.status, 502);
    assert!(raw.body.is_null());
    assert_eq!(raw.text.as_deref(), Some("<html>Bad gateway</html>"));
    assert!(raw.error_codes().is_empty());
    let json = Raw::from_body(200, vec![], br#"{"data":{"a":1}}"#);
    assert_eq!(json.text, None);
    assert_eq!(json.data()["a"], 1);
}

#[test]
fn the_checked_in_testbed_file_parses_and_has_invented_team_keys() {
    let tb = testbed();
    assert_eq!((tb.team_a.as_str(), tb.team_b.as_str()), ("TBA", "TBB"));
    assert_eq!(tb.url_key, "project-boards-linear-testbed");
}

#[test]
fn a_key_source_never_prints_the_key() {
    let source = key_source(|k| (k == "PB_LINEAR_TESTBED_KEY").then(|| "lin_api_invented".into()));
    let shown = format!("{source:?}");
    assert!(!shown.contains("lin_api_invented"), "{shown}");
    assert_eq!(shown, "Env(<redacted>)");
}

#[tokio::test]
async fn an_unguarded_client_sends_only_the_viewer_check() {
    use graphql_client::GraphQLQuery;
    use project_boards_queries::linear::{SeedTeams, seed_teams};
    let client = Linear::for_error_probe("lin_api_invented".into());
    let body = SeedTeams::build_query(seed_teams::Variables {});
    let err = client.post(&body).await.unwrap_err();
    assert_eq!(
        err,
        "only TestbedViewer may be sent without the workspace guard"
    );
}

#[test]
fn the_viewer_check_passes_the_viewer_operation() {
    use graphql_client::GraphQLQuery;
    use project_boards_queries::linear::{TestbedViewer, testbed_viewer};
    let body = TestbedViewer::build_query(testbed_viewer::Variables {});
    assert_eq!(viewer_only_check(&body), Ok(()));
}
