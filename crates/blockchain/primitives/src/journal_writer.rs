use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter},
    path::Path,
    sync::Mutex,
};

pub(super) struct Journal {
    pub(super) writer: Mutex<BufWriter<File>>,
}

impl Journal {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let writer = BufWriter::new(file);
        Ok(Self {
            writer: Mutex::new(writer),
        })
    }
}

/// Runs one fresh test process that reopens a journal and appends one record.
///
/// `test_name` is the exact test filter. Sets that journal's directory and
/// block variables in the child's inherited environment.
#[cfg(test)]
pub(super) fn append_record_in_reopened_child(
    test_name: &str,
    dir_env: &str,
    block_env: &str,
    directory: &Path,
    block_number: &str,
) {
    let exe = std::env::current_exe().expect("current test binary");
    let output = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(dir_env, directory)
        .env(block_env, block_number)
        .output()
        .expect("spawn child process");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn reopening_preserves_records_and_creates_parent_directory() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("nested/journal.jsonl");
        for record in ["old record", "new record"] {
            let journal = Journal::open(&path)?;
            let mut writer = journal.writer.lock().expect("fresh fixture mutex");
            writeln!(writer, "{record}")?;
            writer.flush()?;
        }
        assert_eq!(std::fs::read_to_string(path)?, "old record\nnew record\n");
        Ok(())
    }
}
