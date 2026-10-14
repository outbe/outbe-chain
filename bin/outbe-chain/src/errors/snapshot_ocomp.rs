//! Typed errors for offline OCOMP snapshot validation.

use alloy_primitives::B256;
use thiserror::Error;

/// Errors that occur during offline OCOMP snapshot validation.
#[derive(Debug, Error)]
pub(crate) enum SnapshotOcompError {
    /// The local plan differs from canonical export or frozen inputs.
    #[error("local plan differs from canonical export or frozen inputs")]
    LocalPlanMismatch,
    /// An admission locator name is not valid UTF-8.
    #[error("invalid admission locator")]
    InvalidAdmissionLocatorPlain,
    /// An admission directory contains an unexpected entry.
    #[error("unexpected admission entry {name}")]
    UnexpectedAdmissionEntry {
        /// Entry name.
        name: String,
    },
    /// An admission locator is invalid.
    #[error("invalid admission locator {name}")]
    InvalidAdmissionLocator {
        /// Locator name.
        name: String,
    },
    /// A present admission differs from plan authority.
    #[error("present admission differs from plan authority at ordinal {ordinal}")]
    PresentAdmissionPlanMismatch {
        /// Admission ordinal.
        ordinal: u32,
    },
    /// Result chunk count calculation overflowed.
    #[error("result chunk count overflow")]
    ResultChunkCountOverflow,
    /// A root reduce leaf lacks its admitted result entry.
    #[error("ROOT_REDUCE leaf lacks its exact admitted result entry")]
    RootReduceLeafLacksResultEntry,
    /// A non-leaf admission contains a result entry.
    #[error("non-leaf admission contains a result entry")]
    NonLeafAdmissionHasResultEntry,
    /// A present admission unit identifier differs at an ordinal.
    #[error("present admission UnitId differs at ordinal {ordinal}")]
    PresentAdmissionUnitIdMismatch {
        /// Admission ordinal.
        ordinal: u32,
    },
    /// Present admission count calculation overflowed.
    #[error("present admission count overflow")]
    PresentAdmissionCountOverflow,
    /// Local result comparison has no canonical finality.
    #[error("local result comparison requires canonical finality")]
    LocalResultComparisonRequiresFinality,
    /// A local result lacks canonical finality.
    #[error("local result lacks canonical finality")]
    LocalResultLacksFinality,
    /// A local result differs from the canonical job identifier.
    #[error("local result differs from canonical JobId")]
    LocalResultJobIdMismatch,
    /// A local result differs from the canonical completed binding.
    #[error("local result differs from canonical completed binding")]
    LocalResultCompletedBindingMismatch,
    /// A native result file name is invalid.
    #[error("invalid native result filename")]
    InvalidNativeResultFilename,
    /// Local result count calculation overflowed.
    #[error("local result count overflow")]
    LocalResultCountOverflow,
    /// Local result digest count calculation overflowed.
    #[error("local result digest count overflow")]
    LocalResultDigestCountOverflow,
    /// Bundle codec limit calculation overflowed.
    #[error("bundle codec limit overflow")]
    BundleCodecLimitOverflow,
    /// The protocol bundle entry is not a file.
    #[error("protocol bundle is not a file")]
    ProtocolBundleNotFile,
    /// The protocol bundle exceeds the codec limit.
    #[error("protocol bundle exceeds codec limit")]
    ProtocolBundleExceedsCodecLimit,
    /// The protocol bundle changed size during reading.
    #[error("protocol bundle changed size")]
    ProtocolBundleChangedSize,
    /// The OCOMP projection checkpoint differs from the retained header.
    #[error("OCOMP projection checkpoint differs from retained canonical header")]
    ProjectionCheckpointHeaderMismatch,
    /// Multiple retained pins bind the same canonical intent.
    #[error("multiple retained pins bind the same canonical intent")]
    MultipleRetainedPinsForIntent,
    /// A required OCOMP source has a conflicting request identity.
    #[error("required OCOMP source has a conflicting frozen request identity")]
    SourceConflictingRequestIdentity,
    /// OCOMP input chunk count calculation overflowed.
    #[error("OCOMP input chunk count overflow")]
    InputChunkCountOverflow,
    /// A CAS object entry is not a regular file.
    #[error("CAS object is not a regular file")]
    CasObjectNotRegularFile,
    /// A CAS object locator is invalid.
    #[error("invalid CAS object locator")]
    InvalidCasObjectLocator,
    /// A CAS digest locator has noncanonical encoding.
    #[error("noncanonical CAS digest locator")]
    NoncanonicalCasDigestLocator,
    /// Total CAS byte count calculation overflowed.
    #[error("CAS total byte count overflow")]
    CasTotalByteCountOverflow,
    /// CAS object count calculation overflowed.
    #[error("CAS object count overflow")]
    CasObjectCountOverflow,
    /// The CAS root is not a native directory.
    #[error("CAS root is not a native directory")]
    CasRootNotDirectory,
    /// A CAS shard name is not valid UTF-8.
    #[error("invalid CAS shard")]
    InvalidCasShard,
    /// A native CAS shard is invalid.
    #[error("invalid native CAS shard")]
    InvalidNativeCasShard,
    /// A present receipt lacks canonical finalized job authority.
    #[error("present receipt lacks canonical finalized job authority")]
    PresentReceiptLacksFinality,
    /// A receipt manifest differs from the canonical job identifier.
    #[error("receipt manifest differs from canonical JobId")]
    ReceiptManifestJobIdMismatch,
    /// A receipt manifest differs from the canonical protocol bundle.
    #[error("receipt manifest differs from canonical protocol bundle")]
    ReceiptManifestBundleMismatch,
    /// A receipt manifest differs from the canonical attempt or day.
    #[error("receipt manifest differs from canonical attempt or WWD")]
    ReceiptManifestAttemptOrWwdMismatch,
    /// A receipt manifest differs from frozen Tribute authority.
    #[error("receipt manifest differs from canonical frozen Tribute authority")]
    ReceiptManifestTributeMismatch,
    /// A receipt checkpoint differs from the canonical request or schema.
    #[error("receipt checkpoint differs from canonical request or CE schema")]
    ReceiptCheckpointMismatch,
    /// A saved pin export differs from receipt authority.
    #[error("saved pin export differs from receipt authority")]
    SavedPinExportDiffersFromReceipt,
    /// A complete export lacks canonical finalized job authority.
    #[error("complete export lacks canonical finalized job authority")]
    CompleteExportLacksFinality,
    /// An export checkpoint differs from the canonical request or schema.
    #[error("export checkpoint differs from canonical request or CE schema")]
    ExportCheckpointMismatch,
    /// A receipt and binding describe different export authorities.
    #[error("receipt and binding describe different export authorities")]
    ReceiptAndBindingAuthorityMismatch,
    /// A job specification lacks canonical finality.
    #[error("job spec lacks canonical finality")]
    JobSpecLacksFinality,
    /// A discovery bundle locator is invalid.
    #[error("invalid discovery bundle locator")]
    InvalidDiscoveryBundleLocator,
    /// A discovery offer differs from its bundle locator.
    #[error("discovery offer differs from bundle locator")]
    DiscoveryOfferBundleMismatch,
    /// A discovery offer differs from the canonical job specification.
    #[error("discovery offer differs from canonical job spec")]
    DiscoveryOfferJobSpecMismatch,
    /// Discovery offer count calculation overflowed.
    #[error("discovery offer count overflow")]
    DiscoveryOfferCountOverflow,
    /// Discovery record count calculation overflowed.
    #[error("discovery record count overflow")]
    DiscoveryRecordCountOverflow,
    /// Discovery spool count calculation overflowed.
    #[error("discovery spool count overflow")]
    DiscoverySpoolCountOverflow,
    /// A series identifier has noncanonical format.
    #[error("noncanonical series identity")]
    NoncanonicalSeriesIdentity,
    /// A series worldwide day is invalid.
    #[error("invalid series worldwide day")]
    InvalidSeriesWorldwideDay,
    /// A series identifier has noncanonical codes.
    #[error("noncanonical series codes")]
    NoncanonicalSeriesCodes,
    /// A closure checkpoint differs from the canonical header.
    #[error("closure {name} differs from canonical header at {number}")]
    ClosureHeaderMismatch {
        /// Checkpoint name.
        name: String,
        /// Block number.
        number: u64,
    },
    /// The closure checkpoint conflicts with durable projection.
    #[error("closure checkpoint is ahead of or conflicts with durable projection")]
    ClosureAheadOrConflictsWithProjection,
    /// A nonzero closure checkpoint exists without durable projection.
    #[error("nonzero closure checkpoint exists without durable projection")]
    NonzeroClosureWithoutProjection,
    /// A replay header differs from canonical identity.
    #[error("replay header differs from canonical identity at {height}")]
    ReplayHeaderIdentityMismatch {
        /// Block height.
        height: u64,
    },
    /// A replay header parent hash differs from the previous block.
    #[error("replay header parent differs at {height}")]
    ReplayHeaderParentMismatch {
        /// Block height.
        height: u64,
    },
    /// The replay target hash differs at the end height.
    #[error("replay target hash differs at {height}")]
    ReplayTargetHashMismatch {
        /// Block height.
        height: u64,
    },
    /// The replay transaction range overflows at a height.
    #[error("replay transaction range overflows at {height}")]
    ReplayTxRangeOverflow {
        /// Block height.
        height: u64,
    },
    /// Replay transaction count calculation overflowed.
    #[error("replay transaction count overflow")]
    ReplayTxCountOverflow,
    /// Replay block count calculation overflowed.
    #[error("replay block count overflow")]
    ReplayBlockCountOverflow,
    /// A request frame contains multiple OCOMP requests.
    #[error("request frame B={request_height} contains multiple OCOMP requests")]
    MultipleRequestsInFrame {
        /// Request block height.
        request_height: u64,
    },
    /// A complete request frame contains no OCOMP request.
    #[error("complete request frame B={request_height} contains no OCOMP request")]
    NoRequestInFrame {
        /// Request block height.
        request_height: u64,
    },
    /// The request event day differs from the artifact.
    #[error("request event day differs from artifact")]
    RequestEventDayMismatch,
    /// The request event or frozen context differs from the canonical job.
    #[error("request event or frozen block context differs from canonical job")]
    RequestEventContextMismatch,
    /// The artifact job identifier differs from canonical request identity.
    #[error("artifact JobId differs from canonical request identity")]
    ArtifactJobIdMismatch,
    /// A retained reference does not belong to the verified job.
    #[error("retained reference does not belong to verified job {job}")]
    RetainedReferenceNotInJob {
        /// Job identifier.
        job: B256,
    },
    /// NOD FIFO queue bounds are invalid.
    #[error("invalid NOD FIFO bounds")]
    InvalidNodFifoBounds,
    /// The NOD FIFO next-free tail slot is occupied.
    #[error("NOD FIFO next-free tail is occupied")]
    NodFifoTailOccupied,
    /// NOD FIFO head presence disagrees with queue bounds.
    #[error("NOD FIFO head presence differs")]
    NodFifoHeadPresenceMismatch,
    /// A NOD FIFO day is invalid at a queue sequence.
    #[error("invalid NOD FIFO day at {sequence}")]
    InvalidNodFifoDay {
        /// Queue sequence.
        sequence: u64,
    },
    /// A certified NOD generation is missing at a queue sequence.
    #[error("missing certified NOD generation at {sequence}")]
    MissingCertifiedNodGeneration {
        /// Queue sequence.
        sequence: u64,
    },
    /// An invalid or completed NOD generation remains queued.
    #[error("invalid or completed NOD generation remains queued at {sequence}")]
    InvalidOrCompletedNodGenerationQueued {
        /// Queue sequence.
        sequence: u64,
    },
    /// The first NOD FIFO projection differs from the native head.
    #[error("NOD FIFO first projection differs from native head")]
    NodFifoFirstProjectionMismatch,
    /// A duplicate NOD FIFO day or job was found.
    #[error("duplicate NOD FIFO day/job")]
    DuplicateNodFifoDayOrJob,
    /// An Intex series record differs from the permanent index.
    #[error("Intex series record identity/day differs from permanent index")]
    IntexSeriesRecordMismatch,
    /// A duplicate Intex series identity was found.
    #[error("duplicate Intex series identity")]
    DuplicateIntexSeriesIdentity,
    /// Canonical NOD program semantics differ from the protocol bundle.
    #[error("canonical NOD program semantics differ from protocol bundle")]
    NodProgramSemanticsMismatch,
    /// Canonical NOD generation differs from the native plan.
    #[error("canonical NOD generation differs from native plan")]
    NodGenerationPlanMismatch,
    /// NOD ordinal calculation overflowed.
    #[error("NOD ordinal overflow")]
    NodOrdinalOverflow,
    /// NOD batch progress is invalid.
    #[error("invalid NOD batch progress")]
    InvalidNodBatchProgress,
    /// NOD batch count calculation overflowed.
    #[error("NOD batch count overflow")]
    NodBatchCountOverflow,
    /// NOD action count calculation overflowed.
    #[error("NOD action count overflow")]
    NodActionCountOverflow,
    /// NOD job count calculation overflowed.
    #[error("NOD job count overflow")]
    NodJobCountOverflow,
    /// A validated NOD generation disappeared during traversal.
    #[error("validated NOD generation disappeared")]
    ValidatedNodGenerationDisappeared,
    /// A retention journal key differs from the request block hash.
    #[error("retention journal key differs from request block hash")]
    RetentionKeyBlockHashMismatch,
    /// A retained candidate differs from the canonical request or lease.
    #[error("retained candidate differs from canonical request or input lease")]
    RetainedCandidateRequestMismatch,
    /// A retained candidate differs from the canonical request header.
    #[error("retained candidate differs from canonical request header B={number}")]
    RetainedCandidateHeaderMismatch {
        /// Request block number.
        number: u64,
    },
    /// A retained finalized pin lacks canonical finality.
    #[error("retained finalized pin lacks canonical finality")]
    RetainedFinalizedPinLacksFinality,
    /// A retained pin finality window differs from canonical finality.
    #[error("retained pin finality window differs from canonical finality")]
    RetainedPinFinalityWindowMismatch,
    /// Tribute visit count calculation overflowed.
    #[error("Tribute visit count overflow")]
    TributeVisitCountOverflow,
    /// A surviving discovery ACK differs from receipt commitment.
    #[error("surviving discovery ACK differs from receipt commitment")]
    SurvivingAckReceiptCommitmentMismatch,
    /// Present receipt and binding manifests disagree.
    #[error("present receipt/binding manifest mismatch")]
    PresentReceiptBindingManifestMismatch,
    /// Present receipt and binding export authorities disagree.
    #[error("present receipt/binding export authority mismatch")]
    PresentReceiptBindingExportMismatch,
    /// A present binding differs from the saved pin export.
    #[error("present binding differs from saved pin export")]
    PresentBindingSavedPinMismatch,
    /// Canonical authority conflicts for a present job.
    #[error("conflicting canonical authority for present job")]
    ConflictingCanonicalAuthorityForPresentJob,
    /// Surviving evidence conflicts for the same job.
    #[error("conflicting surviving evidence for same job")]
    ConflictingSurvivingEvidenceForJob,
    /// A scratch evidence key has an invalid length.
    #[error("invalid scratch evidence key")]
    InvalidScratchEvidenceKey,
    /// A scratch discovery ACK is invalid.
    #[error("invalid scratch discovery ACK")]
    InvalidScratchDiscoveryAck,
    /// A scratch local result binding is invalid.
    #[error("invalid scratch local-result binding")]
    InvalidScratchLocalResultBinding,
    /// A scratch export authority record is invalid.
    #[error("invalid scratch export authority")]
    InvalidScratchExportAuthority,
    /// A scratch reference record is invalid.
    #[error("invalid scratch reference")]
    InvalidScratchReference,
    /// A scratch reference kind tag is invalid.
    #[error("invalid scratch reference kind")]
    InvalidScratchReferenceKind,
    /// A path is not a native directory.
    #[error("not a native directory: {path}")]
    NotNativeDirectory {
        /// Directory path.
        path: String,
    },
    /// A path is not a native file.
    #[error("not a native file: {path}")]
    NotNativeFile {
        /// File path.
        path: String,
    },
    /// A present job locator is not a directory.
    #[error("present job locator is not a directory")]
    PresentJobLocatorNotDirectory,
    /// A present job locator name is not valid UTF-8.
    #[error("invalid present job locator")]
    InvalidPresentJobLocator,
    /// A present job locator has noncanonical encoding.
    #[error("noncanonical present job locator")]
    NoncanonicalPresentJobLocator,
    /// Present job count calculation overflowed.
    #[error("present job count overflow")]
    PresentJobCountOverflow,
    /// Present admission and receipt manifests disagree.
    #[error("present admission/receipt manifest mismatch")]
    PresentAdmissionReceiptManifestMismatch,
    /// A present input chunk differs from the canonical job.
    #[error("present input chunk differs from canonical job")]
    PresentInputChunkJobMismatch,
    /// A present input catalog differs from the authenticated manifest.
    #[error("present input catalog differs from authenticated manifest")]
    PresentInputCatalogManifestMismatch,
    /// A result catalog exists without a finalized job.
    #[error("result catalog without finalized job")]
    ResultCatalogWithoutFinalizedJob,
    /// A materialization reference job differs from the certified generation.
    #[error("materialization reference job differs from canonical certified generation")]
    MaterializationRefJobCertifiedMismatch,
    /// Result allocation calculation overflowed.
    #[error("result allocation overflow")]
    ResultAllocationOverflow,
    /// Result NOD cost calculation overflowed.
    #[error("result NOD cost overflow")]
    ResultNodCostOverflow,
    /// The native result catalog did not reach the complete step.
    #[error("native result catalog did not reach Complete")]
    ResultCatalogNotComplete,
    /// A duplicate native bucket record sort key was found.
    #[error("duplicate native bucket record sort key")]
    DuplicateNativeBucketSortKey,
    /// The complete present result catalog differs from certified outputs.
    #[error("complete present result catalog differs from canonical certified outputs")]
    PresentResultCatalogCertifiedMismatch,
    /// A scratch job union entry is invalid.
    #[error("invalid scratch job union")]
    InvalidScratchJobUnion,
    /// Present job union count calculation overflowed.
    #[error("present job union overflow")]
    PresentJobUnionOverflow,
    /// Reference record count calculation overflowed.
    #[error("reference record count overflow")]
    ReferenceRecordCountOverflow,
    /// Present artifact count calculation overflowed.
    #[error("present artifact count overflow")]
    PresentArtifactCountOverflow,
    /// A present manifest has no canonical finalized job.
    #[error("present manifest has no canonical finalized job")]
    PresentManifestLacksFinalizedJob,
    /// A present manifest differs from frozen job authority.
    #[error("present manifest differs from canonical frozen job authority")]
    PresentManifestAuthorityMismatch,
    /// A present manifest differs from the canonical checkpoint.
    #[error("present manifest differs from canonical checkpoint")]
    PresentManifestCheckpointMismatch,
    /// A surviving discovery ACK differs from export authority.
    #[error("surviving discovery ACK differs from export authority")]
    SurvivingAckExportMismatch,
    /// A surviving local result differs from the input manifest.
    #[error("surviving local result differs from input manifest")]
    SurvivingLocalResultManifestMismatch,
    /// A surviving local result differs from the plan.
    #[error("surviving local result differs from plan")]
    SurvivingLocalResultPlanMismatch,
}
