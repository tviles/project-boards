use crate::fsutil::write_atomic;
use crate::model::BoardKey;
use crate::store::snapshot::BoardSnapshot;
use std::path::{Path, PathBuf};

/// Bump when `BoardSnapshot`'s serialized shape changes; older caches are discarded.
/// 4: board keys as strings, `ContentRef.container` and `key`, option kinds, view filters.
pub const CACHE_VERSION: u32 = 4;

pub fn cache_path(state_dir: &Path, board: &BoardKey) -> PathBuf {
    state_dir
        .join("cache")
        .join(format!("{}.json", board.key()))
}

/// The cached snapshot, or `None` when missing, unreadable or from another cache version.
pub fn load_cache(path: &Path) -> Option<BoardSnapshot> {
    let text = std::fs::read_to_string(path).ok()?;
    // Older versions have other shapes; judge the version before parsing the rest.
    if let Some(found) = cache_version(&text)
        && found != CACHE_VERSION
    {
        tracing::info!(
            found,
            expected = CACHE_VERSION,
            "discarding cache from another version"
        );
        return None;
    }
    match serde_json::from_str::<BoardSnapshot>(&text) {
        Ok(s) => Some(s),
        Err(_) => {
            // The parse error can quote cached text, so it is not logged.
            tracing::warn!(path = %path.display(), "discarding unreadable cache");
            None
        }
    }
}

fn cache_version(text: &str) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct Version {
        version: u32,
    }
    serde_json::from_str::<Version>(text)
        .ok()
        .map(|v| v.version)
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
    fn a_version_3_cache_in_the_old_shape_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(dir.path(), &"acme/3".parse().unwrap());
        // A 0.3 file: board as a map, `repo` instead of `container`, string view filters.
        let old = r#"{
            "version": 3,
            "project": {
                "id": "P1",
                "title": "Example",
                "board": {"owner": "acme", "number": 3},
                "views": [{"id": "V1", "name": "Open", "filter": "is:open"}]
            },
            "items": [{"id": "I1", "number": 1, "title": "Example", "repo": "acme/app"}]
        }"#;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, old).unwrap();
        assert_eq!(load_cache(&path), None);
        assert_eq!(CACHE_VERSION, 4);
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
