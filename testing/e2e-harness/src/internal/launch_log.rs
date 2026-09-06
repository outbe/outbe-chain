//! A bounded observation of one process incarnation in an append-only log.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use eyre::{ensure, Result, WrapErr as _};

#[derive(Debug)]
pub(crate) struct LaunchLog {
    path: PathBuf,
    file: File,
    start: u64,
    observed_end: u64,
    end: Option<u64>,
}

impl LaunchLog {
    /// Capture before spawn, never after a process has started writing.
    pub(crate) fn arm(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Self::capture(path, file)
    }

    /// Observe a new phase of an already running process. Unlike `arm`, this
    /// must not create a missing log or accept messages written before the phase.
    pub(crate) fn checkpoint(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .wrap_err_with(|| format!("open required phase log {}", path.display()))?;
        Self::capture(path, file)
    }

    fn capture(path: &Path, file: File) -> Result<Self> {
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "log is not a regular file: {}",
            path.display()
        );
        let start = metadata.len();
        Ok(Self {
            path: path.to_owned(),
            file,
            start,
            observed_end: start,
            end: None,
        })
    }

    /// Freeze before a replacement process is allowed to append to this file.
    pub(crate) fn seal(&mut self) -> Result<()> {
        self.read()?;
        if self.end.is_none() {
            self.end = Some(self.observed_end);
        }
        Ok(())
    }

    pub(crate) fn start_offset(&self) -> u64 {
        self.start
    }

    pub(crate) fn read(&mut self) -> Result<String> {
        let original = self.file.metadata()?;
        let current = fs::metadata(&self.path)
            .wrap_err_with(|| format!("read required launch log {}", self.path.display()))?;
        ensure!(
            original.dev() == current.dev() && original.ino() == current.ino(),
            "launch log was replaced: {}",
            self.path.display()
        );
        let end = self.end.unwrap_or(original.len());
        ensure!(
            original.len() >= self.observed_end && original.len() >= end && end >= self.start,
            "launch log was truncated: {}",
            self.path.display()
        );
        self.observed_end = original.len();
        self.file.seek(SeekFrom::Start(self.start))?;
        let mut text = String::new();
        self.file
            .by_ref()
            .take(end - self.start)
            .read_to_string(&mut text)?;
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn phase_log_requires_an_existing_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.log");
        assert!(LaunchLog::checkpoint(&path).is_err());
        assert!(!path.exists());
        assert!(LaunchLog::checkpoint(dir.path()).is_err());
        fs::write(&path, "").unwrap();
        assert_eq!(LaunchLog::checkpoint(&path).unwrap().read().unwrap(), "");
    }

    #[test]
    fn fifo_logs_are_rejected_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fifo.log");
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        assert!(LaunchLog::checkpoint(&path)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
        assert!(LaunchLog::arm(&path)
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }

    #[test]
    fn phase_log_excludes_earlier_messages_and_rejects_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        fs::write(&path, "old phase success\n").unwrap();
        let mut log = LaunchLog::checkpoint(&path).unwrap();
        assert_eq!(log.read().unwrap(), "");
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(writer, "new phase").unwrap();
        assert_eq!(log.read().unwrap(), "new phase\n");
        log.seal().unwrap();
        writeln!(writer, "later phase").unwrap();
        assert_eq!(log.read().unwrap(), "new phase\n");
        fs::rename(&path, dir.path().join("old.log")).unwrap();
        fs::write(&path, "old phase success\nnew phase\nlater phase\n").unwrap();
        assert!(log.read().is_err());
    }

    #[test]
    fn launch_log_excludes_previous_and_subsequent_processes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        fs::write(&path, "old\n").unwrap();
        let mut log = LaunchLog::arm(&path).unwrap();
        let mut writer = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(writer, "current").unwrap();
        log.seal().unwrap();
        writeln!(writer, "replacement").unwrap();
        log.seal().unwrap();
        assert_eq!(log.read().unwrap(), "current\n");
    }

    #[test]
    fn launch_log_rejects_shrink_after_reading_from_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        let mut log = LaunchLog::arm(&path).unwrap();
        fs::write(&path, "current process\n").unwrap();
        assert_eq!(log.read().unwrap(), "current process\n");
        fs::write(&path, "short\n").unwrap();
        assert!(log.read().is_err());
    }

    #[test]
    fn launch_log_rejects_missing_replaced_and_truncated_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.log");
        fs::write(&path, "previous process\n").unwrap();
        let mut log = LaunchLog::arm(&path).unwrap();
        fs::write(&path, "").unwrap();
        assert!(log.read().is_err());
        fs::remove_file(&path).unwrap();
        assert!(log.read().is_err());
        fs::write(&path, "different inode with enough bytes\n").unwrap();
        assert!(log.read().is_err());
    }
}
