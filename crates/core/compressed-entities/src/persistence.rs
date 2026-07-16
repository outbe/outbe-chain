//! CE-owned finalized sparse-tree persistence.
//!
//! This module owns deterministic local codecs, the separate MDBX environment,
//! atomic contiguous finalized application, and restart/ACK classification. It
//! is authenticated materialization only: the exact EVM root remains the sole
//! consensus authority.

use std::{
    cmp::Ordering as CmpOrdering,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use alloy_primitives::B256;
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use reth_db::{
    database::Database,
    mdbx::{create_db, tx::Tx, DatabaseArguments, RO},
    transaction::{DbTx, DbTxMut},
    ClientVersion, DatabaseEnv,
};
use thiserror::Error;

use crate::staging::{FinalizedTreeSnapshot, StagedTreeBatch, StagingError, TreeChange};

pub const LOCAL_STORAGE_SCHEMA_VERSION: u32 = 1;
pub const FINALIZED_MARKER_ENCODED_LEN: usize = 4 + 8 + 32 * 4;
pub const CE_SMT_RELATIVE_PATH: &str = "compressed_entities/smt";

const IDENTITY_KEY: &[u8] = b"environment_identity";
const LAST_APPLIED_KEY: &[u8] = b"last_applied";

/// Canonical BN254 field bytes. Zero is allowed for structural emptiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldValue(B256);

impl FieldValue {
    #[must_use]
    pub const fn into_inner(self) -> B256 {
        self.0
    }

    #[must_use]
    pub fn is_zero(self) -> bool {
        self.0 == B256::ZERO
    }

    #[must_use]
    pub const fn encode(self) -> [u8; 32] {
        self.0 .0
    }
}

impl TryFrom<B256> for FieldValue {
    type Error = PersistenceError;

    fn try_from(value: B256) -> Result<Self, Self::Error> {
        validate_field(value)?;
        Ok(Self(value))
    }
}

/// Exact CKB 256-level path key. Key zero is a valid position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeKey(FieldValue);

impl Ord for TreeKey {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.encode().iter().rev().cmp(other.encode().iter().rev())
    }
}

impl PartialOrd for TreeKey {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl TreeKey {
    #[must_use]
    pub const fn into_inner(self) -> B256 {
        self.0.into_inner()
    }

    #[must_use]
    pub const fn encode(self) -> [u8; 32] {
        self.0.encode()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        Ok(Self(FieldValue::try_from(decode_b256(bytes, "tree key")?)?))
    }
}

impl TryFrom<B256> for TreeKey {
    type Error = PersistenceError;

    fn try_from(value: B256) -> Result<Self, Self::Error> {
        Ok(Self(FieldValue::try_from(value)?))
    }
}

/// A persisted non-zero body leaf. Delete is represented by record absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeafValue(FieldValue);

impl LeafValue {
    #[must_use]
    pub const fn into_inner(self) -> B256 {
        self.0.into_inner()
    }

    #[must_use]
    pub const fn encode(self) -> [u8; 32] {
        self.0.encode()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        Self::try_from(decode_b256(bytes, "leaf value")?)
    }
}

impl TryFrom<B256> for LeafValue {
    type Error = PersistenceError;

    fn try_from(value: B256) -> Result<Self, Self::Error> {
        let field = FieldValue::try_from(value)?;
        if field.is_zero() {
            return Err(PersistenceError::ZeroPersistedLeaf);
        }
        Ok(Self(field))
    }
}

/// A canonical CKB node path stored together with its height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BranchKey {
    pub height: u8,
    pub node_key: FieldValue,
}

impl Ord for BranchKey {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.height.cmp(&other.height).then_with(|| {
            self.node_key
                .encode()
                .iter()
                .rev()
                .cmp(other.node_key.encode().iter().rev())
        })
    }
}

impl PartialOrd for BranchKey {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl BranchKey {
    pub fn new(height: u8, node_key: B256) -> Result<Self, PersistenceError> {
        Ok(Self {
            height,
            node_key: FieldValue::try_from(node_key)?,
        })
    }

    #[must_use]
    pub fn encode(self) -> [u8; 33] {
        let mut bytes = [0_u8; 33];
        bytes[0] = self.height;
        bytes[1..].copy_from_slice(&self.node_key.encode());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        if bytes.len() != 33 {
            return Err(PersistenceError::MalformedCodec {
                record: "branch key",
                expected: "33 bytes",
                actual: bytes.len(),
            });
        }
        Self::new(bytes[0], decode_b256(&bytes[1..], "branch node key")?)
    }
}

/// The only MergeValue variants retained by the vendored production subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeValue {
    Value(FieldValue),
    MergeWithZero {
        base_node: FieldValue,
        zero_bits: FieldValue,
        zero_count: u8,
    },
}

