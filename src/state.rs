//! What the plugin remembers between runs: the board per repository and the last view per board.

use crate::fsutil::write_atomic;
use crate::model::{BoardRef, RepoSlug, ViewId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// `owner/name` → `owner/number`
    #[serde(default)]
    pub boards_by_repo: BTreeMap<String, String>,
    /// `owner/number` → view id
    #[serde(default)]
    pub last_view: BTreeMap<String, String>,
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
    fn garbage_state_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), "nope").unwrap();
        assert_eq!(State::load(dir.path()), State::default());
    }
}
