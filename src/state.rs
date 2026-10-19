//! What the plugin remembers between runs: the board per repository and the last view per board.

use crate::fsutil::write_atomic;
use crate::model::{BoardKey, Layout, RepoSlug, ViewId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A layout the user chose for one view with `L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutOverride {
    pub layout: Layout,
    /// The view's own layout on the server when the choice was made. When the server's
    /// layout later differs, the choice is stale and dropped. Older state files call it
    /// `github_layout`.
    #[serde(alias = "github_layout")]
    pub server_layout: Layout,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// `owner/name` → board key
    #[serde(default)]
    pub boards_by_repo: BTreeMap<String, String>,
    /// board key → view id
    #[serde(default)]
    pub last_view: BTreeMap<String, String>,
    /// `board key/view id` → the layout chosen for that view
    #[serde(default)]
    pub layout_overrides: BTreeMap<String, LayoutOverride>,
}

impl State {
    fn path(dir: &Path) -> std::path::PathBuf {
        dir.join("state.json")
    }

    /// The saved state; a missing or unreadable file gives the empty state.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(&Self::path(dir), &bytes)
    }

    /// Reloads, applies `f`, and saves, so concurrent panes lose as little as possible.
    pub fn update(dir: &Path, f: impl FnOnce(&mut State)) -> std::io::Result<State> {
        let mut state = Self::load(dir);
        f(&mut state);
        state.save(dir)?;
        Ok(state)
    }

    pub fn remembered_board(&self, repo: &RepoSlug) -> Option<BoardKey> {
        self.boards_by_repo.get(&repo.to_string())?.parse().ok()
    }

    pub fn remember_board(&mut self, repo: &RepoSlug, board: &BoardKey) {
        self.boards_by_repo
            .insert(repo.to_string(), board.to_string());
    }

    pub fn last_view(&self, board: &BoardKey) -> Option<ViewId> {
        self.last_view.get(&board.to_string()).map(ViewId::new)
    }

    pub fn set_last_view(&mut self, board: &BoardKey, view: &ViewId) {
        self.last_view.insert(board.to_string(), view.0.clone());
    }

    fn layout_key(board: &BoardKey, view: &ViewId) -> String {
        format!("{board}/{}", view.0)
    }

    pub fn set_layout_override(&mut self, board: &BoardKey, view: &ViewId, o: LayoutOverride) {
        self.layout_overrides
            .insert(Self::layout_key(board, view), o);
    }

    pub fn clear_layout_override(&mut self, board: &BoardKey, view: &ViewId) {
        self.layout_overrides.remove(&Self::layout_key(board, view));
    }

    /// The overrides saved for `board`'s views.
    pub fn layout_overrides_for(&self, board: &BoardKey) -> Vec<(ViewId, LayoutOverride)> {
        let prefix = format!("{board}/");
        self.layout_overrides
            .iter()
            .filter_map(|(k, o)| Some((ViewId::new(k.strip_prefix(&prefix)?), *o)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_and_panes_files_still_find_their_boards() {
        let dir = tempfile::tempdir().unwrap();
        // state.json and panes.json exactly as 0.3 wrote them.
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"boards_by_repo": {"acme/app": "acme/3"}, "last_view": {"acme/3": "V2"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("panes.json"), r#"{"acme/3": "p7"}"#).unwrap();
        let board = BoardKey::github("acme", 3);
        let s = State::load(dir.path());
        assert_eq!(
            s.remembered_board(&"acme/app".parse().unwrap()),
            Some(board.clone())
        );
        assert_eq!(s.last_view(&board), Some(ViewId::new("V2")));
        let panes = crate::herdr::registry::PaneRegistry::new(dir.path());
        assert_eq!(panes.lookup(&board).as_deref(), Some("p7"));
    }

    #[test]
    fn a_linear_board_is_remembered_beside_github_ones() {
        let dir = tempfile::tempdir().unwrap();
        let team: BoardKey = "linear:team:t1".parse().unwrap();
        State::update(dir.path(), |s| {
            s.remember_board(&"acme/app".parse().unwrap(), &team);
            s.set_last_view(&team, &ViewId::new("board"));
        })
        .unwrap();
        let s = State::load(dir.path());
        assert_eq!(
            s.remembered_board(&"acme/app".parse().unwrap()),
            Some(team.clone())
        );
        assert_eq!(s.last_view(&team), Some(ViewId::new("board")));
        assert_eq!(s.last_view(&BoardKey::github("acme", 3)), None);
    }

    #[test]
    fn remembers_boards_and_views_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let repo: RepoSlug = "tviles/app".parse().unwrap();
        let board: BoardKey = "tviles/3".parse().unwrap();
        State::update(dir.path(), |s| {
            s.remember_board(&repo, &board);
            s.set_last_view(&board, &ViewId::new("PVTV_1"));
        })
        .unwrap();
        let s = State::load(dir.path());
        assert_eq!(s.remembered_board(&repo), Some(board.clone()));
        assert_eq!(s.last_view(&board), Some(ViewId::new("PVTV_1")));
    }

    #[test]
    fn layout_overrides_round_trip_per_board_and_view() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b): (BoardKey, BoardKey) =
            ("tviles/3".parse().unwrap(), "tviles/4".parse().unwrap());
        let o = LayoutOverride {
            layout: Layout::Board,
            server_layout: Layout::Table,
        };
        State::update(dir.path(), |s| {
            s.set_layout_override(&a, &ViewId::new("V1"), o);
            s.set_layout_override(&b, &ViewId::new("V1"), o);
            s.set_layout_override(&a, &ViewId::new("V2"), o);
            s.clear_layout_override(&a, &ViewId::new("V2"));
        })
        .unwrap();
        let text = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
        assert!(text.contains("\"tviles/3/V1\"") && text.contains("\"board\""));
        let s = State::load(dir.path());
        assert_eq!(s.layout_overrides_for(&a), vec![(ViewId::new("V1"), o)]);
        assert_eq!(s.layout_overrides_for(&b), vec![(ViewId::new("V1"), o)]);
    }

    #[test]
    fn old_state_without_layout_overrides_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"boards_by_repo":{"a/b":"a/1"},"last_view":{"a/1":"V1"}}"#,
        )
        .unwrap();
        let s = State::load(dir.path());
        assert_eq!(
            s.last_view(&"a/1".parse().unwrap()),
            Some(ViewId::new("V1"))
        );
        assert!(s.layout_overrides.is_empty());
    }

    #[test]
    fn garbage_state_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), "nope").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }

    #[test]
    fn an_old_layout_override_still_loads_and_saves_under_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"layout_overrides": {"acme/3/V1": {"layout": "board", "github_layout": "table"}}}"#,
        )
        .unwrap();
        let board = BoardKey::github("acme", 3);
        let expected = LayoutOverride {
            layout: Layout::Board,
            server_layout: Layout::Table,
        };
        let s = State::load(dir.path());
        assert_eq!(
            s.layout_overrides_for(&board),
            vec![(ViewId::new("V1"), expected)]
        );
        s.save(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
        assert!(
            text.contains("server_layout") && !text.contains("github_layout"),
            "{text}"
        );
        // A Linear board's overrides never pick up a GitHub board's, or the other way round.
        let team: BoardKey = "linear:team:t1".parse().unwrap();
        assert!(s.layout_overrides_for(&team).is_empty());
    }
}
