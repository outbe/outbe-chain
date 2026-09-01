//! Crash-conservative wire-bounded multi-job OCOMP retention journal.
//!
//! Candidate discovery uses an event only as a bounded locator. The production
//! source re-opens the exact execution-valid block state and authenticates the
//! typed Metadosis record before this coordinator persists anything.

#[cfg(test)]
use std::time::Duration;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use alloy_consensus::{BlockHeader as _, TxReceipt as _};
use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolEvent as _;
use outbe_common::WorldwideDay;
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::FinalizedParentCertStore,
    ocomp_retention::{OcompRetentionHook, OcompRetentionHookError},
};
use outbe_metadosis::{
    config::poc_schema_limits, precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, job_id_from_intent_id, FinalizedIntentProofV1, JobIntentV1},
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_offchain_data::TributeRetentionSelector;
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    error::PrecompileError,
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        types::StorageKey as _,
        StorageHandle,
    },
    OutbeHeader, OutbeReceipt,
};
use outbe_tribute::RetainedTributePin;
pub use outbe_tribute::RetainedTributeWriter;
use reth_provider::{HeaderProvider, ReceiptProvider, StateProviderFactory};
use reth_storage_api::StateProvider;

use super::finality::RethFinalizedIntentProofBuilder;
use crate::finalized_frame::FinalizedFrame;

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBPIN1";
const JOURNAL_VERSION: u16 = 5;
const PIN_RECORD_VERSION: u16 = 5;
const PIN_RECORD_MAX_BYTES: usize = 512;
/// The registry has no OCOMP product count limit. Its only cardinality ceiling
/// is the count width committed by the durable journal wire format.
const JOURNAL_RECORD_COUNT_MAX: usize = u16::MAX as usize;
const JOURNAL_RECORD_PRESSURE_WATERMARK: usize =
    JOURNAL_RECORD_COUNT_MAX - JOURNAL_RECORD_COUNT_MAX / 4;
const JOURNAL_MAX_BYTES: usize =
    (PIN_RECORD_MAX_BYTES + B256::len_bytes() + std::mem::size_of::<u16>())
        * JOURNAL_RECORD_COUNT_MAX
        + 8
        + std::mem::size_of::<u16>()
        + std::mem::size_of::<u64>()
        + B256::len_bytes()
        + std::mem::size_of::<u16>()
        + B256::len_bytes();
const JOURNAL_FILENAME: &str = "pin.v1";
const JOURNAL_TEMP_FILENAME: &str = "pin.v1.tmp";
const RETAINED_EVIDENCE_WINDOW_BLOCKS: u64 = 64;

type PendingCandidateReceipts = Option<(B256, Vec<OutbeReceipt>)>;
type PendingCandidateReceiptReader =
    dyn Fn() -> Result<PendingCandidateReceipts, String> + Send + Sync;

/// Exact source identity retained before a local positive vote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidatePinV1 {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
    pub intent_id: B256,
    pub wwd: u32,
    pub ce_sealed_root: B256,
    pub protocol_bundle_hash: B256,
    pub input_lease_id: B256,
}

/// The single decoded `OffchainJobRequested` observation from one finalized
/// frame. It is a locator only; retention and discovery independently reopen
/// the exact block state before treating it as authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedRequestObservationV1 {
    pub intent_id: B256,
    pub wwd: u32,
    pub pending_nonce: u64,
    pub attempt: u32,
    pub activation_preconditions_hash: B256,
}

/// Exact finalized job derived from the candidate's typed state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedJobPinV1 {
    pub candidate: CandidatePinV1,
    pub job_id: B256,
    pub finality_recorded_height: u64,
    pub open_height: u64,
    pub deadline_height: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// Both variants are protocol-bounded and this value crosses the finality seam
// by value. Retaining `Copy` avoids introducing fallible heap allocation into
// candidate classification.
#[allow(clippy::large_enum_variant)]
pub enum CandidateFinalityV1 {
    Finalized(FinalizedJobPinV1),
    Orphaned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinReleaseReason {
    Orphaned,
    RetentionSatisfied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportAuthorityV1 {
    pub source_generation: u64,
    pub lease_generation: u64,
    pub manifest_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinStateV1 {
    Tentative {
        candidate: CandidatePinV1,
    },
    Finalized {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
    },
    Exported {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
        export: ExportAuthorityV1,
    },
    Terminal {
        candidate: CandidatePinV1,
        job_id: B256,
        finality_recorded_height: u64,
        open_height: u64,
        deadline_height: u64,
        export: ExportAuthorityV1,
        terminal_height: u64,
        release_height: u64,
    },
    Released {
        candidate: CandidatePinV1,
        job_id: Option<B256>,
        reason: PinReleaseReason,
        observed_height: u64,
        export: Option<ExportAuthorityV1>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinRecordV1 {
    pub generation: u64,
    pub state: PinStateV1,
}

/// Read-only decoded view of one durable retention journal. This is used by
/// operational diagnostics and behavioral evidence; it shares the production
/// decoder and never creates, repairs or rewrites journal state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetentionJournalSnapshotV1 {
    pub generation: u64,
    pub last_updated: B256,
    pub records: Vec<(B256, PinRecordV1)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
// `Ready` deliberately returns the complete bounded durable record. Boxing it
// would make a read-only status observation depend on a heap allocation.
#[allow(clippy::large_enum_variant)]
pub enum RetentionStatus {
    Empty,
    Ready(PinRecordV1),
    Quarantined { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurablePinAck {
    pub generation: u64,
    pub record_hash: B256,
}

#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    #[error("OCOMP retention is quarantined: {0}")]
    Quarantined(String),
    #[error("OCOMP pin journal mutex is poisoned")]
    Poisoned,
    #[error("pin journal {operation} failed at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pin journal is ambiguous: {0}")]
    AmbiguousJournal(&'static str),
    #[error("pin journal is malformed: {0}")]
    MalformedJournal(&'static str),
    #[error("pin journal version {actual} is unsupported")]
    UnsupportedJournalVersion { actual: u16 },
    #[error("pin generation overflow")]
    GenerationOverflow,
    #[error("OCOMP journal record count exceeds its u16 wire format")]
    RegistryCapacity,
    #[error("conflicting tentative candidate cannot replace the active pin")]
    ConflictingCandidate,
    #[error("fork-orphaned candidate cannot be pinned again")]
    OrphanedCandidate,
    #[error("stale pin generation: expected {expected}, actual {actual}")]
    StaleGeneration { expected: u64, actual: u64 },
    #[error("pin transition is invalid: {0}")]
    InvalidTransition(&'static str),
    #[error("finalized input source failed: {0}")]
    Source(String),
    #[error("retained Tribute storage is not configured")]
    RetainedTributeStorageUnavailable,
    #[error("retained Tribute garbage collection failed: {0}")]
    RetainedTributeGc(String),
    #[error("OCOMP retention coordinator is not installed")]
    RetentionCoordinatorNotInstalled,
    #[error("OCOMP retention coordinator is already installed")]
    RetentionCoordinatorAlreadyInstalled,
}

/// Typed exact-block source used by the retention coordinator.
///
/// OCM-10 extends this seam with bounded raw proof/opening construction. OCM-09
/// uses only the two operations necessary to authenticate one tentative record
/// and derive its finalized JobId.
pub trait FinalizedInputProofSource: Send + Sync {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError>;

    /// Authenticate the one request observation decoded by the unified
    /// finalized-frame reader. Production finalized reconciliation must use
    /// this seam and must not traverse receipts again.
    fn candidate_for_finalized_observation(
        &self,
        _frame: &FinalizedFrame,
        _observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        Err(RetentionError::Source(
            "finalized-frame candidate authentication is unavailable".to_owned(),
        ))
    }

    /// Resolve a tentative candidate only from persisted consensus finality.
    ///
    /// An unavailable or ambiguous proof is an error and leaves the candidate
    /// tentative/non-signable. `Orphaned` requires an exact competing
    /// finalization at the candidate height; live canonical state is never
    /// enough.
    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError>;

    fn terminal_height_at(
        &self,
        _block: &ConsensusBlock,
        _candidate: CandidatePinV1,
        _job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Ok(None)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        _frame: &FinalizedFrame,
        _candidate: CandidatePinV1,
        _job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Err(RetentionError::Source(
            "finalized-frame terminal observation is unavailable".to_owned(),
        ))
    }

    fn build_finalized_intent_proof(
        &self,
        _candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        Err(RetentionError::Source(
            "finalized-intent proof construction is unavailable".to_owned(),
        ))
    }

    fn build_lysis_openings(
        &self,
        _candidate: CandidatePinV1,
        _subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        Err(RetentionError::Source(
            "Lysis opening construction is unavailable".to_owned(),
        ))
    }
}

/// Reth-backed typed state source. Request events are locators, never authority.
pub struct RethFinalizedInputProofSource<P> {
    pub(super) provider: P,
    parent_proofs: FinalizedParentCertStore,
    pub(super) proof_builder: RethFinalizedIntentProofBuilder<P>,
    pending_receipts: Arc<PendingCandidateReceiptReader>,
    result_deadline_blocks: u64,
    pub(super) limits: SchemaLimits,
}

impl<P: Clone> RethFinalizedInputProofSource<P> {
    pub fn new(
        provider: P,
        parent_proofs: FinalizedParentCertStore,
        pending_receipts: impl Fn() -> Result<Option<(B256, Vec<OutbeReceipt>)>, String>
            + Send
            + Sync
            + 'static,
        result_deadline_blocks: u64,
    ) -> Self {
        Self {
            provider: provider.clone(),
            parent_proofs: parent_proofs.clone(),
            proof_builder: RethFinalizedIntentProofBuilder::new(
                provider,
                parent_proofs,
                poc_schema_limits(),
            ),
            pending_receipts: Arc::new(pending_receipts),
            result_deadline_blocks,
            limits: poc_schema_limits(),
        }
    }
}

impl<P> RethFinalizedInputProofSource<P>
where
    P: ReceiptProvider + StateProviderFactory + Send + Sync,
{
    fn record_at(
        &self,
        block: &ConsensusBlock,
        intent_id: B256,
    ) -> Result<OcompJobRecordV1, RetentionError> {
        self.record_at_hash(block.block_hash(), intent_id)
    }

    fn record_at_hash(
        &self,
        block_hash: B256,
        intent_id: B256,
    ) -> Result<OcompJobRecordV1, RetentionError> {
        read_ocomp_job_record_at(&self.provider, block_hash, intent_id, &self.limits)
    }

    fn candidate_from_observation(
        &self,
        block_number: u64,
        block_hash: B256,
        state_root: B256,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        let record = self.record_at_hash(block_hash, observation.intent_id)?;
        if record.status != OcompJobStatus::AwaitingFinality {
            return Err(RetentionError::Source(
                "event locator does not open the exact pending intent".to_owned(),
            ));
        }
        validate_request_observation(&record.intent, observation, &self.limits)?;
        Ok(CandidatePinV1 {
            block_number,
            block_hash,
            state_root,
            intent_id: observation.intent_id,
            wwd: record.intent.wwd,
            ce_sealed_root: record.intent.ce_sealed_root,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            input_lease_id: record
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?,
        })
    }
}

/// Decode `OffchainJobRequested` exactly once from a finalized frame shared by
/// projection, retention and discovery. More than one request in a block is a
/// protocol contradiction and fails closed.
pub fn observe_finalized_request(
    frame: &FinalizedFrame,
) -> Result<Option<FinalizedRequestObservationV1>, RetentionError> {
    observe_request_in_receipts(frame.receipts())
}

fn observe_request_in_receipts(
    receipts: &[OutbeReceipt],
) -> Result<Option<FinalizedRequestObservationV1>, RetentionError> {
    let mut observation = None;
    for receipt in receipts {
        if !receipt.status() {
            continue;
        }
        for log in receipt.logs() {
            if log.address != METADOSIS_ADDRESS
                || log.data.topics().first()
                    != Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
            {
                continue;
            }
            let event = IMetadosis::OffchainJobRequested::decode_log(log).map_err(|error| {
                RetentionError::Source(format!("decode finalized OCOMP request: {error}"))
            })?;
            let decoded = FinalizedRequestObservationV1 {
                intent_id: event.data.intentId,
                wwd: event.data.wwd,
                pending_nonce: event.data.pendingNonce,
                attempt: event.data.attempt,
                activation_preconditions_hash: event.data.activationPreconditionsHash,
            };
            if observation.replace(decoded).is_some() {
                return Err(RetentionError::Source(
                    "finalized frame contains more than one OCOMP request".to_owned(),
                ));
            }
        }
    }
    Ok(observation)
}

/// Read one exact typed OCOMP job record from canonical state at `block_hash`.
///
/// Events are locators only. Embedded Supervisor and retention both use this
/// single state decoder so neither can accidentally trust event payloads as the
/// job authority.
pub fn read_ocomp_job_record_at<P>(
    provider: &P,
    block_hash: B256,
    intent_id: B256,
    limits: &SchemaLimits,
) -> Result<OcompJobRecordV1, RetentionError>
where
    P: StateProviderFactory,
{
    let state = provider
        .state_by_block_hash(block_hash)
        .map_err(|error| RetentionError::Source(format!("open exact block state: {error}")))?;
    let logical_key = intent_storage_key(intent_id)
        .map_err(|error| RetentionError::Source(format!("derive intent slot: {error}")))?;
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    let encoded = read_storage_bytes(state.as_ref(), base, limits.max_bounded_bytes)?;
    let record = OcompJobRecordV1::decode_canonical(&encoded, limits)
        .map_err(|error| RetentionError::Source(format!("decode typed job record: {error}")))?;
    let decoded_id = record
        .intent
        .intent_id(limits)
        .map_err(|error| RetentionError::Source(format!("hash typed job record: {error}")))?;
    if decoded_id != intent_id {
        return Err(RetentionError::Source(
            "storage key does not open the exact typed JobIntent".to_owned(),
        ));
    }
    Ok(record)
}

/// Resolve whether one local OCOMP key belongs to the job's exact historical
/// ValidatorSet snapshot at the request block. A promoted or re-entered
/// Validator must not submit a vote for a job whose pinned snapshot predates
/// that membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OcompSnapshotEligibilityV1 {
    Eligible,
    NotMember,
    Unavailable { detail: String },
    Corrupt { detail: String },
}

pub fn ocomp_snapshot_contains_key_at<P>(
    provider: &P,
    block_hash: B256,
    intent: &JobIntentV1,
    ocomp_key_hash: B256,
) -> OcompSnapshotEligibilityV1
where
    P: StateProviderFactory,
{
    if ocomp_key_hash.is_zero() {
        return OcompSnapshotEligibilityV1::Corrupt {
            detail: "local OCOMP key hash is zero".to_owned(),
        };
    }
    let state = match provider.state_by_block_hash(block_hash) {
        Ok(state) => state,
        Err(error) => {
            return OcompSnapshotEligibilityV1::Unavailable {
                detail: format!("open exact snapshot state: {error}"),
            };
        }
    };
    let reader = OcompSnapshotStateReader {
        state: state.as_ref(),
    };
    let mut readonly = ReadOnlyStorageProvider::new(reader);
    let storage = StorageHandle::new(&mut readonly);
    let extension = match outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        storage.clone(),
        intent.result_validator_set_epoch,
        intent.result_committee_set_hash,
        intent.result_ocomp_binding_hash,
    ) {
        Ok(Some(extension)) => extension,
        Ok(None) => {
            return OcompSnapshotEligibilityV1::Corrupt {
                detail: "pinned OCOMP snapshot is missing".to_owned(),
            };
        }
        Err(error) => return classify_snapshot_read_error("read pinned OCOMP snapshot", error),
    };
    if extension.member_count != intent.result_member_count {
        return OcompSnapshotEligibilityV1::Corrupt {
            detail: "pinned OCOMP snapshot member count disagrees with JobIntent".to_owned(),
        };
    }
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        intent.result_validator_set_epoch,
        intent.result_committee_set_hash,
    );
    for index in 0..extension.member_count {
        let member = match outbe_validatorset::read_ocomp_snapshot_member_at(
            storage.clone(),
            snapshot_key,
            index,
        ) {
            Ok(Some(member)) => member,
            Ok(None) => {
                return OcompSnapshotEligibilityV1::Corrupt {
                    detail: "pinned OCOMP member is missing".to_owned(),
                };
            }
            Err(error) => return classify_snapshot_read_error("read pinned OCOMP member", error),
        };
        if keccak256(member.ocomp_public_key_sec1) == ocomp_key_hash {
            return OcompSnapshotEligibilityV1::Eligible;
        }
    }
    OcompSnapshotEligibilityV1::NotMember
}

fn classify_snapshot_read_error(
    context: &'static str,
    error: PrecompileError,
) -> OcompSnapshotEligibilityV1 {
    match error {
        PrecompileError::Storage(detail) => OcompSnapshotEligibilityV1::Unavailable {
            detail: format!("{context}: {detail}"),
        },
        error => OcompSnapshotEligibilityV1::Corrupt {
            detail: format!("{context}: {error}"),
        },
    }
}

struct OcompSnapshotStateReader<'a> {
    state: &'a dyn StateProvider,
}

impl StorageReader for OcompSnapshotStateReader<'_> {
    fn read_storage(
        &self,
        address: alloy_primitives::Address,
        key: B256,
    ) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "pinned OCOMP snapshot state read failed: {error}"
                ))
            })
    }
}

