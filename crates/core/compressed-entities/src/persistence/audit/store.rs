//! Native CKB storage confined to a caller-owned scratch transaction.

use alloy_primitives::B256;
use outbe_sparse_merkle_tree_v061::{
    error::Error as CkbError,
    merge::MergeValue as CkbMergeValue,
    traits::{StoreReadOps, StoreWriteOps},
    BranchKey as CkbBranchKey, BranchNode as CkbBranchNode, H256,
};
use reth_db::transaction::{DbTx, DbTxMut};

use crate::persistence::{
    prefixed_key, tables, BranchKey, BranchNode, FieldValue, LeafValue, MergeValue,
    PersistenceError, TreeKey, TreeNamespace,
};

/// Borrow an externally created scratch transaction. The adapter neither opens
/// a database nor commits it, and never receives the read-only source handle.
pub(super) struct ScratchTreeStore<'a, T: DbTx + DbTxMut> {
    tx: &'a T,
    namespace: TreeNamespace,
}

impl<'a, T: DbTx + DbTxMut> ScratchTreeStore<'a, T> {
    pub(super) const fn new(tx: &'a T, namespace: TreeNamespace) -> Self {
        Self { tx, namespace }
    }
}

impl<T: DbTx + DbTxMut> StoreReadOps<H256> for ScratchTreeStore<'_, T> {
    fn get_branch(&self, key: &CkbBranchKey) -> Result<Option<CkbBranchNode>, CkbError> {
        let key = branch_key(key).map_err(store_error)?;
        self.tx
            .get::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()))
            .map_err(store_error)?
            .map(|bytes| {
                BranchNode::decode(&bytes)
                    .map(to_ckb_branch)
                    .map_err(store_error)
            })
            .transpose()
    }

    fn get_leaf(&self, key: &H256) -> Result<Option<H256>, CkbError> {
        let key = tree_key(*key).map_err(store_error)?;
        self.tx
            .get::<tables::CeLeaves>(prefixed_key(self.namespace, &key.encode()))
            .map_err(store_error)?
            .map(|bytes| {
                LeafValue::decode(&bytes)
                    .map(|leaf| H256::from(leaf.encode()))
                    .map_err(store_error)
            })
            .transpose()
    }
}

impl<T: DbTx + DbTxMut> StoreWriteOps<H256> for ScratchTreeStore<'_, T> {
    fn insert_branch(&mut self, key: CkbBranchKey, branch: CkbBranchNode) -> Result<(), CkbError> {
        let key = branch_key(&key).map_err(store_error)?;
        let node = BranchNode {
            left: from_ckb_merge(&branch.left).map_err(store_error)?,
            right: from_ckb_merge(&branch.right).map_err(store_error)?,
        };
        self.tx
            .put::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()), node.encode())
            .map_err(store_error)
    }

    fn insert_leaf(&mut self, key: H256, leaf: H256) -> Result<(), CkbError> {
        let key = tree_key(key).map_err(store_error)?;
        let leaf = LeafValue::try_from(B256::from(<[u8; 32]>::from(leaf))).map_err(store_error)?;
        self.tx
            .put::<tables::CeLeaves>(
                prefixed_key(self.namespace, &key.encode()),
                leaf.encode().to_vec(),
            )
            .map_err(store_error)
    }

    fn remove_branch(&mut self, key: &CkbBranchKey) -> Result<(), CkbError> {
        let key = branch_key(key).map_err(store_error)?;
        self.tx
            .delete::<tables::CeBranches>(prefixed_key(self.namespace, &key.encode()), None)
            .map_err(store_error)?;
        Ok(())
    }

    fn remove_leaf(&mut self, key: &H256) -> Result<(), CkbError> {
        let key = tree_key(*key).map_err(store_error)?;
        self.tx
            .delete::<tables::CeLeaves>(prefixed_key(self.namespace, &key.encode()), None)
            .map_err(store_error)?;
        Ok(())
    }
}

fn tree_key(key: H256) -> Result<TreeKey, PersistenceError> {
    TreeKey::try_from(B256::from(<[u8; 32]>::from(key)))
}

fn branch_key(key: &CkbBranchKey) -> Result<BranchKey, PersistenceError> {
    BranchKey::new(key.height, B256::from(<[u8; 32]>::from(key.node_key)))
}

