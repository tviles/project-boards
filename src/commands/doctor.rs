//! `project-boards doctor`: checks everything the board needs and says how to fix it.

use crate::github::Github;
use crate::github::token::resolve_token_from_system;
use crate::github::transport::HttpTransport;
use crate::herdr::cli::{HerdrCli, ProcessHerdr, action_cwd};
use crate::herdr::env::PluginEnv;
use crate::herdr::repo::detect_repo;
use crate::model::RepoSlug;
use crate::ui::keymap::{KeySpec, Keymap};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = match self.level {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Fail => "✗",
        };
        write!(f, "{mark} {}: {}", self.name, self.detail)
    }
}

/// What the token can do. Editing boards needs `project`; editing issues and pull requests
/// also needs `repo` (or `public_repo` for public repositories). `None` means a fine-grained
/// token, which reports no scopes and cannot read boards owned by a user account at all.
pub fn scope_check(scopes: &Option<Vec<String>>) -> Check {
    let check = |level, detail: String| Check {
        level,
        name: "scopes",
        detail,
    };
    let Some(s) = scopes else {
        return check(
            Level::Warn,
            "fine-grained token: works for organisation boards only; boards owned by a user account need `gh auth login`".into(),
        );
    };
    let has = |x: &str| s.iter().any(|y| y == x);
    let list = s.join(", ");
    match (
        has("project"),
        has("repo") || has("public_repo"),
        has("read:project"),
    ) {
        (true, true, _) => check(Level::Ok, format!("{list}; can edit boards and issues")),
        (true, false, _) => check(
            Level::Warn,
            format!(
                "{list}; can edit boards; editing issues needs `gh auth refresh -s repo` (or public_repo)"
            ),
        ),
        (false, _, true) => check(
            Level::Warn,
            format!("{list}; read:project only; editing needs `gh auth refresh -s project`"),
        ),
        (false, _, false) => check(
            Level::Fail,
            "missing the project scope; run `gh auth refresh -s project`".into(),
        ),
    }
}

