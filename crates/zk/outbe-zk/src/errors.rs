//! Single error type for the generic protocol core.

use thiserror::Error;

/// Errors that cryptographic primitives and protocol logic return.
#[derive(Debug, Error)]
pub enum Error {
    /// A hash or KDF operation failed.
    #[error("hash error: {0}")]
    Hash(String),

    /// A curve point was the identity when the protocol requires a non-identity point.
    #[error("unexpected identity point: {0}")]
    Identity(&'static str),

    /// A signature failed verification or signing failed.
    #[error("signature error: {0}")]
    Signature(&'static str),

    /// The owner commitment did not match the entity owner value.
    #[error("owner mismatch: commit(pk, nonce) != entity.owner()")]
    OwnerMismatch,

    /// A value is outside the field modulus or target range.
    /// Conversion would silently reduce or truncate the value.
    #[error("non-canonical field encoding: {0}")]
    NonCanonical(&'static str),

    /// An entity vector is not a canonical set.
    /// Elements must be strictly ascending with no duplicates.
    /// The caller must sort and deduplicate the vector before encoding.
    #[error("non-canonical set encoding: {0}")]
    UnsortedSet(String),

    /// A zero-knowledge proof backend operation failed.
    #[error("zk proof error: {0}")]
    Proof(String),

    /// A Merkle tree operation failed because of an invalid depth, index, or path.
    #[error("merkle error: {0}")]
    Merkle(String),
}

impl From<outbe_poseidon::Error> for Error {
    fn from(error: outbe_poseidon::Error) -> Self {
        Self::Hash(error.to_string())
    }
}

/// Typed reasons for [`Error::Identity`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum IdentityError {
    /// Curve point was the point at infinity when extracting affine coordinates.
    #[error("curve point coords")]
    CurvePointCoords,
}

impl IdentityError {
    /// Static message preserved in [`Error::Identity`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CurvePointCoords => "curve point coords",
        }
    }
}

impl From<IdentityError> for Error {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error.as_str())
    }
}

/// Typed reasons for [`Error::Signature`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum SignatureError {
    /// Compressed ark-serialize write failed.
    #[error("serialize failed")]
    SerializeFailed,
    /// Compressed serialization did not fill the 32-byte buffer.
    #[error("serialized length != 32")]
    SerializedLengthMismatch,
    /// Compressed Grumpkin scalar deserialization failed.
    #[error("bad scalar")]
    BadScalar,
    /// Compressed Grumpkin public key deserialization failed.
    #[error("bad public key")]
    BadPublicKey,
}

impl SignatureError {
    /// Static message preserved in [`Error::Signature`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SerializeFailed => "serialize failed",
            Self::SerializedLengthMismatch => "serialized length != 32",
            Self::BadScalar => "bad scalar",
            Self::BadPublicKey => "bad public key",
        }
    }
}

impl From<SignatureError> for Error {
    fn from(error: SignatureError) -> Self {
        Self::Signature(error.as_str())
    }
}

/// Typed reasons for [`Error::NonCanonical`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum NonCanonicalError {
    /// Field value is not a canonical boolean (`0` or `1`).
    #[error("bool")]
    Bool,
    /// Slice passed to `fields_to_u256` did not have length 3.
    #[error("uint256 requires three field limbs")]
    Uint256RequiresThreeFieldLimbs,
    /// `uint256` limb values exceeded their `[120, 120, 16]`-bit bounds.
    #[error("uint256 limbs")]
    Uint256Limbs,
    /// Public-input word is not a canonical BN254 field element.
    #[error("public input")]
    PublicInput,
}

impl NonCanonicalError {
    /// Static message preserved in [`Error::NonCanonical`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Uint256RequiresThreeFieldLimbs => "uint256 requires three field limbs",
            Self::Uint256Limbs => "uint256 limbs",
            Self::PublicInput => "public input",
        }
    }
}

impl From<NonCanonicalError> for Error {
    fn from(error: NonCanonicalError) -> Self {
        Self::NonCanonical(error.as_str())
    }
}

/// Typed reasons for [`Error::UnsortedSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum UnsortedSetError {
    /// Entity vector field elements are not strictly ascending by canonical field value.
    #[error(
        "entity vector field must be strictly ascending by field value (sorted + de-duplicated)"
    )]
    EntityVectorNotStrictlyAscending,
}

impl From<UnsortedSetError> for Error {
    fn from(error: UnsortedSetError) -> Self {
        Self::UnsortedSet(error.to_string())
    }
}

