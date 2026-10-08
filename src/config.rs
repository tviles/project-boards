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
    let Ok(text) = std::fs::read_to_string(dir.join("config.toml")) else {
        return (Config::default(), Vec::new());
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
    (config, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

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