impl<P> FinalizedInputProofSource for RethFinalizedInputProofSource<P>
where
    P: ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + HeaderProvider<Header = OutbeHeader>
        + Send
        + Sync,
{
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        let pending = (self.pending_receipts)().map_err(|error| {
            RetentionError::Source(format!("load pending candidate receipts: {error}"))
        })?;
        let receipts = match pending {
            Some((pending_hash, receipts)) if pending_hash == block.block_hash() => receipts,
            _ => self
                .provider
                .receipts_by_block(block.block_hash().into())
                .map_err(|error| {
                    RetentionError::Source(format!("load canonical candidate receipts: {error}"))
                })?
                .ok_or_else(|| {
                    RetentionError::Source(
                        "candidate receipts are unavailable from pending and canonical execution"
                            .to_owned(),
                    )
                })?,
        };
        let Some(observation) = observe_request_in_receipts(&receipts)? else {
            return Ok(None);
        };
        self.candidate_from_observation(
            block.number(),
            block.block_hash(),
            block.header().state_root(),
            observation,
        )
        .map(Some)
    }

    fn candidate_for_finalized_observation(
        &self,
        frame: &FinalizedFrame,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        let identity = frame.identity();
        self.candidate_from_observation(
            identity.number,
            identity.hash,
            frame.state_root(),
            observation,
        )
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        let records = self
            .parent_proofs
            .finalizations_at_height(candidate.block_number);
        if records.is_empty() {
            return Err(RetentionError::Source(
                "candidate-height finalization proof is unavailable".to_owned(),
            ));
        }
        let hashes = records
            .iter()
            .map(|record| record.finalized_block_hash)
            .collect::<BTreeSet<_>>();
        if hashes.len() != 1 {
            return Err(RetentionError::Source(
                "candidate-height finalization proofs disagree".to_owned(),
            ));
        }
        let finalized_hash = *hashes
            .first()
            .expect("non-empty finalization set has one hash");
        if finalized_hash != candidate.block_hash {
            return Ok(CandidateFinalityV1::Orphaned);
        }
        if records.len() != 1 {
            return Err(RetentionError::Source(
                "candidate has ambiguous finalization proof records".to_owned(),
            ));
        }
        let header = self
            .provider
            .sealed_header_by_hash(candidate.block_hash)
            .map_err(|error| {
                RetentionError::Source(format!("load finalized candidate header: {error}"))
            })?
            .ok_or_else(|| {
                RetentionError::Source("finalized candidate header is unavailable".to_owned())
            })?;
        if header.number() != candidate.block_number
            || header.hash() != candidate.block_hash
            || header.state_root() != candidate.state_root
        {
            return Err(RetentionError::Source(
                "finalized header does not match tentative source identity".to_owned(),
            ));
        }
        let (_, verified) = self
            .proof_builder
            .build_and_verify_header(header.header(), header.hash(), candidate.intent_id)
            .map_err(|error| {
                RetentionError::Source(format!(
                    "build and verify exact finalized intent proof: {error}"
                ))
            })?;
        if verified.request.block_number != candidate.block_number
            || verified.request.block_hash != candidate.block_hash
            || verified.request.state_root != candidate.state_root
            || verified.intent_id != candidate.intent_id
        {
            return Err(RetentionError::Source(
                "verified finalized intent differs from tentative source identity".to_owned(),
            ));
        }
        if verified.intent.wwd != candidate.wwd
            || verified.intent.ce_sealed_root != candidate.ce_sealed_root
            || verified.intent.protocol_bundle_hash != candidate.protocol_bundle_hash
            || verified
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
            || job_id_from_intent_id(
                candidate.intent_id,
                candidate.block_hash,
                candidate.state_root,
            )
            .map_err(|error| RetentionError::Source(format!("derive tentative JobId: {error}")))?
                != verified.job_id
        {
            return Err(RetentionError::Source(
                "finalized intent differs from tentative pin".to_owned(),
            ));
        }
        let finality_recorded_height = records[0].stored_at_height;
        let open_height = finality_recorded_height
            .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
            .ok_or_else(|| RetentionError::Source("voting-open height overflow".to_owned()))?;
        let deadline_height = open_height
            .checked_add(self.result_deadline_blocks)
            .ok_or_else(|| RetentionError::Source("result deadline height overflow".to_owned()))?;
        if self.result_deadline_blocks == 0 {
            return Err(RetentionError::Source(
                "result deadline window is zero".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id: verified.job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        }))
    }

    fn terminal_height_at(
        &self,
        block: &ConsensusBlock,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let record = self.record_at(block, candidate.intent_id)?;
        terminal_height_from_record(&record, block.number(), candidate, job_id)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let identity = frame.identity();
        let record = self.record_at_hash(identity.hash, candidate.intent_id)?;
        terminal_height_from_record(&record, identity.number, candidate, job_id)
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        let (proof, verified) = self
            .proof_builder
            .build_and_verify_header(
                self.provider
                    .sealed_header_by_hash(candidate.block_hash)
                    .map_err(|error| {
                        RetentionError::Source(format!("load finalized opening header: {error}"))
                    })?
                    .ok_or_else(|| {
                        RetentionError::Source("finalized opening header is unavailable".to_owned())
                    })?
                    .header(),
                candidate.block_hash,
                candidate.intent_id,
            )
            .map_err(|error| RetentionError::Source(error.to_string()))?;
        if verified.job_id != candidate_job_id(candidate)?
            || verified
                .intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
        {
            return Err(RetentionError::Source(
                "finalized-intent proof opens a different JobId".to_owned(),
            ));
        }
        Ok(proof)
    }

    fn build_lysis_openings(
        &self,
        candidate: CandidatePinV1,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        super::openings::build_lysis_openings(&self.provider, &self.limits, candidate, subjects)
    }
}

