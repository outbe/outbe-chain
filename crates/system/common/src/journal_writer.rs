use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    path::Path,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
};

const CAPACITY: usize = 1024;

struct JournalQueue {
    queue: VecDeque<String>,
    pending_sync: u64,
    completed_sync: u64,
    closed: bool,
}

struct Shared {
    state: Mutex<JournalQueue>,
    cv: Condvar,
}

pub(super) struct Journal {
    shared: Arc<Shared>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

fn lock_queue(mutex: &Mutex<JournalQueue>) -> MutexGuard<'_, JournalQueue> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Journal {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_writer(path, true)
    }

    fn open_with_writer(path: &Path, spawn_writer: bool) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(JournalQueue {
                queue: VecDeque::new(),
                pending_sync: 0,
                completed_sync: 0,
                closed: false,
            }),
            cv: Condvar::new(),
        });
        let writer = if spawn_writer {
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            let shared_for_thread = Arc::clone(&shared);
            let handle = std::thread::Builder::new()
                .name("outbe-slashing-journal".into())
                .spawn(move || writer_loop(shared_for_thread, BufWriter::new(file)))?;
            Some(handle)
        } else {
            None
        };
        Ok(Self {
            shared,
            writer: Mutex::new(writer),
        })
    }

    /// Queue one line. Disk I/O happens on the background writer.
    ///
    /// When the queue is already at [`CAPACITY`], the oldest line is dropped
    /// before the new line is pushed.
    pub(super) fn enqueue(&self, line: String) {
        let mut queue = lock_queue(&self.shared.state);
        if queue.queue.len() >= CAPACITY {
            queue.queue.pop_front();
            tracing::warn!(
                target: "outbe::slashing::journal",
                capacity = CAPACITY,
                "slashing journal queue is full; dropped the oldest queued record",
            );
        }
        queue.queue.push_back(line);
        self.shared.cv.notify_one();
    }

    /// Wait until every line queued so far has been written and flushed.
    pub(super) fn flush(&self) {
        let mut queue = lock_queue(&self.shared.state);
        queue.pending_sync = queue.pending_sync.wrapping_add(1);
        let target = queue.pending_sync;
        self.shared.cv.notify_one();
        while queue.completed_sync < target {
            queue = self
                .shared
                .cv
                .wait(queue)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        {
            let mut queue = lock_queue(&self.shared.state);
            queue.closed = true;
            self.shared.cv.notify_one();
        }
        let handle = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

fn writer_loop(shared: Arc<Shared>, mut writer: BufWriter<File>) {
    loop {
        if !wait_for_work(&shared) {
            acknowledge_sync(&shared, &mut writer);
            break;
        }
        write_queued_lines(&shared, &mut writer);
        if sync_if_idle(&shared, &mut writer) {
            break;
        }
    }
}

fn wait_for_work(shared: &Shared) -> bool {
    let mut queue = lock_queue(&shared.state);
    while queue.queue.is_empty() && queue.completed_sync == queue.pending_sync && !queue.closed {
        queue = shared
            .cv
            .wait(queue)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    !(queue.closed && queue.queue.is_empty())
}

fn write_queued_lines(shared: &Shared, writer: &mut BufWriter<File>) {
    let lines = {
        let mut queue = lock_queue(&shared.state);
        queue.queue.drain(..).collect::<Vec<_>>()
    };
    for line in lines {
        if let Err(error) = writeln!(writer, "{line}") {
            tracing::warn!(
                target: "outbe::slashing::journal",
                %error,
                "failed to append journal record",
            );
        }
    }
}

fn sync_if_idle(shared: &Shared, writer: &mut BufWriter<File>) -> bool {
    let idle = lock_queue(&shared.state).queue.is_empty();
    if !idle {
        return false;
    }
    acknowledge_sync(shared, writer);
    let queue = lock_queue(&shared.state);
    queue.closed && queue.queue.is_empty()
}

fn acknowledge_sync(shared: &Shared, writer: &mut BufWriter<File>) {
    if let Err(error) = writer.flush() {
        tracing::warn!(
            target: "outbe::slashing::journal",
            %error,
            "failed to flush journal record",
        );
    }
    let mut queue = lock_queue(&shared.state);
    if queue.queue.is_empty() {
        queue.completed_sync = queue.pending_sync;
        drop(queue);
        shared.cv.notify_all();
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
            let journal = Journal::open(&path)?;
            journal.enqueue(record.to_string());
            journal.flush();
        }
        assert_eq!(std::fs::read_to_string(path)?, "old record\nnew record\n");
        Ok(())
    }

    #[test]
    fn enqueue_drops_the_oldest_record_when_capacity_is_exceeded() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("journal.jsonl");
        let journal = Journal::open_with_writer(&path, false)?;
        let overflow = CAPACITY + 1;
        for index in 0..overflow {
            journal.enqueue(format!("line-{index}"));
        }
        let queue = lock_queue(&journal.shared.state);
        let newest = format!("line-{}", overflow - 1);
        assert_eq!(queue.queue.len(), CAPACITY);
        assert_eq!(queue.queue.front().map(String::as_str), Some("line-1"));
        assert_eq!(
            queue.queue.back().map(String::as_str),
            Some(newest.as_str())
        );
        assert!(queue.queue.iter().all(|line| line != "line-0"));
        Ok(())
    }
}
