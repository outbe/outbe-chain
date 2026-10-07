//! Disk-backed compact fork archive for one authenticated frozen Tribute inventory.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use alloy_primitives::B256;
use outbe_sparse_merkle_tree_v061::{merge::MergeValue, MerkleProof, H256};

use crate::{
    collection::{collection_root, partition_collection_key, CeDomain},
    collection_reconstruction::{
        TributePartitionExpectationV1, TributePartitionReconstructionError,
    },
    proof::{top_siblings, CkbCompiledProofV1, CollectionBodyProofV1},
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::{derive_tree_key, SortedNodeObserver, TreeError, TreeKey, TreeLeaf, TreeProof},
    PartitionRef, WwdEntityId, ACTIVE_COMMITMENT_SCHEME,
};

type Result<T> = std::result::Result<T, TributePartitionReconstructionError>;
const MAGIC: &[u8; 8] = b"OUTBPA01";
const HEADER: &str = "proof-archive.header";
const NODE_BYTES: usize = 182;
const HEADER_BYTES: usize = 8 + 4 + 4 + 32 + SHARDS * 40;
const SHARDS: usize = 16;

/// An immutable archive retains O(N) nonzero forks, rather than 256 nodes per leaf.
pub struct TributeProofArchiveV1 {
    root: PathBuf,
    expectation: TributePartitionExpectationV1,
    shard_roots: [B256; SHARDS],
    root_ids: [u64; SHARDS],
}

impl TributeProofArchiveV1 {
    pub fn open(
        root: impl AsRef<Path>,
        expectation: TributePartitionExpectationV1,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let path = root.join(HEADER);
        let mut bytes = Vec::with_capacity(HEADER_BYTES + 1);
        File::open(&path)
            .map_err(|e| archive_io("open archive header", &path, e))?
            .take((HEADER_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| archive_io("read archive header", &path, e))?;
        if bytes.len() != HEADER_BYTES || &bytes[..8] != MAGIC {
            return Err(corrupt(&path));
        }
        if expectation.commitment_scheme != ACTIVE_COMMITMENT_SCHEME {
            return Err(corrupt(&path));
        }
        if bytes[..48] != archive_header_prefix(expectation) {
            return Err(corrupt(&path));
        }
        let mut shard_roots = [B256::ZERO; SHARDS];
        let mut root_ids = [0; SHARDS];
        for index in 0..SHARDS {
            let start = 48 + index * 40;
            shard_roots[index] = B256::from_slice(&bytes[start..start + 32]);
            root_ids[index] = u64::from_be_bytes(
                bytes[start + 32..start + 40]
                    .try_into()
                    .map_err(|_| corrupt(&path))?,
            );
        }
        let archive = Self {
            root,
            expectation,
            shard_roots,
            root_ids,
        };
        archive.check_root()?;
        Ok(archive)
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
    pub fn collection_root(&self) -> B256 {
        self.expectation.expected_collection_root
    }
    pub fn exact_leaf_count(&self) -> u32 {
        self.expectation.exact_leaf_count
    }

    /// Reads at most 256 compact forks; the proof is checked against the frozen root.
    pub fn proof(&self, id: WwdEntityId) -> Result<CollectionBodyProofV1> {
        if id.worldwide_day() != self.expectation.day {
            return Err(corrupt(&self.root));
        }
        let key = derive_tree_key(crate::schema::Collection::Tribute, id).map_err(tree_error)?;
        let shard = shard_index(key, SHARDS as u32).map_err(tree_error)? as usize;
        let path = shard_path(&self.root, shard);
        let mut file = File::open(&path).map_err(|e| archive_io("open proof shard", &path, e))?;
        let size = file
            .metadata()
            .map_err(|e| archive_io("inspect proof shard", &path, e))?
            .len();
        if size % NODE_BYTES as u64 != 0 {
            return Err(corrupt(&path));
        }
        let (proof, leaf) =
            compile_archived_proof(&mut file, &path, size, self.root_ids[shard], key)?;
        let computed = TreeProof::from_bytes(proof.0.clone())
            .compute_root(
                key,
                TreeLeaf::from_be_bytes(leaf.into()).map_err(tree_error)?,
            )
            .map_err(tree_error)?;
        if B256::from(computed.as_bytes()) != self.shard_roots[shard] {
            return Err(corrupt(&path));
        }
        Ok(CollectionBodyProofV1 {
            shard_smt_proof: CkbCompiledProofV1::new(proof.0)
                .map_err(|e| protocol_error(e.to_string()))?,
            shard_top_siblings: top_siblings(&self.shard_roots, shard as u32)
                .map_err(|e| protocol_error(e.to_string()))?,
        })
    }

    pub(crate) fn publish(
        root: PathBuf,
        expectation: TributePartitionExpectationV1,
        shard_roots: [B256; SHARDS],
        root_ids: [u64; SHARDS],
    ) -> Result<Self> {
        let archive = Self {
            root,
            expectation,
            shard_roots,
            root_ids,
        };
        archive.check_root()?;
        let mut bytes = archive_header_prefix(expectation).to_vec();
        for index in 0..SHARDS {
            bytes.extend_from_slice(archive.shard_roots[index].as_slice());
            bytes.extend_from_slice(&archive.root_ids[index].to_be_bytes());
        }
        let path = archive.root.join(HEADER);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|e| archive_io("create archive header", &path, e))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| archive_io("persist archive header", &path, e))?;
        File::open(&archive.root)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| archive_io("sync archive directory", &archive.root, e))?;
        Ok(archive)
    }

    fn check_root(&self) -> Result<()> {
        let top = aggregate_b256_shard_roots(&self.shard_roots).map_err(tree_error)?;
        let (_, key) = partition_collection_key(PartitionRef::TributeWwd(self.expectation.day))?;
        let actual = collection_root(CeDomain::Tribute, key, top)?;
        if actual != self.expectation.expected_collection_root {
            return Err(TributePartitionReconstructionError::RootMismatch {
                expected: self.expectation.expected_collection_root,
                actual,
            });
        }
        Ok(())
    }
}

