//! Filesystem checks that durable-state code shares.

use std::io;
use std::path::Path;

/// Returns `true` when a directory entry exists at `path`.
///
/// The check does not follow a final symlink, so a dangling symlink also
/// counts. `NotFound` gives `false`. The function returns every other error,
/// for example a permission error, so callers fail closed.
pub fn entry_exists(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::entry_exists;

    /// A missing entry is `false`. A file and a dangling symlink are `true`. A
    /// path through a regular file is an error other than `NotFound`.
    #[test]
    fn reports_entries_without_following_symlinks() -> std::io::Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("file");
        std::fs::write(&file, b"x")?;
        let link = directory.path().join("dangling");
        std::os::unix::fs::symlink(directory.path().join("absent"), &link)?;
        assert!(!entry_exists(&directory.path().join("missing"))?);
        assert!(entry_exists(&file)?);
        assert!(entry_exists(&link)?);
        assert!(matches!(
            entry_exists(&file.join("child")),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound
        ));
        Ok(())
    }
}
