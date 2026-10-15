//! Typed error definitions for the OcompRegistry system precompile.

use outbe_ocomp_protocol::ProtocolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Domain errors for the OCOMP protocol registry and fork-install manifest.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OcompRegistryError {
    /// Underlying OCOMP protocol validation or codec failure.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// Unknown fork-install classification byte.
    #[error("unknown OCOMP fork-install classification")]
    UnknownForkInstallClassification,

    /// Fork activation height is zero.
    #[error("OCOMP fork activation height must be non-zero")]
    ForkActivationHeightZero,

    /// Final PoC fork activation height differs from 32.
    #[error("final OCOMP PoC fork must activate at height 32")]
    FinalPocForkActivationHeightMismatch,

    /// Fork-install chain identity does not match expected chain ID or genesis hash.
    #[error("OCOMP fork-install chain identity mismatch")]
    ForkInstallChainIdentityMismatch,

    /// Initial protocol bundle version is not 1.
    #[error("unsupported initial OCOMP protocol bundle version")]
    UnsupportedInitialProtocolBundleVersion,

    /// Consensus validator bound exceeds `usize`.
    #[error("consensus validator bound exceeds usize")]
    ConsensusValidatorBoundExceedsUsize,

    /// Founder registration count is outside `1..=max`.
    #[error("OCOMP founder registration count must be 1..={max}")]
    FounderRegistrationCountOutOfBounds {
        /// Maximum allowed founder registration count.
        max: usize,
    },

    /// Founder registration is invalid or duplicated.
    #[error("invalid or duplicate OCOMP founder registration")]
    InvalidOrDuplicateFounderRegistration,

    /// Fork-install total length overflowed `usize`.
    #[error("OCOMP fork-install length overflow")]
    ForkInstallLengthOverflow,

    /// Founder registration count exceeds `u32::MAX`.
    #[error("OCOMP founder count exceeds u32")]
    FounderCountExceedsU32,

    /// Fork-install magic or version header mismatch.
    #[error("OCOMP fork-install magic/version mismatch")]
    ForkInstallMagicOrVersionMismatch,

    /// Decoded founder registration count exceeds `usize::MAX`.
    #[error("OCOMP founder count exceeds usize")]
    FounderCountExceedsUsize,

    /// Decoded founder registration count is zero or exceeds maximum.
    #[error("invalid OCOMP founder registration count")]
    InvalidFounderRegistrationCount,

    /// Trailing bytes remain after decoding fork-install manifest.
    #[error("trailing OCOMP fork-install bytes")]
    TrailingForkInstallBytes,

    /// Re-encoded fork-install manifest differs from input bytes.
    #[error("non-canonical OCOMP fork-install encoding")]
    NonCanonicalForkInstallEncoding,

    /// Fork-install field length exceeds `u32::MAX`.
    #[error("OCOMP fork-install field exceeds u32")]
    ForkInstallFieldExceedsU32,

    /// Fork-install nested field exceeds maximum allocation byte cap.
    #[error("OCOMP fork-install field exceeds byte cap")]
    ForkInstallFieldExceedsByteCap,

    /// Fork-install total length is below minimum or exceeds byte cap.
    #[error("OCOMP fork-install exceeds byte cap")]
    ForkInstallExceedsByteCap,

    /// Reader offset overflowed while decoding fork-install manifest.
    #[error("OCOMP fork-install offset overflow")]
    ForkInstallOffsetOverflow,

    /// Fork-install manifest is truncated.
    #[error("truncated OCOMP fork-install")]
    TruncatedForkInstall,

    /// Fixed-width slice conversion failed while decoding fork-install manifest.
    #[error("OCOMP fork-install fixed-width decode")]
    ForkInstallFixedWidthDecode,

    /// Fork-install bounded field length exceeds `usize::MAX`.
    #[error("OCOMP field length exceeds usize")]
    FieldLengthExceedsUsize,

    /// Reader offset overflowed while decoding a bounded fork-install field.
    #[error("OCOMP field offset overflow")]
    FieldOffsetOverflow,

    /// Fork-install bounded field is truncated.
    #[error("truncated OCOMP fork-install field")]
    TruncatedForkInstallField,

    /// Protocol version increment overflowed `u16::MAX`.
    #[error("OCOMP protocol version overflow")]
    ProtocolVersionOverflow,

    /// Successor violates predecessor binding or immutable policy invariants.
    #[error("OCOMP successor violates predecessor or immutable-policy invariants")]
    SuccessorInvariantViolation,

    /// Encoded successor authority length exceeds `u32::MAX`.
    #[error("OCOMP successor authority exceeds u32")]
    SuccessorAuthorityExceedsU32,

    /// Successor header is truncated.
    #[error("truncated OCOMP successor")]
    TruncatedSuccessor,

    /// Successor magic or version header mismatch.
    #[error("OCOMP successor magic/version mismatch")]
    SuccessorMagicOrVersionMismatch,

    /// Successor activation height field is truncated.
    #[error("truncated OCOMP successor height")]
    TruncatedSuccessorHeight,

    /// Successor authority length field is truncated.
    #[error("truncated OCOMP successor authority length")]
    TruncatedSuccessorAuthorityLength,

    /// Successor authority length exceeds `usize::MAX`.
    #[error("OCOMP successor authority length exceeds usize")]
    SuccessorAuthorityLengthExceedsUsize,

    /// Successor authority length does not match remaining payload bytes.
    #[error("OCOMP successor authority length mismatch")]
    SuccessorAuthorityLengthMismatch,

    /// Re-encoded successor differs from input bytes.
    #[error("non-canonical OCOMP successor encoding")]
    NonCanonicalSuccessorEncoding,

    /// Protocol bundle fields differ from request profile fields.
    #[error("OCOMP protocol bundle differs from the request profile")]
    ProtocolBundleDiffersFromRequestProfile,

    /// Encoded authority request profile length exceeds `u32::MAX`.
    #[error("OCOMP authority profile exceeds u32")]
    AuthorityProfileExceedsU32,

    /// Encoded authority protocol bundle length exceeds `u32::MAX`.
    #[error("OCOMP authority bundle exceeds u32")]
    AuthorityBundleExceedsU32,

    /// Authority magic header mismatch.
    #[error("OCOMP authority magic mismatch")]
    AuthorityMagicMismatch,

    /// Authority request profile length field is truncated.
    #[error("truncated OCOMP authority profile length")]
    TruncatedAuthorityProfileLength,

    /// Authority request profile length exceeds `usize::MAX`.
    #[error("OCOMP authority profile length exceeds usize")]
    AuthorityProfileLengthExceedsUsize,

    /// Authority request profile end offset overflowed `usize`.
    #[error("OCOMP authority profile length overflow")]
    AuthorityProfileLengthOverflow,

    /// Authority protocol bundle length offset overflowed `usize`.
    #[error("OCOMP authority bundle offset overflow")]
    AuthorityBundleOffsetOverflow,

    /// Authority request profile payload is truncated.
    #[error("truncated OCOMP authority profile")]
    TruncatedAuthorityProfile,

    /// Authority protocol bundle length field is truncated.
    #[error("truncated OCOMP authority bundle length")]
    TruncatedAuthorityBundleLength,

    /// Authority protocol bundle length exceeds `usize::MAX`.
    #[error("OCOMP authority bundle length exceeds usize")]
    AuthorityBundleLengthExceedsUsize,

    /// Authority protocol bundle end offset overflowed `usize`.
    #[error("OCOMP authority bundle length overflow")]
    AuthorityBundleLengthOverflow,

    /// Authority protocol bundle end offset does not match input length.
    #[error("OCOMP authority bundle length mismatch")]
    AuthorityBundleLengthMismatch,

    /// Re-encoded authority differs from input bytes.
    #[error("non-canonical OCOMP authority encoding")]
    NonCanonicalAuthorityEncoding,

    /// Canonical object is empty or exceeds maximum allocation byte cap.
    #[error("OCOMP Registry canonical object exceeds byte cap")]
    CanonicalObjectExceedsByteCap,

    /// Request profile contains a zero chain ID or zero hash identifier.
    #[error("OCOMP request profile contains a reserved zero identity")]
    RequestProfileReservedZeroIdentity,

    /// Request profile capacity parameters violate frozen PoC bounds.
    #[error("OCOMP request profile violates frozen PoC bounds")]
    RequestProfileViolatesFrozenPocBounds,

    /// Generated candidate unit size exceeds `u32::MAX`.
    #[error("generated unit size exceeds u32")]
    GeneratedUnitSizeExceedsU32,

    /// Generated reference-currency cap exceeds `u16::MAX`.
    #[error("generated reference-currency cap exceeds u16")]
    GeneratedReferenceCurrencyCapExceedsU16,

    /// Generated Oracle entry cap exceeds `u32::MAX`.
    #[error("generated Oracle entry cap exceeds u32")]
    GeneratedOracleEntryCapExceedsU32,

    /// Generated S-curve entry cap exceeds `u32::MAX`.
    #[error("generated S-curve entry cap exceeds u32")]
    GeneratedScurveEntryCapExceedsU32,

    /// Encoding of the capacity profile failed.
    #[error("encode OCOMP capacity profile: {0}")]
    EncodeCapacityProfile(ProtocolError),

    /// Encoded capacity profile length exceeds `u32::MAX`.
    #[error("capacity profile length exceeds u32")]
    CapacityProfileLengthExceedsU32,

    /// Request profile total length overflowed `usize`.
    #[error("OCOMP request profile length overflow")]
    RequestProfileLengthOverflow,

    /// Memory reservation for encoded request profile failed.
    #[error("allocate OCOMP request profile")]
    AllocateRequestProfile,

    /// Encoded request profile length does not match expected total length.
    #[error("OCOMP request profile encoded length mismatch")]
    RequestProfileEncodedLengthMismatch,

    /// Request profile bytes are shorter than the fixed header or truncated.
    #[error("truncated OCOMP request profile")]
    TruncatedRequestProfile,

    /// Request profile magic or version header mismatch.
    #[error("OCOMP request profile magic/version mismatch")]
    RequestProfileMagicOrVersionMismatch,

    /// Decoded capacity profile length exceeds `usize::MAX`.
    #[error("OCOMP capacity profile length exceeds usize")]
    CapacityProfileLengthExceedsUsize,

    /// Decoded capacity profile length does not match remaining bytes.
    #[error("OCOMP capacity profile length mismatch")]
    CapacityProfileLengthMismatch,

    /// Decoding of the capacity profile failed.
    #[error("decode OCOMP capacity profile: {0}")]
    DecodeCapacityProfile(ProtocolError),

    /// Re-encoded request profile differs from input bytes.
    #[error("non-canonical OCOMP request profile encoding")]
    NonCanonicalRequestProfileEncoding,

    /// Reader offset overflowed while decoding request profile.
    #[error("OCOMP request profile offset overflow")]
    RequestProfileOffsetOverflow,

    /// Fixed-width slice conversion failed while decoding request profile.
    #[error("invalid OCOMP request profile field width")]
    InvalidRequestProfileFieldWidth,

    /// Dynamic field in request profile is truncated.
    #[error("truncated OCOMP request profile field")]
    TruncatedRequestProfileField,

    /// Trailing bytes remain after decoding request profile.
    #[error("OCOMP request profile has trailing bytes")]
    RequestProfileTrailingBytes,

    /// Active authority storage slots are only partially populated.
    #[error("OCOMP Registry active authority is partial")]
    ActiveAuthorityPartial,

    /// Authority byte cap computation overflowed `usize`.
    #[error("OCOMP Registry authority byte cap overflow")]
    AuthorityByteCapOverflow,

    /// Stored active authority exceeds maximum allocation byte cap.
    #[error("OCOMP Registry active authority exceeds byte cap")]
    ActiveAuthorityExceedsByteCap,

    /// Genesis authority installation was attempted outside its activation height.
    #[error("OCOMP Registry install attempted outside its activation height")]
    InstallOutsideActivationHeight,

    /// Genesis install hash is zero.
    #[error("OCOMP Registry install hash must be non-zero")]
    InstallHashZero,

    /// Request profile chain identity does not match storage chain identity.
    #[error("OCOMP Registry chain identity mismatch")]
    ChainIdentityMismatch,

    /// Conflicting genesis authority replay attempted to mutate installed authority.
    #[error("OCOMP Registry genesis authority is immutable")]
    GenesisAuthorityImmutable,

    /// Genesis install hash or activation height slot is non-empty when authority is absent.
    #[error("OCOMP Registry genesis authority is partial")]
    GenesisAuthorityPartial,

    /// Reading back the freshly written genesis authority did not match the input.
    #[error("OCOMP Registry authority write/read mismatch")]
    AuthorityWriteReadMismatch,

    /// Stored active bundle hash slot does not match the decoded request profile.
    #[error("OCOMP Registry active bundle hash slot is inconsistent")]
    ActiveBundleHashInconsistent,

    /// Staged successor bytes and proposal ID slots are inconsistently populated.
    #[error("OCOMP Registry staged successor is partial")]
    StagedSuccessorPartial,

    /// Successor proposal ID is zero.
    #[error("OCOMP successor proposal id must be non-zero")]
    SuccessorProposalIdZero,

    /// Registry has no active authority installed.
    #[error("OCOMP Registry is not initialized")]
    NotInitialized,

    /// Predecessor authority is still retiring when staging a new successor.
    #[error("OCOMP predecessor retirement must finish before staging another successor")]
    PredecessorRetirementPending,

    /// Another successor is already staged under a different proposal or payload.
    #[error("another OCOMP successor is already staged")]
    AnotherSuccessorAlreadyStaged,

    /// Discard request proposal ID does not match the staged successor's proposal ID.
    #[error("cannot discard another Update proposal's OCOMP successor")]
    CannotDiscardAnotherProposalSuccessor,

    /// Promotion height does not match the storage block number.
    #[error("OCOMP successor activation height does not match storage context")]
    SuccessorActivationHeightContextMismatch,

    /// Promotion requested when no successor is staged.
    #[error("OCOMP successor is not staged")]
    SuccessorNotStaged,

    /// Staged successor proposal ID or activation height does not match promotion arguments.
    #[error("OCOMP successor activation proposal or height mismatch")]
    SuccessorActivationProposalOrHeightMismatch,

    /// Retiring predecessor slot is already occupied during successor promotion.
    #[error("OCOMP Registry already has a retiring predecessor")]
    AlreadyHasRetiringPredecessor,

    /// Lineage identifier is zero.
    #[error("OCOMP lineage id must be non-zero")]
    LineageIdZero,

    /// Existing lineage references a bundle hash that is neither active nor retiring.
    #[error("OCOMP lineage references an unavailable bundle")]
    LineageBundleUnavailable,

    /// Live lineage counter overflowed `u32::MAX`.
    #[error("OCOMP live lineage count overflow")]
    LiveLineageCountOverflow,

    /// Inherited lineage or predecessor lineage is zero or identical.
    #[error("invalid OCOMP inherited lineage binding")]
    InvalidInheritedLineageBinding,

    /// Predecessor lineage has no pinned bundle hash.
    #[error("OCOMP predecessor lineage is not pinned")]
    PredecessorLineageNotPinned,

    /// Predecessor lineage's bundle authority is no longer available.
    #[error("OCOMP predecessor lineage authority is unavailable")]
    PredecessorLineageAuthorityUnavailable,

    /// Re-pinning an inherited lineage resolved to a different bundle than previously recorded.
    #[error("OCOMP inherited lineage binding changed")]
    InheritedLineageBindingChanged,

    /// Live lineage counter underflowed zero on release.
    #[error("OCOMP live lineage count underflow")]
    LiveLineageCountUnderflow,

    /// Retirement check height does not match the storage block number.
    #[error("OCOMP retirement height does not match storage context")]
    RetirementHeightContextMismatch,

    /// Predecessor retention deadline addition overflowed `u64::MAX`.
    #[error("OCOMP predecessor retention height overflow")]
    PredecessorRetentionHeightOverflow,
}

impl From<OcompRegistryError> for PrecompileError {
    fn from(error: OcompRegistryError) -> Self {
        Self::Revert(error.to_string())
    }
}
