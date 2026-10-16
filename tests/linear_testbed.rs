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
    };
    assert_eq!(raw.error_codes(), ["RATELIMITED"]);
    assert_eq!(raw.header("X-Complexity"), Some("2301"));
    assert!(raw.data().is_null());
}
