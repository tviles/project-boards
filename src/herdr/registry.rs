//! Which herdr pane shows which board, so `open` can focus instead of duplicating.

use crate::fsutil::write_atomic;
use crate::herdr::cli::{HerdrCli, pane_exists};
use crate::model::BoardKey;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct PaneRegistry {
    path: PathBuf,
}

impl PaneRegistry {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("panes.json"),
        }
    }

    fn read(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn write(&self, map: &BTreeMap<String, String>) -> std::io::Result<()> {
        write_atomic(
            &self.path,
            &serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?,
        )
    }

    pub fn lookup(&self, board: &BoardKey) -> Option<String> {
        self.read().get(&board.to_string()).cloned()
    }

    pub fn register(&self, board: &BoardKey, pane: &str) -> std::io::Result<()> {
        let mut map = self.read();
        map.insert(board.to_string(), pane.to_string());
        self.write(&map)
    }

    /// Removes the record only if it still points at `pane`.
    pub fn unregister(&self, board: &BoardKey, pane: &str) -> std::io::Result<()> {
        let mut map = self.read();
        if map.get(&board.to_string()).map(String::as_str) == Some(pane) {
            map.remove(&board.to_string());
            self.write(&map)?;
        }
        Ok(())
    }

    /// Records `pane` under `to` and drops its record under `from` (if it still points at
    /// `pane`), in one write: a board opened by another key resolved to its canonical one.
    pub fn rekey(&self, from: &BoardKey, to: &BoardKey, pane: &str) -> std::io::Result<()> {
        let mut map = self.read();
        if map.get(&from.to_string()).map(String::as_str) == Some(pane) {
            map.remove(&from.to_string());
        }
        map.insert(to.to_string(), pane.to_string());
        self.write(&map)
    }

    /// The live pane showing `board`. A record whose pane is gone is discarded.
    pub fn live_pane(&self, cli: &dyn HerdrCli, board: &BoardKey) -> Option<String> {
        let pane = self.lookup(board)?;
        match pane_exists(cli, &pane) {
            Ok(true) => Some(pane),
            Ok(false) => {
                let _ = self.unregister(board, &pane);
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not check pane; treating as not open");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::cli::FakeHerdr;
    use serde_json::json;

    #[test]
    fn live_pane_keeps_live_records_and_drops_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PaneRegistry::new(dir.path());
        let (a, b): (BoardKey, BoardKey) =
            ("tviles/1".parse().unwrap(), "tviles/2".parse().unwrap());
        reg.register(&a, "p1").unwrap();
        reg.register(&b, "p2").unwrap();
        let fake = FakeHerdr::default();
        fake.respond(&["pane", "get", "p1"], Ok(json!({})));
        fake.respond(&["pane", "get", "p2"], FakeHerdr::not_found());
        assert_eq!(reg.live_pane(&fake, &a).as_deref(), Some("p1"));
        assert_eq!(reg.live_pane(&fake, &b), None);
        assert_eq!(reg.lookup(&b), None);
    }

    #[test]
    fn rekey_moves_this_panes_record_to_the_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PaneRegistry::new(dir.path());
        let (key, uuid, other): (BoardKey, BoardKey, BoardKey) = (
            "linear:team:TBA".parse().unwrap(),
            "linear:team:1b4e28ba-2fa1-4d3b-a3f5-ef19b5a7633b"
                .parse()
                .unwrap(),
            "tviles/1".parse().unwrap(),
        );
        reg.register(&key, "p1").unwrap();
        reg.register(&other, "p2").unwrap();
        reg.rekey(&key, &uuid, "p1").unwrap();
        assert_eq!(reg.lookup(&uuid).as_deref(), Some("p1"));
        assert_eq!(reg.lookup(&key), None);
        assert_eq!(reg.lookup(&other).as_deref(), Some("p2"));
        // The old key now shown by another pane keeps that pane's record.
        reg.register(&key, "p3").unwrap();
        reg.rekey(&key, &uuid, "p1").unwrap();
        assert_eq!(reg.lookup(&key).as_deref(), Some("p3"));
    }

    #[test]
    fn unregister_ignores_a_record_owned_by_another_pane() {
        let dir = tempfile::tempdir().unwrap();
        let reg = PaneRegistry::new(dir.path());
        let a: BoardKey = "tviles/1".parse().unwrap();
        reg.register(&a, "p2").unwrap();
        reg.unregister(&a, "p1").unwrap();
        assert_eq!(reg.lookup(&a).as_deref(), Some("p2"));
    }
}
