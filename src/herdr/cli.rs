//! The herdr CLI is the plugin API. Every call returns the parsed `.result` object or the
//! `{"error":{"code","message"}}` envelope herdr prints on stderr.

use crate::cli::Placement;
use serde_json::Value;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("herdr {command}: {code}: {message}")]
pub struct HerdrError {
    pub code: String,
    pub message: String,
    pub command: String,
}

pub trait HerdrCli: Send + Sync {
    fn call(&self, args: &[String]) -> Result<Value, HerdrError>;
}

pub struct ProcessHerdr {
    bin: String,
}

impl ProcessHerdr {
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string()),
        }
    }
}

impl HerdrCli for ProcessHerdr {
    fn call(&self, args: &[String]) -> Result<Value, HerdrError> {
        let command = args.join(" ");
        let fail = |code: &str, message: String| HerdrError {
            code: code.into(),
            message,
            command: command.clone(),
        };
        let out = Command::new(&self.bin)
            .args(args)
            .output()
            .map_err(|e| fail("spawn_failed", e.to_string()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let envelope: Option<Value> = serde_json::from_str(stderr.trim()).ok();
            let field = |k: &str| {
                envelope
                    .as_ref()
                    .and_then(|v| v["error"][k].as_str())
                    .map(String::from)
            };
            return Err(fail(
                &field("code").unwrap_or_else(|| "error".into()),
                field("message").unwrap_or_else(|| stderr.trim().to_string()),
            ));
        }
        let value: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| fail("bad_output", e.to_string()))?;
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

pub fn pane_exists(cli: &dyn HerdrCli, pane: &str) -> Result<bool, HerdrError> {
    match cli.call(&args(&["pane", "get", pane])) {
        Ok(_) => Ok(true),
        Err(e) if e.code == "pane_not_found" => Ok(false),
        Err(e) => Err(e),
    }
}

pub fn focus_plugin_pane(cli: &dyn HerdrCli, pane: &str) -> Result<(), HerdrError> {
    cli.call(&args(&["plugin", "pane", "focus", pane]))
        .map(|_| ())
}

/// The live working directory of a pane (its foreground process), else its launch directory.
pub fn pane_live_cwd(cli: &dyn HerdrCli, pane: &str) -> Option<String> {
    let result = cli.call(&args(&["pane", "list"])).ok()?;
    let p = result["panes"]
        .as_array()?
        .iter()
        .find(|p| p["pane_id"] == pane)?;
    p["foreground_cwd"]
        .as_str()
        .or_else(|| p["cwd"].as_str())
        .map(String::from)
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenRequest {
    pub placement: Placement,
    pub workspace: Option<String>,
    /// Required by herdr for split and zoomed placements.
    pub target_pane: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

/// Opens the `board` entrypoint and returns the new pane's id.
pub fn open_plugin_pane(
    cli: &dyn HerdrCli,
    plugin_id: &str,
    req: &OpenRequest,
) -> Result<String, HerdrError> {
    let mut a = args(&[
        "plugin",
        "pane",
        "open",
        "--plugin",
        plugin_id,
        "--entrypoint",
        "board",
        "--placement",
        req.placement.as_str(),
    ]);
    if let Some(w) = &req.workspace {
        a.extend(args(&["--workspace", w]));
    }
    if let Some(t) = &req.target_pane {
        a.extend(args(&["--target-pane", t]));
    }
    if req.placement == Placement::Split {
        a.extend(args(&["--direction", "right"]));
    }
    if let Some(c) = &req.cwd {
        a.extend(args(&["--cwd", c]));
    }
    for (k, v) in &req.env {
        a.push("--env".into());
        a.push(format!("{k}={v}"));
    }
    a.push("--focus".into());
    let result = cli.call(&a)?;
    result["plugin_pane"]["pane"]["pane_id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| HerdrError {
            code: "bad_output".into(),
            message: "plugin pane open returned no pane id".into(),
            command: a.join(" "),
        })
}

#[cfg(test)]
type ScriptedResponse = (Vec<String>, Result<Value, HerdrError>);

/// A scripted herdr for tests: each call is matched against the queued responses by prefix.
#[cfg(test)]
#[derive(Default)]
pub struct FakeHerdr {
    pub calls: std::sync::Mutex<Vec<Vec<String>>>,
    responses: std::sync::Mutex<Vec<ScriptedResponse>>,
}

#[cfg(test)]
impl FakeHerdr {
    pub fn respond(&self, prefix: &[&str], result: Result<Value, HerdrError>) {
        self.responses.lock().unwrap().push((args(prefix), result));
    }
    pub fn not_found() -> Result<Value, HerdrError> {
        Err(HerdrError {
            code: "pane_not_found".into(),
            message: "gone".into(),
            command: String::new(),
        })
    }
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl HerdrCli for FakeHerdr {
    fn call(&self, a: &[String]) -> Result<Value, HerdrError> {
        self.calls.lock().unwrap().push(a.to_vec());
        let responses = self.responses.lock().unwrap();
        responses
            .iter()
            .find(|(prefix, _)| a.starts_with(prefix))
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| {
                Err(HerdrError {
                    code: "unscripted".into(),
                    message: a.join(" "),
                    command: a.join(" "),
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pane_exists_maps_not_found_to_false() {
        let fake = FakeHerdr::default();
        fake.respond(&["pane", "get", "p1"], Ok(json!({"pane": {}})));
        fake.respond(&["pane", "get", "p2"], FakeHerdr::not_found());
        assert_eq!(pane_exists(&fake, "p1"), Ok(true));
        assert_eq!(pane_exists(&fake, "p2"), Ok(false));
    }

    #[test]
    fn live_cwd_prefers_foreground() {
        let fake = FakeHerdr::default();
        fake.respond(
            &["pane", "list"],
            Ok(json!({"panes": [
            {"pane_id": "p1", "cwd": "/launch", "foreground_cwd": "/now"},
            {"pane_id": "p2", "cwd": "/only"}]})),
        );
        assert_eq!(pane_live_cwd(&fake, "p1").as_deref(), Some("/now"));
        assert_eq!(pane_live_cwd(&fake, "p2").as_deref(), Some("/only"));
        assert_eq!(pane_live_cwd(&fake, "p3"), None);
    }

    #[test]
    fn open_builds_the_full_command() {
        let fake = FakeHerdr::default();
        fake.respond(
            &["plugin", "pane", "open"],
            Ok(json!({"plugin_pane": {"pane": {"pane_id": "new1"}}})),
        );
        let req = OpenRequest {
            placement: Placement::Split,
            workspace: Some("w1".into()),
            target_pane: Some("p1".into()),
            cwd: Some("/code".into()),
            env: vec![("PB_REPO".into(), "tviles/app".into())],
        };
        assert_eq!(
            open_plugin_pane(&fake, "tviles.project-boards", &req).unwrap(),
            "new1"
        );
        let call = fake.calls()[0].join(" ");
        assert_eq!(
            call,
            "plugin pane open --plugin tviles.project-boards --entrypoint board --placement split --workspace w1 --target-pane p1 --direction right --cwd /code --env PB_REPO=tviles/app --focus"
        );
    }
}