/// herdr bindings without the prefix that equal a plugin key. herdr sees keys first, so
/// those plugin keys would never arrive.
pub fn key_collisions(herdr_config: &str, keymap: &Keymap) -> Vec<String> {
    let Ok(config) = herdr_config.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(keys) = config.get("keys").and_then(|k| k.as_table()) else {
        return Vec::new();
    };
    let mut bound: Vec<String> = Vec::new();
    for (name, value) in keys {
        match value {
            toml::Value::String(s) if name != "command" => bound.push(s.clone()),
            toml::Value::Array(entries) => {
                bound.extend(
                    entries
                        .iter()
                        .filter_map(|e| e.get("key").and_then(|k| k.as_str()).map(String::from)),
                );
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for spec in bound
        .iter()
        .filter(|b| !b.starts_with("prefix+"))
        .filter_map(|b| KeySpec::parse(b).ok())
    {
        for (plugin_key, action) in keymap.bindings() {
            if *plugin_key == spec {
                out.push(format!("{} ({})", plugin_key.label(), action.description()));
            }
        }
    }
    out
}

/// The board `open` would pick for the directory `cwd`.
pub fn repo_check(cwd: Option<&str>, detect: impl Fn(&Path) -> Option<RepoSlug>) -> Check {
    let check = |level, detail: String| Check {
        level,
        name: "repo",
        detail,
    };
    match cwd {
        Some(dir) => match detect(Path::new(dir)) {
            Some(repo) => check(Level::Ok, format!("{repo} ({dir})")),
            None => check(
                Level::Warn,
                format!("{dir} is not a GitHub checkout; the board picker will open"),
            ),
        },
        None => check(
            Level::Warn,
            "no working directory known; the board picker will open".into(),
        ),
    }
}

pub fn run_doctor(notify: bool) -> anyhow::Result<bool> {
    let env = PluginEnv::from_system();
    let mut checks = Vec::new();

    let (config, warnings) = crate::config::load_config(&env.config_dir);
    let token = resolve_token_from_system(config.gh_user.as_deref());
    match &token {
        Ok(t) => checks.push(Check {
            level: Level::Ok,
            name: "token",
            detail: format!("from {}", t.source.describe()),
        }),
        Err(e) => checks.push(Check {
            level: Level::Fail,
            name: "token",
            detail: e.to_string(),
        }),
    }
    if let Ok(t) = token {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let gh = Github::new(Arc::new(HttpTransport::new(t.value)));
        match runtime.block_on(gh.viewer()) {
            Ok(v) => {
                checks.push(Check {
                    level: Level::Ok,
                    name: "github",
                    detail: format!("signed in as {}", v.login),
                });
                checks.push(scope_check(&v.scopes));
            }
            Err(e) => checks.push(Check {
                level: Level::Fail,
                name: "github",
                detail: e.to_string(),
            }),
        }
    }

    let herdr = ProcessHerdr::from_env();
    match herdr.call(&["pane".into(), "list".into()]) {
        Ok(_) => checks.push(Check {
            level: Level::Ok,
            name: "herdr",
            detail: "reachable".into(),
        }),
        Err(e) if env.in_herdr => checks.push(Check {
            level: Level::Fail,
            name: "herdr",
            detail: e.to_string(),
        }),
        Err(_) => checks.push(Check {
            level: Level::Warn,
            name: "herdr",
            detail: "not running inside herdr".into(),
        }),
    }

    // The same directory `open` uses: herdr runs this action from the plugin root.
    checks.push(repo_check(action_cwd(&env, &herdr).as_deref(), detect_repo));

    let (keymap, key_warnings) = Keymap::with_overrides(&config.keys);
    for w in warnings.into_iter().chain(key_warnings) {
        checks.push(Check {
            level: Level::Warn,
            name: "config",
            detail: w,
        });
    }
    let herdr_config = std::env::var("HERDR_CONFIG_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| std::path::Path::new(&h).join(".config/herdr/config.toml"))
        });
    let collisions = herdr_config
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| key_collisions(&t, &keymap))
        .unwrap_or_default();
    if collisions.is_empty() {
        checks.push(Check {
            level: Level::Ok,
            name: "keys",
            detail: "no herdr binding shadows a board key".into(),
        });
    } else {
        checks.push(Check {
            level: Level::Warn,
            name: "keys",
            detail: format!(
                "herdr takes these first: {}; rebind them in [keys]",
                collisions.join(", ")
            ),
        });
    }

    for c in &checks {
        println!("{c}");
    }
    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warned = checks.iter().filter(|c| c.level == Level::Warn).count();
    if notify {
        let body = format!(
            "{failed} failed, {warned} warnings — see `herdr plugin log list --plugin {}`",
            env.plugin_id
        );
        let _ = herdr.call(&[
            "notification".into(),
            "show".into(),
            "project-boards doctor".into(),
            "--body".into(),
            body,
        ]);
    }
    Ok(failed == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_check_names_the_repo_of_the_action_directory() {
        let detect =
            |d: &Path| (d == Path::new("/code/app")).then(|| "tviles/app".parse().unwrap());
        let ok = repo_check(Some("/code/app"), detect);
        assert_eq!(
            (ok.level, ok.detail.as_str()),
            (Level::Ok, "tviles/app (/code/app)")
        );
        let elsewhere = repo_check(Some("/plugin/root"), detect);
        assert_eq!(elsewhere.level, Level::Warn);
        assert!(
            elsewhere
                .detail
                .starts_with("/plugin/root is not a GitHub checkout")
        );
        assert_eq!(repo_check(None, detect).level, Level::Warn);
    }

    #[test]
    fn scopes_with_project_pass_and_without_warn() {
        assert_eq!(
            scope_check(&Some(vec!["repo".into(), "project".into()])).level,
            Level::Ok
        );
        assert_eq!(
            scope_check(&Some(vec!["repo".into(), "read:project".into()])).level,
            Level::Warn
        );
        let missing = scope_check(&Some(vec!["repo".into()]));
        assert_eq!(missing.level, Level::Fail);
        assert!(missing.detail.contains("gh auth refresh -s project"));
        let fine_grained = scope_check(&None);
        assert_eq!(
            fine_grained.level,
            Level::Warn,
            "fine-grained tokens cannot read user-owned boards"
        );
        assert!(fine_grained.detail.contains("gh auth login"));
    }

    #[test]
    fn finds_herdr_bindings_that_shadow_plugin_keys() {
        let config = r#"
[keys]
focus_left = "ctrl+h"
next_tab = "tab"

[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "tviles.project-boards.open"

[[keys.command]]
key = "q"
type = "shell"
command = "true"
"#;
        let found = key_collisions(config, &Keymap::defaults());
        assert_eq!(
            found,
            vec![
                "q (quit)".to_string(),
                "tab (next view (next link in detail))".to_string()
            ]
        );
    }

    #[test]
    fn the_clear_filter_key_is_checked_too() {
        let config = "[keys]\nclose_pane = \"x\"\n";
        assert_eq!(
            key_collisions(config, &Keymap::defaults()),
            vec!["x (clear the local filter)".to_string()]
        );
    }

    #[test]
    fn unparseable_herdr_config_has_no_collisions() {
        assert!(key_collisions("not = [toml", &Keymap::defaults()).is_empty());
    }

    #[test]
    fn checks_format_with_their_level() {
        let c = Check {
            level: Level::Warn,
            name: "repo",
            detail: "not in a GitHub checkout".into(),
        };
        assert_eq!(c.to_string(), "! repo: not in a GitHub checkout");
    }

    #[test]
    fn scopes_say_what_the_token_can_edit() {
        let s = |list: &[&str]| scope_check(&Some(list.iter().map(|x| x.to_string()).collect()));
        let full = s(&["repo", "project", "read:org"]);
        assert_eq!(full.level, Level::Ok);
        assert!(
            full.detail.contains("can edit boards and issues"),
            "{}",
            full.detail
        );
        assert_eq!(s(&["public_repo", "project"]).level, Level::Ok);
        let boards_only = s(&["project"]);
        assert_eq!(boards_only.level, Level::Warn);
        assert!(
            boards_only.detail.contains("gh auth refresh -s repo"),
            "{}",
            boards_only.detail
        );
        let read = s(&["repo", "read:project"]);
        assert_eq!(read.level, Level::Warn);
        assert!(read.detail.contains("gh auth refresh -s project"));
        assert_eq!(s(&["repo"]).level, Level::Fail);
    }
}