fn terminal_height_from_record(
    record: &OcompJobRecordV1,
    observed_height: u64,
    candidate: CandidatePinV1,
    job_id: B256,
) -> Result<Option<u64>, RetentionError> {
    if record
        .intent
        .input_lease_id()
        .map_err(|error| RetentionError::Source(error.to_string()))?
        != candidate.input_lease_id
    {
        return Err(RetentionError::Source(
            "terminal JobIntent changed its authenticated input lease".to_owned(),
        ));
    }
    match record.status {
        OcompJobStatus::AwaitingFinality | OcompJobStatus::VotingOpen => Ok(None),
        OcompJobStatus::Completed
        | OcompJobStatus::Expired
        | OcompJobStatus::Conflicted
        | OcompJobStatus::Canceled => {
            let finalized = record.finalized.as_ref().ok_or_else(|| {
                RetentionError::Source("terminal Job is missing finalized binding".to_owned())
            })?;
            let terminal = record.terminal.as_ref().ok_or_else(|| {
                RetentionError::Source("terminal Job is missing terminal record".to_owned())
            })?;
            if finalized.job_id != job_id
                || finalized.finalized_request_block_hash != candidate.block_hash
                || finalized.finalized_request_state_root != candidate.state_root
                || terminal.terminal_height > observed_height
            {
                return Err(RetentionError::Source(
                    "terminal Job binding differs from retained finalized Job".to_owned(),
                ));
            }
            retention_terminal_height_for_status(
                record.status,
                observed_height,
                finalized.deadline_height,
                terminal.terminal_height,
            )
        }
    }
}

fn validate_request_observation(
    intent: &JobIntentV1,
    observation: FinalizedRequestObservationV1,
    limits: &SchemaLimits,
) -> Result<(), RetentionError> {
    let activation_hash = intent
        .activation_preconditions
        .activation_preconditions_hash(limits)
        .map_err(|error| {
            RetentionError::Source(format!("hash activation preconditions: {error}"))
        })?;
    if intent.wwd != observation.wwd
        || intent.pending_nonce != observation.pending_nonce
        || intent.attempt != observation.attempt
        || activation_hash != observation.activation_preconditions_hash
    {
        return Err(RetentionError::Source(
            "request event locator disagrees with typed state".to_owned(),
        ));
    }
    Ok(())
}

fn read_storage_bytes(
    state: &dyn reth_storage_api::StateProvider,
    base: U256,
    max_len: usize,
) -> Result<Vec<u8>, RetentionError> {
    let base_key = B256::new(base.to_be_bytes::<32>());
    let word = state
        .storage(METADOSIS_ADDRESS, base_key)
        .map_err(|error| RetentionError::Source(format!("read job record base slot: {error}")))?
        .unwrap_or_default();
    let encoded_word = word.to_be_bytes::<32>();
    if encoded_word[31] & 1 == 0 {
        let len = usize::from(encoded_word[31] / 2);
        if len > 31 || len > max_len || encoded_word[len..31].iter().any(|byte| *byte != 0) {
            return Err(RetentionError::Source(
                "non-canonical inline StorageBytes".to_owned(),
            ));
        }
        return Ok(encoded_word[..len].to_vec());
    }

    let encoded_len = word
        .checked_sub(U256::from(1))
        .ok_or_else(|| RetentionError::Source("invalid StorageBytes length word".to_owned()))?
        / U256::from(2);
    if encoded_len > U256::from(max_len) {
        return Err(RetentionError::Source(
            "job record exceeds bounded StorageBytes length".to_owned(),
        ));
    }
    let len = encoded_len.to::<usize>();
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    let mut encoded = Vec::with_capacity(len);
    for index in 0..len.div_ceil(32) {
        let slot = data_base + U256::from(index);
        let chunk = state
            .storage(METADOSIS_ADDRESS, B256::new(slot.to_be_bytes::<32>()))
            .map_err(|error| {
                RetentionError::Source(format!("read job record data slot {index}: {error}"))
            })?
            .unwrap_or_default()
            .to_be_bytes::<32>();
        let remaining = len - encoded.len();
        let take = remaining.min(32);
        encoded.extend_from_slice(&chunk[..take]);
        if take < 32 && chunk[take..].iter().any(|byte| *byte != 0) {
            return Err(RetentionError::Source(
                "non-canonical final StorageBytes word".to_owned(),
            ));
        }
    }
    Ok(encoded)
}

pub(crate) trait JournalDurability: Send + Sync {
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn sync_directory(&self, directory: &File) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
struct OsJournalDurability;

impl JournalDurability for OsJournalDurability {
    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> std::io::Result<()> {
        directory.sync_all()
    }
}

struct JournalStore {
    root: PathBuf,
    journal: PathBuf,
    temporary: PathBuf,
    durability: Arc<dyn JournalDurability>,
}

impl JournalStore {
    fn new(root: PathBuf, durability: Arc<dyn JournalDurability>) -> Self {
        Self {
            journal: root.join(JOURNAL_FILENAME),
            temporary: root.join(JOURNAL_TEMP_FILENAME),
            root,
            durability,
        }
    }

    fn initialize(&self) -> Result<Option<JobRegistryV1>, RetentionError> {
        fs::create_dir_all(&self.root)
            .map_err(|source| self.io("create directory", &self.root, source))?;
        self.recover_temporary()?;
        self.read_registry_at(&self.journal)
    }

    fn read_registry_at(&self, path: &Path) -> Result<Option<JobRegistryV1>, RetentionError> {
        if !path.exists() {
            return Ok(None);
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|source| self.io("stat", path, source))?;
        if !metadata.file_type().is_file() {
            return Err(RetentionError::AmbiguousJournal(
                "journal is not a regular file",
            ));
        }
        if metadata.len() > JOURNAL_MAX_BYTES as u64 {
            return Err(RetentionError::MalformedJournal("journal exceeds byte cap"));
        }
        let mut file = File::open(path).map_err(|source| self.io("open", path, source))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|source| self.io("read", path, source))?;
        decode_registry(&bytes).map(Some)
    }

