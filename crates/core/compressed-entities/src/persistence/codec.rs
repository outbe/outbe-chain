use alloy_primitives::B256;
use ark_bn254::Fr;
use ark_ff::BigInteger;
use ark_ff::PrimeField;
use std::cmp::Ordering as CmpOrdering;

use super::PersistenceError;
use crate::staging::ShardIndex;
use crate::CollectionKey;
use crate::K_PROVISIONAL;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TreeNamespace {
    Catalog,
    CollectionShard(CollectionKey, ShardIndex),
}

impl TreeNamespace {
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::Catalog => vec![0],
            Self::CollectionShard(collection, shard) => {
                let mut bytes = Vec::with_capacity(37);
                bytes.push(1);
                bytes.extend_from_slice(collection.as_bytes());
                bytes.extend_from_slice(&shard.to_be_bytes());
                bytes
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        match bytes {
            [0] => Ok(Self::Catalog),
            [1, collection @ .., a, b, c, d] if collection.len() == 32 => {
                let collection = CollectionKey::try_from(B256::from_slice(collection))
                    .map_err(|_| PersistenceError::NonCanonicalTreeNamespace)?;
                let shard = u32::from_be_bytes([*a, *b, *c, *d]);
                if shard >= K_PROVISIONAL {
                    return Err(PersistenceError::InvalidNamespaceShard { shard });
                }
                Ok(Self::CollectionShard(collection, shard))
            }
            _ => Err(PersistenceError::NonCanonicalTreeNamespace),
        }
    }
}

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

pub(super) fn decode_b256(bytes: &[u8], record: &'static str) -> Result<B256, PersistenceError> {
    if bytes.len() != 32 {
        return Err(PersistenceError::MalformedCodec {
            record,
            expected: "32 bytes",
            actual: bytes.len(),
        });
    }
    Ok(B256::from_slice(bytes))
}

pub(super) struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
    record: &'static str,
}

impl<'a> Decoder<'a> {
    pub(super) const fn new(bytes: &'a [u8], record: &'static str) -> Self {
        Self {
            bytes,
            offset: 0,
            record,
        }
    }

    pub(super) fn take(&mut self, len: usize) -> Result<&'a [u8], PersistenceError> {
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

    pub(super) fn u32(&mut self) -> Result<u32, PersistenceError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(bytes))
    }

    pub(super) fn u16(&mut self) -> Result<u16, PersistenceError> {
        let mut bytes = [0_u8; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_be_bytes(bytes))
    }

    pub(super) fn u64(&mut self) -> Result<u64, PersistenceError> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(bytes))
    }

    pub(super) fn b256(&mut self) -> Result<B256, PersistenceError> {
        Ok(B256::from_slice(self.take(32)?))
    }

    pub(super) fn string_u16(&mut self) -> Result<String, PersistenceError> {
        let mut length = [0_u8; 2];
        length.copy_from_slice(self.take(2)?);
        let bytes = self.take(usize::from(u16::from_be_bytes(length)))?;
        String::from_utf8(bytes.to_vec()).map_err(|_| PersistenceError::InvalidUtf8 {
            record: self.record,
        })
    }

    pub(super) fn finish(self) -> Result<(), PersistenceError> {
        if self.offset != self.bytes.len() {
            return Err(PersistenceError::TrailingBytes {
                record: self.record,
                trailing: self.bytes.len() - self.offset,
            });
        }
        Ok(())
    }
}