fn from_ckb_merge(value: &CkbMergeValue) -> Result<MergeValue, PersistenceError> {
    let field = |value: H256| FieldValue::try_from(B256::from(<[u8; 32]>::from(value)));
    match value {
        CkbMergeValue::Value(value) => Ok(MergeValue::Value(field(*value)?)),
        CkbMergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => Ok(MergeValue::MergeWithZero {
            base_node: field(*base_node)?,
            zero_bits: field(*zero_bits)?,
            zero_count: *zero_count,
        }),
    }
}

fn to_ckb_merge(value: MergeValue) -> CkbMergeValue {
    match value {
        MergeValue::Value(value) => CkbMergeValue::Value(H256::from(value.encode())),
        MergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => CkbMergeValue::MergeWithZero {
            base_node: H256::from(base_node.encode()),
            zero_bits: H256::from(zero_bits.encode()),
            zero_count,
        },
    }
}

fn to_ckb_branch(node: BranchNode) -> CkbBranchNode {
    CkbBranchNode {
        left: to_ckb_merge(node.left),
        right: to_ckb_merge(node.right),
    }
}

fn store_error(error: impl std::fmt::Display) -> CkbError {
    CkbError::Store(error.to_string())
}

use super::{CeAuditError, CeAuditWork};
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const RUN_MAGIC: [u8; 8] = *b"CEAUDT01";
const RUN_HEADER_BYTES: u64 = 32;
static NEXT_SORT_ID: AtomicU64 = AtomicU64::new(0);

/// Sort fixed-width records by an encoded identity prefix. Payload bytes are
/// compared only after identity. Every identity must occur exactly once.
/// Work owns all files, and both this builder and its output borrow that owner.
pub(super) struct RecordSorter<'a, const N: usize> {
    work: &'a CeAuditWork,
    directory: PathBuf,
    key_len: usize,
    buffer: Vec<[u8; N]>,
    run_count: u64,
    peak_buffered: usize,
    failed: bool,
}

impl<'a, const N: usize> RecordSorter<'a, N> {
    pub(super) fn new(work: &'a CeAuditWork, key_len: usize) -> Result<Self, CeAuditError> {
        let invalid_dimensions = N == 0 || key_len == 0 || key_len > N;
        let invalid_limits = work.limits.records_per_run == 0 || work.limits.merge_fan_in < 2;
        if invalid_dimensions || invalid_limits {
            return Err(sort_invalid("invalid sort dimensions or limits"));
        }
        // Reject impossible layouts before allocating or creating anything.
        checked_buffer_bytes(N, work.limits.records_per_run)?;
        let merge_entry_bytes = std::mem::size_of::<RunReader<N>>()
            .checked_add(std::mem::size_of::<Reverse<([u8; N], usize)>>())
            .ok_or_else(|| sort_invalid("sort merge layout overflow"))?;
        checked_buffer_bytes(merge_entry_bytes, work.limits.merge_fan_in)?;
        run_length::<N>(
            u64::try_from(work.limits.records_per_run)
                .map_err(|_| sort_invalid("sort run count overflow"))?,
        )?;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(work.limits.records_per_run)
            .map_err(|_| sort_invalid("cannot reserve sort buffer"))?;
        let id = NEXT_SORT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| sort_invalid("sort directory counter overflow"))?;
        let directory = work.root.join(format!("sort-{id:020}"));
        fs::create_dir(&directory)?;
        Ok(Self {
            work,
            directory,
            key_len,
            buffer,
            run_count: 0,
            peak_buffered: 0,
            failed: false,
        })
    }

    pub(super) fn peak_buffered_records(&self) -> usize {
        self.peak_buffered
    }

    pub(super) fn push(&mut self, record: [u8; N]) -> Result<(), CeAuditError> {
        if self.failed {
            return Err(sort_invalid("sorter already failed"));
        }
        self.buffer.push(record);
        self.peak_buffered = self.peak_buffered.max(self.buffer.len());
        if self.buffer.len() == self.work.limits.records_per_run {
            if let Err(error) = self.flush() {
                self.failed = true;
                return Err(error);
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), CeAuditError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        // Unstable sorting is in-place and does not allocate a second run buffer.
        self.buffer.sort_unstable();
        let next_count = self
            .run_count
            .checked_add(1)
            .ok_or_else(|| sort_invalid("sort run counter overflow"))?;
        let count =
            u64::try_from(self.buffer.len()).map_err(|_| sort_invalid("sort count overflow"))?;
        let mut writer = RunWriter::<N>::create(
            &run_path(&self.directory, 0, self.run_count),
            self.key_len,
            count,
        )?;
        for record in &self.buffer {
            writer.write_record(*record)?;
        }
        writer.finish()?;
        self.buffer.clear();
        self.run_count = next_count;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<SortedRecords<'a, N>, CeAuditError> {
        if self.failed {
            return Err(sort_invalid("sorter already failed"));
        }
        self.flush()?;
        // Release the input buffer before allocating merge readers and heads.
        self.buffer = Vec::new();
        let fan_in = u64::try_from(self.work.limits.merge_fan_in)
            .map_err(|_| sort_invalid("sort fan-in overflow"))?;
        let mut pass = 0_u64;
        while self.run_count > 1 {
            let next_pass = pass
                .checked_add(1)
                .ok_or_else(|| sort_invalid("sort pass overflow"))?;
            let mut start = 0_u64;
            let mut outputs = 0_u64;
            while start < self.run_count {
                let count = fan_in.min(self.run_count - start);
                let merge = RunMerge {
                    pass,
                    start,
                    count,
                    next_pass,
                    output: outputs,
                };
                merge_runs::<N>(&self.directory, merge, self.key_len)?;
                start = start
                    .checked_add(count)
                    .ok_or_else(|| sort_invalid("sort run counter overflow"))?;
                outputs = outputs
                    .checked_add(1)
                    .ok_or_else(|| sort_invalid("sort run counter overflow"))?;
            }
            self.run_count = outputs;
            pass = next_pass;
        }
        let reader = if self.run_count == 0 {
            None
        } else {
            Some(RunReader::open(
                &run_path(&self.directory, pass, 0),
                self.key_len,
            )?)
        };
        Ok(SortedRecords {
            _work: self.work,
            reader,
        })
    }
}

