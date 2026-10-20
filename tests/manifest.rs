use std::fs;

fn manifest() -> toml::Table {
    let text = fs::read_to_string("herdr-plugin.toml").expect("herdr-plugin.toml exists");
    text.parse::<toml::Table>().expect("manifest parses")
}

#[test]
fn identity_and_platforms() {
    let m = manifest();
    assert_eq!(m["id"].as_str(), Some("tviles.project-boards"));
    assert_eq!(m["min_herdr_version"].as_str(), Some("0.9.0"));
    let platforms: Vec<_> = m["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(platforms, ["linux", "macos"]);
}

#[test]
fn version_matches_cargo() {
    assert_eq!(
        manifest()["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn board_pane_runs_launcher_through_plugin_root() {
    let m = manifest();
    let panes = m["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1);
    let pane = panes[0].as_table().unwrap();
    assert_eq!(pane["id"].as_str(), Some("board"));
    assert_eq!(pane["placement"].as_str(), Some("tab"));
    let command: Vec<_> = pane["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(
        command
            .join(" ")
            .contains("$HERDR_PLUGIN_ROOT/herdr/launch.sh")
    );
    assert!(command.join(" ").ends_with("pane"));
}

#[test]
fn actions_have_local_ids_and_valid_placements() {
    let m = manifest();
    let ids: Vec<_> = m["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        [
            "open",
            "open-picker",
            "open-overlay",
            "open-split",
            "open-zoomed",
            "doctor"
        ]
    );
    for id in &ids {
        assert!(!id.contains('.'), "action id {id} must not contain dots");
    }
    for action in m["actions"].as_array().unwrap() {
        let command: Vec<_> = action["command"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        if let Some(i) = command.iter().position(|c| *c == "--placement") {
            assert!(["tab", "overlay", "split", "zoomed"].contains(&command[i + 1]));
        }
    }
}

#[test]
fn changelog_has_a_section_for_the_version() {
    let version = manifest()["version"].as_str().unwrap().to_string();
    let changelog = fs::read_to_string("CHANGELOG.md").expect("CHANGELOG.md exists");
    let heading = if version.ends_with("-dev") {
        "## [Unreleased]".to_string()
    } else {
        format!("## [{version}] - ")
    };
    assert!(
        changelog.lines().any(|l| l.starts_with(&heading)),
        "CHANGELOG.md has no `{heading}` section for version {version}"
    );
}

fn changelog_section(version: &str) -> std::process::Output {
    std::process::Command::new("sh")
        .arg("scripts/changelog-section.sh")
        .arg(version)
        .output()
        .expect("sh runs")
}

#[test]
fn changelog_section_prints_one_version_body() {
    let out = changelog_section("0.3.1");
    assert!(out.status.success(), "{out:?}");
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(
        body.starts_with("### "),
        "body starts at its first heading: {body:?}"
    );
    assert!(body.ends_with('\n') && !body.ends_with("\n\n"), "{body:?}");
    assert!(
        !body.contains("## ["),
        "stops before the next version: {body:?}"
    );
    assert!(
        !body.contains("]: https://"),
        "leaves the compare links out: {body:?}"
    );
    // The oldest section ends at the compare links, not the end of the file.
    let oldest = String::from_utf8(changelog_section("0.1.0").stdout).unwrap();
    assert!(oldest.contains("### Added"), "{oldest:?}");
    assert!(!oldest.contains("]: https://"), "{oldest:?}");
    assert!(
        oldest.ends_with('\n') && !oldest.ends_with("\n\n"),
        "{oldest:?}"
    );
}

#[test]
fn changelog_section_fails_for_a_missing_version() {
    for missing in ["9.9.9", "0.3", "0.3.1.0", ""] {
        let out = changelog_section(missing);
        assert!(!out.status.success(), "{missing:?} has no section");
        assert!(out.stdout.is_empty(), "{missing:?}");
    }
}
