//! What the plugin remembers between runs: the board per repository and the last view per board.

use crate::fsutil::write_atomic;
use crate::model::{BoardRef, Layout, RepoSlug, ViewId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A layout the user chose for one view with `L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutOverride {
    pub layout: Layout,
    /// The view's own layout on GitHub when the choice was made. When GitHub's layout later
    /// differs, the choice is stale and dropped.
    pub github_layout: Layout,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// `owner/name` → `owner/number`
    #[serde(default)]
    pub boards_by_repo: BTreeMap<String, String>,
    /// `owner/number` → view id
    #[serde(default)]
    pub last_view: BTreeMap<String, String>,
    /// `owner/number/view id` → the layout chosen for that view
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

    pub fn remembered_board(&self, repo: &RepoSlug) -> Option<BoardRef> {
        self.boards_by_repo.get(&repo.to_string())?.parse().ok()
    }

    pub fn remember_board(&mut self, repo: &RepoSlug, board: &BoardRef) {
        self.boards_by_repo
            .insert(repo.to_string(), board.to_string());
    }

    pub fn last_view(&self, board: &BoardRef) -> Option<ViewId> {
        self.last_view.get(&board.to_string()).map(ViewId::new)
    }

    pub fn set_last_view(&mut self, board: &BoardRef, view: &ViewId) {
        self.last_view.insert(board.to_string(), view.0.clone());
    }

    fn layout_key(board: &BoardRef, view: &ViewId) -> String {
        format!("{board}/{}", view.0)
    }

    pub fn set_layout_override(&mut self, board: &BoardRef, view: &ViewId, o: LayoutOverride) {
        self.layout_overrides
            .insert(Self::layout_key(board, view), o);
    }

    pub fn clear_layout_override(&mut self, board: &BoardRef, view: &ViewId) {
        self.layout_overrides.remove(&Self::layout_key(board, view));
    }

    /// The overrides saved for `board`'s views.
    pub fn layout_overrides_for(&self, board: &BoardRef) -> Vec<(ViewId, LayoutOverride)> {
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
    fn remembers_boards_and_views_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let repo: RepoSlug = "tviles/app".parse().unwrap();
        let board: BoardRef = "tviles/3".parse().unwrap();
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
        let (a, b): (BoardRef, BoardRef) =
            ("tviles/3".parse().unwrap(), "tviles/4".parse().unwrap());
        let o = LayoutOverride {
            layout: Layout::Board,
            github_layout: Layout::Table,
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
}