impl MergeValue {
    #[must_use]
    pub fn encoded_len(self) -> usize {
        match self {
            Self::Value(_) => 33,
            Self::MergeWithZero { .. } => 66,
        }
    }

    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.encoded_len());
        match self {
            Self::Value(value) => {
                bytes.push(0);
                bytes.extend_from_slice(&value.encode());
            }
            Self::MergeWithZero {
                base_node,
                zero_bits,
                zero_count,
            } => {
                bytes.push(1);
                bytes.extend_from_slice(&base_node.encode());
                bytes.extend_from_slice(&zero_bits.encode());
                bytes.push(zero_count);
            }
        }
        bytes
    }

    fn decode_prefix(bytes: &[u8]) -> Result<(Self, usize), PersistenceError> {
        let Some(tag) = bytes.first().copied() else {
            return Err(PersistenceError::MalformedCodec {
                record: "merge value",
                expected: "tag and payload",
                actual: 0,
            });
        };
        match tag {
            0 if bytes.len() >= 33 => {
                let value = FieldValue::try_from(decode_b256(&bytes[1..33], "merge value")?)?;
                Ok((Self::Value(value), 33))
            }
            1 if bytes.len() >= 66 => {
                let base_node =
                    FieldValue::try_from(decode_b256(&bytes[1..33], "merge base node")?)?;
                let zero_bits =
                    FieldValue::try_from(decode_b256(&bytes[33..65], "merge zero bits")?)?;
                Ok((
                    Self::MergeWithZero {
                        base_node,
                        zero_bits,
                        zero_count: bytes[65],
                    },
                    66,
                ))
            }
            0 => Err(PersistenceError::MalformedCodec {
                record: "merge value",
                expected: "33 bytes",
                actual: bytes.len(),
            }),
            1 => Err(PersistenceError::MalformedCodec {
                record: "merge-with-zero value",
                expected: "66 bytes",
                actual: bytes.len(),
            }),
            actual => Err(PersistenceError::UnknownMergeValueTag(actual)),
        }
    }
}

/// CKB branch record value: two self-delimiting MergeValues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BranchNode {
    pub left: MergeValue,
    pub right: MergeValue,
}

impl BranchNode {
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        let mut bytes = self.left.encode();
        bytes.extend_from_slice(&self.right.encode());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        let (left, consumed) = MergeValue::decode_prefix(bytes)?;
        let (right, right_consumed) = MergeValue::decode_prefix(&bytes[consumed..])?;
        let total = consumed
            .checked_add(right_consumed)
            .ok_or(PersistenceError::LengthOverflow)?;
        if total != bytes.len() {
            return Err(PersistenceError::TrailingBytes {
                record: "branch value",
                trailing: bytes.len() - total,
            });
        }
        Ok(Self { left, right })
    }
}

/// Persistent environment identity. A mismatch requires an explicit rebuild or
/// migration; it is never silently reinterpreted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentIdentity {
    pub local_storage_schema_version: u32,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub commitment_scheme_version: u32,
    pub tree_format: String,
    pub vendor_revision: String,
}

impl EnvironmentIdentity {
    pub fn encode(&self) -> Result<Vec<u8>, PersistenceError> {
        let tree = self.tree_format.as_bytes();
        let vendor = self.vendor_revision.as_bytes();
        let tree_len = u16::try_from(tree.len()).map_err(|_| PersistenceError::LengthOverflow)?;
        let vendor_len =
            u16::try_from(vendor.len()).map_err(|_| PersistenceError::LengthOverflow)?;
        let mut bytes = Vec::with_capacity(52 + tree.len() + vendor.len());
        bytes.extend_from_slice(&self.local_storage_schema_version.to_be_bytes());
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(self.genesis_hash.as_slice());
        bytes.extend_from_slice(&self.commitment_scheme_version.to_be_bytes());
        bytes.extend_from_slice(&tree_len.to_be_bytes());
        bytes.extend_from_slice(tree);
        bytes.extend_from_slice(&vendor_len.to_be_bytes());
        bytes.extend_from_slice(vendor);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        let mut decoder = Decoder::new(bytes, "environment identity");
        let local_storage_schema_version = decoder.u32()?;
        let chain_id = decoder.u64()?;
        let genesis_hash = decoder.b256()?;
        let commitment_scheme_version = decoder.u32()?;
        let tree_format = decoder.string_u16()?;
        let vendor_revision = decoder.string_u16()?;
        decoder.finish()?;
        Ok(Self {
            local_storage_schema_version,
            chain_id,
            genesis_hash,
            commitment_scheme_version,
            tree_format,
            vendor_revision,
        })
    }
}

/// Atomic finalized progress marker, encoded exactly as specified by ADR-008.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedMarker {
    pub commitment_scheme_version: u32,
    pub height: u64,
    pub block_hash: B256,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub new_root: B256,
}

impl FinalizedMarker {
    #[must_use]
    pub fn encode(self) -> [u8; FINALIZED_MARKER_ENCODED_LEN] {
        let mut bytes = [0_u8; FINALIZED_MARKER_ENCODED_LEN];
        bytes[0..4].copy_from_slice(&self.commitment_scheme_version.to_be_bytes());
        bytes[4..12].copy_from_slice(&self.height.to_be_bytes());
        bytes[12..44].copy_from_slice(self.block_hash.as_slice());
        bytes[44..76].copy_from_slice(self.parent_block_hash.as_slice());
        bytes[76..108].copy_from_slice(self.parent_root.as_slice());
        bytes[108..140].copy_from_slice(self.new_root.as_slice());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        if bytes.len() != FINALIZED_MARKER_ENCODED_LEN {
            return Err(PersistenceError::MalformedCodec {
                record: "last_applied",
                expected: "140 bytes",
                actual: bytes.len(),
            });
        }
        let mut decoder = Decoder::new(bytes, "last_applied");
        let marker = Self {
            commitment_scheme_version: decoder.u32()?,
            height: decoder.u64()?,
            block_hash: decoder.b256()?,
            parent_block_hash: decoder.b256()?,
            parent_root: decoder.b256()?,
            new_root: decoder.b256()?,
        };
        decoder.finish()?;
        validate_root(marker.parent_root)?;
        validate_root(marker.new_root)?;
        Ok(marker)
    }

