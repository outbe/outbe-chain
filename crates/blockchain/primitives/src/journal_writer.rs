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