fn archive_header_prefix(expectation: TributePartitionExpectationV1) -> [u8; 48] {
    let mut bytes = [0; 48];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..12].copy_from_slice(&expectation.day.value().to_be_bytes());
    bytes[12..16].copy_from_slice(&expectation.exact_leaf_count.to_be_bytes());
    bytes[16..].copy_from_slice(expectation.expected_collection_root.as_slice());
    bytes
}

fn compile_archived_proof(
    file: &mut File,
    path: &Path,
    size: u64,
    root_id: u64,
    key: TreeKey,
) -> Result<(outbe_sparse_merkle_tree_v061::CompiledMerkleProof, H256)> {
    let mut current = root_id;
    let mut bitmap = H256::zero();
    let mut siblings = Vec::with_capacity(256);
    let mut previous_height = 256_u16;
    let leaf = loop {
        let node = read_node(file, path, size, current)?;
        if node.kind == 0 {
            break node.leaf_for(key, path)?;
        }
        let (next, sibling) = node.descend(key, current, previous_height, path)?;
        previous_height = u16::from(node.height);
        bitmap.set_bit(node.height);
        siblings.push(sibling);
        current = next;
    };
    siblings.reverse();
    let proof = MerkleProof::new(vec![bitmap], siblings)
        .compile(vec![H256::from(key.as_bytes())])
        .map_err(|e| tree_error(TreeError::Vendor(e.to_string())))?;
    Ok((proof, leaf))
}

pub(crate) struct ArchiveNodeWriter {
    file: File,
    count: u64,
}
impl ArchiveNodeWriter {
    pub(crate) fn create(root: &Path, shard: usize) -> Result<Self> {
        let path = shard_path(root, shard);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| archive_io("create proof shard", &path, e))?;
        Ok(Self { file, count: 0 })
    }
    pub(crate) fn finish(self) -> Result<()> {
        self.file
            .sync_all()
            .map_err(|e| protocol_error(format!("sync proof shard: {e}")))
    }
    fn append(&mut self, node: ArchiveNode) -> std::result::Result<u64, TreeError> {
        let mut bytes = Vec::with_capacity(NODE_BYTES);
        bytes.extend_from_slice(&[node.kind, node.height]);
        bytes.extend_from_slice(&node.key);
        bytes.extend_from_slice(&node.left_id.to_be_bytes());
        bytes.extend_from_slice(&node.right_id.to_be_bytes());
        encode_merge(&mut bytes, &node.left);
        encode_merge(&mut bytes, &node.right);
        debug_assert_eq!(bytes.len(), NODE_BYTES);
        self.file
            .write_all(&bytes)
            .map_err(|e| TreeError::Vendor(e.to_string()))?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or(TreeError::ReducerInvariant("archive node count overflow"))?;
        Ok(self.count)
    }
}
impl SortedNodeObserver for ArchiveNodeWriter {
    fn leaf(&mut self, key: TreeKey, leaf: TreeLeaf) -> std::result::Result<u64, TreeError> {
        self.append(ArchiveNode {
            kind: 0,
            height: 0,
            key: key.as_bytes(),
            left_id: 0,
            right_id: 0,
            left: MergeValue::Value(H256::from(leaf.as_bytes())),
            right: MergeValue::zero(),
        })
    }
    fn fork(
        &mut self,
        height: u8,
        left: &MergeValue,
        right: &MergeValue,
        left_id: u64,
        right_id: u64,
    ) -> std::result::Result<u64, TreeError> {
        self.append(ArchiveNode {
            kind: 1,
            height,
            key: [0; 32],
            left_id,
            right_id,
            left: left.clone(),
            right: right.clone(),
        })
    }
}
struct ArchiveNode {
    kind: u8,
    height: u8,
    key: [u8; 32],
    left_id: u64,
    right_id: u64,
    left: MergeValue,
    right: MergeValue,
}
impl ArchiveNode {
    fn leaf_for(&self, key: TreeKey, path: &Path) -> Result<H256> {
        match &self.left {
            MergeValue::Value(value) if self.key == key.as_bytes() => Ok(*value),
            _ => Err(corrupt(path)),
        }
    }
    fn descend(
        &self,
        key: TreeKey,
        current: u64,
        previous_height: u16,
        path: &Path,
    ) -> Result<(u64, MergeValue)> {
        if self.kind != 1 || u16::from(self.height) >= previous_height {
            return Err(corrupt(path));
        }
        let (next, sibling) = if H256::from(key.as_bytes()).is_right(self.height) {
            (self.right_id, self.left.clone())
        } else {
            (self.left_id, self.right.clone())
        };
        if next == 0 || next >= current {
            return Err(corrupt(path));
        }
        Ok((next, sibling))
    }
}

