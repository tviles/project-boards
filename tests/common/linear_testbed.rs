//! The Linear testbed for the seed program, the probes and (plan 2) the live suite: which
//! workspace it is, where its key comes from, and a guard that stops before any request
//! reaches another workspace. Included with `#[path]` (tests/linear_testbed.rs, examples).
#![allow(dead_code)] // each includer uses a different part

use serde_json::Value;

pub const ENDPOINT: &str = "https://api.linear.app/graphql";
const KEYCHAIN_SERVICE: &str = "project-boards-linear-testbed";

/// Where the testbed key comes from. Never `LINEAR_API_KEY`: that is the user's own workspace.
#[derive(Debug, PartialEq)]
pub enum KeySource {
    Env(String),
    Keychain(Vec<String>),
}

pub fn key_source(env: impl Fn(&str) -> Option<String>) -> KeySource {
    match env("PB_LINEAR_TESTBED_KEY")
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
    {
        Some(key) => KeySource::Env(key),
        None => KeySource::Keychain(
            [
                "security",
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
            ]
            .map(String::from)
            .to_vec(),
        ),
    }
}

pub fn resolve_key() -> Result<String, String> {
    let missing = || {
        format!(
            "no testbed key: set PB_LINEAR_TESTBED_KEY or add the Keychain item `{KEYCHAIN_SERVICE}`"
        )
    };
    match key_source(|k| std::env::var(k).ok()) {
        KeySource::Env(key) => Ok(key),
        KeySource::Keychain(argv) => {
            let out = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .map_err(|e| format!("{}: {e}", argv[0]))?;
            if !out.status.success() {
                return Err(missing());
            }
            let key = String::from_utf8(out.stdout)
                .map_err(|_| "the Keychain item is not UTF-8".to_string())?
                .trim()
                .to_string();
            if key.is_empty() {
                Err(missing())
            } else {
                Ok(key)
            }
        }
    }
}

/// tests/linear-testbed.toml: invented names only.
#[derive(Debug, Clone, PartialEq)]
pub struct Testbed {
    /// The workspace's URL key (linear.app/<url_key>).
    pub url_key: String,
    pub team_a: String,
    pub team_b: String,
    pub project: String,
}

pub fn testbed_from(text: &str) -> Result<Testbed, String> {
    let t: toml::Table = text
        .parse()
        .map_err(|e| format!("linear-testbed.toml: {e}"))?;
    let field = |name: &str| {
        t.get(name)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
            .ok_or_else(|| format!("linear-testbed.toml needs `{name}`"))
    };
    Ok(Testbed {
        url_key: field("url_key")?,
        team_a: field("team_a")?,
        team_b: field("team_b")?,
        project: field("project")?,
    })
}

pub fn testbed() -> Testbed {
    let text = std::fs::read_to_string("tests/linear-testbed.toml")
        .expect("tests/linear-testbed.toml (run from the repository root)");
    testbed_from(&text).unwrap_or_else(|e| panic!("{e}"))
}

/// Stops unless the key reaches the testbed. The other workspace is not named: it may be a
/// real one.
pub fn check_workspace(seen: &str, testbed: &Testbed) -> Result<(), String> {
    if seen == testbed.url_key {
        Ok(())
    } else {
        Err(format!(
            "the key reaches another workspace, not the testbed `{}`; stopping before any other request",
            testbed.url_key
        ))
    }
}

/// A response as Linear sent it: status, the rate and complexity headers, and the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Raw {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn data(&self) -> &Value {
        &self.body["data"]
    }

    /// `errors[].extensions.code`, in order.
    pub fn error_codes(&self) -> Vec<String> {
        self.body["errors"]
            .as_array()
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|e| e["extensions"]["code"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub struct Linear {
    http: reqwest::Client,
    key: String,
}

impl Linear {
    /// No workspace check: only for probe L6's deliberately bad key.
    pub fn unguarded(key: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("project-boards-testbed")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client builds");
        Self { http, key }
    }

    /// The testbed client: resolves the testbed key and checks the workspace before
    /// returning, so no other request can reach another workspace.
    pub async fn connect(testbed: &Testbed) -> Result<Self, String> {
        use graphql_client::GraphQLQuery;
        use project_boards_queries::linear::{TestbedViewer, testbed_viewer};
        let me = Self::unguarded(resolve_key()?);
        let raw = me
            .post(&TestbedViewer::build_query(testbed_viewer::Variables {}))
            .await?;
        let seen = raw.data()["viewer"]["organization"]["urlKey"]
            .as_str()
            .ok_or_else(|| format!("the viewer query failed (HTTP {})", raw.status))?;
        check_workspace(seen, testbed)?;
        Ok(me)
    }

    pub async fn post(&self, body: &impl serde::Serialize) -> Result<Raw, String> {
        let response = self
            .http
            .post(ENDPOINT)
            .header("Authorization", &self.key)
            .json(body)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter(|(n, _)| {
                let n = n.as_str();
                n.starts_with("x-ratelimit") || n == "x-complexity"
            })
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = response.json().await.unwrap_or(Value::Null);
        Ok(Raw {
            status,
            headers,
            body,
        })
    }
}