/// Typed reasons for [`Error::Merkle`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MerkleError {
    /// Inclusion path depth did not match the expected circuit depth.
    #[error("expected an inclusion path of depth {expected}, got {actual}")]
    InclusionPathDepthMismatch {
        /// Expected inclusion path depth.
        expected: usize,
        /// Actual inclusion path depth.
        actual: usize,
    },
    /// Inclusion path domain did not match the circuit's tree domain.
    #[error("inclusion path uses the wrong tree domain")]
    WrongInclusionPathDomain,
    /// Sibling slice length did not match the fixed circuit array length.
    #[error("siblings length")]
    SiblingsLength,
    /// Direction-index vector length did not match the fixed circuit array length.
    #[error("indices length")]
    IndicesLength,
    /// Tree depth is outside the supported `1..=63` range.
    #[error("tree depth must be in 1..=63, got {0}")]
    InvalidTreeDepth(usize),
    /// `empty_roots` slice length did not equal `frontier.len() + 1`.
    #[error("frontier_append: empty_roots.len() ({actual}) must be frontier.len()+1 ({expected})")]
    FrontierAppendEmptyRootsLength {
        /// Actual `empty_roots` slice length.
        actual: usize,
        /// Expected `empty_roots` slice length (`depth + 1`).
        expected: usize,
    },
    /// `next_index` is outside the capacity of a tree of `depth`.
    #[error("frontier_append: next_index {next_index} overflows depth-{depth} tree")]
    FrontierAppendIndexOverflow {
        /// Leaf index being appended.
        next_index: u64,
        /// Tree depth.
        depth: usize,
    },
    /// `leaf_index` is outside the capacity of a tree of `depth` when computing a root.
    #[error("root_from_inclusion_path: leaf_index {leaf_index} overflows depth-{depth} tree")]
    InclusionPathLeafIndexOverflow {
        /// Leaf index in the inclusion path.
        leaf_index: u64,
        /// Tree depth.
        depth: usize,
    },
    /// Commitment tree has reached its maximum capacity (`2^depth` leaves).
    #[error("commitment tree is full")]
    CommitmentTreeFull,
    /// Leaf index does not fit in `usize`.
    #[error("leaf index exceeds usize")]
    LeafIndexExceedsUsize,
    /// Leaf index has not been appended to the tree yet.
    #[error("leaf index outside tree")]
    LeafIndexOutsideTree,
    /// `leaf_index` is outside the capacity of a tree of `depth` when computing circuit indices.
    #[error("circuit_indices: leaf_index {leaf_index} overflows depth-{depth} tree")]
    CircuitIndicesLeafIndexOverflow {
        /// Leaf index in the inclusion path.
        leaf_index: u64,
        /// Tree depth.
        depth: usize,
    },
}

impl From<MerkleError> for Error {
    fn from(error: MerkleError) -> Self {
        Self::Merkle(error.to_string())
    }
}

/// Typed reasons for [`crate::wire::ProofMarshalingError::MalformedAbi`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum MalformedAbiReason {
    /// Dynamic offset word exceeds `u64` or `usize`.
    #[error("offset too large")]
    OffsetTooLarge,
    /// Dynamic offset is not the canonical 64-byte two-head offset.
    #[error("non-canonical offset")]
    NonCanonicalOffset,
    /// Dynamic offset plus 32-byte length word overflows `usize`.
    #[error("offset overflow")]
    OffsetOverflow,
    /// Input ends before the end of the dynamic length word.
    #[error("offset past end")]
    OffsetPastEnd,
    /// Dynamic byte-length word exceeds `u64` or `usize`.
    #[error("length too large")]
    LengthTooLarge,
    /// Header end plus payload length overflows `usize`.
    #[error("length overflow")]
    LengthOverflow,
    /// Input ends before the declared payload length.
    #[error("payload truncated")]
    PayloadTruncated,
    /// Payload end plus 32-byte ABI tail padding overflows `usize`.
    #[error("padding overflow")]
    PaddingOverflow,
    /// Input length does not equal the canonical padded ABI length.
    #[error("non-canonical total length")]
    NonCanonicalTotalLength,
    /// ABI tail padding bytes are not all zero.
    #[error("non-zero padding")]
    NonZeroPadding,
}

impl MalformedAbiReason {
    /// Static message preserved in [`crate::wire::ProofMarshalingError::MalformedAbi`].
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OffsetTooLarge => "offset too large",
            Self::NonCanonicalOffset => "non-canonical offset",
            Self::OffsetOverflow => "offset overflow",
            Self::OffsetPastEnd => "offset past end",
            Self::LengthTooLarge => "length too large",
            Self::LengthOverflow => "length overflow",
            Self::PayloadTruncated => "payload truncated",
            Self::PaddingOverflow => "padding overflow",
            Self::NonCanonicalTotalLength => "non-canonical total length",
            Self::NonZeroPadding => "non-zero padding",
        }
    }
}