fn read_node(file: &mut File, path: &Path, size: u64, id: u64) -> Result<ArchiveNode> {
    let offset = id
        .checked_sub(1)
        .and_then(|n| n.checked_mul(NODE_BYTES as u64))
        .ok_or_else(|| corrupt(path))?;
    if offset
        .checked_add(NODE_BYTES as u64)
        .is_none_or(|end| end > size)
    {
        return Err(corrupt(path));
    }
    let mut bytes = [0; NODE_BYTES];
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|e| archive_io("read proof node", path, e))?;
    Ok(ArchiveNode {
        kind: bytes[0],
        height: bytes[1],
        key: bytes[2..34].try_into().map_err(|_| corrupt(path))?,
        left_id: u64::from_be_bytes(bytes[34..42].try_into().map_err(|_| corrupt(path))?),
        right_id: u64::from_be_bytes(bytes[42..50].try_into().map_err(|_| corrupt(path))?),
        left: decode_merge(&bytes[50..116], path)?,
        right: decode_merge(&bytes[116..182], path)?,
    })
}
fn encode_merge(bytes: &mut Vec<u8>, value: &MergeValue) {
    match value {
        MergeValue::Value(v) => {
            bytes.extend_from_slice(&[0, 0]);
            bytes.extend_from_slice(v.as_slice());
            bytes.extend_from_slice(&[0; 32]);
        }
        MergeValue::MergeWithZero {
            base_node,
            zero_bits,
            zero_count,
        } => {
            bytes.extend_from_slice(&[1, *zero_count]);
            bytes.extend_from_slice(base_node.as_slice());
            bytes.extend_from_slice(zero_bits.as_slice());
        }
    }
}
fn decode_merge(bytes: &[u8], path: &Path) -> Result<MergeValue> {
    let base = H256::from(<[u8; 32]>::try_from(&bytes[2..34]).map_err(|_| corrupt(path))?);
    let bits = H256::from(<[u8; 32]>::try_from(&bytes[34..66]).map_err(|_| corrupt(path))?);
    match bytes[0] {
        0 if bytes[1] == 0 && bits.is_zero() => Ok(MergeValue::Value(base)),
        1 => Ok(MergeValue::MergeWithZero {
            base_node: base,
            zero_bits: bits,
            zero_count: bytes[1],
        }),
        _ => Err(corrupt(path)),
    }
}
fn shard_path(root: &Path, shard: usize) -> PathBuf {
    root.join(format!("proof-shard-{shard:02}.bin"))
}
fn tree_error(error: TreeError) -> TributePartitionReconstructionError {
    protocol_error(error.to_string())
}
fn protocol_error(message: String) -> TributePartitionReconstructionError {
    TributePartitionReconstructionError::Tree(message)
}
fn corrupt(path: &Path) -> TributePartitionReconstructionError {
    TributePartitionReconstructionError::CorruptRun(path.to_path_buf())
}
fn archive_io(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> TributePartitionReconstructionError {
    TributePartitionReconstructionError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