/// Streaming output. An I/O or integrity error terminates this iterator.
pub(super) struct SortedRecords<'a, const N: usize> {
    _work: &'a CeAuditWork,
    reader: Option<RunReader<N>>,
}

impl<const N: usize> Iterator for SortedRecords<'_, N> {
    type Item = Result<[u8; N], CeAuditError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.reader.as_mut()?.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.reader = None;
                None
            }
            Err(error) => {
                self.reader = None;
                Some(Err(error))
            }
        }
    }
}

impl<const N: usize> std::iter::FusedIterator for SortedRecords<'_, N> {}

fn checked_buffer_bytes(width: usize, count: usize) -> Result<(), CeAuditError> {
    let bytes = width
        .checked_mul(count)
        .ok_or_else(|| sort_invalid("sort buffer size overflow"))?;
    if bytes > isize::MAX as usize {
        return Err(sort_invalid("sort buffer exceeds addressable memory"));
    }
    Ok(())
}

fn sort_invalid(message: &str) -> CeAuditError {
    CeAuditError::Invalid(message.into())
}

fn run_path(directory: &Path, pass: u64, index: u64) -> PathBuf {
    directory.join(format!("run-{pass:020}-{index:020}.bin"))
}

fn run_length<const N: usize>(count: u64) -> Result<u64, CeAuditError> {
    u64::try_from(N)
        .ok()
        .and_then(|width| width.checked_mul(count))
        .and_then(|bytes| bytes.checked_add(RUN_HEADER_BYTES))
        .ok_or_else(|| sort_invalid("sort run length overflow"))
}

struct RunWriter<const N: usize> {
    file: File,
    key_len: usize,
    expected: u64,
    written: u64,
    previous: Option<[u8; N]>,
}

impl<const N: usize> RunWriter<N> {
    fn create(path: &Path, key_len: usize, count: u64) -> Result<Self, CeAuditError> {
        run_length::<N>(count)?;
        let width = u64::try_from(N).map_err(|_| sort_invalid("sort width overflow"))?;
        let key_width =
            u64::try_from(key_len).map_err(|_| sort_invalid("sort key width overflow"))?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&RUN_MAGIC)?;
        file.write_all(&width.to_be_bytes())?;
        file.write_all(&key_width.to_be_bytes())?;
        file.write_all(&count.to_be_bytes())?;
        Ok(Self {
            file,
            key_len,
            expected: count,
            written: 0,
            previous: None,
        })
    }

    fn write_record(&mut self, record: [u8; N]) -> Result<(), CeAuditError> {
        if self.written >= self.expected {
            return Err(sort_invalid("too many sort records"));
        }
        ensure_increasing(self.previous.as_ref(), &record, self.key_len)?;
        self.file.write_all(&record)?;
        self.written += 1; // Bounded above by the checked expected count.
        self.previous = Some(record);
        Ok(())
    }

    fn finish(mut self) -> Result<(), CeAuditError> {
        if self.written != self.expected {
            return Err(sort_invalid("missing sort records"));
        }
        self.file.flush()?;
        Ok(())
    }
}

