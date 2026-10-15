//! Typed errors for snapshot creation and validation.

use alloy_primitives::B256;
use outbe_primitives::error::PrecompileError;
use outbe_snapshot::manifest::DomainKind;
use thiserror::Error;

/// Errors that occur during snapshot creation and core validation.
#[derive(Debug, Error)]
pub(crate) enum SnapshotError {
    #[cfg(all(test, feature = "snapshot-integration"))]
    #[error("Lysis terminal receipt intent or job binding differs")]
    LysisTerminalReceiptMismatch,
    /// Projection configuration is missing.
    #[error("missing required projection configuration")]
    MissingProjectionConfig,
    /// Native CLI inputs do not describe a node.
    #[error("snapshot native inputs must describe a node")]
    InputsNotNode,
    /// The node data directory has no parent for OCOMP.
    #[error("node data directory has no OCOMP parent")]
    MissingOcompParent,
    /// Offchain storage does not use RocksDB.
    #[error("filesystem snapshots require RocksDB offchain storage")]
    RequiresRocksDbStorage,
    /// The snapshot output path already exists.
    #[error("snapshot output already exists: {path}")]
    OutputAlreadyExists {
        /// Output path.
        path: String,
    },
    /// A native domain label is invalid.
    #[error("invalid native domain label")]
    InvalidDomainLabel,
    /// A native member path is not valid UTF-8.
    #[error("native member path is not UTF-8: {path}")]
    MemberPathNotUtf8 {
        /// Member path.
        path: String,
    },
    /// A native member path is not valid UTF-8.
    #[error("native member path is not UTF-8")]
    MemberPathNotUtf8Plain,
    /// The snapshot signer callback did not run.
    #[error("snapshot signer was not invoked")]
    SignerNotInvoked,
    /// A required native domain is missing.
    #[error("missing required native domain: {path}")]
    MissingRequiredDomain {
        /// Domain path.
        path: String,
    },
    /// A native file has an unsupported file type.
    #[error("unsupported native file: {path}")]
    UnsupportedNativeFile {
        /// File path.
        path: String,
    },
    /// The static file directory is missing.
    #[error("missing static-file directory")]
    MissingStaticFileDir,
    /// A retained header is missing at a block number.
    #[error("missing retained header {number}")]
    MissingRetainedHeader {
        /// Block number.
        number: u64,
    },
    /// A canonical hash is missing at a block number.
    #[error("missing canonical hash {number}")]
    MissingCanonicalHash {
        /// Block number.
        number: u64,
    },
    /// A stored header does not match canonical identity.
    #[error("stored header does not match canonical identity at {number}")]
    StoredHeaderIdentityMismatch {
        /// Block number.
        number: u64,
    },
    /// The execution database belongs to another genesis.
    #[error("execution database belongs to a different genesis")]
    DifferentGenesis,
    /// The durable finalized block marker is missing.
    #[error("missing durable LastFinalizedBlock")]
    MissingLastFinalizedBlock,
    /// The execution stage checkpoint is missing.
    #[error("missing execution stage checkpoint")]
    MissingExecutionCheckpoint,
    /// The offchain projection checkpoint is missing.
    #[error("missing initialized offchain projection checkpoint")]
    MissingOffchainProjectionCheckpoint,
    /// The artifact path has no file name.
    #[error("artifact path has no filename")]
    ArtifactPathHasNoFilename,
    /// The artifact is not a regular file.
    #[error("artifact is not a regular file")]
    ArtifactNotRegularFile,
    /// Formatted diagnostic report for artifact errors.
    #[error("{message}")]
    ErrorReport {
        /// Diagnostic message.
        message: String,
    },
    /// Artifact metadata exceeds the size limit.
    #[error("artifact metadata exceeds size limit")]
    ArtifactMetadataExceedsLimit,
    /// An archive member is invalid.
    #[error("invalid {name} archive member")]
    InvalidArchiveMember {
        /// Archive member name.
        name: String,
    },
    /// The detached manifest differs from the archive manifest.
    #[error("detached manifest differs from exact archive manifest bytes")]
    DetachedManifestMismatch,
    /// The detached signature has a different creator.
    #[error("detached signature has a different authenticated creator")]
    DetachedSignatureCreatorMismatch,
    /// The snapshot creator does not match the expected key.
    #[error("snapshot creator does not match expected public key")]
    SnapshotCreatorKeyMismatch,
    /// The archive manifest changed between passes.
    #[error("archive manifest changed between metadata and payload passes")]
    ArchiveManifestChangedBetweenPasses,
    /// A manifest domain has no matching native population.
    #[error("manifest domain has no matching native population: {kind:?}")]
    ManifestDomainNoMatchingPopulation {
        /// Domain kind.
        kind: DomainKind,
    },
    /// A native population appears twice in the manifest.
    #[error("duplicate native population in manifest: {kind:?}")]
    DuplicatePopulationInManifest {
        /// Domain kind.
        kind: DomainKind,
    },
    /// A native member size differs from the manifest.
    #[error("native member size differs: {name}")]
    MemberSizeDiffers {
        /// Member name.
        name: String,
    },
    /// Native file size calculation overflowed.
    #[error("native file size overflow")]
    FileSizeOverflow,
    /// Native file length calculation overflowed.
    #[error("native file length overflow")]
    FileLengthOverflow,
    /// A native member length changed during reading.
    #[error("native member length changed: {name}")]
    MemberLengthChanged {
        /// Member name.
        name: String,
    },
    /// A native member checksum differs from the manifest.
    #[error("native member checksum differs: {name}")]
    MemberChecksumDiffers {
        /// Member name.
        name: String,
    },
    /// Native file count calculation overflowed.
    #[error("native file count overflow")]
    FileCountOverflow,
    /// Native byte count calculation overflowed.
    #[error("native byte count overflow")]
    ByteCountOverflow,
    /// The native member inventory differs for a domain.
    #[error("native member inventory differs for {kind:?}")]
    MemberInventoryDiffers {
        /// Domain kind.
        kind: DomainKind,
    },
    /// The native root mode differs for a domain.
    #[error("native root mode differs for {kind:?}")]
    RootModeDiffers {
        /// Domain kind.
        kind: DomainKind,
    },
    /// A native member is not declared in the manifest.
    #[error("native member is not declared")]
    MemberNotDeclared,
    /// A native member mode differs from the manifest.
    #[error("native member mode differs: {name}")]
    MemberModeDiffers {
        /// Member name.
        name: String,
    },
    /// A native member kind differs from the manifest.
    #[error("native member kind differs: {name}")]
    MemberKindDiffers {
        /// Member name.
        name: String,
    },
    /// Native domains changed during verification.
    #[error("native domains changed during verification")]
    DomainsChangedDuringVerification,
    /// A native domain disappeared during verification.
    #[error("native domain disappeared during verification")]
    DomainDisappearedDuringVerification,
    /// The native member inventory changed during verification.
    #[error("native member inventory changed during verification")]
    MemberInventoryChangedDuringVerification,
    /// The manifest omits a nonempty native population.
    #[error("manifest omits nonempty native population: {kind:?}")]
    ManifestOmitsNonemptyPopulation {
        /// Domain kind.
        kind: DomainKind,
    },
    /// Native file inventory totals differ from the manifest.
    #[error("native file inventory totals differ")]
    FileInventoryTotalsDiffer,
    /// Reading canonical owner state failed at an execution height.
    #[error("canonical owner state at E={number}: {error}")]
    CanonicalOwnerState {
        /// Execution block number.
        number: u64,
        /// Underlying precompile error.
        error: PrecompileError,
    },
    /// Canonical Metadosis job intent or day binding differs.
    #[error("canonical Metadosis job intent/day binding differs")]
    MetadosisJobIntentDayMismatch,
    /// Canonical Metadosis job identifier binding differs.
    #[error("canonical Metadosis JobId binding differs")]
    MetadosisJobIdMismatch,
    /// Canonical Metadosis request binding differs from the retained header.
    #[error("canonical Metadosis request binding differs from retained header B={number}")]
    MetadosisRequestBindingMismatch {
        /// Request block number.
        number: u64,
    },
    /// The execution header does not match the canonical identity.
    #[error("execution header does not match the observed canonical identity")]
    ExecutionHeaderIdentityMismatch,
    /// The current EVM state root differs from the header.
    #[error("current EVM state root differs at E={number}: computed {computed}, header {header}")]
    CurrentStateRootMismatch {
        /// Execution block number.
        number: u64,
        /// Computed state root.
        computed: B256,
        /// Header state root.
        header: B256,
    },
    /// The state storage version is unsupported.
    #[error("unsupported state storage version {version}")]
    UnsupportedStateStorageVersion {
        /// Storage version.
        version: u32,
    },
    /// A native table contains a malformed value.
    #[error("malformed native value in {table}")]
    MalformedNativeValue {
        /// Table name.
        table: String,
    },
    /// Storage exists for a missing account.
    #[error("orphan storage for account {address}")]
    OrphanStorage {
        /// Account key hash.
        address: B256,
    },
    /// A storage slot contains a noncanonical zero value.
    #[error("noncanonical zero storage at {address}/{key}")]
    NoncanonicalZeroStorage {
        /// Account key hash.
        address: B256,
        /// Storage slot hash.
        key: B256,
    },
    /// A storage slot is duplicated.
    #[error("duplicate storage at {address}/{key}")]
    DuplicateStorage {
        /// Account key hash.
        address: B256,
        /// Storage slot hash.
        key: B256,
    },
    /// A bytecode entry differs from its hash.
    #[error("bytecode differs from its hash {hash}")]
    BytecodeHashMismatch {
        /// Bytecode hash.
        hash: B256,
    },
    /// An account references missing bytecode.
    #[error("missing bytecode {hash} for account {address}")]
    MissingBytecode {
        /// Bytecode hash.
        hash: B256,
        /// Account key hash.
        address: B256,
    },
    /// A header segment entry is not a file.
    #[error("header segment is not a file")]
    HeaderSegmentNotFile,
    /// A header segment file name is ambiguous.
    #[error("ambiguous header segment filename")]
    AmbiguousHeaderSegmentFilename,
    /// A native header segment is missing.
    #[error("missing native header segment {path}")]
    MissingNativeHeaderSegment {
        /// Segment path.
        path: String,
    },
    /// Header segment range metadata disagrees with the file name.
    #[error("header segment range metadata disagrees with filename")]
    HeaderSegmentMetadataMismatch,
    /// A header segment has rows without a retained range.
    #[error("header segment has rows but no retained range")]
    HeaderSegmentRowsWithoutRange,
    /// A retained header range is invalid.
    #[error("invalid retained header range {start}..={end}")]
    InvalidRetainedHeaderRange {
        /// Start block.
        start: u64,
        /// End block.
        end: u64,
    },
    /// Header range length calculation overflowed.
    #[error("header range length overflow")]
    HeaderRangeLengthOverflow,
    /// Header segment row count disagrees with the retained range.
    #[error("header segment rows disagree with retained range {start}..={end}")]
    HeaderSegmentRowsDisagreeWithRange {
        /// Start block.
        start: u64,
        /// End block.
        end: u64,
    },
    /// Retained header segment ranges overlap.
    #[error("overlapping retained header segment ranges")]
    OverlappingHeaderSegmentRanges,
    /// A header segment is missing for a block number.
    #[error("missing header segment for {number}")]
    MissingHeaderSegmentForBlock {
        /// Block number.
        number: u64,
    },
    /// A header is missing inside a static range.
    #[error("missing header {number} inside static range")]
    MissingHeaderInsideStaticRange {
        /// Block number.
        number: u64,
    },
    /// A canonical hash is missing inside a static range.
    #[error("missing canonical hash {number} inside static range")]
    MissingCanonicalHashInsideStaticRange {
        /// Block number.
        number: u64,
    },
    /// A retained header is missing for a canonical row.
    #[error("missing retained header {number} for canonical row")]
    MissingRetainedHeaderForCanonicalRow {
        /// Block number.
        number: u64,
    },
    /// A canonical hash is missing for a retained header.
    #[error("missing canonical hash for retained header {number}")]
    MissingCanonicalHashForRetainedHeader {
        /// Block number.
        number: u64,
    },
    /// A header number does not match its row number.
    #[error("header number mismatch at {number}")]
    HeaderNumberMismatch {
        /// Block number.
        number: u64,
    },
    /// A header hash does not match its canonical hash.
    #[error("canonical header hash mismatch at {number}")]
    CanonicalHeaderHashMismatch {
        /// Block number.
        number: u64,
    },
    /// An MDBX header number does not match its row number.
    #[error("MDBX header number mismatch at {number}")]
    MdbxHeaderNumberMismatch {
        /// Block number.
        number: u64,
    },
    /// MDBX and static header hashes conflict.
    #[error("conflicting MDBX/static header hash at {number}")]
    ConflictingMdbxStaticHeaderHash {
        /// Block number.
        number: u64,
    },
    /// An MDBX canonical hash does not match the header hash.
    #[error("MDBX canonical hash mismatch at {number}")]
    MdbxCanonicalHashMismatch {
        /// Block number.
        number: u64,
    },
    /// The retained genesis block differs from the configured chain.
    #[error("retained genesis differs from configured chain")]
    RetainedGenesisMismatch,
    /// A retained header parent hash does not match the previous block.
    #[error("retained header parent mismatch at {number}")]
    RetainedHeaderParentMismatch {
        /// Block number.
        number: u64,
    },
    /// Verified header count calculation overflowed.
    #[error("verified header count overflow")]
    VerifiedHeaderCountOverflow,
    /// The header CE commitment scheme is unsupported.
    #[error("unsupported header CE commitment scheme")]
    UnsupportedHeaderCeScheme,
    /// The CE marker commitment scheme is unsupported.
    #[error("unsupported CE marker scheme")]
    UnsupportedCeMarkerScheme,
    /// The CE marker height disagrees with the header.
    #[error("CE marker/header height mismatch")]
    CeMarkerHeightMismatch,
    /// The CE marker block hash disagrees with the header.
    #[error("CE marker/header block hash mismatch")]
    CeMarkerBlockHashMismatch,
    /// The CE marker parent hash disagrees with the header.
    #[error("CE marker/header parent hash mismatch")]
    CeMarkerParentHashMismatch,
    /// A header CE commitment is missing at a height.
    #[error("missing header CE commitment at {height}")]
    MissingHeaderCeCommitment {
        /// Block height.
        height: u64,
    },
    /// The CE marker root disagrees with the header root.
    #[error("CE marker/header root mismatch")]
    CeMarkerRootMismatch,
    /// A selected native source has the wrong file kind.
    #[error("selected native source has wrong file kind: {path}")]
    SourceWrongFileKind {
        /// Source path.
        path: String,
    },
    /// An unknown snapshot check name was requested.
    #[error("unknown snapshot check {name:?}")]
    UnknownSnapshotCheck {
        /// Requested check name.
        name: String,
    },
    /// Removing the validation scratch directory failed.
    #[error("remove validation scratch: {source}")]
    RemoveValidationScratch {
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Precompile storage errors raised while reading canonical snapshot state.
#[derive(Debug, Error)]
pub(crate) enum CanonicalStatePrecompileError {
    /// Reading verified storage failed.
    #[error("read verified storage: {details}")]
    ReadVerifiedStorage {
        /// Error details.
        details: String,
    },
    /// Reading a verified retained header failed.
    #[error("read verified retained header {number}: {details}")]
    ReadVerifiedRetainedHeader {
        /// Block number.
        number: u64,
        /// Error details.
        details: String,
    },
    /// Decoding an active generation record failed.
    #[error("{0}")]
    ActiveGenerationDecode(#[from] outbe_ocomp_protocol::ProtocolError),
}

impl From<CanonicalStatePrecompileError> for PrecompileError {
    fn from(error: CanonicalStatePrecompileError) -> Self {
        Self::Fatal(error.to_string())
    }
}