    fn recover_temporary(&self) -> Result<(), RetentionError> {
        let pending = match self.read_registry_at(&self.temporary) {
            Ok(Some(pending)) => pending,
            Ok(None) => return Ok(()),
            Err(
                RetentionError::MalformedJournal(_)
                | RetentionError::UnsupportedJournalVersion { .. },
            ) => {
                // The temp name is never published authority. A crash before
                // its fsync may leave arbitrary/truncated bytes; discard those
                // and replay from the last durable frame/journal generation.
                self.discard_temporary()?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let current = self.read_registry_at(&self.journal)?;
        let valid_successor = match current.as_ref() {
            None => {
                pending.generation == 1
                    && pending.records.len() == 1
                    && pending.records.contains_key(&pending.last_updated)
            }
            Some(current) => journal_successor_is_exact(current, &pending),
        };
        if !valid_successor {
            return Err(RetentionError::AmbiguousJournal(
                "temporary write is not the exact next journal generation",
            ));
        }
        fs::rename(&self.temporary, &self.journal)
            .map_err(|source| self.io("recover temporary", &self.temporary, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync recovered directory", &self.root, source))?;
        Ok(())
    }

    fn discard_temporary(&self) -> Result<(), RetentionError> {
        fs::remove_file(&self.temporary)
            .map_err(|source| self.io("discard torn temporary", &self.temporary, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync discarded temporary", &self.root, source))
    }

    fn persist(
        &self,
        registry: &JobRegistryV1,
        changed: PinRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let encoded = encode_registry(registry);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.temporary)
            .map_err(|source| self.io("create temporary", &self.temporary, source))?;
        file.write_all(&encoded)
            .map_err(|source| self.io("write temporary", &self.temporary, source))?;
        self.durability
            .sync_file(&file)
            .map_err(|source| self.io("fsync temporary", &self.temporary, source))?;
        fs::rename(&self.temporary, &self.journal)
            .map_err(|source| self.io("publish", &self.journal, source))?;
        File::open(&self.root)
            .and_then(|directory| self.durability.sync_directory(&directory))
            .map_err(|source| self.io("fsync directory", &self.root, source))?;
        Ok(ack_for(changed))
    }

    fn io(&self, operation: &'static str, path: &Path, source: std::io::Error) -> RetentionError {
        RetentionError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

fn journal_successor_is_exact(current: &JobRegistryV1, pending: &JobRegistryV1) -> bool {
    if current.generation.checked_add(1) != Some(pending.generation)
        || !pending.records.contains_key(&pending.last_updated)
    {
        return false;
    }
    current.records.iter().all(|(key, record)| {
        *key == pending.last_updated
            || pending.records.get(key) == Some(record)
            || (pending.records.get(key).is_none()
                && matches!(record.state, PinStateV1::Released { .. }))
    }) && pending
        .records
        .keys()
        .all(|key| current.records.contains_key(key) || *key == pending.last_updated)
}

#[derive(Clone, Debug)]
struct JobRegistryV1 {
    generation: u64,
    last_updated: B256,
    records: BTreeMap<B256, PinRecordV1>,
}

struct CoordinatorInner {
    status: RetentionStatus,
    registry: Option<JobRegistryV1>,
}

/// Node-owned independently keyed multi-job OCOMP pin coordinator.
pub struct OcompRetentionCoordinator {
    store: JournalStore,
    inner: Mutex<CoordinatorInner>,
    source: Arc<dyn FinalizedInputProofSource>,
    retained_tributes: Option<Arc<RetainedTributeWriter>>,
}

/// Process-local retention selector that can be shared with projection before
/// the provider-backed coordinator is available.
///
/// Installation is one-way: every clone of the surrounding `Arc` observes the
/// exact same coordinator, and an attempted replacement fails closed.
#[derive(Default)]
pub struct SharedOcompRetentionSelector {
    coordinator: OnceLock<Arc<OcompRetentionCoordinator>>,
}

impl SharedOcompRetentionSelector {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            coordinator: OnceLock::new(),
        }
    }

    pub fn install(
        &self,
        coordinator: Arc<OcompRetentionCoordinator>,
    ) -> Result<(), RetentionError> {
        self.coordinator
            .set(coordinator)
            .map_err(|_| RetentionError::RetentionCoordinatorAlreadyInstalled)
    }

    /// Returns the exact finalized retention generation used by snapshot handoff.
    pub fn finalized_job_record(
        &self,
        job_id: B256,
    ) -> Result<(u64, FinalizedJobPinV1), RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .finalized_job_record(job_id)
    }

    /// Returns restart-safe source-generation candidates for discovery handoff.
    ///
    /// A terminal journal may have been reached directly from `Finalized` or
    /// through `Exported`; both exact predecessor generations are returned so
    /// the durable discovery spool can select its existing authority.
    pub fn discovery_job_records(
        &self,
        job_id: B256,
    ) -> Result<Vec<(u64, FinalizedJobPinV1)>, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .discovery_job_records(job_id)
    }

    /// Durably binds an exporter ACK to the exact finalized retention generation.
    pub fn confirm_export_ack(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .confirm_export_ack(job_id, source_generation, lease_generation, manifest_hash)
    }

    /// Returns the exact durable authority for an already released export.
    pub fn released_export_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ExportAuthorityV1>, RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .released_export_authority(job_id)
    }

    /// Reconcile one frame already loaded by the single finalized reader.
    pub fn reconcile_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> Result<(), RetentionError> {
        self.coordinator
            .get()
            .ok_or(RetentionError::RetentionCoordinatorNotInstalled)?
            .reconcile_finalized_frame(frame, observation)
    }
}

#[cfg(test)]
const FINALITY_NOTIFICATION_CAPACITY: usize = 256;
#[cfg(test)]
const FINALITY_RECONCILIATION_INITIAL_BACKOFF_MS: u64 = 25;
#[cfg(test)]
const FINALITY_RECONCILIATION_MAX_BACKOFF_MS: u64 = 800;

/// Bounded, ordered node-owned worker for finalized-block reconciliation.
///
/// Before a tentative pin exists, every finalized block is an input-discovery
/// boundary and must remain observable: a later block cannot stand in for the
/// request receipts of an earlier block. The bounded FIFO therefore preserves
/// exact notification order and rejects overflow instead of silently
/// coalescing away a possible request.
#[cfg(test)]
pub struct OcompRetentionService {
    coordinator: Arc<OcompRetentionCoordinator>,
    snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
    finalized_rx: tokio::sync::mpsc::Receiver<ConsensusBlock>,
    execution_ready_rx: tokio::sync::watch::Receiver<u64>,
}

/// Arms the exact finalized snapshot before later finalized blocks can advance
/// the CE marker. This callback runs in the node-owned retention worker, never
/// in the consensus finalization actor.
#[cfg(test)]
pub trait FinalizedSnapshotArmer: Send + Sync {
    fn arm_finalized_snapshot(&self, job_id: B256) -> Result<(), String>;
}

/// Consensus-facing handle. Candidate preparation is intentionally synchronous
/// because a positive vote depends on its durable ack; finality notification is
/// a constant-time local enqueue.
#[derive(Clone)]
pub struct OcompRetentionHandle {
    coordinator: Arc<OcompRetentionCoordinator>,
    #[cfg(test)]
    finalized_tx: Option<tokio::sync::mpsc::Sender<ConsensusBlock>>,
}

/// Executor-facing notification that exact canonical receipts through `height`
/// are locally readable. It carries no consensus authority: certified block
/// identity still comes exclusively from [`OcompRetentionHandle`].
#[derive(Clone)]
#[cfg(test)]
pub struct OcompRetentionExecutionHandle {
    execution_ready_tx: tokio::sync::watch::Sender<u64>,
}

#[cfg(test)]
impl OcompRetentionService {
    pub fn new(coordinator: Arc<OcompRetentionCoordinator>) -> (Self, OcompRetentionHandle) {
        Self::new_with_snapshot_armer(coordinator, None)
    }

    pub fn new_with_snapshot_armer(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
    ) -> (Self, OcompRetentionHandle) {
        let (execution_ready_tx, execution_ready_rx) = tokio::sync::watch::channel(u64::MAX);
        drop(execution_ready_tx);
        Self::new_inner(coordinator, snapshot_armer, execution_ready_rx)
    }

    /// Construct the production ordered join between certified finality and
    /// exact local execution. `initial_execution_ready_height` is the executor
    /// recovery anchor; later progress is supplied through the returned handle.
    pub fn new_with_execution_readiness(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
        initial_execution_ready_height: u64,
    ) -> (Self, OcompRetentionHandle, OcompRetentionExecutionHandle) {
        let (execution_ready_tx, execution_ready_rx) =
            tokio::sync::watch::channel(initial_execution_ready_height);
        let (service, handle) = Self::new_inner(coordinator, snapshot_armer, execution_ready_rx);
        (
            service,
            handle,
            OcompRetentionExecutionHandle { execution_ready_tx },
        )
    }

    fn new_inner(
        coordinator: Arc<OcompRetentionCoordinator>,
        snapshot_armer: Option<Arc<dyn FinalizedSnapshotArmer>>,
        execution_ready_rx: tokio::sync::watch::Receiver<u64>,
    ) -> (Self, OcompRetentionHandle) {
        let (finalized_tx, finalized_rx) =
            tokio::sync::mpsc::channel(FINALITY_NOTIFICATION_CAPACITY);
        (
            Self {
                coordinator: coordinator.clone(),
                snapshot_armer,
                finalized_rx,
                execution_ready_rx,
            },
            OcompRetentionHandle {
                coordinator,
                finalized_tx: Some(finalized_tx),
            },
        )
    }

    pub async fn run(mut self) {
        while let Some(block) = self.finalized_rx.recv().await {
            let block_number = block.number();
            let block_hash = block.block_hash();
            while block_number > *self.execution_ready_rx.borrow_and_update() {
                if self.execution_ready_rx.changed().await.is_err() {
                    tracing::warn!(
                        target: "outbe::ocomp",
                        block_number,
                        block_hash = %block_hash,
                        execution_ready_height = *self.execution_ready_rx.borrow(),
                        "OCOMP execution-readiness source closed with finalized work pending; marshal replay will recover it after restart"
                    );
                    return;
                }
            }

            let mut attempt = 0_u64;
            loop {
                let coordinator = self.coordinator.clone();
                let snapshot_armer = self.snapshot_armer.clone();
                let finalized_block = block.clone();
                match tokio::task::spawn_blocking(move || {
                    coordinator
                        .reconcile_finalized(&finalized_block)
                        .map_err(|error| error.to_string())?;
                    if let Some(snapshot_armer) = snapshot_armer {
                        for job in coordinator
                            .finalized_live_jobs()
                            .map_err(|error| error.to_string())?
                        {
                            snapshot_armer.arm_finalized_snapshot(job.job_id)?;
                        }
                    }
                    Ok::<(), String>(())
                })
                .await
                {
                    Ok(Ok(())) => break,
                    Ok(Err(error)) => {
                        attempt = attempt.saturating_add(1);
                        let backoff_ms = FINALITY_RECONCILIATION_INITIAL_BACKOFF_MS
                            .saturating_mul(1_u64 << attempt.saturating_sub(1).min(5))
                            .min(FINALITY_RECONCILIATION_MAX_BACKOFF_MS);
                        if attempt == 1 || attempt.is_multiple_of(64) {
                            tracing::warn!(
                                target: "outbe::ocomp",
                                block_number,
                                block_hash = %block_hash,
                                attempt,
                                backoff_ms,
                                %error,
                                "OCOMP pin reconciliation is stalled; retaining the exact finalized block"
                            );
                        } else {
                            tracing::debug!(
                                target: "outbe::ocomp",
                                block_number,
                                block_hash = %block_hash,
                                attempt,
                                backoff_ms,
                                %error,
                                "OCOMP pin reconciliation will retry the same exact finalized block"
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    }
                    Err(error) => {
                        attempt = attempt.saturating_add(1);
                        tracing::warn!(
                            target: "outbe::ocomp",
                            block_number,
                            block_hash = %block_hash,
                            attempt,
                            %error,
                            "OCOMP pin reconciliation task failed; retaining the exact finalized block"
                        );
                        tokio::time::sleep(Duration::from_millis(
                            FINALITY_RECONCILIATION_MAX_BACKOFF_MS,
                        ))
                        .await;
                    }
                }
            }
        }
    }
}

impl OcompRetentionHandle {
    /// Candidate-only consensus hook used with the unified finalized reader.
    /// Finalized reconciliation is owned exclusively by that reader.
    #[must_use]
    pub fn for_unified_finalized_reader(coordinator: Arc<OcompRetentionCoordinator>) -> Self {
        Self {
            coordinator,
            #[cfg(test)]
            finalized_tx: None,
        }
    }
}

#[cfg(test)]
impl OcompRetentionExecutionHandle {
    pub fn notify_execution_finalized(&self, height: u64) -> Result<(), OcompRetentionHookError> {
        if self.execution_ready_tx.receiver_count() == 0 {
            return Err(OcompRetentionHookError::new(
                "OCOMP retention execution-readiness worker is unavailable",
            ));
        }
        self.execution_ready_tx.send_if_modified(|current| {
            if height > *current {
                *current = height;
                true
            } else {
                false
            }
        });
        Ok(())
    }
}

impl OcompRetentionCoordinator {
    /// Open a managed journal root. Corrupt, ambiguous or unavailable storage
    /// quarantines OCOMP but does not fail node/consensus startup.
    pub fn open(root: impl Into<PathBuf>, source: Arc<dyn FinalizedInputProofSource>) -> Self {
        Self::open_with_durability(root, source, Arc::new(OsJournalDurability))
    }

    pub fn open_with_retained_tributes(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        retained_tributes: Arc<RetainedTributeWriter>,
    ) -> Self {
        Self::open_inner(
            root.into(),
            source,
            Arc::new(OsJournalDurability),
            Some(retained_tributes),
        )
    }

    pub(crate) fn open_with_durability(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        Self::open_inner(root.into(), source, durability, None)
    }

    fn open_inner(
        root: PathBuf,
        source: Arc<dyn FinalizedInputProofSource>,
        durability: Arc<dyn JournalDurability>,
        retained_tributes: Option<Arc<RetainedTributeWriter>>,
    ) -> Self {
        let store = JournalStore::new(root, durability);
        let (status, registry) = match store.initialize() {
            Ok(Some(registry)) => {
                let status = registry
                    .records
                    .get(&registry.last_updated)
                    .copied()
                    .map(RetentionStatus::Ready)
                    .unwrap_or_else(|| RetentionStatus::Quarantined {
                        reason: "OCOMP registry last-updated key is missing".to_owned(),
                    });
                (status, Some(registry))
            }
            Ok(None) => (RetentionStatus::Empty, None),
            Err(error) => (
                RetentionStatus::Quarantined {
                    reason: error.to_string(),
                },
                None,
            ),
        };
        Self {
            store,
            inner: Mutex::new(CoordinatorInner { status, registry }),
            source,
            retained_tributes,
        }
    }

    pub fn status(&self) -> RetentionStatus {
        self.lock()
            .map(|inner| inner.status.clone())
            .unwrap_or_else(|error| RetentionStatus::Quarantined {
                reason: error.to_string(),
            })
    }

    /// Returns every independently addressable finalized/exported job.
    pub fn finalized_live_jobs(&self) -> Result<Vec<FinalizedJobPinV1>, RetentionError> {
        let inner = self.lock()?;
        if let RetentionStatus::Quarantined { ref reason } = inner.status {
            return Err(RetentionError::Quarantined(reason.clone()));
        }
        let mut jobs = Vec::new();
        for record in inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.values())
        {
            match record.state {
                PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }
                | PinStateV1::Exported {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    ..
                } => jobs.push(FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                }),
                PinStateV1::Tentative { .. }
                | PinStateV1::Terminal { .. }
                | PinStateV1::Released { .. } => {}
            }
        }
        jobs.sort_by_key(|job| (job.candidate.block_number, job.candidate.block_hash));
        Ok(jobs)
    }

    pub fn finalized_job_record(
        &self,
        job_id: B256,
    ) -> Result<(u64, FinalizedJobPinV1), RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => Ok((
                record.generation,
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
            )),
            _ => Err(RetentionError::InvalidTransition(
                "snapshot handoff requires the exact finalized Job",
            )),
        }
    }

    pub fn discovery_job_records(
        &self,
        job_id: B256,
    ) -> Result<Vec<(u64, FinalizedJobPinV1)>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let (pin, generations) = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(record.generation), None],
            ),
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(export.source_generation), None],
            ),
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
                ..
            } => (
                FinalizedJobPinV1 {
                    candidate,
                    job_id,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                },
                [Some(export.source_generation), None],
            ),
            PinStateV1::Tentative { .. } | PinStateV1::Released { .. } => {
                return Err(RetentionError::InvalidTransition(
                    "discovery requires a live or terminal finalized job",
                ));
            }
        };
        let generations = generations
            .into_iter()
            .flatten()
            .map(|generation| (generation, pin))
            .collect::<Vec<_>>();
        if generations.is_empty() {
            return Err(RetentionError::GenerationOverflow);
        }
        Ok(generations)
    }

    pub fn released_export_authority(
        &self,
        job_id: B256,
    ) -> Result<Option<ExportAuthorityV1>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(match record.state {
            PinStateV1::Released {
                job_id: Some(existing),
                reason: PinReleaseReason::RetentionSatisfied,
                export,
                ..
            } if existing == job_id => export,
            PinStateV1::Tentative { .. }
            | PinStateV1::Finalized { .. }
            | PinStateV1::Exported { .. }
            | PinStateV1::Terminal { .. }
            | PinStateV1::Released { .. } => None,
        })
    }

    /// Returns the exact live `Exported` record addressed by `JobId`.
    ///
    /// This deliberately consults the durable multi-job registry rather than
    /// [`Self::status`], whose single operational summary may describe a newer
    /// job. Callers must not substitute another live job.
    #[cfg(test)]
    pub(crate) fn exported_job_record(&self, job_id: B256) -> Result<PinRecordV1, RetentionError> {
        let inner = self.lock()?;
        if let RetentionStatus::Quarantined { ref reason } = inner.status {
            return Err(RetentionError::Quarantined(reason.clone()));
        }
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Exported {
                job_id: current, ..
            } if current == job_id => Ok(record),
            _ => Err(RetentionError::InvalidTransition(
                "attestation requires the exact exported Job",
            )),
        }
    }

    pub fn prepare_candidate(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        let candidate = self.source.candidate_for_block(block).map_err(hook_error)?;
        if let Some(candidate) = candidate {
            self.record_tentative(candidate).map_err(hook_error)?;
        }
        Ok(())
    }

    pub fn reconcile_finalized(
        &self,
        block: &ConsensusBlock,
    ) -> Result<(), OcompRetentionHookError> {
        if let RetentionStatus::Quarantined { reason } = self.status() {
            return Err(OcompRetentionHookError::new(reason));
        }
        if let Some(candidate) = self.source.candidate_for_block(block).map_err(hook_error)? {
            self.record_tentative(candidate).map_err(hook_error)?;
        }
        let candidates = {
            let inner = self.lock().map_err(hook_error)?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Tentative { candidate }
                        if candidate.block_number <= block.number() =>
                    {
                        Some(candidate)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for candidate in candidates {
            match self
                .source
                .resolve_finality(candidate)
                .map_err(hook_error)?
            {
                CandidateFinalityV1::Finalized(finalized) => {
                    self.finalize_exact(finalized).map_err(hook_error)?;
                }
                CandidateFinalityV1::Orphaned => {
                    self.release_orphan(candidate, block.number())
                        .map_err(hook_error)?;
                }
            }
        }
        let live = {
            let inner = self.lock().map_err(hook_error)?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Finalized {
                        candidate, job_id, ..
                    }
                    | PinStateV1::Exported {
                        candidate, job_id, ..
                    } => Some((record.generation, candidate, job_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (generation, candidate, job_id) in live {
            if let Some(terminal_height) = self
                .source
                .terminal_height_at(block, candidate, job_id)
                .map_err(hook_error)?
            {
                let terminal_finality_height = terminal_height.max(block.number());
                self.observe_terminal(job_id, generation, terminal_finality_height)
                    .map_err(hook_error)?;
            }
        }
        while self
            .release_due(block.number())
            .map_err(hook_error)?
            .is_some()
        {}
        Ok(())
    }

    /// Reconcile retention from the exact block and receipts owned by the
    /// unified finalized reader. This is the production finalized path; unlike
    /// [`Self::reconcile_finalized`], it performs no receipt-provider query.
    pub fn reconcile_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> Result<(), RetentionError> {
        if let RetentionStatus::Quarantined { reason } = self.status() {
            return Err(RetentionError::Quarantined(reason));
        }
        let height = frame.identity().number;
        if let Some(observation) = observation {
            let candidate = self
                .source
                .candidate_for_finalized_observation(frame, observation)?;
            self.record_tentative(candidate)?;
        }
        let candidates = {
            let inner = self.lock()?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Tentative { candidate } if candidate.block_number <= height => {
                        Some(candidate)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for candidate in candidates {
            match self.source.resolve_finality(candidate)? {
                CandidateFinalityV1::Finalized(finalized) => {
                    self.finalize_exact(finalized)?;
                }
                CandidateFinalityV1::Orphaned => {
                    self.release_orphan(candidate, height)?;
                }
            }
        }
        let live = {
            let inner = self.lock()?;
            inner
                .registry
                .as_ref()
                .into_iter()
                .flat_map(|registry| registry.records.values())
                .filter_map(|record| match record.state {
                    PinStateV1::Finalized {
                        candidate, job_id, ..
                    }
                    | PinStateV1::Exported {
                        candidate, job_id, ..
                    } => Some((record.generation, candidate, job_id)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (generation, candidate, job_id) in live {
            if let Some(terminal_height) = self
                .source
                .terminal_height_at_finalized_frame(frame, candidate, job_id)?
            {
                self.observe_terminal(job_id, generation, terminal_height.max(height))?;
            }
        }
        while self.release_due(height)?.is_some() {}
        Ok(())
    }

    pub fn record_tentative(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        if let RetentionStatus::Quarantined { reason } = &inner.status {
            return Err(RetentionError::Quarantined(reason.clone()));
        }
        let key = candidate.block_hash;
        if let Some(record) = inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            .copied()
        {
            return match record.state {
                PinStateV1::Tentative {
                    candidate: existing,
                } if existing == candidate => Ok(ack_for(record)),
                PinStateV1::Released {
                    candidate: existing,
                    reason: PinReleaseReason::Orphaned,
                    ..
                } if existing == candidate => Err(RetentionError::OrphanedCandidate),
                _ => Err(RetentionError::ConflictingCandidate),
            };
        }
        if inner.registry.as_ref().is_some_and(|registry| {
            registry
                .records
                .values()
                .filter(|record| !matches!(record.state, PinStateV1::Released { .. }))
                .count()
                >= JOURNAL_RECORD_COUNT_MAX
        }) {
            return Err(RetentionError::RegistryCapacity);
        }
        let generation = next_registry_generation(&inner)?;
        self.persist_locked(
            &mut inner,
            key,
            PinRecordV1 {
                generation,
                state: PinStateV1::Tentative { candidate },
            },
        )
    }

    pub fn record_exported(
        &self,
        job_id: B256,
        expected_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        if exact_export_replay(
            record,
            job_id,
            expected_generation,
            lease_generation,
            manifest_hash,
        ) {
            return Ok(ack_for(record));
        }
        ensure_generation(record, expected_generation)?;
        let (candidate, finality_recorded_height, open_height, deadline_height) = match record.state
        {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if existing == job_id => (
                candidate,
                finality_recorded_height,
                open_height,
                deadline_height,
            ),
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "export requires the exact finalized job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export: ExportAuthorityV1 {
                    source_generation: expected_generation,
                    lease_generation,
                    manifest_hash,
                },
            },
        )
    }

    /// Confirm a durable spool ACK across restart windows. A live finalized
    /// record performs the Exported transition; later states accept only the
    /// exact source generation that must have preceded them.
    pub fn confirm_export_ack(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<DurablePinAck, RetentionError> {
        if source_generation == 0 || lease_generation == 0 || manifest_hash.is_zero() {
            return Err(RetentionError::InvalidTransition(
                "export ACK authority is incomplete",
            ));
        }
        match self.record_exported(job_id, source_generation, lease_generation, manifest_hash) {
            Ok(ack) => return Ok(ack),
            Err(RetentionError::InvalidTransition(_) | RetentionError::StaleGeneration { .. }) => {}
            Err(error) => return Err(error),
        }
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        let export = match record.state {
            PinStateV1::Terminal {
                job_id: existing,
                export,
                ..
            } if existing == job_id => Some(export),
            PinStateV1::Released {
                job_id: Some(existing),
                reason: PinReleaseReason::RetentionSatisfied,
                export,
                ..
            } if existing == job_id => export,
            _ => None,
        };
        if export
            == Some(ExportAuthorityV1 {
                source_generation,
                lease_generation,
                manifest_hash,
            })
        {
            Ok(ack_for(record))
        } else {
            Err(RetentionError::InvalidTransition(
                "export ACK does not match the retained source generation",
            ))
        }
    }

    pub fn replay_exported(
        &self,
        job_id: B256,
        source_generation: u64,
        lease_generation: u64,
        manifest_hash: B256,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        Ok(exact_export_replay(
            record,
            job_id,
            source_generation,
            lease_generation,
            manifest_hash,
        )
        .then(|| ack_for(record)))
    }

    pub fn build_finalized_intent_proof(
        &self,
        job_id: B256,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        let candidate = self.live_candidate(job_id)?;
        let proof = self.source.build_finalized_intent_proof(candidate)?;
        let limits = poc_schema_limits();
        let intent = proof
            .decoded_intent(&limits)
            .map_err(|error| RetentionError::Source(format!("decode finalized intent: {error}")))?;
        let proof_intent_id = intent
            .intent_id(&limits)
            .map_err(|error| RetentionError::Source(format!("derive proof IntentId: {error}")))?;
        let proof_job_id = intent
            .job_id(candidate.block_hash, candidate.state_root, &limits)
            .map_err(|error| RetentionError::Source(format!("derive proof JobId: {error}")))?;
        if proof_job_id != job_id
            || proof_intent_id != candidate.intent_id
            || proof.protocol_bundle_hash != candidate.protocol_bundle_hash
            || proof.parent_accounting.finalized_block_number != candidate.block_number
            || proof.parent_accounting.finalized_block_hash != candidate.block_hash
            || intent.wwd != candidate.wwd
            || intent.ce_sealed_root != candidate.ce_sealed_root
            || intent
                .input_lease_id()
                .map_err(|error| RetentionError::Source(error.to_string()))?
                != candidate.input_lease_id
        {
            return Err(RetentionError::Source(
                "finalized-intent proof differs from the exact live pin".to_owned(),
            ));
        }
        Ok(proof)
    }

    pub fn build_lysis_openings(
        &self,
        job_id: B256,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, RetentionError> {
        let candidate = self.live_candidate(job_id)?;
        let proof = self.source.build_lysis_openings(candidate, subjects)?;
        if proof.job_id != job_id
            || proof.protocol_bundle_hash != candidate.protocol_bundle_hash
            || proof.finalized_block_hash != candidate.block_hash
            || proof.finalized_state_root != candidate.state_root
            || proof.wwd != candidate.wwd
        {
            return Err(RetentionError::Source(
                "Lysis openings differ from the exact live pin".to_owned(),
            ));
        }
        Ok(proof)
    }

    pub fn observe_terminal(
        &self,
        job_id: B256,
        expected_generation: u64,
        terminal_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let release_height = terminal_height
            .checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS)
            .ok_or(RetentionError::InvalidTransition(
                "terminal release height overflows",
            ))?;
        let mut inner = self.lock()?;
        let (key, record) = record_for_job(&inner, job_id)?;
        ensure_generation(record, expected_generation)?;
        let (candidate, finality_recorded_height, open_height, deadline_height, export) =
            match record.state {
                // An exporter outage must not make retained input eligible for GC.
                // The canonical terminal fact is observed again on every later
                // finalized frame; defer the transition until the durable export
                // ACK has advanced this exact generation to `Exported`.
                PinStateV1::Finalized {
                    job_id: existing, ..
                } if existing == job_id => return Ok(ack_for(record)),
                PinStateV1::Exported {
                    candidate,
                    job_id: existing,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    export,
                } if existing == job_id => (
                    candidate,
                    finality_recorded_height,
                    open_height,
                    deadline_height,
                    export,
                ),
                PinStateV1::Terminal {
                    job_id: existing,
                    terminal_height: existing_terminal,
                    release_height: existing_release,
                    ..
                } if existing == job_id
                    && existing_terminal == terminal_height
                    && existing_release == release_height =>
                {
                    return Ok(ack_for(record));
                }
                _ => {
                    return Err(RetentionError::InvalidTransition(
                        "terminal transition requires the exact live job",
                    ));
                }
            };
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
                terminal_height,
                release_height,
            },
        )
    }

    pub fn release_due(
        &self,
        finalized_height: u64,
    ) -> Result<Option<DurablePinAck>, RetentionError> {
        let mut inner = self.lock()?;
        if let RetentionStatus::Quarantined { reason } = &inner.status {
            return Err(RetentionError::Quarantined(reason.clone()));
        }
        let Some((key, record)) = inner.registry.as_ref().and_then(|registry| {
            registry.records.iter().find_map(|(key, record)| {
                matches!(
                    record.state,
                    PinStateV1::Terminal { release_height, .. }
                        if release_height <= finalized_height
                )
                .then_some((*key, *record))
            })
        }) else {
            return Ok(None);
        };
        let (candidate, job_id, release_height, export) = match record.state {
            PinStateV1::Terminal {
                candidate,
                job_id,
                release_height,
                export,
                ..
            } => (candidate, job_id, release_height, export),
            _ => unreachable!("release candidate is selected from terminal records"),
        };
        if finalized_height < release_height {
            return Ok(None);
        }
        let complete = if lease_has_other_references(&inner, key, candidate.input_lease_id) {
            true
        } else if let Some(retained_tributes) = self.retained_tributes.as_ref() {
            retained_tributes
                .release_input_lease_page(candidate.input_lease_id)
                .map_err(|error| RetentionError::RetainedTributeGc(error.to_string()))?
        } else {
            true
        };
        if !complete {
            return Ok(None);
        }
        self.persist_next(
            &mut inner,
            key,
            record,
            PinStateV1::Released {
                candidate,
                job_id: Some(job_id),
                reason: PinReleaseReason::RetentionSatisfied,
                observed_height: finalized_height,
                export: Some(export),
            },
        )
        .map(Some)
    }

    pub fn is_signable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    pub fn is_exportable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        record_for_job(&inner, job_id).is_ok_and(|(_, record)| {
            matches!(
                record.state,
                PinStateV1::Finalized {
                    job_id: current, ..
                } | PinStateV1::Exported {
                    job_id: current, ..
                } if current == job_id
            )
        })
    }

    fn live_candidate(&self, job_id: B256) -> Result<CandidatePinV1, RetentionError> {
        let inner = self.lock()?;
        let (_, record) = record_for_job(&inner, job_id)?;
        match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: current,
                ..
            }
            | PinStateV1::Exported {
                candidate,
                job_id: current,
                ..
            } if current == job_id => Ok(candidate),
            _ => Err(RetentionError::InvalidTransition(
                "proof construction requires the exact live finalized job",
            )),
        }
    }

    fn finalize_exact(
        &self,
        finalized: FinalizedJobPinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let key = finalized.candidate.block_hash;
        let record = record_for_candidate(&inner, finalized.candidate)?;
        match record.state {
            PinStateV1::Tentative { candidate } if candidate == finalized.candidate => self
                .persist_next(
                    &mut inner,
                    key,
                    record,
                    PinStateV1::Finalized {
                        candidate,
                        job_id: finalized.job_id,
                        finality_recorded_height: finalized.finality_recorded_height,
                        open_height: finalized.open_height,
                        deadline_height: finalized.deadline_height,
                    },
                ),
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            } if candidate == finalized.candidate
                && job_id == finalized.job_id
                && finality_recorded_height == finalized.finality_recorded_height
                && open_height == finalized.open_height
                && deadline_height == finalized.deadline_height =>
            {
                Ok(ack_for(record))
            }
            PinStateV1::Released { candidate, .. } if candidate == finalized.candidate => {
                Err(RetentionError::OrphanedCandidate)
            }
            _ => Err(RetentionError::InvalidTransition(
                "finality does not match the tentative candidate",
            )),
        }
    }

    fn release_orphan(
        &self,
        candidate: CandidatePinV1,
        observed_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let key = candidate.block_hash;
        let record = record_for_candidate(&inner, candidate)?;
        match record.state {
            PinStateV1::Tentative { candidate: current } if current == candidate => {
                self.release_orphan_locked(&mut inner, key, record, candidate, observed_height)
            }
            PinStateV1::Released {
                candidate: current,
                reason: PinReleaseReason::Orphaned,
                ..
            } if current == candidate => Ok(ack_for(record)),
            _ => Err(RetentionError::InvalidTransition(
                "orphan release does not match the tentative candidate",
            )),
        }
    }

    fn release_orphan_locked(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        record: PinRecordV1,
        candidate: CandidatePinV1,
        observed_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        if !lease_has_other_references(inner, key, candidate.input_lease_id) {
            if let Some(retained_tributes) = &self.retained_tributes {
                let complete = retained_tributes
                    .release_input_lease_page(candidate.input_lease_id)
                    .map_err(|error| RetentionError::RetainedTributeGc(error.to_string()))?;
                if !complete {
                    return Ok(ack_for(record));
                }
            }
        }
        self.persist_next(
            inner,
            key,
            record,
            PinStateV1::Released {
                candidate,
                job_id: None,
                reason: PinReleaseReason::Orphaned,
                observed_height,
                export: None,
            },
        )
    }

    fn persist_next(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        current: PinRecordV1,
        state: PinStateV1,
    ) -> Result<DurablePinAck, RetentionError> {
        if inner
            .registry
            .as_ref()
            .and_then(|registry| registry.records.get(&key))
            != Some(&current)
        {
            return Err(RetentionError::InvalidTransition(
                "Job Registry entry changed before transition",
            ));
        }
        let generation = next_registry_generation(inner)?;
        self.persist_locked(inner, key, PinRecordV1 { generation, state })
    }

    fn persist_locked(
        &self,
        inner: &mut CoordinatorInner,
        key: B256,
        record: PinRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut registry = inner.registry.clone().unwrap_or_else(|| JobRegistryV1 {
            generation: record.generation,
            last_updated: key,
            records: BTreeMap::new(),
        });
        if !registry.records.contains_key(&key)
            && registry.records.len() >= JOURNAL_RECORD_PRESSURE_WATERMARK
        {
            registry
                .records
                .retain(|_, existing| !matches!(existing.state, PinStateV1::Released { .. }));
        }
        registry.generation = record.generation;
        registry.last_updated = key;
        registry.records.insert(key, record);
        match self.store.persist(&registry, record) {
            Ok(ack) => {
                inner.registry = Some(registry);
                inner.status = RetentionStatus::Ready(record);
                Ok(ack)
            }
            Err(error) => {
                inner.status = RetentionStatus::Quarantined {
                    reason: error.to_string(),
                };
                Err(error)
            }
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, CoordinatorInner>, RetentionError> {
        self.inner.lock().map_err(|_| RetentionError::Poisoned)
    }
}

impl TributeRetentionSelector for OcompRetentionCoordinator {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        if self.retained_tributes.is_none() {
            return Err(RetentionError::RetainedTributeStorageUnavailable.to_string());
        }
        let inner = self.lock().map_err(|error| error.to_string())?;
        if let RetentionStatus::Quarantined { reason } = &inner.status {
            return Err(RetentionError::Quarantined(reason.clone()).to_string());
        }
        let mut selected = BTreeSet::new();
        for record in inner
            .registry
            .as_ref()
            .into_iter()
            .flat_map(|registry| registry.records.values())
        {
            match record.state {
                PinStateV1::Tentative { candidate } if candidate.wwd == worldwide_day.value() => {
                    selected.insert(candidate.input_lease_id);
                }
                PinStateV1::Finalized { candidate, .. }
                | PinStateV1::Exported { candidate, .. }
                | PinStateV1::Terminal { candidate, .. }
                    if candidate.wwd == worldwide_day.value() =>
                {
                    selected.insert(candidate.input_lease_id);
                }
                _ => {}
            }
        }
        match selected.len() {
            0 => Ok(None),
            1 => Ok(Some(RetainedTributePin {
                input_lease_id: *selected.first().expect("one selected retention key"),
                worldwide_day,
            })),
            _ => Err("multiple input retention identities exist for one WWD".to_owned()),
        }
    }
}

impl TributeRetentionSelector for SharedOcompRetentionSelector {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        self.coordinator
            .get()
            .ok_or_else(|| RetentionError::RetentionCoordinatorNotInstalled.to_string())?
            .active_pin_for(worldwide_day)
    }
}

impl OcompRetentionHook for OcompRetentionHandle {
    fn prepare_candidate(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        self.coordinator.prepare_candidate(block)
    }

    fn reconcile_finalized(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        #[cfg(not(test))]
        let _ = block;
        #[cfg(test)]
        {
            let Some(finalized_tx) = &self.finalized_tx else {
                return Ok(());
            };
            finalized_tx
                .try_send(block.clone())
                .map_err(|error| match error {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                        OcompRetentionHookError::new("OCOMP retention finality queue is full")
                    }
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        OcompRetentionHookError::new("OCOMP retention worker is unavailable")
                    }
                })?;
        }
        Ok(())
    }
}

fn hook_error(error: RetentionError) -> OcompRetentionHookError {
    OcompRetentionHookError::new(error.to_string())
}

fn next_registry_generation(inner: &CoordinatorInner) -> Result<u64, RetentionError> {
    inner
        .registry
        .as_ref()
        .map_or(0, |registry| registry.generation)
        .checked_add(1)
        .ok_or(RetentionError::GenerationOverflow)
}

fn record_for_candidate(
    inner: &CoordinatorInner,
    candidate: CandidatePinV1,
) -> Result<PinRecordV1, RetentionError> {
    inner
        .registry
        .as_ref()
        .and_then(|registry| registry.records.get(&candidate.block_hash))
        .copied()
        .filter(|record| record_candidate(*record) == candidate)
        .ok_or(RetentionError::InvalidTransition(
            "candidate has no exact Job Registry entry",
        ))
}

fn record_for_job(
    inner: &CoordinatorInner,
    job_id: B256,
) -> Result<(B256, PinRecordV1), RetentionError> {
    inner
        .registry
        .as_ref()
        .into_iter()
        .flat_map(|registry| registry.records.iter())
        .find_map(|(key, record)| {
            matches!(
                record.state,
                PinStateV1::Finalized {
                    job_id: current, ..
                } | PinStateV1::Exported {
                    job_id: current, ..
                } | PinStateV1::Terminal {
                    job_id: current, ..
                } | PinStateV1::Released {
                    job_id: Some(current),
                    ..
                } if current == job_id
            )
            .then_some((*key, *record))
        })
        .ok_or(RetentionError::InvalidTransition(
            "JobId has no exact Job Registry entry",
        ))
}

fn lease_has_other_references(
    inner: &CoordinatorInner,
    excluded_key: B256,
    input_lease_id: B256,
) -> bool {
    inner.registry.as_ref().is_some_and(|registry| {
        registry.records.iter().any(|(key, record)| {
            *key != excluded_key
                && record_candidate(*record).input_lease_id == input_lease_id
                && !matches!(record.state, PinStateV1::Released { .. })
        })
    })
}

const fn record_candidate(record: PinRecordV1) -> CandidatePinV1 {
    match record.state {
        PinStateV1::Tentative { candidate }
        | PinStateV1::Finalized { candidate, .. }
        | PinStateV1::Exported { candidate, .. }
        | PinStateV1::Terminal { candidate, .. }
        | PinStateV1::Released { candidate, .. } => candidate,
    }
}

fn ensure_generation(record: PinRecordV1, expected_generation: u64) -> Result<(), RetentionError> {
    if record.generation != expected_generation {
        return Err(RetentionError::StaleGeneration {
            expected: expected_generation,
            actual: record.generation,
        });
    }
    Ok(())
}

fn ack_for(record: PinRecordV1) -> DurablePinAck {
    DurablePinAck {
        generation: record.generation,
        record_hash: keccak256(encode_record(record)),
    }
}

fn exact_export_replay(
    record: PinRecordV1,
    job_id: B256,
    source_generation: u64,
    lease_generation: u64,
    manifest_hash: B256,
) -> bool {
    matches!(
        record.state,
        PinStateV1::Exported {
            job_id: existing,
            export,
            ..
        } if existing == job_id
            && export == ExportAuthorityV1 {
                source_generation,
                lease_generation,
                manifest_hash,
            }
    )
}

fn candidate_job_id(candidate: CandidatePinV1) -> Result<B256, RetentionError> {
    job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .map_err(|error| RetentionError::Source(format!("derive tentative JobId: {error}")))
}

pub(super) fn retention_terminal_height_for_status(
    status: OcompJobStatus,
    observed_height: u64,
    deadline_height: u64,
    terminal_height: u64,
) -> Result<Option<u64>, RetentionError> {
    match status {
        OcompJobStatus::AwaitingFinality | OcompJobStatus::VotingOpen => Ok(None),
        OcompJobStatus::Completed | OcompJobStatus::Conflicted => {
            if terminal_height >= deadline_height {
                return Err(RetentionError::Source(
                    "OCOMP quorum terminal height is outside its response window".to_owned(),
                ));
            }
            Ok((observed_height >= deadline_height).then_some(deadline_height))
        }
        OcompJobStatus::Expired | OcompJobStatus::Canceled => Ok(Some(terminal_height)),
    }
}

fn encode_registry(registry: &JobRegistryV1) -> Vec<u8> {
    encode_registry_with(registry, encode_record)
}

fn encode_registry_with(
    registry: &JobRegistryV1,
    encode: impl Fn(PinRecordV1) -> Vec<u8>,
) -> Vec<u8> {
    let mut encoded =
        Vec::with_capacity(8 + 2 + 8 + 32 + 2 + registry.records.len() * PIN_RECORD_MAX_BYTES + 32);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    encoded.extend_from_slice(&registry.generation.to_be_bytes());
    encoded.extend_from_slice(registry.last_updated.as_slice());
    encoded.extend_from_slice(
        &u16::try_from(registry.records.len())
            .expect("journal registry length fits its u16 wire count")
            .to_be_bytes(),
    );
    for (key, record) in &registry.records {
        let record = encode(*record);
        encoded.extend_from_slice(key.as_slice());
        encoded.extend_from_slice(
            &u16::try_from(record.len())
                .expect("bounded pin record length fits u16")
                .to_be_bytes(),
        );
        encoded.extend_from_slice(&record);
    }
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn decode_registry(encoded: &[u8]) -> Result<JobRegistryV1, RetentionError> {
    if encoded.len() < JOURNAL_MAGIC.len() + 2 + 32 {
        return Err(RetentionError::MalformedJournal(
            "truncated registry header",
        ));
    }
    let version = u16::from_be_bytes(
        encoded
            .get(8..10)
            .ok_or(RetentionError::MalformedJournal(
                "truncated registry version",
            ))?
            .try_into()
            .map_err(|_| RetentionError::MalformedJournal("registry version length"))?,
    );
    if version != JOURNAL_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual: version });
    }
    let (body, checksum) = encoded.split_at(encoded.len() - 32);
    if keccak256(body).as_slice() != checksum {
        return Err(RetentionError::MalformedJournal("checksum mismatch"));
    }
    let mut reader = JournalReader::new(body);
    if reader.take::<8>()? != JOURNAL_MAGIC {
        return Err(RetentionError::MalformedJournal("wrong magic"));
    }
    let actual = u16::from_be_bytes(reader.take::<2>()?);
    if actual != JOURNAL_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual });
    }
    let generation = u64::from_be_bytes(reader.take::<8>()?);
    if generation == 0 {
        return Err(RetentionError::MalformedJournal("zero registry generation"));
    }
    let last_updated = B256::new(reader.take::<32>()?);
    let count = usize::from(u16::from_be_bytes(reader.take::<2>()?));
    if count == 0 {
        return Err(RetentionError::MalformedJournal(
            "registry must use an absent file for zero records",
        ));
    }
    let mut records = BTreeMap::new();
    for _ in 0..count {
        let key = B256::new(reader.take::<32>()?);
        let length = usize::from(u16::from_be_bytes(reader.take::<2>()?));
        if length == 0 || length > PIN_RECORD_MAX_BYTES {
            return Err(RetentionError::MalformedJournal(
                "pin record length is outside its bound",
            ));
        }
        let end = reader
            .offset
            .checked_add(length)
            .ok_or(RetentionError::MalformedJournal(
                "pin record offset overflow",
            ))?;
        let bytes = reader
            .encoded
            .get(reader.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated pin record"))?;
        reader.offset = end;
        let record = decode_record(bytes)?;
        if record_candidate(record).block_hash != key || records.insert(key, record).is_some() {
            return Err(RetentionError::MalformedJournal(
                "duplicate or mismatched registry key",
            ));
        }
    }
    reader.finish()?;
    if !records.contains_key(&last_updated)
        || records.values().map(|record| record.generation).max() != Some(generation)
    {
        return Err(RetentionError::MalformedJournal(
            "registry generation or last-updated key is inconsistent",
        ));
    }
    Ok(JobRegistryV1 {
        generation,
        last_updated,
        records,
    })
}

