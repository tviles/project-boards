//! Helpers shared by the live suites (tests/live.rs, tests/live_edits.rs): where the testbed
//! is, and a GitHub client that records with PB_RECORD=1.
#![allow(dead_code)] // each test crate uses a different part

use project_boards::github::Github;
use project_boards::github::record::RecordingTransport;
use project_boards::github::token::resolve_token_from_system;
use project_boards::github::transport::{HttpTransport, Transport};
use project_boards::model::*;
use std::sync::Arc;

fn testbed_toml() -> toml::Table {
    std::fs::read_to_string("tests/testbed.toml")
        .unwrap()
        .parse()
        .unwrap()
}

pub fn testbed() -> (BoardRef, ProjectId) {
    let t = testbed_toml();
    let board = BoardRef {
        owner: t["owner"].as_str().unwrap().into(),
        number: t["number"].as_integer().unwrap() as u32,
    };
    (board, ProjectId::new(t["project_id"].as_str().unwrap()))
}

pub fn testbed_repo() -> RepoSlug {
    testbed_toml()["repo"].as_str().unwrap().parse().unwrap()
}

pub fn http() -> Arc<dyn Transport> {
    let token = resolve_token_from_system(None).expect("a token for the testbed");
    Arc::new(HttpTransport::new(token.value))
}

pub fn recording() -> bool {
    std::env::var("PB_RECORD").as_deref() == Ok("1")
}

/// A client for one test; with PB_RECORD=1 it records into tests/fixtures/recorded/<name>/.
pub fn github(name: &str) -> Github {
    let http = http();
    if recording() {
        let dir = std::path::Path::new("tests/fixtures/recorded").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        Github::new(Arc::new(RecordingTransport::new(http, dir)))
    } else {
        Github::new(http)
    }
}

/// Prints the rate-limit points a test used, so the cost of writes stays visible.
pub fn report_points(name: &str, before: Option<u32>, after: Option<u32>) {
    match (before, after) {
        (Some(b), Some(a)) => println!("{name}: {} rate-limit points", b.saturating_sub(a)),
        _ => println!("{name}: rate-limit points unknown"),
    }
}
