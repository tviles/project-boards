use crate::cli::Placement;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub poll_interval_secs: u64,
    pub background_poll_interval_secs: u64,
    pub idle_after_secs: u64,
    pub full_refresh_mins: u64,
    pub max_items: usize,
    pub placement: Placement,
    /// Action name to key, e.g. `next_view = "]"`.
    pub keys: BTreeMap<String, String>,
    /// The `gh` account whose token to use (`gh auth token --user <gh_user>`) when neither
    /// GH_TOKEN nor GITHUB_TOKEN is set. Unset: the active `gh` account.
    pub gh_user: Option<String>,
    /// Program and arguments (run without a shell) that print the Linear API key, used when
    /// LINEAR_API_KEY is not set, e.g. `["security", "find-generic-password", "-w", "-s", "linear"]`.
    pub linear_api_key_command: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_secs: 30,
            background_poll_interval_secs: 300,
            idle_after_secs: 300,
            full_refresh_mins: 10,
            max_items: 2000,
            placement: Placement::Tab,
            keys: BTreeMap::new(),
            gh_user: None,
            linear_api_key_command: None,
        }
    }
}

fn clamp<T: PartialOrd + Copy + Display>(
    value: &mut T,
    min: T,
    max: T,
    name: &str,
    warnings: &mut Vec<String>,
) {
    if *value < min || *value > max {
        let fixed = if *value < min { min } else { max };
        warnings.push(format!(
            "{name} = {value} is outside {min}..={max}; using {fixed}"
        ));
        *value = fixed;
    }
}

/// Loads `config.toml` from `dir`. Missing file: defaults. Unparseable file: defaults and a
/// warning. Out-of-range numbers are clamped with a warning. Never fails.
pub fn load_config(dir: &Path) -> (Config, Vec<String>) {
    let text = match std::fs::read_to_string(dir.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Config::default(), Vec::new());
        }
        Err(e) => {
            return (
                Config::default(),
                vec![format!("config.toml ignored: could not read it ({e})")],
            );
        }
    };
    let mut warnings = Vec::new();
    let mut config = match toml::from_str::<Config>(&text) {
        Ok(c) => c,
        Err(e) => {
            let first = e
                .to_string()
                .lines()
                .next()
                .unwrap_or("parse error")
                .to_string();
            warnings.push(format!("config.toml ignored: {first}"));
            return (Config::default(), warnings);
        }
    };
    clamp(
        &mut config.poll_interval_secs,
        5,
        3600,
        "poll_interval_secs",
        &mut warnings,
    );
    clamp(
        &mut config.background_poll_interval_secs,
        30,
        7200,
        "background_poll_interval_secs",
        &mut warnings,
    );
    clamp(
        &mut config.idle_after_secs,
        30,
        7200,
        "idle_after_secs",
        &mut warnings,
    );
    clamp(
        &mut config.full_refresh_mins,
        1,
        240,
        "full_refresh_mins",
        &mut warnings,
    );
    clamp(
        &mut config.max_items,
        100,
        50_000,
        "max_items",
        &mut warnings,
    );
    if let Some(user) = config.gh_user.take() {
        if is_github_login(&user) {
            config.gh_user = Some(user);
        } else {
            warnings.push(format!(
                "gh_user = {user:?} is not a GitHub login; using the active gh account"
            ));
        }
    }
    if let Some(argv) = config.linear_api_key_command.take() {
        if argv
            .first()
            .is_some_and(|program| !program.trim().is_empty())
        {
            config.linear_api_key_command = Some(argv);
        } else {
            warnings.push("linear_api_key_command ignored: it must name a program".into());
        }
    }
    (config, warnings)
}

/// GitHub logins: 1 to 39 ASCII letters, digits or hyphens, not starting with a hyphen. Also
/// keeps the value from being read as a `gh` option.
fn is_github_login(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_user_is_read_and_validated() {
        assert_eq!(
            with("gh_user = \"tviles\"").0.gh_user.as_deref(),
            Some("tviles")
        );
        assert_eq!(with("").0.gh_user, None);
        for bad in ["--hostname", "a b", "", "x;y"] {
            let (c, w) = with(&format!("gh_user = {bad:?}"));
            assert_eq!(c.gh_user, None, "{bad}");
            assert_eq!(w.len(), 1, "{bad}");
        }
    }

    #[test]
    fn linear_key_command_is_read_and_an_empty_one_is_dropped() {
        let (c, w) = with("linear_api_key_command = [\"pass\", \"show\", \"linear\"]");
        assert_eq!(
            c.linear_api_key_command,
            Some(vec!["pass".into(), "show".into(), "linear".into()])
        );
        assert!(w.is_empty());
        assert_eq!(with("").0.linear_api_key_command, None);
        for bad in ["[]", "[\"\"]", "[\"  \", \"x\"]"] {
            let (c, w) = with(&format!("linear_api_key_command = {bad}"));
            assert_eq!(c.linear_api_key_command, None, "{bad}");
            assert_eq!(
                w,
                vec!["linear_api_key_command ignored: it must name a program".to_string()],
                "{bad}"
            );
        }
    }

    fn with(text: &str) -> (Config, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), text).unwrap();
        load_config(dir.path())
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_config(dir.path()), (Config::default(), vec![]));
    }

    #[test]
    fn reads_values_and_keys() {
        let (c, w) =
            with("poll_interval_secs = 60\nplacement = \"overlay\"\n[keys]\nnext_view = \"]\"\n");
        assert!(w.is_empty());
        assert_eq!(c.poll_interval_secs, 60);
        assert_eq!(c.placement, Placement::Overlay);
        assert_eq!(c.keys["next_view"], "]");
        assert_eq!(c.max_items, 2000);
    }

    #[test]
    fn unreadable_config_warns_and_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("config.toml")).unwrap();
        let (c, w) = load_config(dir.path());
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("config.toml ignored"));
    }

    #[test]
    fn popup_placement_is_rejected_with_a_warning() {
        let (c, w) = with("placement = \"popup\"\n");
        assert_eq!(c, Config::default());
        assert!(w[0].starts_with("config.toml ignored"));
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let (c, w) = with("poll_interval_secs = 1\nmax_items = 999999\n");
        assert_eq!((c.poll_interval_secs, c.max_items), (5, 50_000));
        assert_eq!(w.len(), 2);
    }
}
