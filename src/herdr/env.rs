use serde::Deserialize;
use std::path::PathBuf;

pub const PLUGIN_ID: &str = "tviles.project-boards";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Worktree {
    #[serde(default)]
    pub repo_root: Option<String>,
    #[serde(default)]
    pub checkout_path: Option<String>,
}

/// `HERDR_PLUGIN_CONTEXT_JSON`. Every key is optional; herdr omits what it does not know.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PluginContext {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_cwd: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    /// The pane's launch directory, not where its shell is now.
    #[serde(default)]
    pub focused_pane_cwd: Option<String>,
    #[serde(default)]
    pub worktree: Option<Worktree>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginEnv {
    pub in_herdr: bool,
    pub plugin_id: String,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Set for processes running in a herdr pane, including our own board pane.
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub context: PluginContext,
}

impl PluginEnv {
    /// Outside herdr, the directories fall back to the paths herdr itself would use, so the
    /// CLI and the pane share state.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let home = get("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let context = get("HERDR_PLUGIN_CONTEXT_JSON")
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        Self {
            in_herdr: get("HERDR_ENV").as_deref() == Some("1"),
            plugin_id: get("HERDR_PLUGIN_ID").unwrap_or_else(|| PLUGIN_ID.to_string()),
            config_dir: get("HERDR_PLUGIN_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config/herdr/plugins/config").join(PLUGIN_ID)),
            state_dir: get("HERDR_PLUGIN_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state/herdr/plugins").join(PLUGIN_ID)),
            pane_id: get("HERDR_PANE_ID"),
            workspace_id: get("HERDR_WORKSPACE_ID"),
            context,
        }
    }

    pub fn from_system() -> Self {
        Self::from_vars(|key| std::env::var(key).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_herdr_variables_and_context() {
        let vars = [
            ("HERDR_ENV", "1"),
            ("HERDR_PLUGIN_STATE_DIR", "/s"),
            ("HERDR_PANE_ID", "p9"),
            (
                "HERDR_PLUGIN_CONTEXT_JSON",
                r#"{"workspace_id":"w1","focused_pane_id":"p1","focused_pane_cwd":"/code/app","worktree":{"repo_root":"/code/app"},"selected_text":"ignored"}"#,
            ),
        ];
        let env = PluginEnv::from_vars(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        });
        assert!(env.in_herdr);
        assert_eq!(env.state_dir, PathBuf::from("/s"));
        assert_eq!(env.pane_id.as_deref(), Some("p9"));
        assert_eq!(env.context.focused_pane_id.as_deref(), Some("p1"));
        assert_eq!(
            env.context.worktree.unwrap().repo_root.as_deref(),
            Some("/code/app")
        );
    }

    #[test]
    fn falls_back_to_herdr_paths_outside_herdr() {
        let env = PluginEnv::from_vars(|k| (k == "HOME").then(|| "/home/t".to_string()));
        assert!(!env.in_herdr);
        assert_eq!(
            env.config_dir,
            PathBuf::from("/home/t/.config/herdr/plugins/config/tviles.project-boards")
        );
        assert_eq!(
            env.state_dir,
            PathBuf::from("/home/t/.local/state/herdr/plugins/tviles.project-boards")
        );
        assert_eq!(env.context, PluginContext::default());
    }
}
