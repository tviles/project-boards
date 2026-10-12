use crate::fsutil::write_atomic;
use crate::model::BoardRef;
use crate::store::snapshot::BoardSnapshot;
use std::path::{Path, PathBuf};

/// Bump when `BoardSnapshot`'s serialized shape changes; older caches are discarded.
pub const CACHE_VERSION: u32 = 2;

pub fn cache_path(state_dir: &Path, board: &BoardRef) -> PathBuf {
    state_dir
        .join("cache")
        .join(format!("{}.json", board.key()))
}

/// The cached snapshot, or `None` when missing, unreadable or from another cache version.
pub fn load_cache(path: &Path) -> Option<BoardSnapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<BoardSnapshot>(&text) {
        Ok(s) if s.version == CACHE_VERSION => Some(s),
        Ok(s) => {
            tracing::info!(
                found = s.version,
                expected = CACHE_VERSION,
                "discarding cache from another version"
            );
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "discarding unreadable cache");
            None
        }
    }
}

pub fn save_cache(path: &Path, snapshot: &BoardSnapshot) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(snapshot).map_err(std::io::Error::other)?;
    write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::item::tests::issue;
    use crate::store::snapshot::tests::project;

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(dir.path(), &"tviles/3".parse().unwrap());
        let mut s = BoardSnapshot::new(project());
        s.upsert_items(vec![issue("a", 1, "Emoji 🚀")]);
        save_cache(&path, &s).unwrap();
        assert_eq!(load_cache(&path), Some(s));
        assert!(path.ends_with("cache/tviles__3.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the cache holds private board content");
        }
    }

    #[test]
    fn other_versions_and_garbage_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let mut s = BoardSnapshot::new(project());
        s.version = CACHE_VERSION + 1;
        save_cache(&path, &s).unwrap();
        assert_eq!(load_cache(&path), None);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_cache(&path), None);
        assert_eq!(load_cache(&dir.path().join("missing.json")), None);
    }
}
