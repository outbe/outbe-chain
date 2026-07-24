//! Crash-conservative one-entry OCOMP retention journal.
//!
//! Candidate discovery uses an event only as a bounded locator. The production
//! source re-opens the exact execution-valid block state and authenticates the
//! typed Metadosis record before this coordinator persists anything.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use alloy_consensus::{BlockHeader as _, TxReceipt as _};
use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolEvent as _;
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::FinalizedParentCertStore,
    ocomp_retention::{OcompRetentionHook, OcompRetentionHookError},
};
use outbe_metadosis::{
    ocomp::schema::poc_schema_limits, precompile::IMetadosis, schema::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, JobIntentV1},
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS, storage::types::StorageKey as _, OutbeHeader, OutbeReceipt,
};
use reth_provider::{HeaderProvider, ReceiptProvider, StateProviderFactory};

use super::finality::RethFinalizedIntentProofBuilder;

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBPIN1";
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_MAX_BYTES: usize = 512;
const JOURNAL_FILENAME: &str = "pin.v1";
const JOURNAL_TEMP_FILENAME: &str = "pin.v1.tmp";

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
    pub deadline_height: u64,
}

/// Exact finalized job derived from the candidate's typed state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedJobPinV1 {
    pub candidate: CandidatePinV1,
    pub job_id: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
pub enum PinStateV1 {
    Tentative {
        candidate: CandidatePinV1,
    },
    Finalized {
        candidate: CandidatePinV1,
        job_id: B256,
    },
    Exported {
        candidate: CandidatePinV1,
        job_id: B256,
        lease_generation: u64,
        manifest_hash: B256,
    },
    Terminal {
        candidate: CandidatePinV1,
        job_id: B256,
        terminal_height: u64,
        release_height: u64,
    },
    Released {
        candidate: CandidatePinV1,
        job_id: Option<B256>,
        reason: PinReleaseReason,
        observed_height: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinRecordV1 {
    pub generation: u64,
    pub state: PinStateV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
}

/// Reth-backed typed state source. Request events are locators, never authority.
pub struct RethFinalizedInputProofSource<P> {
    provider: P,
    parent_proofs: FinalizedParentCertStore,
    proof_builder: RethFinalizedIntentProofBuilder<P>,
    pending_receipts: Arc<PendingCandidateReceiptReader>,
    limits: SchemaLimits,
}

impl<P: Clone> RethFinalizedInputProofSource<P> {
    pub fn new(
        provider: P,
        parent_proofs: FinalizedParentCertStore,
        pending_receipts: impl Fn() -> Result<Option<(B256, Vec<OutbeReceipt>)>, String>
            + Send
            + Sync
            + 'static,
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
            limits: poc_schema_limits(),
        }
    }
}

impl<P> RethFinalizedInputProofSource<P>
where
    P: ReceiptProvider + StateProviderFactory + Send + Sync,
{
    fn pending_record(
        &self,
        block: &ConsensusBlock,
        intent_id: B256,
    ) -> Result<OcompJobRecordV1, RetentionError> {
        let state = self
            .provider
            .state_by_block_hash(block.block_hash())
            .map_err(|error| RetentionError::Source(format!("open exact block state: {error}")))?;
        let logical_key = intent_storage_key(intent_id)
            .map_err(|error| RetentionError::Source(format!("derive intent slot: {error}")))?;
        let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
        let encoded = read_storage_bytes(state.as_ref(), base, self.limits.max_bounded_bytes)?;
        let record = OcompJobRecordV1::decode_canonical(&encoded, &self.limits)
            .map_err(|error| RetentionError::Source(format!("decode typed job record: {error}")))?;
        let decoded_id = record
            .intent
            .intent_id(&self.limits)
            .map_err(|error| RetentionError::Source(format!("hash typed job record: {error}")))?;
        if decoded_id != intent_id || record.status != OcompJobStatus::OffchainPending {
            return Err(RetentionError::Source(
                "event locator does not open the exact pending intent".to_owned(),
            ));
        }
        Ok(record)
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
        let mut request = None;
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
                let decoded =
                    IMetadosis::OffchainJobRequested::decode_log(log).map_err(|error| {
                        RetentionError::Source(format!("decode candidate request locator: {error}"))
                    })?;
                if request.replace(decoded).is_some() {
                    return Err(RetentionError::Source(
                        "candidate contains more than one OCOMP request".to_owned(),
                    ));
                }
            }
        }
        let Some(request) = request else {
            return Ok(None);
        };

