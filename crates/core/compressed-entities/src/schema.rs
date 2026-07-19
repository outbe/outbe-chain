use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::contract;
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    storage::types::{Mapping, Slot, StorageBytes, StorageVec},
};

use crate::{Commitment, EntityId36};

pub(crate) const STORAGE_SCHEMA_VERSION: u64 = 3;
const BODY_RECORD_VERSION: u8 = 1;
const INDEX_RECORD_VERSION: u8 = 1;
const BODY_LOCATOR_PREFIX: &[u8] = b"OUTBE_CE_OVERLAY_V1";
const INDEX_DELTA_PREFIX: &[u8] = b"OUTBE_CE_INDEX_DELTA_V1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Collection {
    Tribute,
    NodItem,
    NodBucket,
}

impl Collection {
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Tribute => 1,
            Self::NodItem => 2,
            Self::NodBucket => 3,
        }
    }

    pub(crate) fn from_id(id: u8) -> outbe_primitives::error::Result<Self> {
        match id {
            1 => Ok(Self::Tribute),
            2 => Ok(Self::NodItem),
            3 => Ok(Self::NodBucket),
            _ => Err(fatal(format!(
                "invalid compressed-entity collection id {id}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingWord {
    Untouched,
    Set(Commitment),
    Deleted,
}

impl PendingWord {
    pub(crate) fn decode(word: U256) -> outbe_primitives::error::Result<Self> {
        if word.is_zero() {
            return Ok(Self::Untouched);
        }
        if word == U256::MAX {
            return Ok(Self::Deleted);
        }
        Commitment::try_from(word.to_be_bytes::<32>())
            .map(Self::Set)
            .map_err(|_| fatal(format!("invalid compressed-entity pending word {word:#x}")))
    }

    pub(crate) fn encode(self) -> U256 {
        match self {
            Self::Untouched => U256::ZERO,
            Self::Set(commitment) => commitment.to_u256(),
            Self::Deleted => U256::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeltaStatus {
    NeverTouched,
    Added,
    Removed,
    NoChangeTouched,
}

impl DeltaStatus {
    pub(crate) fn decode(word: U256) -> outbe_primitives::error::Result<Self> {
        match word {
            value if value.is_zero() => Ok(Self::NeverTouched),
            value if value == U256::from(1) => Ok(Self::Added),
            value if value == U256::from(2) => Ok(Self::Removed),
            value if value == U256::from(3) => Ok(Self::NoChangeTouched),
            _ => Err(fatal(format!(
                "invalid compressed-entity index delta word {word:#x}"
            ))),
        }
    }

    pub(crate) const fn encode(self) -> U256 {
        match self {
            Self::NeverTouched => U256::ZERO,
            Self::Added => U256::from_limbs([1, 0, 0, 0]),
            Self::Removed => U256::from_limbs([2, 0, 0, 0]),
            Self::NoChangeTouched => U256::from_limbs([3, 0, 0, 0]),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum IndexKind {
    TributeByOwner,
    TributeByDay,
    NodByOwner,
    NodAll,
}

impl IndexKind {
    const fn id(self) -> u8 {
        match self {
            Self::TributeByOwner => 1,
            Self::TributeByDay => 2,
            Self::NodByOwner => 3,
            Self::NodAll => 4,
        }
    }

    fn from_id(id: u8) -> outbe_primitives::error::Result<Self> {
        match id {
            1 => Ok(Self::TributeByOwner),
            2 => Ok(Self::TributeByDay),
            3 => Ok(Self::NodByOwner),
            4 => Ok(Self::NodAll),
            _ => Err(fatal(format!("invalid compressed-entity index kind {id}"))),
        }
    }

    const fn partition_len(self) -> usize {
        match self {
            Self::TributeByOwner | Self::NodByOwner => 20,
            Self::TributeByDay => 4,
            Self::NodAll => 0,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct IndexRecord {
    pub(crate) kind: IndexKind,
    pub(crate) partition: Vec<u8>,
    pub(crate) entity_id: EntityId36,
}

impl IndexRecord {
    pub(crate) fn owner(kind: IndexKind, owner: Address, entity_id: EntityId36) -> Self {
        Self {
            kind,
            partition: owner.as_slice().to_vec(),
            entity_id,
        }
    }

    pub(crate) fn day(day: u32, entity_id: EntityId36) -> Self {
        Self {
            kind: IndexKind::TributeByDay,
            partition: day.to_be_bytes().to_vec(),
            entity_id,
        }
    }

    pub(crate) fn nod_all(entity_id: EntityId36) -> Self {
        Self {
            kind: IndexKind::NodAll,
            partition: Vec::new(),
            entity_id,
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut record = Vec::with_capacity(3 + self.partition.len() + EntityId36::LEN);
        record.push(INDEX_RECORD_VERSION);
        record.push(self.kind.id());
        record.push(self.partition.len() as u8);
        record.extend_from_slice(&self.partition);
        record.extend_from_slice(self.entity_id.as_bytes());
        record
    }

    pub(crate) fn decode(bytes: &[u8]) -> outbe_primitives::error::Result<Self> {
        if bytes.len() < 3 + EntityId36::LEN {
            return Err(fatal("compressed-entity index record is truncated"));
        }
        if bytes[0] != INDEX_RECORD_VERSION {
            return Err(fatal(format!(
                "unsupported compressed-entity index record version {}",
                bytes[0]
            )));
        }
        let kind = IndexKind::from_id(bytes[1])?;
        let partition_len = usize::from(bytes[2]);
        if partition_len != kind.partition_len()
            || bytes.len() != 3 + partition_len + EntityId36::LEN
        {
            return Err(fatal("non-canonical compressed-entity index record length"));
        }
        let partition = bytes[3..3 + partition_len].to_vec();
        let entity_id = EntityId36::try_from(&bytes[3 + partition_len..])
            .map_err(|error| fatal(error.to_string()))?;
        let record = Self {
            kind,
            partition,
            entity_id,
        };
        if record.encode() != bytes {
            return Err(fatal("non-canonical compressed-entity index record"));
        }
        Ok(record)
    }

    pub(crate) fn key(&self) -> B256 {
        index_delta_key(&self.encode())
    }
}

pub(crate) fn body_identity_record(collection: Collection, entity_id: EntityId36) -> [u8; 38] {
    let mut record = [0_u8; 38];
    record[0] = BODY_RECORD_VERSION;
    record[1] = collection.id();
    record[2..].copy_from_slice(entity_id.as_bytes());
    record
}

pub(crate) fn decode_body_identity_record(
    bytes: &[u8],
) -> outbe_primitives::error::Result<(Collection, EntityId36)> {
    if bytes.len() != 38 || bytes[0] != BODY_RECORD_VERSION {
        return Err(fatal(
            "non-canonical compressed-entity body identity record",
        ));
    }
    let collection = Collection::from_id(bytes[1])?;
    let entity_id = EntityId36::try_from(&bytes[2..]).map_err(|error| fatal(error.to_string()))?;
    Ok((collection, entity_id))
}

pub(crate) fn body_locator(
    collection: Collection,
    entity_id: EntityId36,
) -> outbe_primitives::error::Result<B256> {
    let identity = crate::identity_field(entity_id).map_err(|error| fatal(error.to_string()))?;
    let mut preimage = Vec::with_capacity(BODY_LOCATOR_PREFIX.len() + 1 + 32);
    preimage.extend_from_slice(BODY_LOCATOR_PREFIX);
    preimage.push(collection.id());
    preimage.extend_from_slice(&identity);
    Ok(keccak256(preimage))
}

pub(crate) fn index_delta_key(record: &[u8]) -> B256 {
    let mut preimage = Vec::with_capacity(INDEX_DELTA_PREFIX.len() + record.len());
    preimage.extend_from_slice(INDEX_DELTA_PREFIX);
    preimage.extend_from_slice(record);
    keccak256(preimage)
}

fn fatal(message: impl Into<String>) -> outbe_primitives::error::PrecompileError {
    outbe_primitives::error::PrecompileError::Fatal(message.into())
}

/// Consensus storage schema at `0xEE0D`.
///
/// Field order is protocol-critical: the declaration order below is exactly
/// slots 0 through 12 through ADR-011. Slot 1 is the sole EVM authority for the
/// compressed-entity tree. Slots 2 and 3 deliberately remain reserved; they
/// must not be reused as direct commitment mappings.
#[contract(addr = COMPRESSED_ENTITIES_ADDRESS)]
pub(crate) struct CompressedEntitiesSchema {
    /// Slot 0.
    pub storage_schema_version: Slot<u64>,
    /// Slot 1.
    pub last_smt_root: Slot<U256>,
    /// Slot 2.
    pub reserved_2: Slot<U256>,
    /// Slot 3.
    pub reserved_3: Slot<U256>,
    /// Slot 4.
    pub pending_word: Mapping<B256, U256>,
    /// Slot 5.
    pub pending_body: Mapping<B256, StorageBytes>,
    /// Slot 6.
    pub touched: StorageVec<B256>,
    /// Slot 7.
    pub index_delta_word: Mapping<B256, U256>,
    /// Slot 8.
    pub index_delta_record: Mapping<B256, StorageBytes>,
    /// Slot 9.
    pub touched_index_deltas: StorageVec<B256>,
    /// Slot 10.
    pub body_identity_record: Mapping<B256, StorageBytes>,
    /// Slot 11. Marker `1` means one trusted Tribute WWD retirement request.
    pub pending_retirement: Mapping<B256, U256>,
    /// Slot 12. Canonical WWD identities for unique first-touch cleanup/sealing.
    pub retirement_touched: StorageVec<u32>,
}
