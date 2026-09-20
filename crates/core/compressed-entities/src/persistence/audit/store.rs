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
/// compared only after identity; every identity must occur exactly once.
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
        if N == 0
            || key_len == 0
            || key_len > N
            || work.limits.records_per_run == 0
            || work.limits.merge_fan_in < 2
        {
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
                merge_runs::<N>(
                    &self.directory,
                    pass,
                    start,
                    count,
                    next_pass,
                    outputs,
                    self.key_len,
                )?;
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

/// Streaming output; an I/O or integrity error terminates this iterator.
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

fn merge_runs<const N: usize>(
    directory: &Path,
    pass: u64,
    start: u64,
    count: u64,
    next_pass: u64,
    output: u64,
    key_len: usize,
) -> Result<(), CeAuditError> {
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

#[cfg(test)]
mod sorter_tests {
    use super::RecordSorter;
    use crate::persistence::{
        audit::{CeAuditError, CeAuditLimits, CeAuditWork},
        TreeKey,
    };
    use alloy_primitives::B256;
    use std::{fs, path::PathBuf};

    fn work(records_per_run: usize, merge_fan_in: usize) -> (tempfile::TempDir, CeAuditWork) {
        let parent = tempfile::tempdir().unwrap();
        let work = CeAuditWork::create(
            parent.path().join("sort"),
            CeAuditLimits {
                records_per_run,
                merge_fan_in,
            },
        )
        .unwrap();
        (parent, work)
    }

    fn only_run(work: &CeAuditWork) -> PathBuf {
        let child = fs::read_dir(&work.root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut files = fs::read_dir(child).unwrap();
        let path = files.next().unwrap().unwrap().path();
        assert!(files.next().is_none());
        path
    }

    #[test]
    fn sorter_boundaries_multipass_and_reverse_input_have_identical_output() {
        for count in [0_u16, 1, 2, 3, 4, 5, 17, 33] {
            for reverse in [false, true] {
                let (_parent, work) = work(2, 2);
                let mut sorter = RecordSorter::<3>::new(&work, 2).unwrap();
                let expected = (0..count)
                    .map(|value| {
                        let [a, b] = value.to_be_bytes();
                        [a, b, 99]
                    })
                    .collect::<Vec<_>>();
                let mut input = expected.clone();
                if reverse {
                    input.reverse();
                }
                for record in input {
                    sorter.push(record).unwrap();
                }
                assert_eq!(sorter.peak_buffered_records(), usize::from(count).min(2));
                let mut records = sorter.finish().unwrap();
                let actual = records.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
                assert_eq!(actual, expected);
                assert!(records.next().is_none());
                assert!(records.next().is_none());
            }
        }
    }

    #[test]
    fn sorter_rejects_duplicate_identity_with_different_payload_across_runs() {
        let (_parent, work) = work(2, 2);
        let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
        for record in [[1, 10], [3, 30], [2, 20], [1, 11]] {
            sorter.push(record).unwrap();
        }
        let result = sorter
            .finish()
            .and_then(|records| records.collect::<Result<Vec<_>, _>>());
        assert!(result.is_err(), "duplicate prefix survived separate runs");
    }

    #[test]
    fn sorter_rejects_duplicate_identity_within_one_run() {
        let (_parent, work) = work(3, 2);
        let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
        sorter.push([1, 10]).unwrap();
        sorter.push([1, 11]).unwrap();
        let result = sorter
            .finish()
            .and_then(|records| records.collect::<Result<Vec<_>, _>>());
        assert!(result.is_err());
    }

    #[test]
    fn sorter_uses_reversed_native_tree_key_bytes_in_body_tuples() {
        let (_parent, work) = work(1, 2);
        let mut low_lex = [0_u8; 32];
        low_lex[31] = 1;
        let mut high_lex = [0_u8; 32];
        high_lex[0] = 1;
        assert!(low_lex < high_lex);
        let low = TreeKey::try_from(B256::from(low_lex)).unwrap();
        let high = TreeKey::try_from(B256::from(high_lex)).unwrap();
        assert!(high < low);
        let tuple = |mut key: [u8; 32], payload: u8| {
            let mut record = [0; 101];
            record[0] = 1; // CollectionShard namespace tag.
            key.reverse();
            record[37..69].copy_from_slice(&key);
            record[100] = payload;
            record
        };
        let first = tuple(low.encode(), 1);
        let second = tuple(high.encode(), 2);
        let mut sorter = RecordSorter::<101>::new(&work, 69).unwrap();
        sorter.push(first).unwrap();
        sorter.push(second).unwrap();
        assert_eq!(
            sorter
                .finish()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![second, first]
        );
    }

    #[test]
    fn sorter_instances_share_work_without_overwriting_each_other() {
        let (parent, work) = work(1, 2);
        let sentinel = parent.path().join("protected");
        fs::write(&sentinel, b"keep").unwrap();
        let mut first = RecordSorter::<2>::new(&work, 1).unwrap();
        let mut second = RecordSorter::<2>::new(&work, 1).unwrap();
        first.push([1, 2]).unwrap();
        second.push([3, 4]).unwrap();
        let first = first
            .finish()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let second = second
            .finish()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(first, vec![[1, 2]]);
        assert_eq!(second, vec![[3, 4]]);
        drop(work);
        assert_eq!(fs::read(sentinel).unwrap(), b"keep");
    }

    #[test]
    fn sorter_rejects_invalid_limits_before_creating_files_or_allocating() {
        let (_parent, mut work) = work(2, 2);
        assert!(RecordSorter::<0>::new(&work, 0).is_err());
        assert!(RecordSorter::<2>::new(&work, 0).is_err());
        assert!(RecordSorter::<2>::new(&work, 3).is_err());
        for (records_per_run, merge_fan_in) in [(0, 2), (2, 1), (usize::MAX, 2), (2, usize::MAX)] {
            work.limits = CeAuditLimits {
                records_per_run,
                merge_fan_in,
            };
            assert!(RecordSorter::<2>::new(&work, 1).is_err());
        }
        assert!(fs::read_dir(&work.root).unwrap().next().is_none());
    }

    #[test]
    fn sorter_rejects_corrupt_run_metadata_lengths_and_identity_order() {
        // Header: magic/version[8], record width[8], key length[8], count[8].
        for corruption in 0..9 {
            let (_parent, work) = work(2, 2);
            let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
            sorter.push([1, 10]).unwrap();
            sorter.push([2, 20]).unwrap();
            let path = only_run(&work);
            let mut bytes = fs::read(&path).unwrap();
            match corruption {
                0 => bytes[0] ^= 0xff,
                1 => bytes[8..16].copy_from_slice(&3_u64.to_be_bytes()),
                2 => bytes[16..24].copy_from_slice(&2_u64.to_be_bytes()),
                3 => bytes[24..32].copy_from_slice(&3_u64.to_be_bytes()),
                4 => bytes[24..32].copy_from_slice(&u64::MAX.to_be_bytes()),
                5 => {
                    bytes.pop();
                }
                6 => bytes.push(0),
                7 => {
                    bytes[32] = 2;
                    bytes[34] = 1;
                }
                8 => bytes[34] = 1,
                _ => unreachable!(),
            }
            fs::write(path, bytes).unwrap();
            let result = sorter
                .finish()
                .and_then(|records| records.collect::<Result<Vec<_>, _>>());
            assert!(result.is_err(), "accepted corruption {corruption}");
        }
    }

    #[test]
    fn sorter_iterator_emits_one_terminal_error_then_fuses() {
        let (_parent, work) = work(2, 2);
        let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
        sorter.push([1, 10]).unwrap();
        sorter.push([2, 20]).unwrap();
        let path = only_run(&work);
        let mut records = sorter.finish().unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(32)
            .unwrap();
        assert!(matches!(records.next(), Some(Err(CeAuditError::Io(_)))));
        assert!(records.next().is_none());
        assert!(records.next().is_none());
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::B256;
    use outbe_sparse_merkle_tree_v061::{
        merge::MergeValue as CkbMergeValue,
        traits::{StoreReadOps, StoreWriteOps},
        BranchKey as CkbBranchKey, BranchNode as CkbBranchNode, H256,
    };
    use reth_db::{
        database::Database,
        mdbx::{create_db, DatabaseArguments},
        transaction::{DbTx, DbTxMut},
        DatabaseEnv,
    };

    use super::ScratchTreeStore;
    use crate::persistence::{
        prefixed_key, tables, BranchKey, BranchNode, FieldValue, LeafValue, MergeValue, TreeKey,
        TreeNamespace,
    };

    fn scratch() -> (tempfile::TempDir, DatabaseEnv) {
        let directory = tempfile::tempdir().unwrap();
        let mut db = create_db(directory.path(), DatabaseArguments::test()).unwrap();
        db.create_and_track_tables_for::<tables::CeTables>()
            .unwrap();
        (directory, db)
    }

    fn hash(value: u8) -> H256 {
        H256::from(B256::with_last_byte(value).0)
    }

    #[test]
    fn scratch_round_trips_native_records_updates_removes_and_isolates_namespaces() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let mut store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let key = CkbBranchKey {
            height: 7,
            node_key: hash(0),
        };
        let branch = CkbBranchNode {
            left: CkbMergeValue::Value(hash(5)),
            right: CkbMergeValue::MergeWithZero {
                base_node: hash(6),
                zero_bits: hash(7),
                zero_count: 8,
            },
        };
        assert!(store.get_branch(&key).unwrap().is_none());
        store.insert_branch(key.clone(), branch.clone()).unwrap();
        assert_eq!(store.get_branch(&key).unwrap(), Some(branch));
        let encoded_key = BranchKey::new(7, B256::ZERO).unwrap();
        let persisted = tx
            .get::<tables::CeBranches>(prefixed_key(TreeNamespace::Catalog, &encoded_key.encode()))
            .unwrap()
            .unwrap();
        assert_eq!(
            BranchNode::decode(&persisted).unwrap(),
            BranchNode {
                left: MergeValue::Value(FieldValue::try_from(B256::with_last_byte(5)).unwrap()),
                right: MergeValue::MergeWithZero {
                    base_node: FieldValue::try_from(B256::with_last_byte(6)).unwrap(),
                    zero_bits: FieldValue::try_from(B256::with_last_byte(7)).unwrap(),
                    zero_count: 8,
                },
            }
        );
        store.insert_leaf(hash(1), hash(9)).unwrap();
        store.insert_leaf(hash(1), hash(10)).unwrap();
        assert_eq!(store.get_leaf(&hash(1)).unwrap(), Some(hash(10)));
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 1);
        let leaf_key = TreeKey::try_from(B256::with_last_byte(1)).unwrap();
        let leaf = tx
            .get::<tables::CeLeaves>(prefixed_key(TreeNamespace::Catalog, &leaf_key.encode()))
            .unwrap()
            .unwrap();
        assert_eq!(
            LeafValue::decode(&leaf).unwrap().into_inner(),
            B256::with_last_byte(10)
        );

        let collection = crate::CollectionKey::try_from(B256::with_last_byte(1)).unwrap();
        let mut other = ScratchTreeStore::new(&tx, TreeNamespace::CollectionShard(collection, 0));
        assert!(other.get_leaf(&hash(1)).unwrap().is_none());
        assert!(other.get_branch(&key).unwrap().is_none());
        other.insert_leaf(hash(1), hash(11)).unwrap();
        store.remove_leaf(&hash(1)).unwrap();
        store.remove_branch(&key).unwrap();
        assert!(store.get_leaf(&hash(1)).unwrap().is_none());
        assert!(store.get_branch(&key).unwrap().is_none());
        assert_eq!(other.get_leaf(&hash(1)).unwrap(), Some(hash(11)));
        store.remove_leaf(&hash(1)).unwrap();
        store.remove_branch(&key).unwrap();
        assert_eq!(tx.entries::<tables::CeTreeRoots>().unwrap(), 0);
        assert_eq!(tx.entries::<tables::CeMetadata>().unwrap(), 0);
    }

    #[test]
    fn scratch_rejects_noncanonical_fields_before_writing() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let mut store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let invalid = H256::from([0xff; 32]);
        assert!(store.insert_leaf(invalid, hash(1)).is_err());
        assert!(store.insert_leaf(hash(1), invalid).is_err());
        assert!(store.insert_leaf(hash(1), H256::zero()).is_err());
        assert!(store.get_leaf(&invalid).is_err());
        assert!(store.remove_leaf(&invalid).is_err());
        for invalid_branch in [
            CkbMergeValue::Value(invalid),
            CkbMergeValue::MergeWithZero {
                base_node: invalid,
                zero_bits: hash(0),
                zero_count: 1,
            },
            CkbMergeValue::MergeWithZero {
                base_node: hash(1),
                zero_bits: invalid,
                zero_count: 1,
            },
        ] {
            assert!(store
                .insert_branch(
                    CkbBranchKey {
                        height: 1,
                        node_key: hash(0)
                    },
                    CkbBranchNode {
                        left: CkbMergeValue::Value(hash(0)),
                        right: invalid_branch
                    },
                )
                .is_err());
        }
        let key = CkbBranchKey {
            height: 1,
            node_key: invalid,
        };
        assert!(store.get_branch(&key).is_err());
        assert!(store.remove_branch(&key).is_err());
        assert_eq!(tx.entries::<tables::CeBranches>().unwrap(), 0);
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 0);
    }

    #[test]
    fn scratch_reads_reject_malformed_native_values() {
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let leaf_key = TreeKey::try_from(B256::with_last_byte(1)).unwrap();
        tx.put::<tables::CeLeaves>(
            prefixed_key(TreeNamespace::Catalog, &leaf_key.encode()),
            vec![0; 32],
        )
        .unwrap();
        assert!(store.get_leaf(&hash(1)).is_err());
        let key = BranchKey::new(1, B256::ZERO).unwrap();
        tx.put::<tables::CeBranches>(prefixed_key(TreeNamespace::Catalog, &key.encode()), vec![0])
            .unwrap();
        assert!(store
            .get_branch(&CkbBranchKey {
                height: 1,
                node_key: hash(0)
            })
            .is_err());
    }

    #[test]
    fn poseidon_tree_uses_the_borrowed_native_store() {
        use crate::smt::{PoseidonSmt, TreeKey, TreeLeaf, TreeRoot};
        let (_directory, db) = scratch();
        let tx = db.tx_mut().unwrap();
        let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
        let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
        let mut expected = PoseidonSmt::empty();
        for (key, value) in [(1, 4), (2, 5), (1, 6)] {
            let key = TreeKey::from_be_bytes(B256::with_last_byte(key).0).unwrap();
            let leaf = TreeLeaf::from_be_bytes(B256::with_last_byte(value).0).unwrap();
            assert_eq!(
                tree.update(key, leaf).unwrap(),
                expected.update(key, leaf).unwrap()
            );
        }
        for value in [1, 2] {
            let key = TreeKey::from_be_bytes(B256::with_last_byte(value).0).unwrap();
            assert_eq!(
                tree.update(key, TreeLeaf::ZERO).unwrap(),
                expected.update(key, TreeLeaf::ZERO).unwrap()
            );
        }
        assert_eq!(tree.root().unwrap(), TreeRoot::EMPTY);
        assert_eq!(tx.entries::<tables::CeLeaves>().unwrap(), 0);
    }

    #[test]
    fn audit_rejects_a_wrong_shard_even_when_every_root_and_branch_is_consistent() {
        use super::super::{CeAuditError, CeAuditLimits, CeAuditVisitor, CeAuditWork};
        use crate::{
            persistence::{
                CeMdbx, CeMdbxReadOnly, EnvironmentIdentity, ExactParentIdentity, FinalizedMarker,
                LAST_APPLIED_KEY, LOCAL_STORAGE_SCHEMA_VERSION,
            },
            sharding::{aggregate_b256_shard_roots, shard_index},
            smt::{derive_tree_key, PoseidonSmt, TreeKey as SmtKey, TreeLeaf, TreeRoot},
            CeDomain, CeTopologyV1, WwdEntityId, ACTIVE_COMMITMENT_SCHEME, K_PROVISIONAL,
        };

        struct Observe;
        impl CeAuditVisitor for Observe {
            fn visit_leaf(
                &mut self,
                _: TreeNamespace,
                _: TreeKey,
                _: LeafValue,
            ) -> Result<(), CeAuditError> {
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let genesis_hash = B256::with_last_byte(42);
        let identity = EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 10,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        };
        let genesis = FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: crate::sealed_root(B256::ZERO).unwrap(),
        };
        let writer = CeMdbx::open(directory.path(), identity.clone(), genesis).unwrap();
        let tx = writer.db.tx_mut().unwrap();
        let mut raw_id = [0; 32];
        raw_id[..4].copy_from_slice(&20_260_718_u32.to_be_bytes());
        raw_id[31] = 1;
        let id = WwdEntityId::try_from(raw_id.as_slice()).unwrap();
        let collection = crate::collection_key(CeDomain::Tribute, id).unwrap();
        let key = derive_tree_key(crate::schema::Collection::Tribute, id).unwrap();
        let correct_shard = shard_index(key, K_PROVISIONAL).unwrap();
        let wrong_shard = (correct_shard + 1) % K_PROVISIONAL;
        assert_ne!(correct_shard, wrong_shard);

        // Deliberately bypass the production writer's shard envelope checks.
        // Rebuild all native branch records and commitments so hash checks alone
        // cannot distinguish this malformed namespace placement.
        let mut roots = vec![B256::ZERO; K_PROVISIONAL as usize];
        {
            let store =
                ScratchTreeStore::new(&tx, TreeNamespace::CollectionShard(collection, wrong_shard));
            let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
            roots[wrong_shard as usize] = B256::from(
                tree.update(
                    key,
                    TreeLeaf::from_be_bytes(B256::with_last_byte(7).0).unwrap(),
                )
                .unwrap()
                .as_bytes(),
            );
        }
        for shard in 0..K_PROVISIONAL {
            tx.put::<tables::CeTreeRoots>(
                TreeNamespace::CollectionShard(collection, shard).encode(),
                roots[shard as usize].to_vec(),
            )
            .unwrap();
        }
        let collection_root = crate::collection_root(
            CeDomain::Tribute,
            collection,
            aggregate_b256_shard_roots(&roots).unwrap(),
        )
        .unwrap();
        let catalog_root = {
            let store = ScratchTreeStore::new(&tx, TreeNamespace::Catalog);
            let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
            B256::from(
                tree.update(
                    SmtKey::from_be_bytes(*collection.as_bytes()).unwrap(),
                    TreeLeaf::from_be_bytes(collection_root.0).unwrap(),
                )
                .unwrap()
                .as_bytes(),
            )
        };
        tx.put::<tables::CeTreeRoots>(TreeNamespace::Catalog.encode(), catalog_root.to_vec())
            .unwrap();
        let marker = FinalizedMarker {
            height: 1,
            block_hash: B256::with_last_byte(43),
            parent_block_hash: genesis.block_hash,
            parent_root: genesis.new_root,
            new_root: crate::sealed_root(catalog_root).unwrap(),
            ..genesis
        };
        tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), marker.encode().to_vec())
            .unwrap();
        tx.commit().unwrap();
        drop(writer);

        let reader = CeMdbxReadOnly::open(directory.path(), identity).unwrap();
        let required = ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        };
        reader.open_exact(required).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let work = CeAuditWork::create(
            scratch.path().join("audit"),
            CeAuditLimits {
                records_per_run: 2,
                merge_fan_in: 2,
            },
        )
        .unwrap();
        assert!(
            reader.audit_exact(required, &work, &mut Observe).is_err(),
            "accepted a coherent tree whose leaf is in the wrong native shard",
        );
    }
}