        let record = self.pending_record(block, request.data.intentId)?;
        validate_request_locator(&record.intent, &request.data, &self.limits)?;
        Ok(Some(CandidatePinV1 {
            block_number: block.number(),
            block_hash: block.block_hash(),
            state_root: block.header().state_root(),
            intent_id: request.data.intentId,
            deadline_height: record.intent.deadline_height,
        }))
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
        if verified.intent.deadline_height != candidate.deadline_height {
            return Err(RetentionError::Source(
                "finalized intent deadline differs from tentative pin".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id: verified.job_id,
        }))
    }
}

fn validate_request_locator(
    intent: &JobIntentV1,
    event: &IMetadosis::OffchainJobRequested,
    limits: &SchemaLimits,
) -> Result<(), RetentionError> {
    let activation_hash = intent
        .activation_preconditions
        .activation_preconditions_hash(limits)
        .map_err(|error| {
            RetentionError::Source(format!("hash activation preconditions: {error}"))
        })?;
    if intent.wwd != event.wwd
        || intent.pending_nonce != event.pendingNonce
        || intent.attempt != event.attempt
        || intent.deadline_height != event.deadlineHeight
        || activation_hash != event.activationPreconditionsHash
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

    fn initialize(&self) -> Result<Option<PinRecordV1>, RetentionError> {
        fs::create_dir_all(&self.root)
            .map_err(|source| self.io("create directory", &self.root, source))?;
        if self.temporary.exists() {
            return Err(RetentionError::AmbiguousJournal(
                "temporary write exists after restart",
            ));
        }
        if !self.journal.exists() {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(&self.journal)
            .map_err(|source| self.io("stat", &self.journal, source))?;
        if !metadata.file_type().is_file() {
            return Err(RetentionError::AmbiguousJournal(
                "journal is not a regular file",
            ));
        }
        if metadata.len() > JOURNAL_MAX_BYTES as u64 {
            return Err(RetentionError::MalformedJournal("journal exceeds byte cap"));
        }
        let mut file =
            File::open(&self.journal).map_err(|source| self.io("open", &self.journal, source))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|source| self.io("read", &self.journal, source))?;
        decode_record(&bytes).map(Some)
    }

    fn persist(&self, record: PinRecordV1) -> Result<DurablePinAck, RetentionError> {
        let encoded = encode_record(record);
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
        Ok(DurablePinAck {
            generation: record.generation,
            record_hash: keccak256(encoded),
        })
    }

    fn io(&self, operation: &'static str, path: &Path, source: std::io::Error) -> RetentionError {
        RetentionError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

struct CoordinatorInner {
    status: RetentionStatus,
}

/// Node-owned one-entry PoC pin coordinator.
pub struct OcompRetentionCoordinator {
    store: JournalStore,
    inner: Mutex<CoordinatorInner>,
    source: Arc<dyn FinalizedInputProofSource>,
}

const FINALITY_NOTIFICATION_CAPACITY: usize = 256;

/// Bounded, ordered node-owned worker for finalized-block reconciliation.
///
/// Before a tentative pin exists, every finalized block is an input-discovery
/// boundary and must remain observable: a later block cannot stand in for the
/// request receipts of an earlier block. The bounded FIFO therefore preserves
/// exact notification order and rejects overflow instead of silently
/// coalescing away a possible request.
pub struct OcompRetentionService {
    coordinator: Arc<OcompRetentionCoordinator>,
    finalized_rx: tokio::sync::mpsc::Receiver<ConsensusBlock>,
}

/// Consensus-facing handle. Candidate preparation is intentionally synchronous
/// because a positive vote depends on its durable ack; finality notification is
/// a constant-time local enqueue.
#[derive(Clone)]
pub struct OcompRetentionHandle {
    coordinator: Arc<OcompRetentionCoordinator>,
    finalized_tx: tokio::sync::mpsc::Sender<ConsensusBlock>,
}

impl OcompRetentionService {
    pub fn new(coordinator: Arc<OcompRetentionCoordinator>) -> (Self, OcompRetentionHandle) {
        let (finalized_tx, finalized_rx) =
            tokio::sync::mpsc::channel(FINALITY_NOTIFICATION_CAPACITY);
        (
            Self {
                coordinator: coordinator.clone(),
                finalized_rx,
            },
            OcompRetentionHandle {
                coordinator,
                finalized_tx,
            },
        )
    }

    pub async fn run(mut self) {
        while let Some(block) = self.finalized_rx.recv().await {
            let block_number = block.number();
            let block_hash = block.block_hash();
            let coordinator = self.coordinator.clone();
            match tokio::task::spawn_blocking(move || coordinator.reconcile_finalized(&block)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(
                        target: "outbe::ocomp",
                        block_number,
                        block_hash = %block_hash,
                        %error,
                        "OCOMP pin reconciliation failed in node-owned worker"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        target: "outbe::ocomp",
                        block_number,
                        block_hash = %block_hash,
                        %error,
                        "OCOMP pin reconciliation worker task failed"
                    );
                }
            }
        }
    }
}

impl OcompRetentionCoordinator {
    /// Open a managed journal root. Corrupt, ambiguous or unavailable storage
    /// quarantines OCOMP but does not fail node/consensus startup.
    pub fn open(root: impl Into<PathBuf>, source: Arc<dyn FinalizedInputProofSource>) -> Self {
        Self::open_with_durability(root, source, Arc::new(OsJournalDurability))
    }

    pub(crate) fn open_with_durability(
        root: impl Into<PathBuf>,
        source: Arc<dyn FinalizedInputProofSource>,
        durability: Arc<dyn JournalDurability>,
    ) -> Self {
        let store = JournalStore::new(root.into(), durability);
        let status = match store.initialize() {
            Ok(Some(record)) => RetentionStatus::Ready(record),
            Ok(None) => RetentionStatus::Empty,
            Err(error) => RetentionStatus::Quarantined {
                reason: error.to_string(),
            },
        };
        Self {
            store,
            inner: Mutex::new(CoordinatorInner { status }),
            source,
        }
    }

    pub fn status(&self) -> RetentionStatus {
        self.lock()
            .map(|inner| inner.status.clone())
            .unwrap_or_else(|error| RetentionStatus::Quarantined {
                reason: error.to_string(),
            })
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
        let status = self.status();
        match status {
            RetentionStatus::Quarantined { reason } => Err(OcompRetentionHookError::new(reason)),
            RetentionStatus::Empty => {
                let Some(candidate) = self.source.candidate_for_block(block).map_err(hook_error)?
                else {
                    return Ok(());
                };
                self.record_tentative(candidate).map_err(hook_error)?;
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
                Ok(())
            }
            RetentionStatus::Ready(record) => match record.state {
                PinStateV1::Tentative { candidate } => {
                    if block.number() < candidate.block_number {
                        return Ok(());
                    }
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
                    Ok(())
                }
                PinStateV1::Finalized { candidate, .. }
                | PinStateV1::Exported { candidate, .. }
                | PinStateV1::Terminal { candidate, .. } => {
                    if block.number() == candidate.block_number
                        && block.block_hash() != candidate.block_hash
                    {
                        return Err(OcompRetentionHookError::new(
                            "finalized chain conflicts with the finalized OCOMP pin",
                        ));
                    }
                    Ok(())
                }
                PinStateV1::Released { .. } => Ok(()),
            },
        }
    }

    pub fn record_tentative(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let generation = match &inner.status {
            RetentionStatus::Empty => 1,
            RetentionStatus::Ready(record) => match record.state {
                PinStateV1::Tentative {
                    candidate: existing,
                } if existing == candidate => {
                    return Ok(ack_for(*record));
                }
                PinStateV1::Released {
                    candidate: existing,
                    reason: PinReleaseReason::Orphaned,
                    ..
                } if existing == candidate => return Err(RetentionError::OrphanedCandidate),
                PinStateV1::Released { .. } => record
                    .generation
                    .checked_add(1)
                    .ok_or(RetentionError::GenerationOverflow)?,
                _ => return Err(RetentionError::ConflictingCandidate),
            },
            RetentionStatus::Quarantined { reason } => {
                return Err(RetentionError::Quarantined(reason.clone()));
            }
        };
        self.persist_locked(
            &mut inner,
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
        let record = ready_record(&inner.status)?;
        ensure_generation(record, expected_generation)?;
        let candidate = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
            } if existing == job_id => candidate,
            PinStateV1::Exported {
                job_id: existing,
                lease_generation: existing_lease,
                manifest_hash: existing_manifest,
                ..
            } if existing == job_id
                && existing_lease == lease_generation
                && existing_manifest == manifest_hash =>
            {
                return Ok(ack_for(record));
            }
            _ => {
                return Err(RetentionError::InvalidTransition(
                    "export requires the exact finalized job",
                ));
            }
        };
        self.persist_next(
            &mut inner,
            record,
            PinStateV1::Exported {
                candidate,
                job_id,
                lease_generation,
                manifest_hash,
            },
        )
    }

    pub fn observe_terminal(
        &self,
        job_id: B256,
        expected_generation: u64,
        terminal_height: u64,
        release_height: u64,
    ) -> Result<DurablePinAck, RetentionError> {
        if release_height < terminal_height {
            return Err(RetentionError::InvalidTransition(
                "release height precedes terminal finality",
            ));
        }
        let mut inner = self.lock()?;
        let record = ready_record(&inner.status)?;
        ensure_generation(record, expected_generation)?;
        let candidate = match record.state {
            PinStateV1::Finalized {
                candidate,
                job_id: existing,
            }
            | PinStateV1::Exported {
                candidate,
                job_id: existing,
                ..
            } if existing == job_id => candidate,
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
            record,
            PinStateV1::Terminal {
                candidate,
                job_id,
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
        let record = match &inner.status {
            RetentionStatus::Empty => return Ok(None),
            RetentionStatus::Ready(record) => *record,
            RetentionStatus::Quarantined { reason } => {
                return Err(RetentionError::Quarantined(reason.clone()));
            }
        };
        let (candidate, job_id, release_height) = match record.state {
            PinStateV1::Terminal {
                candidate,
                job_id,
                release_height,
                ..
            } => (candidate, job_id, release_height),
            PinStateV1::Released { .. } => return Ok(Some(ack_for(record))),
            _ => return Ok(None),
        };
        if finalized_height < release_height {
            return Ok(None);
        }
        self.persist_next(
            &mut inner,
            record,
            PinStateV1::Released {
                candidate,
                job_id: Some(job_id),
                reason: PinReleaseReason::RetentionSatisfied,
                observed_height: finalized_height,
            },
        )
        .map(Some)
    }

    pub fn is_signable(&self, job_id: B256) -> bool {
        let Ok(inner) = self.lock() else {
            return false;
        };
        matches!(
            inner.status,
            RetentionStatus::Ready(PinRecordV1 {
                state:
                    PinStateV1::Finalized {
                        job_id: current, ..
                    } | PinStateV1::Exported {
                        job_id: current, ..
                    },
                ..
            }) if current == job_id
        )
    }

    pub fn is_exportable(&self, job_id: B256) -> bool {
        self.is_signable(job_id)
    }

    fn finalize_exact(
        &self,
        finalized: FinalizedJobPinV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let mut inner = self.lock()?;
        let record = ready_record(&inner.status)?;
        match record.state {
            PinStateV1::Tentative { candidate } if candidate == finalized.candidate => self
                .persist_next(
                    &mut inner,
                    record,
                    PinStateV1::Finalized {
                        candidate,
                        job_id: finalized.job_id,
                    },
                ),
            PinStateV1::Finalized { candidate, job_id }
                if candidate == finalized.candidate && job_id == finalized.job_id =>
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
        let record = ready_record(&inner.status)?;
        match record.state {
            PinStateV1::Tentative { candidate: current } if current == candidate => self
                .persist_next(
                    &mut inner,
                    record,
                    PinStateV1::Released {
                        candidate,
                        job_id: None,
                        reason: PinReleaseReason::Orphaned,
                        observed_height,
                    },
                ),
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

    fn persist_next(
        &self,
        inner: &mut CoordinatorInner,
        current: PinRecordV1,
        state: PinStateV1,
    ) -> Result<DurablePinAck, RetentionError> {
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(RetentionError::GenerationOverflow)?;
        self.persist_locked(inner, PinRecordV1 { generation, state })
    }

    fn persist_locked(
        &self,
        inner: &mut CoordinatorInner,
        record: PinRecordV1,
    ) -> Result<DurablePinAck, RetentionError> {
        match self.store.persist(record) {
            Ok(ack) => {
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

impl OcompRetentionHook for OcompRetentionHandle {
    fn prepare_candidate(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        self.coordinator.prepare_candidate(block)
    }

    fn reconcile_finalized(&self, block: &ConsensusBlock) -> Result<(), OcompRetentionHookError> {
        self.finalized_tx
            .try_send(block.clone())
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    OcompRetentionHookError::new("OCOMP retention finality queue is full")
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    OcompRetentionHookError::new("OCOMP retention worker is unavailable")
                }
            })
    }
}

fn hook_error(error: RetentionError) -> OcompRetentionHookError {
    OcompRetentionHookError::new(error.to_string())
}

fn ready_record(status: &RetentionStatus) -> Result<PinRecordV1, RetentionError> {
    match status {
        RetentionStatus::Ready(record) => Ok(*record),
        RetentionStatus::Empty => Err(RetentionError::InvalidTransition("pin journal is empty")),
        RetentionStatus::Quarantined { reason } => Err(RetentionError::Quarantined(reason.clone())),
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

fn encode_record(record: PinRecordV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(JOURNAL_MAX_BYTES);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    encoded.extend_from_slice(&JOURNAL_VERSION.to_be_bytes());
    encoded.extend_from_slice(&record.generation.to_be_bytes());
    match record.state {
        PinStateV1::Tentative { candidate } => {
            encoded.push(1);
            encode_candidate(&mut encoded, candidate);
        }
        PinStateV1::Finalized { candidate, job_id } => {
            encoded.push(2);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
        }
        PinStateV1::Exported {
            candidate,
            job_id,
            lease_generation,
            manifest_hash,
        } => {
            encoded.push(3);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encoded.extend_from_slice(&lease_generation.to_be_bytes());
            encoded.extend_from_slice(manifest_hash.as_slice());
        }
        PinStateV1::Terminal {
            candidate,
            job_id,
            terminal_height,
            release_height,
        } => {
            encoded.push(4);
            encode_candidate(&mut encoded, candidate);
            encoded.extend_from_slice(job_id.as_slice());
            encoded.extend_from_slice(&terminal_height.to_be_bytes());
            encoded.extend_from_slice(&release_height.to_be_bytes());
        }
        PinStateV1::Released {
            candidate,
            job_id,
            reason,
            observed_height,
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
    encoded.extend_from_slice(&candidate.deadline_height.to_be_bytes());
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
    if version != JOURNAL_VERSION {
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
        2 => PinStateV1::Finalized {
            candidate,
            job_id: B256::new(reader.take::<32>()?),
        },
        3 => PinStateV1::Exported {
            candidate,
            job_id: B256::new(reader.take::<32>()?),
            lease_generation: u64::from_be_bytes(reader.take::<8>()?),
            manifest_hash: B256::new(reader.take::<32>()?),
        },
        4 => {
            let job_id = B256::new(reader.take::<32>()?);
            let terminal_height = u64::from_be_bytes(reader.take::<8>()?);
            let release_height = u64::from_be_bytes(reader.take::<8>()?);
            if release_height < terminal_height {
                return Err(RetentionError::MalformedJournal(
                    "release precedes terminal height",
                ));
            }
            PinStateV1::Terminal {
                candidate,
                job_id,
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
        deadline_height: u64::from_be_bytes(reader.take::<8>()?),
    })
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