/// Decode one exact production journal through the same bounded codec used at
/// node startup. `root` is the node's `ocomp_retention` directory.
pub fn inspect_retention_journal(
    root: impl AsRef<Path>,
) -> Result<RetentionJournalSnapshotV1, RetentionError> {
    let path = root.as_ref().join(JOURNAL_FILENAME);
    let metadata = fs::symlink_metadata(&path).map_err(|source| RetentionError::Io {
        operation: "stat",
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(RetentionError::AmbiguousJournal(
            "journal is not a regular file",
        ));
    }
    if metadata.len() > JOURNAL_MAX_BYTES as u64 {
        return Err(RetentionError::MalformedJournal("journal exceeds byte cap"));
    }
    let mut file = File::open(&path).map_err(|source| RetentionError::Io {
        operation: "open",
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|source| RetentionError::Io {
            operation: "read",
            path: path.clone(),
            source,
        })?;
    let registry = decode_registry(&bytes)?;
    Ok(RetentionJournalSnapshotV1 {
        generation: registry.generation,
        last_updated: registry.last_updated,
        records: registry.records.into_iter().collect(),
    })
}

#[cfg(test)]
pub(crate) const fn retention_pressure_watermark_for_test() -> usize {
    JOURNAL_RECORD_PRESSURE_WATERMARK
}

#[cfg(test)]
pub(crate) fn seed_retention_journal_for_test(
    root: impl AsRef<Path>,
    generation: u64,
    last_updated: B256,
    records: Vec<(B256, PinRecordV1)>,
) -> Result<(), RetentionError> {
    let record_count = records.len();
    let records = records.into_iter().collect::<BTreeMap<_, _>>();
    if records.is_empty()
        || records.len() != record_count
        || records.len() > JOURNAL_RECORD_COUNT_MAX
        || !records.contains_key(&last_updated)
        || records.values().map(|record| record.generation).max() != Some(generation)
    {
        return Err(RetentionError::MalformedJournal(
            "invalid canonical test seed registry",
        ));
    }
    let changed = *records
        .get(&last_updated)
        .expect("validated last-updated test record");
    let registry = JobRegistryV1 {
        generation,
        last_updated,
        records,
    };
    let store = JournalStore::new(root.as_ref().to_path_buf(), Arc::new(OsJournalDurability));
    if store.initialize()?.is_some() {
        return Err(RetentionError::InvalidTransition(
            "test seed journal already exists",
        ));
    }
    store.persist(&registry, changed)?;
    Ok(())
}

fn encode_record(record: PinRecordV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(PIN_RECORD_MAX_BYTES);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&PIN_RECORD_VERSION.to_be_bytes());
    encoded.extend_from_slice(&record.generation.to_be_bytes());
    match record.state {
        PinStateV1::Tentative { candidate } => {
            encoded.push(1);
            encode_candidate(&mut encoded, candidate);
        }
        PinStateV1::Finalized {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        } => {
            encoded.push(2);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
        }
        PinStateV1::Exported {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export,
        } => {
            encoded.push(3);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
            encode_export_authority(&mut encoded, export);
        }
        PinStateV1::Terminal {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export,
            terminal_height,
            release_height,
        } => {
            encoded.push(4);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encode_finalized_window(
                &mut encoded,
                finality_recorded_height,
                open_height,
                deadline_height,
            );
            encode_export_authority(&mut encoded, export);
            encoded.extend_from_slice(&terminal_height.to_be_bytes());
            encoded.extend_from_slice(&release_height.to_be_bytes());
        }
        PinStateV1::Released {
            candidate,
            job_id,
            reason,
            observed_height,
            export,
        } => {
            encoded.push(5);
            encode_candidate(&mut encoded, candidate);
            match job_id {
                Some(job_id) => {
                    encoded.push(1);
                    encoded.extend_from_slice(job_id.as_slice());
                }
                None => encoded.push(0),
            }
            match export {
                Some(export) => {
                    encoded.push(1);
                    encode_export_authority(&mut encoded, export);
                }
                None => encoded.push(0),
            }
            encoded.push(match reason {
                PinReleaseReason::Orphaned => 1,
                PinReleaseReason::RetentionSatisfied => 2,
            });
            encoded.extend_from_slice(&observed_height.to_be_bytes());
        }
    }
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn encode_candidate(encoded: &mut Vec<u8>, candidate: CandidatePinV1) {
    encoded.extend_from_slice(&candidate.block_number.to_be_bytes());
    encoded.extend_from_slice(candidate.block_hash.as_slice());
    encoded.extend_from_slice(candidate.state_root.as_slice());
    encoded.extend_from_slice(candidate.intent_id.as_slice());
    encoded.extend_from_slice(&candidate.wwd.to_be_bytes());
    encoded.extend_from_slice(candidate.ce_sealed_root.as_slice());
    encoded.extend_from_slice(candidate.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(candidate.input_lease_id.as_slice());
}

fn encode_finalized_window(
    encoded: &mut Vec<u8>,
    finality_recorded_height: u64,
    open_height: u64,
    deadline_height: u64,
) {
    encoded.extend_from_slice(&finality_recorded_height.to_be_bytes());
    encoded.extend_from_slice(&open_height.to_be_bytes());
    encoded.extend_from_slice(&deadline_height.to_be_bytes());
}

fn encode_export_authority(encoded: &mut Vec<u8>, export: ExportAuthorityV1) {
    encoded.extend_from_slice(&export.source_generation.to_be_bytes());
    encoded.extend_from_slice(&export.lease_generation.to_be_bytes());
    encoded.extend_from_slice(export.manifest_hash.as_slice());
}

fn decode_export_authority(
    reader: &mut JournalReader<'_>,
) -> Result<ExportAuthorityV1, RetentionError> {
    let export = ExportAuthorityV1 {
        source_generation: u64::from_be_bytes(reader.take::<8>()?),
        lease_generation: u64::from_be_bytes(reader.take::<8>()?),
        manifest_hash: B256::new(reader.take::<32>()?),
    };
    if export.source_generation == 0
        || export.lease_generation == 0
        || export.manifest_hash.is_zero()
    {
        return Err(RetentionError::MalformedJournal(
            "incomplete export authority",
        ));
    }
    Ok(export)
}

fn decode_record(encoded: &[u8]) -> Result<PinRecordV1, RetentionError> {
    if encoded.len() < JOURNAL_MAGIC.len() + 2 + 8 + 1 + 32 {
        return Err(RetentionError::MalformedJournal("truncated header"));
    }
    let (body, checksum) = encoded.split_at(encoded.len() - 32);
    if keccak256(body).as_slice() != checksum {
        return Err(RetentionError::MalformedJournal("checksum mismatch"));
    }
    let mut reader = JournalReader::new(body);
    if reader.take::<8>()? != JOURNAL_MAGIC {
        return Err(RetentionError::MalformedJournal("wrong magic"));
    }
    let version = u16::from_be_bytes(reader.take::<2>()?);
    if version != PIN_RECORD_VERSION {
        return Err(RetentionError::UnsupportedJournalVersion { actual: version });
    }
    let generation = u64::from_be_bytes(reader.take::<8>()?);
    if generation == 0 {
        return Err(RetentionError::MalformedJournal("zero generation"));
    }
    let tag = reader.take::<1>()?[0];
    let candidate = decode_candidate(&mut reader)?;
    let state = match tag {
        1 => PinStateV1::Tentative { candidate },
        2 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
            }
        }
        3 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            let export = decode_export_authority(&mut reader)?;
            PinStateV1::Exported {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
            }
        }
        4 => {
            let job_id = B256::new(reader.take::<32>()?);
            let (finality_recorded_height, open_height, deadline_height) =
                decode_finalized_window(&mut reader)?;
            let export = decode_export_authority(&mut reader)?;
            let terminal_height = u64::from_be_bytes(reader.take::<8>()?);
            let release_height = u64::from_be_bytes(reader.take::<8>()?);
            if terminal_height.checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS) != Some(release_height)
            {
                return Err(RetentionError::MalformedJournal(
                    "release height is not terminal finality plus evidence window",
                ));
            }
            PinStateV1::Terminal {
                candidate,
                job_id,
                finality_recorded_height,
                open_height,
                deadline_height,
                export,
                terminal_height,
                release_height,
            }
        }
        5 => {
            let job_id = match reader.take::<1>()?[0] {
                0 => None,
                1 => Some(B256::new(reader.take::<32>()?)),
                _ => return Err(RetentionError::MalformedJournal("invalid job-id flag")),
            };
            let export = match reader.take::<1>()?[0] {
                0 => None,
                1 => Some(decode_export_authority(&mut reader)?),
                _ => {
                    return Err(RetentionError::MalformedJournal(
                        "invalid export-authority flag",
                    ));
                }
            };
            let reason = match reader.take::<1>()?[0] {
                1 => PinReleaseReason::Orphaned,
                2 => PinReleaseReason::RetentionSatisfied,
                _ => return Err(RetentionError::MalformedJournal("invalid release reason")),
            };
            PinStateV1::Released {
                candidate,
                job_id,
                reason,
                observed_height: u64::from_be_bytes(reader.take::<8>()?),
                export,
            }
        }
        _ => return Err(RetentionError::MalformedJournal("unknown state tag")),
    };
    reader.finish()?;
    Ok(PinRecordV1 { generation, state })
}

