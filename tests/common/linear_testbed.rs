//! The Linear testbed for the seed program, the probes and (plan 2) the live suite: which
//! workspace it is, where its key comes from, and a guard that stops before any request
//! reaches another workspace. Included with `#[path]` (tests/linear_testbed.rs, examples).
#![allow(dead_code)] // each includer uses a different part

use serde_json::Value;

pub const ENDPOINT: &str = "https://api.linear.app/graphql";
const KEYCHAIN_SERVICE: &str = "project-boards-linear-testbed";

/// Where the testbed key comes from. Never `LINEAR_API_KEY`: that is the user's own workspace.
#[derive(PartialEq)]
pub enum KeySource {
    Env(String),
    Keychain(Vec<String>),
}

impl std::fmt::Debug for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::Env(_) => f.write_str("Env(<redacted>)"),
            KeySource::Keychain(argv) => write!(f, "Keychain({argv:?})"),
        }
    }
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
/// Every probe name `PB_PROBES` may select; the first nine run when it is unset.
pub const ALL_PROBES: [&str; 12] = [
    "l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l11", "l12", "l13",
];
/// The 0.4 run order (L1 restores TBA-1 after L6, so it stays where it was).
const DEFAULT_PROBES: [&str; 9] = ["l2", "l3", "l4", "l5", "l6", "l1", "l7", "l8", "l9"];

/// The probes `PB_PROBES` selects (comma separated, case and spaces ignored, in the order
/// named, repeats dropped). Unset or blank means L1-L9; the write probes L11-L13 only run when
/// named. Unknown names are an error that lists them.
pub fn select_probes(value: Option<&str>) -> Result<Vec<String>, String> {
    let names: Vec<String> = value
        .unwrap_or("")
        .split(',')
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() {
        return Ok(DEFAULT_PROBES.map(String::from).to_vec());
    }
    let unknown: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !ALL_PROBES.contains(n))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown probe name(s) in PB_PROBES: {}; known: {}",
            unknown.join(", "),
            ALL_PROBES.join(", ")
        ));
    }
    let mut picked: Vec<String> = Vec::new();
    for n in names {
        if !picked.contains(&n) {
            picked.push(n);
        }
    }
    Ok(picked)
}

/// The read-only personal API key for L13, from `PB_LINEAR_READONLY_KEY`. Trimmed, never printed.
pub fn readonly_key() -> Option<String> {
    std::env::var("PB_LINEAR_READONLY_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

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
    /// The body as text when it was not JSON (an error page); `None` for JSON bodies.
    pub text: Option<String>,
}

impl Raw {
    /// Parses a body as JSON; anything else is kept as (lossy) text, never dropped.
    pub fn from_body(status: u16, headers: Vec<(String, String)>, bytes: &[u8]) -> Self {
        match serde_json::from_slice(bytes) {
            Ok(body) => Raw {
                status,
                headers,
                body,
                text: None,
            },
            Err(_) => Raw {
                status,
                headers,
                body: Value::Null,
                text: Some(String::from_utf8_lossy(bytes).into_owned()),
            },
        }
    }

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

/// The error-probe client's restriction: only `TestbedViewer`, checked before any request.
pub fn viewer_only_check(body: &impl serde::Serialize) -> Result<(), String> {
    let operation = serde_json::to_value(body)
        .ok()
        .and_then(|v| v["operationName"].as_str().map(String::from));
    if operation.as_deref() == Some("TestbedViewer") {
        Ok(())
    } else {
        Err("only TestbedViewer may be sent without the workspace guard".into())
    }
}

pub struct Linear {
    http: reqwest::Client,
    key: String,
    /// Set for the error probes' deliberately bad keys: only `TestbedViewer` may be sent.
    viewer_only: bool,
}

impl Linear {
    /// No workspace check, so it can send nothing but `TestbedViewer`: for probe L6's
    /// deliberately bad or revoked keys.
    pub fn for_error_probe(key: String) -> Self {
        Self::build(key, true)
    }

    fn build(key: String, viewer_only: bool) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("project-boards-testbed")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            key,
            viewer_only,
        }
    }

    /// The testbed client: resolves the testbed key and checks the workspace before
    /// returning, so no other request can reach another workspace.
    pub async fn connect(testbed: &Testbed) -> Result<Self, String> {
        Self::connect_with_key(resolve_key()?, testbed).await
    }

    /// Like `connect`, with a key the caller supplies (L13's read-only key); the workspace is
    /// checked exactly the same way, before any other request.
    pub async fn connect_with_key(key: String, testbed: &Testbed) -> Result<Self, String> {
        use graphql_client::GraphQLQuery;
        use project_boards_queries::linear::{TestbedViewer, testbed_viewer};
        let me = Self::build(key, false);
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
        if self.viewer_only {
            viewer_only_check(body)?;
        }
        let mut key = reqwest::header::HeaderValue::from_str(&self.key)
            .map_err(|_| "the testbed key is not a valid header value".to_string())?;
        key.set_sensitive(true);
        let response = self
            .http
            .post(ENDPOINT)
            .header(reqwest::header::AUTHORIZATION, key)
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
        let bytes = response
            .bytes()
            .await
            .map_err(|e| e.without_url().to_string())?;
        Ok(Raw::from_body(status, headers, &bytes))
    }
}