    pub fn verify_exact_parent(
        self,
        required: ExactParentIdentity,
    ) -> Result<(), PersistenceError> {
        if self.commitment_scheme_version != required.commitment_scheme_version
            || self.height != required.block_number
            || self.block_hash != required.block_hash
            || self.new_root != required.root
        {
            return Err(PersistenceError::ExactParentMismatch {
                required,
                actual: self,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactParentIdentity {
    pub commitment_scheme_version: u32,
    pub block_number: u64,
    pub block_hash: B256,
    /// The root read from the exact parent's authoritative EVM slot.
    pub root: B256,
}

/// The durable finalized EVM/finality checkpoint used during restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableFinalizedCheckpoint {
    pub commitment_scheme_version: u32,
    pub height: u64,
    pub block_hash: B256,
    pub root: B256,
    pub parent_block_hash: B256,
    pub parent_root: B256,
    pub consensus_finalized_height: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartClassification {
    Equal,
    Behind { first_missing: u64, target: u64 },
    Ahead,
    Conflict,
}

pub fn classify_restart(
    marker: FinalizedMarker,
    durable: DurableFinalizedCheckpoint,
) -> RestartClassification {
    if marker.commitment_scheme_version != durable.commitment_scheme_version {
        return RestartClassification::Conflict;
    }
    if marker.height > durable.height || marker.height > durable.consensus_finalized_height {
        return RestartClassification::Ahead;
    }
    if marker.height < durable.height {
        return RestartClassification::Behind {
            first_missing: marker.height.saturating_add(1),
            target: durable.height.min(durable.consensus_finalized_height),
        };
    }
    if marker.block_hash == durable.block_hash
        && marker.new_root == durable.root
        && marker.parent_block_hash == durable.parent_block_hash
        && marker.parent_root == durable.parent_root
    {
        RestartClassification::Equal
    } else {
        RestartClassification::Conflict
    }
}

/// Local pruning fence. It is seeded only from a root-verified marker and moves
/// only after a known-successful CE transaction.
#[derive(Debug)]
pub struct CeRetentionCursor(AtomicU64);

impl CeRetentionCursor {
    #[must_use]
    pub const fn from_verified_marker(marker: FinalizedMarker) -> Self {
        Self(AtomicU64::new(marker.height))
    }

    #[must_use]
    pub fn height(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub fn advance_after_known_commit(
        &self,
        previous: FinalizedMarker,
        committed: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        if committed.height != previous.height.saturating_add(1) || self.height() != previous.height
        {
            return Err(PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: self.height(),
                previous: previous.height,
                committed: committed.height,
            });
        }
        self.0.store(committed.height, Ordering::Release);
        Ok(())
    }

    /// Advances by one after a known-successful CE apply, or confirms an
    /// already-observed commit. This covers retry after an MDBX commit whose
    /// first return path was ambiguous without weakening contiguous progress.
    pub fn advance_or_confirm_after_known_commit(
        &self,
        committed: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        let current = self.height();
        if current == committed.height {
            return Ok(());
        }
        if committed.height != current.saturating_add(1) {
            return Err(PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: current,
                previous: current,
                committed: committed.height,
            });
        }
        self.0
            .compare_exchange(
                current,
                committed.height,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|actual| PersistenceError::RetentionAdvanceOutOfOrder {
                cursor: actual,
                previous: current,
                committed: committed.height,
            })?;
        Ok(())
    }
}

/// Crash-injection stages for the finalized persistence/ACK boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FinalizationStage {
    Delivered,
    MarshalDurable,
    RethFinalized,
    RethPersisted,
    ProviderVerified,
    CeCommitUnknown,
    CeCommitted,
    RetentionAdvanced,
    CacheRemoved,
    MarshalAcknowledged,
}

impl FinalizationStage {
    #[must_use]
    pub fn marshal_ack_allowed(self) -> bool {
        matches!(self, Self::CacheRemoved | Self::MarshalAcknowledged)
    }

    #[must_use]
    pub fn restart_requires_marker_classification(self) -> bool {
        matches!(self, Self::CeCommitUnknown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied(FinalizedMarker),
    AlreadyApplied(FinalizedMarker),
}

mod tables {
    use reth_db::{
        table::{Table, TableInfo},
        TableSet,
    };

    #[derive(Debug)]
    pub struct CeMetadata;
    impl Table for CeMetadata {
        const NAME: &'static str = "OutbeCompressedEntitiesMetadataV1";
        const DUPSORT: bool = false;
        type Key = Vec<u8>;
        type Value = Vec<u8>;
    }
    impl TableInfo for CeMetadata {
        fn name(&self) -> &'static str {
            <Self as Table>::NAME
        }
        fn is_dupsort(&self) -> bool {
            <Self as Table>::DUPSORT
        }
    }

    #[derive(Debug)]
    pub struct CeBranches;
    impl Table for CeBranches {
        const NAME: &'static str = "OutbeCompressedEntitiesBranchesV1";
        const DUPSORT: bool = false;
        type Key = Vec<u8>;
        type Value = Vec<u8>;
    }
    impl TableInfo for CeBranches {
        fn name(&self) -> &'static str {
            <Self as Table>::NAME
        }
        fn is_dupsort(&self) -> bool {
            <Self as Table>::DUPSORT
        }
    }

    #[derive(Debug)]
    pub struct CeLeaves;
    impl Table for CeLeaves {
        const NAME: &'static str = "OutbeCompressedEntitiesLeavesV1";
        const DUPSORT: bool = false;
        type Key = Vec<u8>;
        type Value = Vec<u8>;
    }
    impl TableInfo for CeLeaves {
        fn name(&self) -> &'static str {
            <Self as Table>::NAME
        }
        fn is_dupsort(&self) -> bool {
            <Self as Table>::DUPSORT
        }
    }

    #[derive(Debug)]
    pub struct CeTables;
    impl TableSet for CeTables {
        fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
            Box::new(
                [
                    Box::new(CeMetadata) as Box<dyn TableInfo>,
                    Box::new(CeBranches) as Box<dyn TableInfo>,
                    Box::new(CeLeaves) as Box<dyn TableInfo>,
                ]
                .into_iter(),
            )
        }
    }
}

/// Separate CE-owned MDBX environment. It does not share Reth's primary DB.
#[derive(Debug)]
pub struct CeMdbx {
    path: PathBuf,
    identity: EnvironmentIdentity,
    db: DatabaseEnv,
}

impl CeMdbx {
    /// Opens `<datadir>/compressed_entities/smt/`, initializes an empty
    /// environment atomically, or verifies every existing identity field.
    pub fn open(
        datadir: &Path,
        expected_identity: EnvironmentIdentity,
        genesis_marker: FinalizedMarker,
    ) -> Result<Self, PersistenceError> {
        if expected_identity.local_storage_schema_version != LOCAL_STORAGE_SCHEMA_VERSION {
            return Err(PersistenceError::UnsupportedLocalSchema {
                actual: expected_identity.local_storage_schema_version,
            });
        }
        if expected_identity.tree_format.is_empty() || expected_identity.vendor_revision.is_empty()
        {
            return Err(PersistenceError::EmptyEnvironmentIdentityField);
        }
        if genesis_marker.height != 0 || genesis_marker.block_hash != expected_identity.genesis_hash
        {
            return Err(PersistenceError::InvalidGenesisMarker {
                expected_genesis_hash: expected_identity.genesis_hash,
                actual: genesis_marker,
            });
        }
        if genesis_marker.commitment_scheme_version != expected_identity.commitment_scheme_version {
            return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
        }
        validate_root(genesis_marker.parent_root)?;
        validate_root(genesis_marker.new_root)?;

        let path = datadir.join(CE_SMT_RELATIVE_PATH);
        std::fs::create_dir_all(&path).map_err(|error| PersistenceError::Io {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let args = DatabaseArguments::new(ClientVersion::default());
        let client_version = args.client_version().clone();
        let mut db = create_db(&path, args).map_err(|error| PersistenceError::Database {
            path: path.clone(),
            message: error.to_string(),
        })?;
        db.create_and_track_tables_for::<tables::CeTables>()
            .map_err(|error| PersistenceError::Database {
                path: path.clone(),
                message: error.to_string(),
            })?;
        db.record_client_version(client_version)
            .map_err(|error| PersistenceError::Database {
                path: path.clone(),
                message: error.to_string(),
            })?;

        let store = Self {
            path,
            identity: expected_identity.clone(),
            db,
        };
        store.initialize_or_verify(&expected_identity, genesis_marker)?;
        Ok(store)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn identity(&self) -> &EnvironmentIdentity {
        &self.identity
    }

    pub fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        tx.commit().map_err(|error| self.db_error(error))?;
        Ok(marker)
    }

    pub fn open_snapshot(&self) -> Result<Box<dyn FinalizedTreeSnapshot>, PersistenceError> {
        let tx = self.tx()?;
        let marker = read_marker(&tx, &self.path)?;
        Ok(Box::new(MdbxSnapshot {
            path: self.path.clone(),
            tx,
            marker,
        }))
    }

    /// Atomically applies one contiguous finalized batch and writes the marker
    /// last. A commit error is explicitly reported as an unknown outcome.
    pub fn apply_finalized(
        &self,
        batch: &StagedTreeBatch,
    ) -> Result<ApplyOutcome, PersistenceError> {
        batch
            .validate_encoded_size()
            .map_err(|error| PersistenceError::Staging(error.to_string()))?;
        validate_root(batch.parent_root)?;
        validate_root(batch.new_root)?;
        let next = batch.marker(self.identity.commitment_scheme_version);
        let current = self.marker()?;

        if next == current {
            return Ok(ApplyOutcome::AlreadyApplied(current));
        }
        if next.height <= current.height {
            return Err(PersistenceError::ConflictingFinalizedMarker { current, next });
        }
        if next.height != current.height.saturating_add(1)
            || next.parent_block_hash != current.block_hash
            || next.parent_root != current.new_root
            || next.commitment_scheme_version != current.commitment_scheme_version
        {
            return Err(PersistenceError::NonContiguousFinalizedApply { current, next });
        }

        let tx = self.db.tx_mut().map_err(|error| self.db_error(error))?;
        for (key, change) in &batch.branch_changes {
            let key = key.encode().to_vec();
            match change {
                TreeChange::Set(node) => tx
                    .put::<tables::CeBranches>(key, node.encode())
                    .map_err(|error| self.db_error(error))?,
                TreeChange::Delete => {
                    tx.delete::<tables::CeBranches>(key, None)
                        .map_err(|error| self.db_error(error))?;
                }
            }
        }
        for (key, change) in &batch.leaf_changes {
            let key = key.encode().to_vec();
            match change {
                TreeChange::Set(value) => tx
                    .put::<tables::CeLeaves>(key, value.encode().to_vec())
                    .map_err(|error| self.db_error(error))?,
                TreeChange::Delete => {
                    tx.delete::<tables::CeLeaves>(key, None)
                        .map_err(|error| self.db_error(error))?;
                }
            }
        }
        // Progress is deliberately the final write in this transaction.
        tx.put::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec(), next.encode().to_vec())
            .map_err(|error| self.db_error(error))?;
        tx.commit()
            .map_err(|error| PersistenceError::CommitOutcomeUnknown {
                path: self.path.clone(),
                marker: next,
                message: error.to_string(),
            })?;
        Ok(ApplyOutcome::Applied(next))
    }

    fn initialize_or_verify(
        &self,
        expected_identity: &EnvironmentIdentity,
        genesis_marker: FinalizedMarker,
    ) -> Result<(), PersistenceError> {
        let tx = self.db.tx().map_err(|error| self.db_error(error))?;
        let stored_identity = tx
            .get::<tables::CeMetadata>(IDENTITY_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let stored_marker = tx
            .get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
            .map_err(|error| self.db_error(error))?;
        let branch_records = tx
            .entries::<tables::CeBranches>()
            .map_err(|error| self.db_error(error))?;
        let leaf_records = tx
            .entries::<tables::CeLeaves>()
            .map_err(|error| self.db_error(error))?;
        tx.commit().map_err(|error| self.db_error(error))?;

        match (stored_identity, stored_marker) {
            (None, None) => {
                if branch_records != 0 || leaf_records != 0 {
                    return Err(PersistenceError::OrphanTreeRecords {
                        branches: branch_records,
                        leaves: leaf_records,
                    });
                }
                let identity = expected_identity.encode()?;
                let tx = self.db.tx_mut().map_err(|error| self.db_error(error))?;
                tx.put::<tables::CeMetadata>(IDENTITY_KEY.to_vec(), identity)
                    .map_err(|error| self.db_error(error))?;
                tx.put::<tables::CeMetadata>(
                    LAST_APPLIED_KEY.to_vec(),
                    genesis_marker.encode().to_vec(),
                )
                .map_err(|error| self.db_error(error))?;
                tx.commit()
                    .map_err(|error| PersistenceError::CommitOutcomeUnknown {
                        path: self.path.clone(),
                        marker: genesis_marker,
                        message: error.to_string(),
                    })?;
            }
            (Some(identity), Some(marker)) => {
                let actual_identity = EnvironmentIdentity::decode(&identity)?;
                if &actual_identity != expected_identity {
                    return Err(PersistenceError::EnvironmentIdentityMismatch {
                        expected: expected_identity.clone(),
                        actual: actual_identity,
                    });
                }
                let marker = FinalizedMarker::decode(&marker)?;
                if marker.commitment_scheme_version != expected_identity.commitment_scheme_version {
                    return Err(PersistenceError::EnvironmentMarkerSchemeMismatch);
                }
            }
            _ => return Err(PersistenceError::PartialEnvironmentInitialization),
        }
        Ok(())
    }

    fn tx(&self) -> Result<Tx<RO>, PersistenceError> {
        self.db.tx().map_err(|error| self.db_error(error))
    }

    fn db_error(&self, error: impl std::fmt::Display) -> PersistenceError {
        PersistenceError::Database {
            path: self.path.clone(),
            message: error.to_string(),
        }
    }
}

struct MdbxSnapshot {
    path: PathBuf,
    tx: Tx<RO>,
    marker: FinalizedMarker,
}

impl FinalizedTreeSnapshot for MdbxSnapshot {
    fn marker(&self) -> Result<FinalizedMarker, PersistenceError> {
        Ok(self.marker)
    }

    fn read_branch(&self, key: BranchKey) -> Result<Option<BranchNode>, PersistenceError> {
        self.tx
            .get::<tables::CeBranches>(key.encode().to_vec())
            .map_err(|error| PersistenceError::Database {
                path: self.path.clone(),
                message: error.to_string(),
            })?
            .map(|bytes| BranchNode::decode(&bytes))
            .transpose()
    }

    fn read_leaf(&self, key: TreeKey) -> Result<Option<LeafValue>, PersistenceError> {
        self.tx
            .get::<tables::CeLeaves>(key.encode().to_vec())
            .map_err(|error| PersistenceError::Database {
                path: self.path.clone(),
                message: error.to_string(),
            })?
            .map(|bytes| LeafValue::decode(&bytes))
            .transpose()
    }
}

fn read_marker(tx: &Tx<RO>, path: &Path) -> Result<FinalizedMarker, PersistenceError> {
    let bytes = tx
        .get::<tables::CeMetadata>(LAST_APPLIED_KEY.to_vec())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .ok_or(PersistenceError::MissingFinalizedMarker)?;
    FinalizedMarker::decode(&bytes)
}

pub(crate) fn validate_root(root: B256) -> Result<(), PersistenceError> {
    validate_field(root)
}

fn validate_field(value: B256) -> Result<(), PersistenceError> {
    if value == B256::repeat_byte(0xff) {
        return Err(PersistenceError::HashPoison);
    }
    let field = Fr::from_be_bytes_mod_order(value.as_slice());
    let bytes = field.into_bigint().to_bytes_be();
    let mut canonical = [0_u8; 32];
    canonical[32 - bytes.len()..].copy_from_slice(&bytes);
    if canonical != value.0 {
        return Err(PersistenceError::NonCanonicalField);
    }
    Ok(())
}

fn decode_b256(bytes: &[u8], record: &'static str) -> Result<B256, PersistenceError> {
    if bytes.len() != 32 {
        return Err(PersistenceError::MalformedCodec {
            record,
            expected: "32 bytes",
            actual: bytes.len(),
        });
    }
    Ok(B256::from_slice(bytes))
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    record: &'static str,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8], record: &'static str) -> Self {
        Self {
            bytes,
            offset: 0,
            record,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PersistenceError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(PersistenceError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(PersistenceError::MalformedCodec {
                record: self.record,
                expected: "complete deterministic record",
                actual: self.bytes.len(),
            })?;
        self.offset = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, PersistenceError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, PersistenceError> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(bytes))
    }

    fn b256(&mut self) -> Result<B256, PersistenceError> {
        Ok(B256::from_slice(self.take(32)?))
    }

    fn string_u16(&mut self) -> Result<String, PersistenceError> {
        let mut length = [0_u8; 2];
        length.copy_from_slice(self.take(2)?);
        let bytes = self.take(usize::from(u16::from_be_bytes(length)))?;
        String::from_utf8(bytes.to_vec()).map_err(|_| PersistenceError::InvalidUtf8 {
            record: self.record,
        })
    }

    fn finish(self) -> Result<(), PersistenceError> {
        if self.offset != self.bytes.len() {
            return Err(PersistenceError::TrailingBytes {
                record: self.record,
                trailing: self.bytes.len() - self.offset,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("I/O error at {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("MDBX error at {path}: {message}")]
    Database { path: PathBuf, message: String },
    #[error("MDBX commit outcome is unknown at {path} for marker {marker:?}: {message}")]
    CommitOutcomeUnknown {
        path: PathBuf,
        marker: FinalizedMarker,
        message: String,
    },
    #[error("unsupported CE local storage schema {actual}")]
    UnsupportedLocalSchema { actual: u32 },
    #[error("environment identity does not match: expected {expected:?}, actual {actual:?}")]
    EnvironmentIdentityMismatch {
        expected: EnvironmentIdentity,
        actual: EnvironmentIdentity,
    },
    #[error("environment identity and finalized marker are only partially initialized")]
    PartialEnvironmentInitialization,
    #[error(
        "CE MDBX contains tree records without identity/marker: {branches} branches, {leaves} leaves"
    )]
    OrphanTreeRecords { branches: usize, leaves: usize },
    #[error("environment and finalized marker commitment schemes differ")]
    EnvironmentMarkerSchemeMismatch,
    #[error("tree format and vendor revision must be non-empty")]
    EmptyEnvironmentIdentityField,
    #[error("invalid height-0 CE marker for genesis {expected_genesis_hash}: actual {actual:?}")]
    InvalidGenesisMarker {
        expected_genesis_hash: B256,
        actual: FinalizedMarker,
    },
    #[error("finalized marker is missing")]
    MissingFinalizedMarker,
    #[error("malformed {record}: expected {expected}, got {actual} bytes")]
    MalformedCodec {
        record: &'static str,
        expected: &'static str,
        actual: usize,
    },
    #[error("unknown MergeValue tag {0}")]
    UnknownMergeValueTag(u8),
    #[error("trailing bytes in {record}: {trailing}")]
    TrailingBytes {
        record: &'static str,
        trailing: usize,
    },
    #[error("invalid UTF-8 in {record}")]
    InvalidUtf8 { record: &'static str },
    #[error("deterministic record length overflow")]
    LengthOverflow,
    #[error("non-canonical BN254 field element")]
    NonCanonicalField,
    #[error("CKB/Poseidon HASH_ERROR poison value")]
    HashPoison,
    #[error("zero cannot be persisted as a leaf value")]
    ZeroPersistedLeaf,
    #[error("exact parent mismatch: required {required:?}, marker {actual:?}")]
    ExactParentMismatch {
        required: ExactParentIdentity,
        actual: FinalizedMarker,
    },
    #[error("conflicting finalized marker: current {current:?}, next {next:?}")]
    ConflictingFinalizedMarker {
        current: FinalizedMarker,
        next: FinalizedMarker,
    },
    #[error("non-contiguous finalized apply: current {current:?}, next {next:?}")]
    NonContiguousFinalizedApply {
        current: FinalizedMarker,
        next: FinalizedMarker,
    },
    #[error(
        "retention cursor advance out of order: cursor {cursor}, previous {previous}, committed {committed}"
    )]
    RetentionAdvanceOutOfOrder {
        cursor: u64,
        previous: u64,
        committed: u64,
    },
    #[error("staged batch rejected: {0}")]
    Staging(String),
}

impl From<StagingError> for PersistenceError {
    fn from(error: StagingError) -> Self {
        Self::Staging(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn b256(last: u8) -> B256 {
        let mut bytes = [0_u8; 32];
        bytes[31] = last;
        B256::from(bytes)
    }

    fn marker(height: u64) -> FinalizedMarker {
        FinalizedMarker {
            commitment_scheme_version: 1,
            height,
            block_hash: b256(u8::try_from(height).unwrap()),
            parent_block_hash: b256(u8::try_from(height.saturating_sub(1)).unwrap()),
            parent_root: b256(u8::try_from(height.saturating_add(10)).unwrap()),
            new_root: b256(u8::try_from(height.saturating_add(11)).unwrap()),
        }
    }

    fn identity() -> EnvironmentIdentity {
        EnvironmentIdentity {
            local_storage_schema_version: 1,
            chain_id: 99,
            genesis_hash: b256(42),
            commitment_scheme_version: 1,
            tree_format: "ckb-smt-v0.6.1-poseidon".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        }
    }

    #[test]
    fn typed_codecs_round_trip_and_reject_trailing_unknown_and_zero_leaf() {
        let field = FieldValue::try_from(b256(1)).unwrap();
        let node = BranchNode {
            left: MergeValue::Value(field),
            right: MergeValue::MergeWithZero {
                base_node: FieldValue::try_from(b256(2)).unwrap(),
                zero_bits: FieldValue::try_from(b256(3)).unwrap(),
                zero_count: 0,
            },
        };
        let encoded = node.encode();
        assert_eq!(BranchNode::decode(&encoded).unwrap(), node);
        let mut trailing = encoded;
        trailing.push(7);
        assert!(matches!(
            BranchNode::decode(&trailing),
            Err(PersistenceError::TrailingBytes { .. })
        ));
        assert!(matches!(
            BranchNode::decode(&[2; 66]),
            Err(PersistenceError::UnknownMergeValueTag(2))
        ));
        assert!(matches!(
            LeafValue::try_from(B256::ZERO),
            Err(PersistenceError::ZeroPersistedLeaf)
        ));
    }

    #[test]
    fn staged_tree_and_branch_keys_follow_ckb_reversed_byte_order() {
        let mut natural_high = [0_u8; 32];
        natural_high[0] = 2;
        let mut ckb_high = [0_u8; 32];
        ckb_high[31] = 1;
        let natural_high = B256::from(natural_high);
        let ckb_high = B256::from(ckb_high);

        assert!(TreeKey::try_from(natural_high).unwrap() < TreeKey::try_from(ckb_high).unwrap());
        assert!(BranchKey::new(7, natural_high).unwrap() < BranchKey::new(7, ckb_high).unwrap());
        assert!(BranchKey::new(6, ckb_high).unwrap() < BranchKey::new(7, natural_high).unwrap());
    }

    #[test]
    fn marker_and_environment_identity_have_deterministic_exact_codecs() {
        let value = marker(7);
        let encoded = value.encode();
        assert_eq!(encoded.len(), 140);
        assert_eq!(FinalizedMarker::decode(&encoded).unwrap(), value);
        assert!(FinalizedMarker::decode(&encoded[..139]).is_err());

        let environment = identity();
        let encoded = environment.encode().unwrap();
        assert_eq!(EnvironmentIdentity::decode(&encoded).unwrap(), environment);
        let mut trailing = encoded;
        trailing.push(1);
        assert!(matches!(
            EnvironmentIdentity::decode(&trailing),
            Err(PersistenceError::TrailingBytes { .. })
        ));
    }

    #[test]
    fn restart_rows_distinguish_equal_behind_ahead_and_conflict() {
        let current = marker(7);
        let equal = DurableFinalizedCheckpoint {
            commitment_scheme_version: 1,
            height: 7,
            block_hash: current.block_hash,
            root: current.new_root,
            parent_block_hash: current.parent_block_hash,
            parent_root: current.parent_root,
            consensus_finalized_height: 7,
        };
        assert_eq!(
            classify_restart(current, equal),
            RestartClassification::Equal
        );

        let behind = DurableFinalizedCheckpoint {
            height: 9,
            consensus_finalized_height: 9,
            ..equal
        };
        assert_eq!(
            classify_restart(current, behind),
            RestartClassification::Behind {
                first_missing: 8,
                target: 9
            }
        );

        let ahead_marker = marker(10);
        assert_eq!(
            classify_restart(ahead_marker, behind),
            RestartClassification::Ahead
        );
        assert_eq!(
            classify_restart(
                current,
                DurableFinalizedCheckpoint {
                    block_hash: b256(99),
                    ..equal
                }
            ),
            RestartClassification::Conflict
        );
        assert_eq!(
            classify_restart(
                current,
                DurableFinalizedCheckpoint {
                    parent_root: b256(98),
                    ..equal
                }
            ),
            RestartClassification::Conflict
        );
    }

    #[test]
    fn ack_and_retention_require_known_successful_commit() {
        for stage in [
            FinalizationStage::Delivered,
            FinalizationStage::MarshalDurable,
            FinalizationStage::RethFinalized,
            FinalizationStage::RethPersisted,
            FinalizationStage::ProviderVerified,
            FinalizationStage::CeCommitUnknown,
            FinalizationStage::CeCommitted,
            FinalizationStage::RetentionAdvanced,
        ] {
            assert!(!stage.marshal_ack_allowed());
        }
        assert!(FinalizationStage::CacheRemoved.marshal_ack_allowed());
        assert!(FinalizationStage::CeCommitUnknown.restart_requires_marker_classification());

        let previous = marker(7);
        let committed = marker(8);
        let cursor = CeRetentionCursor::from_verified_marker(previous);
        cursor
            .advance_after_known_commit(previous, committed)
            .unwrap();
        assert_eq!(cursor.height(), 8);
        assert!(cursor
            .advance_after_known_commit(previous, committed)
            .is_err());
    }

    #[test]
    fn no_change_batch_still_carries_a_complete_next_marker() {
        let batch = StagedTreeBatch {
            block_number: 8,
            block_hash: b256(8),
            parent_block_hash: b256(7),
            parent_root: b256(18),
            new_root: b256(18),
            branch_changes: BTreeMap::new(),
            leaf_changes: BTreeMap::new(),
            encoded_size: 0,
        };
        let next = batch.marker(1);
        assert_eq!(next.height, 8);
        assert_eq!(next.parent_root, next.new_root);
        assert_eq!(next.block_hash, b256(8));
    }

    #[test]
    fn mdbx_applies_contiguous_batches_atomically_and_reopens_exact_marker() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = FinalizedMarker {
            commitment_scheme_version: 1,
            height: 0,
            block_hash: identity().genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: B256::ZERO,
        };
        let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
        let tree_key = TreeKey::try_from(b256(3)).unwrap();
        let leaf = LeafValue::try_from(b256(4)).unwrap();
        let branch_key = BranchKey::new(1, b256(5)).unwrap();
        let branch = BranchNode {
            left: MergeValue::Value(FieldValue::try_from(b256(6)).unwrap()),
            right: MergeValue::Value(FieldValue::try_from(b256(7)).unwrap()),
        };
        let batch = crate::staging::ProvisionalTreeBatch::new(
            1,
            genesis.block_hash,
            genesis.new_root,
            b256(8),
            BTreeMap::from([(branch_key, TreeChange::Set(branch))]),
            BTreeMap::from([(tree_key, TreeChange::Set(leaf))]),
        )
        .unwrap()
        .freeze(b256(41));

        let applied = store.apply_finalized(&batch).unwrap();
        assert_eq!(applied, ApplyOutcome::Applied(batch.marker(1)));
        assert_eq!(
            store.apply_finalized(&batch).unwrap(),
            ApplyOutcome::AlreadyApplied(batch.marker(1))
        );
        let snapshot = store.open_snapshot().unwrap();
        assert_eq!(snapshot.marker().unwrap(), batch.marker(1));
        assert_eq!(snapshot.read_leaf(tree_key).unwrap(), Some(leaf));
        assert_eq!(snapshot.read_branch(branch_key).unwrap(), Some(branch));

        drop(snapshot);
        drop(store);
        let reopened = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
        assert_eq!(reopened.marker().unwrap(), batch.marker(1));
    }

    #[test]
    fn mdbx_rejects_gap_conflict_and_bad_batch_before_persistent_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = FinalizedMarker {
            commitment_scheme_version: 1,
            height: 0,
            block_hash: identity().genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: B256::ZERO,
        };
        let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
        let gap = crate::staging::ProvisionalTreeBatch::new(
            2,
            genesis.block_hash,
            genesis.new_root,
            b256(51),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap()
        .freeze(b256(52));
        assert!(matches!(
            store.apply_finalized(&gap),
            Err(PersistenceError::NonContiguousFinalizedApply { .. })
        ));

        let mut malformed = crate::staging::ProvisionalTreeBatch::new(
            1,
            genesis.block_hash,
            genesis.new_root,
            b256(51),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap()
        .freeze(b256(51));
        malformed.encoded_size = 1;
        assert!(matches!(
            store.apply_finalized(&malformed),
            Err(PersistenceError::Staging(_))
        ));
        assert_eq!(store.marker().unwrap(), genesis);
    }

    #[test]
    fn open_snapshot_remains_on_one_mdbx_read_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = FinalizedMarker {
            commitment_scheme_version: 1,
            height: 0,
            block_hash: identity().genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: B256::ZERO,
        };
        let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
        let old_snapshot = store.open_snapshot().unwrap();
        let tree_key = TreeKey::try_from(b256(10)).unwrap();
        let leaf = LeafValue::try_from(b256(11)).unwrap();
        let batch = crate::staging::ProvisionalTreeBatch::new(
            1,
            genesis.block_hash,
            genesis.new_root,
            b256(12),
            BTreeMap::new(),
            BTreeMap::from([(tree_key, TreeChange::Set(leaf))]),
        )
        .unwrap()
        .freeze(b256(61));
        store.apply_finalized(&batch).unwrap();

        assert_eq!(old_snapshot.marker().unwrap(), genesis);
        assert_eq!(old_snapshot.read_leaf(tree_key).unwrap(), None);
        let new_snapshot = store.open_snapshot().unwrap();
        assert_eq!(new_snapshot.marker().unwrap(), batch.marker(1));
        assert_eq!(new_snapshot.read_leaf(tree_key).unwrap(), Some(leaf));
    }

    #[test]
    fn reopen_rejects_environment_identity_drift() {
        let directory = tempfile::tempdir().unwrap();
        let genesis = FinalizedMarker {
            commitment_scheme_version: 1,
            height: 0,
            block_hash: identity().genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: B256::ZERO,
        };
        let store = CeMdbx::open(directory.path(), identity(), genesis).unwrap();
        drop(store);
        let mut wrong = identity();
        wrong.chain_id += 1;
        assert!(matches!(
            CeMdbx::open(directory.path(), wrong, genesis),
            Err(PersistenceError::EnvironmentIdentityMismatch { .. })
        ));
    }
}
