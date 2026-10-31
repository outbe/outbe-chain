//! Off-thread append-only JSONL writer for best-effort node journals.
//!
//! Callers on the block execution path only push a serialized line into a
//! bounded in-memory queue. A dedicated writer thread owns the file. It drains
//! the queue in batches and flushes once each time the queue becomes empty.
//! When the queue is full, the writer drops the oldest line, counts it, and
//! logs a warning.

use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write as _},
    path::Path,
    sync::{Arc, Condvar, Mutex, MutexGuard},
};

/// Maximum number of lines that wait for the writer thread.
pub(super) const JOURNAL_QUEUE_CAPACITY: usize = 1024;

#[derive(Default)]
struct QueueState {
    lines: VecDeque<String>,
    /// True while the writer thread holds a batch that it has not flushed.
    writing: bool,
    /// Lines dropped because the queue was full.
    dropped: u64,
    /// True after the [`Journal`] handle dropped. The writer thread then
    /// writes the remaining lines and exits.
    closed: bool,
}

impl QueueState {
    /// Appends `line`. When the queue already holds `capacity` lines, drops
    /// the oldest line first and returns `true`.
    fn push(&mut self, line: String, capacity: usize) -> bool {
        let full = self.lines.len() >= capacity;
        if full {
            self.lines.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.lines.push_back(line);
        full
    }
}

#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    /// Signals the writer thread that lines are available.
    available: Condvar,
    /// Signals waiters that the queue is empty and the last batch is flushed.
    idle: Condvar,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        // A panic while the lock is held cannot corrupt the queue contents.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Handle to one journal file and its writer thread.
pub(super) struct Journal {
    queue: Arc<Queue>,
    target: &'static str,
    capacity: usize,
}

impl Journal {
    /// Opens `path` in append mode and starts its writer thread.
    ///
    /// The function creates the parent directory if it is missing.
    pub(super) fn open(path: &Path, target: &'static str) -> io::Result<Self> {
        Self::open_with_capacity(path, target, JOURNAL_QUEUE_CAPACITY)
    }

    fn open_with_capacity(path: &Path, target: &'static str, capacity: usize) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let queue = Arc::new(Queue::default());
        let writer_queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("outbe-journal-writer".into())
            .spawn(move || write_lines(&writer_queue, BufWriter::new(file), target))?;
        Ok(Self {
            queue,
            target,
            capacity,
        })
    }

    /// Queues one line for the writer thread. This function does no file I/O.
    pub(super) fn submit(&self, line: String) {
        let mut state = self.queue.lock();
        if state.push(line, self.capacity) {
            tracing::warn!(
                target: "outbe::journal",
                journal = self.target,
                dropped = state.dropped,
                "journal queue is full - dropped the oldest record",
            );
        }
        drop(state);
        self.queue.available.notify_one();
    }

    /// Returns the number of lines dropped because the queue was full.
    #[cfg(test)]
    pub(super) fn dropped(&self) -> u64 {
        self.queue.lock().dropped
    }

    /// Blocks until the writer thread wrote and flushed every queued line.
    #[cfg(test)]
    pub(super) fn wait_idle(&self) {
        let mut state = self.queue.lock();
        while state.writing || !state.lines.is_empty() {
            state = self
                .queue
                .idle
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.queue.lock().closed = true;
        self.queue.available.notify_one();
    }
}

fn write_lines(queue: &Queue, mut writer: BufWriter<File>, target: &'static str) {
    loop {
        let batch: Vec<String> = {
            let mut state = queue.lock();
            while state.lines.is_empty() {
                state.writing = false;
                queue.idle.notify_all();
                if state.closed {
                    return;
                }
                state = queue
                    .available
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            state.writing = true;
            state.lines.drain(..).collect()
        };
        for line in &batch {
            if let Err(error) = writeln!(writer, "{line}") {
                tracing::warn!(
                    target: "outbe::journal",
                    journal = target,
                    %error,
                    "failed to append journal record",
                );
            }
        }
        if queue.lock().lines.is_empty() {
            if let Err(error) = writer.flush() {
                tracing::warn!(
                    target: "outbe::journal",
                    journal = target,
                    %error,
                    "failed to flush journal records",
                );
            }
        }
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

    #[test]
    fn reopening_preserves_records_and_creates_parent_directory() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("nested/journal.jsonl");
        for record in ["old record", "new record"] {
            let journal = Journal::open(&path, "test")?;
            journal.submit(record.into());
            journal.wait_idle();
        }
        assert_eq!(std::fs::read_to_string(path)?, "old record\nnew record\n");
        Ok(())
    }

    #[test]
    fn full_queue_drops_the_oldest_record() {
        let mut state = QueueState::default();
        assert!(!state.push("first".into(), 2));
        assert!(!state.push("second".into(), 2));
        assert!(state.push("third".into(), 2));
        assert_eq!(state.lines, ["second", "third"]);
        assert_eq!(state.dropped, 1);
    }

    #[test]
    fn submit_writes_every_queued_line_in_order() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("journal.jsonl");
        let journal = Journal::open_with_capacity(&path, "test", JOURNAL_QUEUE_CAPACITY)?;
        for index in 0..100 {
            journal.submit(format!("line {index}"));
        }
        journal.wait_idle();
        let expected: String = (0..100).map(|index| format!("line {index}\n")).collect();
        assert_eq!(std::fs::read_to_string(path)?, expected);
        assert_eq!(journal.dropped(), 0);
        Ok(())
    }
}