fn decode_candidate(reader: &mut JournalReader<'_>) -> Result<CandidatePinV1, RetentionError> {
    Ok(CandidatePinV1 {
        block_number: u64::from_be_bytes(reader.take::<8>()?),
        block_hash: B256::new(reader.take::<32>()?),
        state_root: B256::new(reader.take::<32>()?),
        intent_id: B256::new(reader.take::<32>()?),
        wwd: u32::from_be_bytes(reader.take::<4>()?),
        ce_sealed_root: B256::new(reader.take::<32>()?),
        protocol_bundle_hash: B256::new(reader.take::<32>()?),
        input_lease_id: B256::new(reader.take::<32>()?),
    })
}

fn decode_finalized_window(
    reader: &mut JournalReader<'_>,
) -> Result<(u64, u64, u64), RetentionError> {
    let finality_recorded_height = u64::from_be_bytes(reader.take::<8>()?);
    let open_height = u64::from_be_bytes(reader.take::<8>()?);
    let deadline_height = u64::from_be_bytes(reader.take::<8>()?);
    if finality_recorded_height
        .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
        != Some(open_height)
        || open_height >= deadline_height
    {
        return Err(RetentionError::MalformedJournal(
            "invalid finalized response window",
        ));
    }
    Ok((finality_recorded_height, open_height, deadline_height))
}

struct JournalReader<'a> {
    encoded: &'a [u8],
    offset: usize,
}

impl<'a> JournalReader<'a> {
    const fn new(encoded: &'a [u8]) -> Self {
        Self { encoded, offset: 0 }
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], RetentionError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(RetentionError::MalformedJournal("offset overflow"))?;
        let value = self
            .encoded
            .get(self.offset..end)
            .ok_or(RetentionError::MalformedJournal("truncated field"))?;
        self.offset = end;
        value
            .try_into()
            .map_err(|_| RetentionError::MalformedJournal("field length"))
    }

    fn finish(self) -> Result<(), RetentionError> {
        if self.offset != self.encoded.len() {
            return Err(RetentionError::MalformedJournal("trailing bytes"));
        }
        Ok(())
    }
}
