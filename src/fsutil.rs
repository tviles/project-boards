use std::io::Write;
use std::path::Path;

/// Options that create a file readable and writable by its owner only (0600 on unix). The
/// cache, state and log files hold private board content.
fn private_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Writes `bytes` to `path` atomically: a temporary file (mode 0600) in the same directory,
/// fsynced, then renamed over the target. Creates parent directories.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!("{name}.tmp.{}", std::process::id()));
    {
        let mut file = private_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Creates `path` with mode 0600 if missing, and narrows an existing file to 0600, without
/// touching its contents. For files another library opens, such as the log.
pub fn ensure_private_file(path: &Path) -> std::io::Result<()> {
    private_options().append(true).create(true).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn replaces_contents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.json");
        super::write_atomic(&path, b"one").unwrap();
        super::write_atomic(&path, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1);
    }

    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn written_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache/board.json");
        super::write_atomic(&path, b"{}").unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_file_creates_or_narrows_and_keeps_contents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new.log");
        super::ensure_private_file(&new).unwrap();
        assert_eq!(mode(&new), 0o600);
        let old = dir.path().join("old.log");
        std::fs::write(&old, "kept").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::ensure_private_file(&old).unwrap();
        assert_eq!(mode(&old), 0o600);
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "kept");
    }
}
