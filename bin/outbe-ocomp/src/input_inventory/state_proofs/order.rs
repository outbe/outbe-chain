//! External ordering of fixed-size source identities with bounded merge fan-in.

use super::*;

const KEYS_PER_RUN: usize = 4_096;
const MERGE_FAN_IN: usize = 16;

struct Keys {
    file: File,
    remaining: u64,
}

impl Keys {
    fn open(path: &Path) -> Result<Self, TributeInventoryError> {
        let file = open_regular_readonly(path)?;
        let bytes = file
            .metadata()
            .map_err(|e| io_error("stat source key run", path, e))?
            .len();
        if bytes % 32 != 0 {
            return Err(TributeInventoryError::Corrupt("source key run length"));
        }
        Ok(Self {
            file,
            remaining: bytes / 32,
        })
    }

    fn next(&mut self) -> Result<Option<B256>, TributeInventoryError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut bytes = [0; 32];
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| io_error("read source key run", Path::new(STATE_INDEX), e))?;
        self.remaining -= 1;
        Ok(Some(B256::from(bytes)))
    }
}

pub(super) fn sort_index(
    build: &Path,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    sort_with_bounds(build, KEYS_PER_RUN, MERGE_FAN_IN, on_progress)
}

fn sort_with_bounds(
    build: &Path,
    keys_per_run: usize,
    fan_in: usize,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let work = build.join("key-order");
    create_private_directory(&work)?;
    let mut input = Keys::open(&build.join(STATE_INDEX))?;
    let mut runs = 0_u64;
    while input.remaining != 0 {
        let mut keys = Vec::with_capacity(keys_per_run);
        for _ in 0..keys_per_run {
            let Some(key) = input.next()? else { break };
            keys.push(key);
        }
        keys.sort_unstable();
        if keys.windows(2).any(|keys| keys[0] == keys[1]) {
            return Err(TributeInventoryError::Authority(
                "duplicate source identity",
            ));
        }
        let path = run_path(&work, 0, runs);
        let mut file = create(&path)?;
        for key in keys {
            file.write_all(key.as_slice())
                .map_err(|e| io_error("write source key run", &path, e))?;
        }
        file.sync_all()
            .map_err(|e| io_error("fsync source key run", &path, e))?;
        runs += 1;
        on_progress();
    }
    if runs == 0 {
        persist_new(&build.join(CANONICAL_INDEX), &[])?;
        return Ok(());
    }
    let mut pass = 0_u32;
    while runs > 1 {
        let mut merged = 0;
        let mut first = 0;
        while first < runs {
            let count = (runs - first).min(fan_in as u64);
            merge(&work, pass, first, count, merged, on_progress)?;
            first += count;
            merged += 1;
        }
        runs = merged;
        pass = pass
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
    }
    fs::rename(run_path(&work, pass, 0), build.join(CANONICAL_INDEX))
        .map_err(|e| io_error("install canonical source index", build, e))?;
    Ok(())
}

fn merge(
    work: &Path,
    pass: u32,
    first: u64,
    count: u64,
    output: u64,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let mut readers = (first..first + count)
        .map(|run| Keys::open(&run_path(work, pass, run)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::new();
    for (index, reader) in readers.iter_mut().enumerate() {
        if let Some(key) = reader.next()? {
            heap.push(Reverse((key, index)));
        }
    }
    let path = run_path(work, pass + 1, output);
    let mut file = create(&path)?;
    let mut previous = None;
    let mut since_progress = 0;
    while let Some(Reverse((key, index))) = heap.pop() {
        if previous.is_some_and(|old| old >= key) {
            return Err(TributeInventoryError::Authority("source key merge order"));
        }
        file.write_all(key.as_slice())
            .map_err(|e| io_error("write source key merge", &path, e))?;
        previous = Some(key);
        if let Some(next) = readers[index].next()? {
            heap.push(Reverse((next, index)));
        }
        since_progress += 1;
        if since_progress == KEYS_PER_RUN {
            on_progress();
            since_progress = 0;
        }
    }
    file.sync_all()
        .map_err(|e| io_error("fsync source key merge", &path, e))?;
    drop(readers);
    for run in first..first + count {
        let path = run_path(work, pass, run);
        fs::remove_file(&path).map_err(|e| io_error("remove merged source key run", &path, e))?;
    }
    on_progress();
    Ok(())
}

fn run_path(root: &Path, pass: u32, run: u64) -> PathBuf {
    root.join(format!("{pass}-{run}"))
}

fn create(path: &Path) -> Result<File, TributeInventoryError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
        .map_err(|e| io_error("create source key run", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_bounded_merge_passes_preserve_every_identity_in_canonical_order() {
        let root = tempfile::tempdir().unwrap();
        let mut file = create(&root.path().join(STATE_INDEX)).unwrap();
        for value in (1..=71_u64).rev() {
            file.write_all(B256::from(U256::from(value).to_be_bytes::<32>()).as_slice())
                .unwrap();
        }
        drop(file);
        sort_with_bounds(root.path(), 3, 2, &|| {}).unwrap();
        let mut keys = Keys::open(&root.path().join(CANONICAL_INDEX)).unwrap();
        for value in 1..=71_u64 {
            assert_eq!(
                keys.next().unwrap(),
                Some(B256::from(U256::from(value).to_be_bytes::<32>()))
            );
        }
        assert!(keys.next().unwrap().is_none());
    }

    #[test]
    fn duplicate_identity_across_separate_runs_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut file = create(&root.path().join(STATE_INDEX)).unwrap();
        for value in [1_u64, 2, 3, 4, 1] {
            file.write_all(B256::from(U256::from(value).to_be_bytes::<32>()).as_slice())
                .unwrap();
        }
        drop(file);
        assert!(sort_with_bounds(root.path(), 2, 2, &|| {}).is_err());
        assert!(!root.path().join(CANONICAL_INDEX).exists());
    }
}