struct RunReader<const N: usize> {
    file: File,
    key_len: usize,
    count: u64,
    read: u64,
    previous: Option<[u8; N]>,
}

impl<const N: usize> RunReader<N> {
    fn open(path: &Path, key_len: usize) -> Result<Self, CeAuditError> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(sort_invalid("sort run is not a regular file"));
        }
        let mut header = [0; RUN_HEADER_BYTES as usize];
        file.read_exact(&mut header)?;
        let word = |offset| {
            let mut bytes = [0; 8];
            bytes.copy_from_slice(&header[offset..offset + 8]);
            u64::from_be_bytes(bytes)
        };
        let count = word(24);
        if header[..8] != RUN_MAGIC
            || word(8) != u64::try_from(N).map_err(|_| sort_invalid("sort width overflow"))?
            || word(16)
                != u64::try_from(key_len).map_err(|_| sort_invalid("sort key width overflow"))?
            || metadata.len() != run_length::<N>(count)?
        {
            return Err(sort_invalid("invalid sort run header or length"));
        }
        Ok(Self {
            file,
            key_len,
            count,
            read: 0,
            previous: None,
        })
    }

    fn next_record(&mut self) -> Result<Option<[u8; N]>, CeAuditError> {
        if self.read == self.count {
            if self.file.read(&mut [0; 1])? != 0 {
                return Err(sort_invalid("trailing sort run bytes"));
            }
            return Ok(None);
        }
        let mut record = [0; N];
        self.file.read_exact(&mut record)?;
        ensure_increasing(self.previous.as_ref(), &record, self.key_len)?;
        self.read += 1; // Bounded above by the checked header count.
        self.previous = Some(record);
        Ok(Some(record))
    }
}

fn ensure_increasing<const N: usize>(
    previous: Option<&[u8; N]>,
    record: &[u8; N],
    key_len: usize,
) -> Result<(), CeAuditError> {
    if previous.is_some_and(|previous| previous[..key_len] >= record[..key_len]) {
        return Err(sort_invalid("duplicate or unordered sort identity"));
    }
    Ok(())
}

/// Runs `start..start + count` of `pass` that merge into run `output` of `next_pass`.
struct RunMerge {
    pass: u64,
    start: u64,
    count: u64,
    next_pass: u64,
    output: u64,
}

fn merge_runs<const N: usize>(
    directory: &Path,
    merge: RunMerge,
    key_len: usize,
) -> Result<(), CeAuditError> {
    let RunMerge {
        pass,
        start,
        count,
        next_pass,
        output,
    } = merge;
    let capacity = usize::try_from(count).map_err(|_| sort_invalid("sort merge count overflow"))?;
    let mut readers = Vec::new();
    readers
        .try_reserve_exact(capacity)
        .map_err(|_| sort_invalid("cannot reserve sort readers"))?;
    let mut heads = BinaryHeap::new();
    heads
        .try_reserve_exact(capacity)
        .map_err(|_| sort_invalid("cannot reserve sort heads"))?;
    let mut total = 0_u64;
    for offset in 0..count {
        let index = start
            .checked_add(offset)
            .ok_or_else(|| sort_invalid("sort run counter overflow"))?;
        let reader = RunReader::<N>::open(&run_path(directory, pass, index), key_len)?;
        total = total
            .checked_add(reader.count)
            .ok_or_else(|| sort_invalid("sort record count overflow"))?;
        readers.push(reader);
    }
    let mut writer =
        RunWriter::<N>::create(&run_path(directory, next_pass, output), key_len, total)?;
    for (index, reader) in readers.iter_mut().enumerate() {
        if let Some(record) = reader.next_record()? {
            heads.push(Reverse((record, index)));
        }
    }
    while let Some(Reverse((record, index))) = heads.pop() {
        writer.write_record(record)?;
        if let Some(next) = readers[index].next_record()? {
            heads.push(Reverse((next, index)));
        }
    }
    writer.finish()?;
    drop(readers);
    for offset in 0..count {
        let index = start
            .checked_add(offset)
            .ok_or_else(|| sort_invalid("sort run counter overflow"))?;
        fs::remove_file(run_path(directory, pass, index))?;
    }
    Ok(())
}

#[cfg(all(test, feature = "snapshot-integration"))]
mod sorter_tests;

#[cfg(all(test, feature = "snapshot-integration"))]
mod tests;
