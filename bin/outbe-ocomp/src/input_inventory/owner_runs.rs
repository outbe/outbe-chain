use super::*;

pub struct OwnerBatchReader {
    file: File,
    remaining: u64,
    previous: Option<Address>,
}

impl OwnerBatchReader {
    pub(super) fn open(path: PathBuf, expected_count: u64) -> Result<Self, TributeInventoryError> {
        let mut file = open_regular_readonly(&path)?;
        let count = read_run_header(&mut file, &path)?;
        if count != expected_count {
            return Err(TributeInventoryError::Corrupt("owner inventory count"));
        }
        Ok(Self {
            file,
            remaining: count,
            previous: None,
        })
    }

    pub fn next_batch(
        &mut self,
        max_owners: usize,
    ) -> Result<Option<Vec<Address>>, TributeInventoryError> {
        if max_owners == 0 {
            return Err(TributeInventoryError::InvalidWorkConfig);
        }
        if self.remaining == 0 {
            return Ok(None);
        }
        let take = usize::try_from(self.remaining.min(max_owners as u64))
            .map_err(|_| TributeInventoryError::IntegerOverflow)?;
        let mut owners = Vec::with_capacity(take);
        for _ in 0..take {
            let owner = read_owner(&mut self.file)?;
            if self.previous.is_some_and(|previous| previous >= owner) {
                return Err(TributeInventoryError::Corrupt("owner inventory order"));
            }
            self.previous = Some(owner);
            self.remaining -= 1;
            owners.push(owner);
        }
        Ok(Some(owners))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct HeapOwner {
    owner: Address,
    reader_index: usize,
}

impl Ord for HeapOwner {
    fn cmp(&self, other: &Self) -> Ordering {
        self.owner
            .cmp(&other.owner)
            .then_with(|| self.reader_index.cmp(&other.reader_index))
    }
}

impl PartialOrd for HeapOwner {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct OwnerRunReader {
    file: File,
    remaining: u64,
}

impl OwnerRunReader {
    fn open(path: PathBuf) -> Result<Self, TributeInventoryError> {
        let mut file = open_regular_readonly(&path)?;
        let remaining = read_run_header(&mut file, &path)?;
        Ok(Self { file, remaining })
    }

    fn next(&mut self) -> Result<Option<Address>, TributeInventoryError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        read_owner(&mut self.file).map(Some)
    }
}

pub(super) struct OwnerRunWriter {
    path: PathBuf,
    file: File,
    count: u64,
}

impl OwnerRunWriter {
    pub(super) fn create(path: PathBuf) -> Result<Self, TributeInventoryError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&path)
            .map_err(|source| io_error("create owner run", &path, source))?;
        file.write_all(&RUN_MAGIC)
            .and_then(|()| file.write_all(&0_u64.to_be_bytes()))
            .map_err(|source| io_error("write owner run header", &path, source))?;
        Ok(Self {
            path,
            file,
            count: 0,
        })
    }

    pub(super) fn write(&mut self, owner: Address) -> Result<(), TributeInventoryError> {
        self.file
            .write_all(owner.as_slice())
            .map_err(|source| io_error("write owner run", &self.path, source))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<u64, TributeInventoryError> {
        self.file
            .seek(SeekFrom::Start(8))
            .and_then(|_| self.file.write_all(&self.count.to_be_bytes()))
            .and_then(|()| self.file.sync_all())
            .map_err(|source| io_error("finish owner run", &self.path, source))?;
        Ok(self.count)
    }
}

pub(super) struct OwnerRunGroup {
    pub(super) pass: u32,
    pub(super) range: std::ops::Range<u64>,
}

pub(super) struct OwnerRunPosition {
    pub(super) pass: u32,
    pub(super) index: u64,
}

pub(super) fn merge_run_group(
    root: &Path,
    input: OwnerRunGroup,
    output: OwnerRunPosition,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let OwnerRunGroup {
        pass: input_pass,
        range,
    } = input;
    let start = range.start;
    let end = range.end;
    let mut readers = Vec::with_capacity(
        usize::try_from(end - start).map_err(|_| TributeInventoryError::IntegerOverflow)?,
    );
    for index in start..end {
        readers.push(OwnerRunReader::open(run_path(root, input_pass, index))?);
    }
    let mut heap = BinaryHeap::new();
    for (reader_index, reader) in readers.iter_mut().enumerate() {
        if let Some(owner) = reader.next()? {
            heap.push(Reverse(HeapOwner {
                owner,
                reader_index,
            }));
        }
    }
    let mut writer = OwnerRunWriter::create(run_path(root, output.pass, output.index))?;
    let mut previous = None;
    let mut records_since_progress = 0_u64;
    while let Some(Reverse(item)) = heap.pop() {
        if previous != Some(item.owner) {
            writer.write(item.owner)?;
            previous = Some(item.owner);
        }
        if let Some(owner) = readers[item.reader_index].next()? {
            heap.push(Reverse(HeapOwner {
                owner,
                reader_index: item.reader_index,
            }));
        }
        records_since_progress = records_since_progress.saturating_add(1);
        if records_since_progress == INVENTORY_PROGRESS_RECORD_HEARTBEAT {
            on_progress();
            records_since_progress = 0;
        }
    }
    writer.finish()?;
    Ok(())
}

pub(super) fn install_owner_file(
    final_run: Option<&Path>,
    destination: &Path,
    on_progress: &impl Fn(),
) -> Result<u64, TributeInventoryError> {
    if let Some(source) = final_run {
        let mut input = open_regular_readonly(source)?;
        let count = read_run_header(&mut input, source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(destination)
            .map_err(|source| io_error("create owner inventory", destination, source))?;
        output
            .write_all(&RUN_MAGIC)
            .and_then(|()| output.write_all(&count.to_be_bytes()))
            .map_err(|source| io_error("write owner inventory header", destination, source))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|source| io_error("read owner inventory", destination, source))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|source| io_error("copy owner inventory", destination, source))?;
            on_progress();
        }
        output
            .sync_all()
            .map_err(|source| io_error("fsync owner inventory", destination, source))?;
        Ok(count)
    } else {
        OwnerRunWriter::create(destination.to_path_buf())?.finish()
    }
}

pub(super) fn verify_owner_file(
    path: &Path,
    expected_count: u64,
    on_progress: &impl Fn(),
) -> Result<(), TributeInventoryError> {
    let mut reader = OwnerRunReader::open(path.to_path_buf())?;
    let mut count = 0_u64;
    let mut previous = None;
    let mut records_since_progress = 0_u64;
    while let Some(owner) = reader.next()? {
        if previous.is_some_and(|candidate| candidate >= owner) {
            return Err(TributeInventoryError::Corrupt("owner inventory order"));
        }
        previous = Some(owner);
        count = count
            .checked_add(1)
            .ok_or(TributeInventoryError::IntegerOverflow)?;
        records_since_progress = records_since_progress.saturating_add(1);
        if records_since_progress == INVENTORY_PROGRESS_RECORD_HEARTBEAT {
            on_progress();
            records_since_progress = 0;
        }
    }
    if count != expected_count {
        return Err(TributeInventoryError::Corrupt("owner inventory count"));
    }
    Ok(())
}
