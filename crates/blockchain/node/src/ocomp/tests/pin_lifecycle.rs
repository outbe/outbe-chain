//! OCM-PIN-001: production retention notifications and journal on a real filesystem.
// OCOMP-TEST-ID: OCM-PIN-001

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{b256, keccak256, Address, Bytes, Log, B256, U256};
use alloy_sol_types::SolEvent as _;
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_compressed_entities::WwdEntityId;
use outbe_consensus::{
    block::ConsensusBlock, finalization::parent_cert_store::FinalizedParentCertStore,
    ocomp_retention::OcompRetentionHook,
};
use outbe_metadosis::{
    config::poc_schema_limits, precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, ProofBytes},
    intent::{
        intent_storage_key, job_id_from_intent_id, ActivationPreconditionsV1,
        AuctionEntryPriceSource, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, FinalizedIntentProofV1, FrozenMetadosisValuesV1,
        JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus,
        NodTargetPreconditionV1, ParentProofKind, ReferenceEntryPriceV1, TributeInputBindingV1,
    },
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_offchain_data::TributeRetentionSelector;
use outbe_offchain_storage::{MemoryStorage, StorageWriter as _};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, VALIDATOR_SET_ADDRESS},
    storage::{hashmap::HashMapStorageProvider, types::StorageKey as _, StorageHandle},
    OutbeExecutionData, OutbeHeader, OutbePayloadTypes, OutbePrimitives,
};
use outbe_tribute::{
    RetainedTributePin, RetainedTributeReader, RetainedTributeWriter, TributeData,
    TributeRepositoryWriter,
};
use reth_chainspec::{ChainSpec, ChainSpecBuilder};
use reth_ethereum::{primitives::SealedBlock, Block, Receipt, TxType};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};

use outbe_validatorset::{
    contract::ValidatorSet, read_ocomp_snapshot_extension, write_committee_snapshot,
    CommitteeEntry, CommitteeSnapshot,
};

use crate::finalized_frame::FinalizedFrame;
use crate::ocomp::retention::{
    inspect_retention_journal, observe_finalized_request, ocomp_snapshot_contains_key_at,
    retention_pressure_watermark_for_test, retention_terminal_height_for_status,
    seed_retention_journal_for_test, CandidateFinalityV1, CandidatePinV1, ExportAuthorityV1,
    FinalizedInputProofSource, FinalizedJobPinV1, FinalizedSnapshotArmer, JournalDurability,
    OcompRetentionCoordinator, OcompRetentionService, OcompSnapshotEligibilityV1, PinRecordV1,
    PinReleaseReason, PinStateV1, RetentionError, RetentionStatus, RethFinalizedInputProofSource,
    SharedOcompRetentionSelector,
};
#[derive(Clone, Default)]
struct DeterministicProofSource {
    jobs: Arc<Mutex<BTreeMap<B256, (CandidatePinV1, B256)>>>,
    finalized: Arc<Mutex<BTreeMap<u64, (B256, B256)>>>,
    terminal: Arc<Mutex<BTreeMap<B256, u64>>>,
    finality_available: Arc<AtomicBool>,
}

impl DeterministicProofSource {
    fn with_jobs(jobs: impl IntoIterator<Item = (CandidatePinV1, B256)>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(
                jobs.into_iter()
                    .map(|(candidate, job_id)| (candidate.block_hash, (candidate, job_id)))
                    .collect(),
            )),
            finalized: Arc::new(Mutex::new(BTreeMap::new())),
            terminal: Arc::new(Mutex::new(BTreeMap::new())),
            finality_available: Arc::new(AtomicBool::new(true)),
        }
    }

    fn set_finality_available(&self, available: bool) {
        self.finality_available.store(available, Ordering::SeqCst);
    }

    fn observe_finalized(&self, block: &ConsensusBlock) {
        self.finalized
            .lock()
            .expect("deterministic finality lock")
            .insert(block.number(), (block.block_hash(), block.parent_hash()));
    }

    fn observe_terminal(&self, job_id: B256, terminal_height: u64) {
        self.terminal
            .lock()
            .expect("deterministic terminal lock")
            .insert(job_id, terminal_height);
    }
}

impl FinalizedInputProofSource for DeterministicProofSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok(self
            .jobs
            .lock()
            .expect("deterministic source lock")
            .get(&block.block_hash())
            .map(|(candidate, _)| *candidate))
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if !self.finality_available.load(Ordering::SeqCst) {
            return Err(RetentionError::Source(
                "finalization proof is unavailable".to_owned(),
            ));
        }
        let finalized = self.finalized.lock().expect("deterministic finality lock");
        let (finality_recorded_height, finalized_hash) =
            if let Some((hash, _)) = finalized.get(&candidate.block_number) {
                (candidate.block_number, *hash)
            } else {
                let successor_height = candidate
                    .block_number
                    .checked_add(1)
                    .ok_or_else(|| RetentionError::Source("finality height overflow".to_owned()))?;
                let (_, parent_hash) = finalized.get(&successor_height).ok_or_else(|| {
                    RetentionError::Source("candidate-height finality is unavailable".to_owned())
                })?;
                (successor_height, *parent_hash)
            };
        if finalized_hash != candidate.block_hash {
            return Ok(CandidateFinalityV1::Orphaned);
        }
        let jobs = self.jobs.lock().expect("deterministic source lock");
        let (expected, job_id) = jobs
            .get(&candidate.block_hash)
            .copied()
            .ok_or_else(|| RetentionError::Source("unknown finalized fixture".to_owned()))?;
        if expected != candidate {
            return Err(RetentionError::Source(
                "finalized fixture differs from candidate".to_owned(),
            ));
        }
        Ok(CandidateFinalityV1::Finalized(FinalizedJobPinV1 {
            candidate,
            job_id,
            finality_recorded_height,
            open_height: finality_recorded_height + 4,
            deadline_height: finality_recorded_height + 14,
        }))
    }

    fn terminal_height_at(
        &self,
        _block: &ConsensusBlock,
        _candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        Ok(self
            .terminal
            .lock()
            .expect("deterministic terminal lock")
            .get(&job_id)
            .copied())
    }
}

#[derive(Clone)]
struct ResponseWindowCompletedSource {
    inner: DeterministicProofSource,
}

impl FinalizedInputProofSource for ResponseWindowCompletedSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        self.inner.candidate_for_block(block)
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        self.inner.resolve_finality(candidate)
    }

    fn terminal_height_at(
        &self,
        block: &ConsensusBlock,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        let Some(quorum_height) = self
            .inner
            .terminal
            .lock()
            .expect("deterministic terminal lock")
            .get(&job_id)
            .copied()
        else {
            return Ok(None);
        };
        let CandidateFinalityV1::Finalized(finalized) = self.inner.resolve_finality(candidate)?
        else {
            return Err(RetentionError::Source(
                "completed response-window fixture became orphaned".to_owned(),
            ));
        };
        retention_terminal_height_for_status(
            OcompJobStatus::Completed,
            block.number(),
            finalized.deadline_height,
            quorum_height,
        )
    }
}

#[derive(Clone)]
struct TransientFinalitySource {
    inner: DeterministicProofSource,
    resolve_calls: Arc<AtomicUsize>,
    failures_before_ready: usize,
}

impl FinalizedInputProofSource for TransientFinalitySource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        self.inner.candidate_for_block(block)
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        if self.resolve_calls.fetch_add(1, Ordering::SeqCst) < self.failures_before_ready {
            return Err(RetentionError::Source(
                "finalized candidate header is not persisted yet".to_owned(),
            ));
        }
        self.inner.resolve_finality(candidate)
    }
}

#[derive(Clone)]
struct ProofReturningSource {
    inner: DeterministicProofSource,
    proofs: BTreeMap<B256, FinalizedIntentProofV1>,
}

impl FinalizedInputProofSource for ProofReturningSource {
    fn candidate_for_block(
        &self,
        block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        self.inner.candidate_for_block(block)
    }

    fn resolve_finality(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        self.inner.resolve_finality(candidate)
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        self.proofs
            .get(&candidate.block_hash)
            .cloned()
            .ok_or_else(|| {
                RetentionError::Source("missing finalized-intent proof fixture".to_owned())
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VoteOutcome {
    Positive,
    Abstained,
}

struct DeterministicConsensusDriver;

impl DeterministicConsensusDriver {
    fn vote(coordinator: &OcompRetentionCoordinator, block: &ConsensusBlock) -> VoteOutcome {
        coordinator
            .prepare_candidate(block)
            .map(|()| VoteOutcome::Positive)
            .unwrap_or(VoteOutcome::Abstained)
    }

    fn finalize(
        source: &DeterministicProofSource,
        coordinator: &OcompRetentionCoordinator,
        block: &ConsensusBlock,
    ) {
        source.observe_finalized(block);
        coordinator
            .reconcile_finalized(block)
            .expect("finalization notification is node-local and must reconcile");
    }
}

#[derive(Default)]
struct RecordingSnapshotArmer {
    jobs: Mutex<Vec<B256>>,
}

impl FinalizedSnapshotArmer for RecordingSnapshotArmer {
    fn arm_finalized_snapshot(&self, job_id: B256) -> Result<(), String> {
        self.jobs
            .lock()
            .map_err(|_| "recording snapshot armer lock poisoned".to_owned())?
            .push(job_id);
        Ok(())
    }
}

fn block(number: u64, state_root: B256, marker: u8) -> ConsensusBlock {
    block_extending(number, state_root, B256::ZERO, marker)
}

fn block_extending(number: u64, state_root: B256, parent_hash: B256, marker: u8) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.state_root = state_root;
    block.header.parent_hash = parent_hash;
    block.header.extra_data = Bytes::from(vec![marker]);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block.map_header(OutbeHeader::new)))
}

fn candidate(block: &ConsensusBlock, intent_id: B256) -> CandidatePinV1 {
    CandidatePinV1 {
        block_number: block.number(),
        block_hash: block.block_hash(),
        state_root: block.header().inner.state_root,
        intent_id,
        wwd: 7,
        ce_sealed_root: B256::repeat_byte(4),
        protocol_bundle_hash: B256::repeat_byte(3),
        input_lease_id: B256::repeat_byte(0x71),
    }
}

type CandidateProvider = MockEthProvider<OutbePrimitives, ChainSpec<OutbeHeader>>;
type PendingReceiptFixture = Arc<Mutex<Option<(B256, Vec<Receipt>)>>>;

struct ProductionCandidateFixture {
    request: ConsensusBlock,
    candidate: CandidatePinV1,
    source: Arc<RethFinalizedInputProofSource<CandidateProvider>>,
    pending_receipts: PendingReceiptFixture,
}

fn production_intent(block_number: u64) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 42,
        genesis_hash: B256::repeat_byte(1),
        fork_id: B256::repeat_byte(2),
        wwd: 7,
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: B256::repeat_byte(3),
        ce_sealed_root: B256::repeat_byte(4),
        sealed_tribute_collection_key: B256::repeat_byte(5),
        sealed_tribute_collection_root: B256::repeat_byte(6),
        authenticated_day_count: 0,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: B256::repeat_byte(7),
        source_availability_policy_id: B256::repeat_byte(8),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(1_000),
            previous_vwap: U256::from(90),
            current_vwap: U256::from(100),
            gratis_demand: U256::from(25),
            gratis_supply: U256::from(20),
            lysis_budget: U256::from(300),
            auction_base: U256::from(700),
            auction_entry_prices: vec![ReferenceEntryPriceV1 {
                reference_currency: outbe_oracle::constants::DAY_TYPE_ISO,
                entry_price_minor: U256::from(95),
                source: AuctionEntryPriceSource::LastClosedDayVwap,
                source_day: 6,
            }],
            request_budget_split_receipt_hash: B256::repeat_byte(9),
        },
        logical_evaluation_height: block_number,
        logical_evaluation_time: 1_000,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: 7,
                source_generation: 1,
                collection_key: B256::repeat_byte(5),
                sealed_collection_root: B256::repeat_byte(6),
                exact_count: 0,
                exact_nominal_total: U256::ZERO,
            },
            nod: NodTargetPreconditionV1 {
                wwd: 7,
                target_generation: 1,
                namespace_root_before: B256::repeat_byte(10),
                max_nod_count: 0,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: 7,
                expected_series_version: 1,
                max_contributor_count: 0,
                max_eligible_nominal_total: U256::ZERO,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: 7,
                pending_nonce: 1,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: B256::repeat_byte(11),
        result_ocomp_binding_hash: B256::repeat_byte(12),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    }
}

fn encoded_storage_slots(logical_key: B256, encoded: &[u8]) -> Vec<(B256, U256)> {
    let base = logical_key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT));
    if encoded.len() <= 31 {
        let mut inline = [0_u8; 32];
        inline[..encoded.len()].copy_from_slice(encoded);
        inline[31] = (encoded.len() * 2) as u8;
        return vec![(
            B256::new(base.to_be_bytes::<32>()),
            U256::from_be_bytes(inline),
        )];
    }

    let mut slots = Vec::with_capacity(1 + encoded.len().div_ceil(32));
    slots.push((
        B256::new(base.to_be_bytes::<32>()),
        U256::from(encoded.len() * 2 + 1),
    ));
    let data_base = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
    for (index, chunk) in encoded.chunks(32).enumerate() {
        let mut word = [0_u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        slots.push((
            B256::new((data_base + U256::from(index)).to_be_bytes::<32>()),
            U256::from_be_bytes(word),
        ));
    }
    slots
}

fn production_candidate_source() -> ProductionCandidateFixture {
    let request = block(100, B256::repeat_byte(0x7a), 0x7b);
    let intent = production_intent(request.number());
    let limits = poc_schema_limits();
    let intent_id = intent.intent_id(&limits).expect("fixture IntentId");
    let record = OcompJobRecordV1 {
        intent: intent.clone(),
        intent_height: request.number(),
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    };
    let encoded = record
        .encode_canonical(&limits)
        .expect("fixture record encoding");
    let logical_key = intent_storage_key(intent_id).expect("fixture intent storage key");

    let chain_spec: ChainSpec<OutbeHeader> = ChainSpecBuilder::mainnet()
        .build()
        .map_header(OutbeHeader::new);
    let provider = MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec);
    provider.add_header(request.block_hash(), request.header().clone());
    provider.add_account(
        METADOSIS_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO)
            .with_bytecode(Bytes::from_static(&[0xef]))
            .extend_storage(encoded_storage_slots(logical_key, &encoded)),
    );
    let activation_preconditions_hash = intent
        .activation_preconditions
        .activation_preconditions_hash(&limits)
        .expect("fixture activation hash");
    let event = IMetadosis::OffchainJobRequested {
        intentId: intent_id,
        wwd: intent.wwd,
        pendingNonce: intent.pending_nonce,
        attempt: intent.attempt,
        activationPreconditionsHash: activation_preconditions_hash,
    };
    let pending_receipts = Arc::new(Mutex::new(Some((
        request.block_hash(),
        vec![Receipt {
            tx_type: TxType::Legacy,
            success: true,
            cumulative_gas_used: 1,
            logs: vec![Log {
                address: METADOSIS_ADDRESS,
                data: event.encode_log_data(),
            }],
        }],
    ))));
    let expected = CandidatePinV1 {
        block_number: request.number(),
        block_hash: request.block_hash(),
        state_root: request.header().inner.state_root,
        intent_id,
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    let receipt_reader = pending_receipts.clone();
    let source = Arc::new(RethFinalizedInputProofSource::new(
        provider,
        FinalizedParentCertStore::new(),
        move || {
            receipt_reader
                .lock()
                .map(|pending| pending.clone())
                .map_err(|_| "pending receipt fixture lock is poisoned".to_owned())
        },
        10,
    ));
    ProductionCandidateFixture {
        request,
        candidate: expected,
        source,
        pending_receipts,
    }
}

#[test]
fn unified_finalized_frame_supplies_retention_receipts_without_a_second_read() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let receipts = pending_receipts
        .lock()
        .expect("pending receipt fixture")
        .take()
        .expect("fixture receipts")
        .1;
    let frame = FinalizedFrame::for_test(
        BlockNumHash::new(request.number(), request.block_hash()),
        request.header().inner.parent_hash,
        request.header().inner.state_root,
        Block::default(),
        receipts,
    );

    let observation = observe_finalized_request(&frame)
        .expect("frame observation")
        .expect("request observation");
    assert_eq!(
        source
            .candidate_for_finalized_observation(&frame, observation)
            .expect("frame candidate"),
        candidate
    );
    assert!(
        pending_receipts
            .lock()
            .expect("pending receipt fixture")
            .is_none(),
        "the finalized path must not call the pending/canonical receipt reader"
    );
}

fn unverified_proof(candidate: CandidatePinV1, intent: &JobIntentV1) -> FinalizedIntentProofV1 {
    let limits = poc_schema_limits();
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(Vec::new()),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: candidate.block_number,
            finalized_block_hash: candidate.block_hash,
            finalized_epoch: 0,
            finalized_view: 0,
            parent_view: 0,
            ordered_committee: Vec::new(),
            signer_bitmap: BoundedBytes(Vec::new()),
            canonical_commonware_finalization_proof: ProofBytes(Vec::new()),
            committee_set_hash: B256::ZERO,
            vrf_material_version: 0,
            vrf_group_public_key_hash: B256::ZERO,
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(Vec::new()),
        canonical_job_intent: BoundedBytes(
            intent
                .encode_canonical(&limits)
                .expect("fixture intent must encode"),
        ),
        intent_account_proof: ProofBytes(Vec::new()),
        intent_storage_proof: ProofBytes(Vec::new()),
    }
}

fn ready_record(coordinator: &OcompRetentionCoordinator) -> PinRecordV1 {
    match coordinator.status() {
        RetentionStatus::Ready(record) => record,
        other => panic!("expected ready pin record, got {other:?}"),
    }
}

#[test]
fn shared_retention_selector_fails_closed_then_delegates_without_rebinding() {
    let selector = SharedOcompRetentionSelector::new();
    let day = WorldwideDay::new(17);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, day),
        Err("OCOMP retention coordinator is not installed".to_owned())
    );

    let request = block(100, B256::repeat_byte(0x31), 0x32);
    let candidate = candidate(&request, B256::repeat_byte(0x33));
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture tentative JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("first journal root");
    let storage = Arc::new(MemoryStorage::default());
    let coordinator = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source,
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage)),
    ));
    selector
        .install(coordinator.clone())
        .expect("first coordinator installs");

    assert_eq!(
        DeterministicConsensusDriver::vote(coordinator.as_ref(), &request),
        VoteOutcome::Positive
    );
    let selected_day = WorldwideDay::new(candidate.wwd);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, selected_day).unwrap(),
        Some(RetainedTributePin {
            input_lease_id: candidate.input_lease_id,
            worldwide_day: selected_day,
        })
    );

    let replacement_root = tempfile::tempdir().expect("replacement journal root");
    let replacement_storage = Arc::new(MemoryStorage::default());
    let replacement = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        replacement_root.path(),
        Arc::new(DeterministicProofSource::default()),
        Arc::new(RetainedTributeWriter::new(
            replacement_storage.clone(),
            replacement_storage,
        )),
    ));
    assert!(matches!(
        selector.install(replacement),
        Err(RetentionError::RetentionCoordinatorAlreadyInstalled)
    ));
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, selected_day).unwrap(),
        Some(RetainedTributePin {
            input_lease_id: candidate.input_lease_id,
            worldwide_day: selected_day,
        })
    );
}

#[test]
fn discovery_generation_replay_and_export_ack_survive_terminal_and_release() {
    let selector = SharedOcompRetentionSelector::new();
    let request = block(100, B256::repeat_byte(0x41), 0x42);
    let candidate = candidate(&request, B256::repeat_byte(0x43));
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
    selector
        .install(Arc::clone(&coordinator))
        .expect("coordinator installs");

    assert_eq!(
        DeterministicConsensusDriver::vote(coordinator.as_ref(), &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), coordinator.as_ref(), &request);
    let finalized = selector
        .discovery_job_records(job_id)
        .expect("finalized discovery generation");
    assert_eq!(finalized.len(), 1);
    assert_eq!(finalized[0].1.candidate, candidate);
    let source_generation = finalized[0].0;

    let exported = coordinator
        .record_exported(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("export transition");
    assert_eq!(
        selector
            .discovery_job_records(job_id)
            .expect("exported discovery generation")
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![source_generation]
    );

    coordinator
        .observe_terminal(job_id, exported.generation, 120)
        .expect("terminal transition");
    assert_eq!(
        selector
            .discovery_job_records(job_id)
            .expect("terminal restart candidates")
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![source_generation]
    );
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 10, B256::repeat_byte(0x44))
        .is_err());
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x45))
        .is_err());
    selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("terminal restart confirms the prior durable export ACK");
    coordinator
        .release_due(184)
        .expect("release scan")
        .expect("terminal retention is due");
    selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("released restart confirms the prior durable export ACK");
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x45))
        .is_err());
}

#[test]
fn export_authority_survives_interleaved_global_registry_generations() {
    let first_request = block(100, B256::repeat_byte(0x51), 0x52);
    let second_request = block(101, B256::repeat_byte(0x53), 0x54);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x55));
    let second_candidate = candidate(&second_request, B256::repeat_byte(0x56));
    let first_job = job_id_from_intent_id(
        first_candidate.intent_id,
        first_candidate.block_hash,
        first_candidate.state_root,
    )
    .unwrap();
    let second_job = job_id_from_intent_id(
        second_candidate.intent_id,
        second_candidate.block_hash,
        second_candidate.state_root,
    )
    .unwrap();
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job),
        (second_candidate, second_job),
    ]));
    let root = tempfile::tempdir().unwrap();
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &first_request);
    let first_source_generation = coordinator.finalized_job_record(first_job).unwrap().0;

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &second_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &second_request);
    let exported = coordinator
        .record_exported(
            first_job,
            first_source_generation,
            9,
            B256::repeat_byte(0x57),
        )
        .unwrap();
    assert!(exported.generation > first_source_generation + 1);
    coordinator
        .observe_terminal(first_job, exported.generation, 120)
        .unwrap();

    assert_eq!(
        coordinator
            .discovery_job_records(first_job)
            .unwrap()
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![first_source_generation]
    );
    coordinator
        .confirm_export_ack(
            first_job,
            first_source_generation,
            9,
            B256::repeat_byte(0x57),
        )
        .unwrap();
}

#[test]
fn ocm_pin_001_missing_finality_keeps_the_job_non_signable_and_can_reconcile_later() {
    let request = block(100, B256::repeat_byte(0x30), 0);
    let candidate = candidate(&request, B256::repeat_byte(0x40));
    let job_id = B256::repeat_byte(0x50);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    source.set_finality_available(false);
    assert!(coordinator.reconcile_finalized(&request).is_err());
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );
    assert!(!coordinator.is_signable(job_id));
    assert!(!coordinator.is_exportable(job_id));

    source.set_finality_available(true);
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
    assert!(!coordinator.is_signable(job_id));
    assert!(coordinator.is_exportable(job_id));
}

#[test]
fn ocm_pin_001_old_tentative_survives_repeated_finality_misses_and_restart() {
    let request = block(100, B256::repeat_byte(0x72), 0x73);
    let candidate = candidate(&request, B256::repeat_byte(0x74));
    let job_id = B256::repeat_byte(0x75);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("delayed-finality journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    source.set_finality_available(false);
    for ordinal in 0_u64..9 {
        assert!(coordinator
            .reconcile_finalized(&block(
                200 + ordinal,
                keccak256(ordinal.to_be_bytes()),
                u8::try_from(ordinal).expect("bounded fixture ordinal"),
            ))
            .is_err());
    }
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        },
        "age and repeated reconciliation misses cannot evict unresolved Tentative state"
    );
    drop(coordinator);

    source.set_finality_available(true);
    source.observe_finalized(&request);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    restarted
        .reconcile_finalized(&request)
        .expect("a later exact finality observation reconciles after restart");
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Finalized {
            job_id: current,
            ..
        } if current == job_id
    ));
}

#[tokio::test]
async fn ocm_pin_001_restart_reconciles_a_canonical_tentative_from_the_recovered_tip() {
    let request = block(100, B256::repeat_byte(0x38), 8);
    let successor = block_extending(101, B256::repeat_byte(0x39), request.block_hash(), 9);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    drop(coordinator);

    source.observe_finalized(&successor);
    let restarted = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    let (service, handle, execution) =
        OcompRetentionService::new_with_execution_readiness(restarted.clone(), None, 99);
    let worker = tokio::spawn(service.run());
    handle
        .reconcile_finalized(&successor)
        .expect("recovered marshal tip is queued exactly");
    tokio::task::yield_now().await;
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Tentative { .. }
    ));

    execution
        .notify_execution_finalized(successor.number())
        .expect("recovered execution watermark reaches retention");
    drop(handle);
    drop(execution);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("recovered retention worker finishes")
        .expect("recovered retention worker task succeeds");
    assert_eq!(
        ready_record(&restarted),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 101,
                open_height: 105,
                deadline_height: 115,
            },
        }
    );
    assert!(!restarted.is_signable(job_id));
    assert!(restarted.is_exportable(job_id));
}

#[test]
fn ocm_pin_001_production_source_opens_typed_post_state_on_each_independent_node() {
    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts: _pending_receipts,
    } = production_candidate_source();
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();

    for root in &roots {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        assert_eq!(
            ready_record(&coordinator),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative { candidate },
            }
        );
        assert!(root.path().join("pin.v1").is_file());
    }
}

#[test]
fn finalized_intent_export_rejects_a_proof_for_a_different_intent() {
    let request = block(100, B256::repeat_byte(0x7a), 0x7b);
    let intended = production_intent(request.number());
    let limits = poc_schema_limits();
    let intent_id = intended.intent_id(&limits).expect("fixture IntentId");
    let candidate = candidate(&request, intent_id);
    let job_id = job_id_from_intent_id(intent_id, candidate.block_hash, candidate.state_root)
        .expect("fixture JobId");

    let mut different = intended;
    different.pending_nonce += 1;
    different.attempt += 1;
    different.activation_preconditions.metadosis.pending_nonce = different.pending_nonce;
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    let source = Arc::new(ProofReturningSource {
        inner: inner.clone(),
        proofs: BTreeMap::from([(
            candidate.block_hash,
            unverified_proof(candidate, &different),
        )]),
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);

    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin must become durable");
    DeterministicConsensusDriver::finalize(&inner, &coordinator, &request);

    assert!(
        coordinator.build_finalized_intent_proof(job_id).is_err(),
        "the exact live JobId must not export another intent's proof"
    );
}

#[tokio::test]
async fn ocm_pin_001_consensus_finality_notification_does_no_proof_or_disk_work_inline() {
    let request = block(100, B256::repeat_byte(0x79), 0x7a);
    let candidate = candidate(&request, B256::repeat_byte(0x7b));
    let job_id = B256::repeat_byte(0x7c);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    source.observe_finalized(&request);
    let snapshot_armer = Arc::new(RecordingSnapshotArmer::default());
    let (service, handle) = OcompRetentionService::new_with_snapshot_armer(
        coordinator.clone(),
        Some(snapshot_armer.clone()),
    );

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the node-local finality notification");
    assert!(
        snapshot_armer
            .jobs
            .lock()
            .expect("recorded snapshot jobs")
            .is_empty(),
        "consensus finality notification must not arm the snapshot inline"
    );
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );

    drop(handle);
    service.run().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
    assert_eq!(
        snapshot_armer
            .jobs
            .lock()
            .expect("recorded snapshot jobs")
            .as_slice(),
        &[job_id],
        "the node-owned worker must arm the exact finalized snapshot before returning"
    );
}

#[tokio::test]
async fn ocm_pin_001_finality_waits_for_exact_local_execution_without_dropping_the_block() {
    let request = block(100, B256::repeat_byte(0x8a), 0x8b);
    let candidate = candidate(&request, B256::repeat_byte(0x8c));
    let job_id = B256::repeat_byte(0x8d);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    source.observe_finalized(&request);
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");

    let (service, handle, execution) =
        OcompRetentionService::new_with_execution_readiness(coordinator.clone(), None, 99);
    let worker = tokio::spawn(service.run());

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the exact finalized block");
    tokio::task::yield_now().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        },
        "finality must not read execution state before the exact block is locally ready"
    );

    execution
        .notify_execution_finalized(request.number())
        .expect("execution readiness reaches the retention worker");
    drop(handle);
    drop(execution);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("retention worker finishes after its inputs close")
        .expect("retention worker task succeeds");

    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        },
        "the same exact finalized block must reconcile once execution is ready"
    );
}

#[tokio::test]
async fn ocm_pin_001_worker_retries_transient_finalized_header_unavailability() {
    let request = block(100, B256::repeat_byte(0x7d), 0x7e);
    let candidate = candidate(&request, B256::repeat_byte(0x7f));
    let job_id = B256::repeat_byte(0x80);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    inner.observe_finalized(&request);
    let resolve_calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(TransientFinalitySource {
        inner,
        resolve_calls: resolve_calls.clone(),
        failures_before_ready: 1,
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the node-local finality notification");
    drop(handle);
    service.run().await;

    assert_eq!(resolve_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
}

#[tokio::test]
async fn ocm_pin_001_worker_retains_exact_block_beyond_the_old_retry_limit() {
    let request = block(100, B256::repeat_byte(0x8e), 0x8f);
    let candidate = candidate(&request, B256::repeat_byte(0x90));
    let job_id = B256::repeat_byte(0x91);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    inner.observe_finalized(&request);
    let resolve_calls = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(TransientFinalitySource {
        inner,
        resolve_calls: resolve_calls.clone(),
        failures_before_ready: 8,
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    coordinator
        .prepare_candidate(&request)
        .expect("candidate pin is durable before finality");
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("exact finalized block is queued");
    drop(handle);
    tokio::time::timeout(Duration::from_secs(5), service.run())
        .await
        .expect("retention continues beyond the former eight-attempt horizon");

    assert_eq!(resolve_calls.load(Ordering::SeqCst), 9);
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Finalized {
            job_id: current,
            ..
        } if current == job_id
    ));
}

#[tokio::test]
async fn ocm_pin_001_queued_finality_does_not_skip_a_job_in_the_earlier_block() {
    let request = block(100, B256::repeat_byte(0x80), 0x81);
    let next = block_extending(101, B256::repeat_byte(0x82), request.block_hash(), 0x83);
    let candidate = candidate(&request, B256::repeat_byte(0x84));
    let job_id = B256::repeat_byte(0x85);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    source.observe_finalized(&request);
    source.observe_finalized(&next);
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source));
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("request-block finality is queued");
    handle
        .reconcile_finalized(&next)
        .expect("later finality is queued before the worker runs");
    assert_eq!(coordinator.status(), RetentionStatus::Empty);

    drop(handle);
    service.run().await;
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized {
                candidate,
                job_id,
                finality_recorded_height: 100,
                open_height: 104,
                deadline_height: 114,
            },
        }
    );
}

#[tokio::test]
async fn ocm_pin_001_new_payload_pending_receipts_reach_the_production_journal_before_vote() {
    use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
    use reth_ethereum::node::api::BeaconEngineMessage;
    use reth_node_builder::{ConsensusEngineHandle, ExecutionPayload as _};

    let ProductionCandidateFixture {
        request,
        candidate,
        source,
        pending_receipts,
    } = production_candidate_source();
    let execution_output = pending_receipts
        .lock()
        .expect("pending receipt fixture lock")
        .take()
        .expect("pending execution fixture");
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);
    let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel();
    let engine = ConsensusEngineHandle::<OutbePayloadTypes>::new(engine_tx);

    let prepare = async {
        let payload = OutbeExecutionData::new(Arc::new(request.clone().into_inner()));
        let status = engine
            .new_payload(payload)
            .await
            .expect("execution engine responds to locally built payload");
        assert!(status.is_valid());
        coordinator
            .prepare_candidate(&request)
            .expect("production pending receipts and typed state are durable before vote");
    };
    let execute = async {
        let BeaconEngineMessage::NewPayload { payload, tx } = engine_rx
            .recv()
            .await
            .expect("locally built payload reaches the execution engine")
        else {
            panic!("candidate preparation must use new_payload");
        };
        assert_eq!(payload.block_hash(), request.block_hash());
        *pending_receipts
            .lock()
            .expect("pending receipt fixture lock") = Some(execution_output);
        tx.send(Ok(PayloadStatus::new(
            PayloadStatusEnum::Valid,
            Some(request.block_hash()),
        )))
        .expect("candidate preparation is still awaiting execution status");
    };

    futures::join!(prepare, execute);
    assert_eq!(
        ready_record(&coordinator),
        PinRecordV1 {
            generation: 1,
            state: PinStateV1::Tentative { candidate },
        }
    );
    assert!(root.path().join("pin.v1").is_file());
}

#[test]
fn ocm_pin_001_tentative_is_durable_across_independent_nodes_and_finalizes_exactly() {
    let request = block(100, B256::repeat_byte(0x31), 1);
    let candidate = candidate(&request, B256::repeat_byte(0x41));
    let job_id = B256::repeat_byte(0x51);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();
    let coordinators = roots
        .iter()
        .map(|root| OcompRetentionCoordinator::open(root.path(), source.clone()))
        .collect::<Vec<_>>();

    let mut journal_bytes = Vec::new();
    for coordinator in &coordinators {
        assert_eq!(coordinator.status(), RetentionStatus::Empty);
        assert_eq!(
            DeterministicConsensusDriver::vote(coordinator, &request),
            VoteOutcome::Positive
        );
        assert_eq!(
            ready_record(coordinator),
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative { candidate },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        let bytes = fs::read(roots[journal_bytes.len()].path().join("pin.v1"))
            .expect("positive vote must follow a published journal");
        assert!(!bytes.is_empty());
        journal_bytes.push(bytes);
    }
    assert!(journal_bytes.windows(2).all(|pair| pair[0] == pair[1]));

    for coordinator in &coordinators {
        DeterministicConsensusDriver::finalize(source.as_ref(), coordinator, &request);
        assert_eq!(
            ready_record(coordinator),
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Finalized {
                    candidate,
                    job_id,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 114,
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        assert!(coordinator.is_exportable(job_id));
    }
}

#[test]
fn ocm_pin_001_orphan_releases_and_remains_non_signable_after_restart() {
    let request = block(100, B256::repeat_byte(0x32), 2);
    let canonical = block(100, B256::repeat_byte(0x33), 3);
    let candidate = candidate(&request, B256::repeat_byte(0x42));
    let job_id = B256::repeat_byte(0x52);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let roots = (0..4)
        .map(|_| tempfile::tempdir().expect("validator journal root"))
        .collect::<Vec<_>>();

    for root in &roots {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &canonical);
        assert_eq!(
            ready_record(&coordinator),
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Released {
                    candidate,
                    job_id: None,
                    reason: PinReleaseReason::Orphaned,
                    observed_height: canonical.number(),
                    export: None,
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        drop(coordinator);

        let restarted = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
        assert!(!restarted.is_signable(job_id));
        assert!(!restarted.is_exportable(job_id));
        assert_eq!(
            DeterministicConsensusDriver::vote(&restarted, &request),
            VoteOutcome::Abstained,
            "the orphaned candidate must not become live after restart"
        );
    }
}

#[test]
fn tentative_pin_selects_exact_day_and_orphan_finality_garbage_collects_its_retained_body() {
    let request = block(100, B256::repeat_byte(0x62), 0x63);
    let canonical = block(100, B256::repeat_byte(0x64), 0x65);
    let candidate = candidate(&request, B256::repeat_byte(0x66));
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture tentative JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("validator journal root");
    let storage = Arc::new(MemoryStorage::default());
    let retained_writer = Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone()));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        retained_writer,
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    let day = WorldwideDay::new(candidate.wwd);
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&coordinator, day).unwrap(),
        Some(pin)
    );

    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x67; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x68),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("tentative retained copy");
    storage
        .apply_atomic(&retain)
        .expect("tentative retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");

    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &canonical);
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("orphan retained source")
        .records
        .is_empty());
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Released {
            candidate: released,
            job_id: None,
            reason: PinReleaseReason::Orphaned,
            ..
        } if released == candidate
    ));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailSync {
    File,
    Directory,
}

struct FailOnceDurability {
    point: FailSync,
    failed: AtomicBool,
}

impl FailOnceDurability {
    fn at(point: FailSync) -> Self {
        Self {
            point,
            failed: AtomicBool::new(false),
        }
    }

    fn should_fail(&self, point: FailSync) -> bool {
        self.point == point && !self.failed.swap(true, Ordering::SeqCst)
    }
}

impl JournalDurability for FailOnceDurability {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.should_fail(FailSync::File) {
            return Err(io::Error::other("injected file fsync failure"));
        }
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        if self.should_fail(FailSync::Directory) {
            return Err(io::Error::other("injected directory fsync failure"));
        }
        directory.sync_all()
    }
}

#[test]
fn ocm_pin_001_crash_recovery_multi_job_and_ambiguous_restart_are_safe() {
    let first = block(100, B256::repeat_byte(0x34), 4);
    let second = block(101, B256::repeat_byte(0x35), 5);
    let first_candidate = candidate(&first, B256::repeat_byte(0x43));
    let second_candidate = candidate(&second, B256::repeat_byte(0x44));
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, B256::repeat_byte(0x53)),
        (second_candidate, B256::repeat_byte(0x54)),
    ]));
    let fsync_root = tempfile::tempdir().expect("fsync journal root");
    let coordinator = OcompRetentionCoordinator::open_with_durability(
        fsync_root.path(),
        source.clone(),
        Arc::new(FailOnceDurability::at(FailSync::File)),
    );
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first),
        VoteOutcome::Abstained
    );
    assert!(matches!(
        coordinator.status(),
        RetentionStatus::Quarantined { .. }
    ));
    assert!(fsync_root.path().join("pin.v1.tmp").is_file());
    assert!(!fsync_root.path().join("pin.v1").exists());
    drop(coordinator);
    let restarted_after_fsync = OcompRetentionCoordinator::open(fsync_root.path(), source.clone());
    assert!(matches!(
        restarted_after_fsync.status(),
        RetentionStatus::Ready(_)
    ));
    assert_eq!(
        DeterministicConsensusDriver::vote(&restarted_after_fsync, &first),
        VoteOutcome::Positive,
        "a complete exact-next temp generation must recover after restart"
    );

    let successor_root = tempfile::tempdir().expect("successor recovery root");
    let initial = OcompRetentionCoordinator::open(successor_root.path(), source.clone());
    assert_eq!(
        DeterministicConsensusDriver::vote(&initial, &first),
        VoteOutcome::Positive
    );
    drop(initial);
    let interrupted_successor = OcompRetentionCoordinator::open_with_durability(
        successor_root.path(),
        source.clone(),
        Arc::new(FailOnceDurability::at(FailSync::File)),
    );
    assert_eq!(
        DeterministicConsensusDriver::vote(&interrupted_successor, &second),
        VoteOutcome::Abstained
    );
    assert!(successor_root.path().join("pin.v1").is_file());
    assert!(successor_root.path().join("pin.v1.tmp").is_file());
    drop(interrupted_successor);
    let recovered_successor =
        OcompRetentionCoordinator::open(successor_root.path(), source.clone());
    assert!(matches!(
        recovered_successor.status(),
        RetentionStatus::Ready(_)
    ));
    assert_eq!(
        DeterministicConsensusDriver::vote(&recovered_successor, &second),
        VoteOutcome::Positive,
        "a valid temp successor must atomically replace the prior generation"
    );

    let root = tempfile::tempdir().expect("conflict journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first),
        VoteOutcome::Positive
    );
    let before_second_job = fs::read(root.path().join("pin.v1")).unwrap();
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &second),
        VoteOutcome::Positive
    );
    let after_second_job = fs::read(root.path().join("pin.v1")).unwrap();
    assert_ne!(
        after_second_job, before_second_job,
        "an independent candidate must be added durably"
    );
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first),
        VoteOutcome::Positive,
        "adding another Job must not replace the first Job entry"
    );
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        after_second_job,
        "an exact retry remains idempotent"
    );
    drop(coordinator);

    fs::write(root.path().join("pin.v1.tmp"), b"torn").expect("inject torn write");
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert_eq!(
        DeterministicConsensusDriver::vote(&restarted, &first),
        VoteOutcome::Positive
    );
    assert!(!root.path().join("pin.v1.tmp").exists());
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        after_second_job,
        "discarding an unpublished torn temp must preserve the last durable record"
    );

    let directory_fsync_root = tempfile::tempdir().expect("directory fsync journal root");
    let directory_fsync = OcompRetentionCoordinator::open_with_durability(
        directory_fsync_root.path(),
        Arc::new(DeterministicProofSource::with_jobs([(
            first_candidate,
            B256::repeat_byte(0x53),
        )])),
        Arc::new(FailOnceDurability::at(FailSync::Directory)),
    );
    assert_eq!(
        DeterministicConsensusDriver::vote(&directory_fsync, &first),
        VoteOutcome::Abstained
    );
    assert!(directory_fsync_root.path().join("pin.v1").is_file());
    assert!(matches!(
        directory_fsync.status(),
        RetentionStatus::Quarantined { .. }
    ));
}

#[test]
fn ocm_pin_001_export_terminal_release_and_generation_cas_survive_restart() {
    let request = block(100, B256::repeat_byte(0x36), 6);
    let candidate = candidate(&request, B256::repeat_byte(0x45));
    let job_id = B256::repeat_byte(0x55);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let storage = Arc::new(MemoryStorage::default());
    let day = WorldwideDay::new(candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x57; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x58),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained copy");
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");
    let retained_writer = Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone()));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        retained_writer,
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&coordinator, day).unwrap(),
        Some(pin)
    );
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("retained source before release")
            .records
            .len(),
        1
    );
    let finalized = ready_record(&coordinator);
    assert!(!coordinator.is_signable(job_id));
    assert!(coordinator.is_exportable(job_id));
    assert!(matches!(
        coordinator.record_exported(job_id, finalized.generation - 1, 9, B256::repeat_byte(0x65),),
        Err(RetentionError::StaleGeneration { .. })
    ));
    let exported = coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x65))
        .expect("exact exporter CAS");
    assert!(coordinator.is_signable(job_id));
    drop(coordinator);

    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );
    let replayed = coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x65))
        .expect("lost export response replays after restart");
    assert_eq!(replayed, exported);
    assert!(coordinator.is_signable(job_id));
    assert!(matches!(
        coordinator.record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x66)),
        Err(RetentionError::StaleGeneration { .. })
    ));
    let terminal = coordinator
        .observe_terminal(job_id, exported.generation, 120)
        .expect("terminal finality");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            terminal_height: 120,
            release_height: 184,
            ..
        }
    ));
    assert!(!coordinator.is_signable(job_id));
    assert_eq!(coordinator.release_due(183).unwrap(), None);
    let released = coordinator
        .release_due(184)
        .expect("release transition")
        .expect("release is due");
    assert_eq!(released.generation, terminal.generation + 1);
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("exact job retained source after release")
        .records
        .is_empty());
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(!restarted.is_signable(job_id));
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_retained_predecessor_does_not_block_a_retry_job() {
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let retry_request = block(221, B256::repeat_byte(0x32), 2);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x41));
    let retry_candidate = candidate(&retry_request, B256::repeat_byte(0x42));
    let first_job_id = B256::repeat_byte(0x51);
    let retry_job_id = B256::repeat_byte(0x52);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (retry_candidate, retry_job_id),
    ]));
    let root = tempfile::tempdir().expect("multi-job journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &first_request);
    let first_finalized = ready_record(&coordinator);
    let first_exported = coordinator
        .record_exported(
            first_job_id,
            first_finalized.generation,
            9,
            B256::repeat_byte(0x61),
        )
        .expect("first job export is durable");
    let first_terminal = coordinator
        .observe_terminal(first_job_id, first_exported.generation, 219)
        .expect("first job reaches terminal retention");
    assert_eq!(first_terminal.generation, 4);

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &retry_request),
        VoteOutcome::Positive,
        "a retained predecessor must not be a global OCOMP lock"
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &retry_request);
    assert!(coordinator.is_exportable(retry_job_id));
    assert_eq!(
        coordinator
            .observe_terminal(first_job_id, first_terminal.generation, 219)
            .expect("the retained predecessor remains independently addressable"),
        first_terminal
    );
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(restarted.is_exportable(retry_job_id));
    assert_eq!(
        restarted
            .observe_terminal(first_job_id, first_terminal.generation, 219)
            .expect("both Job entries survive restart"),
        first_terminal
    );
}

#[test]
fn durable_registry_tracks_more_than_256_independent_jobs_across_restart() {
    let source = Arc::new(DeterministicProofSource::default());
    let root = tempfile::tempdir().expect("multi-job journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    for ordinal in 0_u64..257 {
        let request = block(
            1_000 + ordinal,
            keccak256(ordinal.to_be_bytes()),
            u8::try_from(ordinal & 0xff).unwrap(),
        );
        coordinator
            .record_tentative(candidate(
                &request,
                keccak256((ordinal + 10_000).to_be_bytes()),
            ))
            .expect("journal wire format, not an OCOMP product cap, bounds records");
    }
    drop(coordinator);

    let snapshot = inspect_retention_journal(root.path()).expect("inspect durable multi-job state");
    assert_eq!(snapshot.records.len(), 257);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert_eq!(
        inspect_retention_journal(root.path())
            .unwrap()
            .records
            .len(),
        257
    );
}

#[test]
fn ocm_pin_001_pressure_watermark_compacts_only_released_records_and_survives_restart() {
    fn pressure_candidate(ordinal: u64) -> CandidatePinV1 {
        let block_hash = keccak256([b"pressure-block".as_slice(), &ordinal.to_be_bytes()].concat());
        CandidatePinV1 {
            block_number: ordinal.saturating_add(1),
            block_hash,
            state_root: keccak256([b"pressure-state".as_slice(), &ordinal.to_be_bytes()].concat()),
            intent_id: keccak256([b"pressure-intent".as_slice(), &ordinal.to_be_bytes()].concat()),
            wwd: 7,
            ce_sealed_root: B256::repeat_byte(4),
            protocol_bundle_hash: B256::repeat_byte(3),
            input_lease_id: B256::repeat_byte(0x71),
        }
    }

    let root = tempfile::tempdir().expect("pressure journal root");
    let watermark = retention_pressure_watermark_for_test();
    let ancient_tentative = pressure_candidate(1);
    let finalized = pressure_candidate(2);
    let due_terminal = pressure_candidate(3);
    let future_terminal = pressure_candidate(4);
    let due_job = keccak256(b"pressure due job");
    let future_job = keccak256(b"pressure future job");
    let mut records = vec![
        (
            ancient_tentative.block_hash,
            PinRecordV1 {
                generation: 1,
                state: PinStateV1::Tentative {
                    candidate: ancient_tentative,
                },
            },
        ),
        (
            finalized.block_hash,
            PinRecordV1 {
                generation: 2,
                state: PinStateV1::Finalized {
                    candidate: finalized,
                    job_id: keccak256(b"pressure finalized job"),
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                },
            },
        ),
        (
            due_terminal.block_hash,
            PinRecordV1 {
                generation: 3,
                state: PinStateV1::Terminal {
                    candidate: due_terminal,
                    job_id: due_job,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                    export: ExportAuthorityV1 {
                        source_generation: 1,
                        lease_generation: 1,
                        manifest_hash: B256::repeat_byte(0x71),
                    },
                    terminal_height: 200,
                    release_height: 264,
                },
            },
        ),
        (
            future_terminal.block_hash,
            PinRecordV1 {
                generation: 4,
                state: PinStateV1::Terminal {
                    candidate: future_terminal,
                    job_id: future_job,
                    finality_recorded_height: 100,
                    open_height: 104,
                    deadline_height: 180,
                    export: ExportAuthorityV1 {
                        source_generation: 2,
                        lease_generation: 1,
                        manifest_hash: B256::repeat_byte(0x72),
                    },
                    terminal_height: 400,
                    release_height: 464,
                },
            },
        ),
    ];
    for ordinal in 4_u64..u64::try_from(watermark).expect("watermark fits u64") {
        let candidate = pressure_candidate(ordinal.saturating_add(10_000));
        records.push((
            candidate.block_hash,
            PinRecordV1 {
                generation: ordinal.saturating_add(1),
                state: PinStateV1::Released {
                    candidate,
                    job_id: (ordinal % 2 == 0).then(|| keccak256(ordinal.to_be_bytes())),
                    reason: if ordinal % 2 == 0 {
                        PinReleaseReason::RetentionSatisfied
                    } else {
                        PinReleaseReason::Orphaned
                    },
                    observed_height: ordinal,
                    export: (ordinal % 2 == 0).then_some(ExportAuthorityV1 {
                        source_generation: ordinal.saturating_add(1),
                        lease_generation: 1,
                        manifest_hash: keccak256(ordinal.to_be_bytes()),
                    }),
                },
            },
        ));
    }
    assert_eq!(records.len(), watermark);
    let last_updated = records.last().expect("seed has records").0;
    seed_retention_journal_for_test(
        root.path(),
        u64::try_from(watermark).expect("watermark fits generation"),
        last_updated,
        records,
    )
    .expect("seed canonical pressure journal");

    let source = Arc::new(DeterministicProofSource::default());
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert_eq!(
        inspect_retention_journal(root.path())
            .unwrap()
            .records
            .len(),
        watermark
    );
    coordinator
        .release_due(300)
        .expect("release due terminal under pressure")
        .expect("one due terminal transitions");
    drop(coordinator);
    let new_candidate = pressure_candidate(99_999);
    let interrupted_compaction = OcompRetentionCoordinator::open_with_durability(
        root.path(),
        source.clone(),
        Arc::new(FailOnceDurability::at(FailSync::File)),
    );
    assert!(interrupted_compaction
        .record_tentative(new_candidate)
        .is_err());
    assert!(root.path().join("pin.v1.tmp").is_file());
    drop(interrupted_compaction);
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert!(matches!(coordinator.status(), RetentionStatus::Ready(_)));
    coordinator
        .record_tentative(new_candidate)
        .expect("recovered compacted admission is idempotent");

    let snapshot = inspect_retention_journal(root.path()).expect("inspect compacted journal");
    let survivors = snapshot.records.into_iter().collect::<BTreeMap<_, _>>();
    assert_eq!(survivors.len(), 4);
    assert_eq!(
        survivors.get(&ancient_tentative.block_hash).unwrap().state,
        PinStateV1::Tentative {
            candidate: ancient_tentative
        }
    );
    assert!(matches!(
        survivors.get(&finalized.block_hash).unwrap().state,
        PinStateV1::Finalized { candidate, .. } if candidate == finalized
    ));
    assert!(!survivors.contains_key(&due_terminal.block_hash));
    assert!(matches!(
        survivors.get(&future_terminal.block_hash).unwrap().state,
        PinStateV1::Terminal {
            candidate,
            job_id,
            release_height: 464,
            ..
        } if candidate == future_terminal && job_id == future_job
    ));
    assert_eq!(
        survivors.get(&new_candidate.block_hash).unwrap().state,
        PinStateV1::Tentative {
            candidate: new_candidate
        }
    );

    drop(coordinator);
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(restarted.status(), RetentionStatus::Ready(_)));
    assert_eq!(
        inspect_retention_journal(root.path()).unwrap().records,
        survivors.into_iter().collect::<Vec<_>>()
    );
}

#[test]
fn ocm_pin_001_each_exported_job_remains_addressable_after_a_newer_export() {
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let second_request = block(221, B256::repeat_byte(0x32), 2);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x41));
    let second_candidate = candidate(&second_request, B256::repeat_byte(0x42));
    let first_job_id = B256::repeat_byte(0x51);
    let second_job_id = B256::repeat_byte(0x52);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (second_candidate, second_job_id),
    ]));
    let root = tempfile::tempdir().expect("multi-export journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    for (request, job_id, manifest_byte) in [
        (&first_request, first_job_id, 0x61),
        (&second_request, second_job_id, 0x62),
    ] {
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, request);
        let (generation, _) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(manifest_byte))
            .expect("job reaches exported state");
    }

    for (job_id, manifest_byte) in [(first_job_id, 0x61), (second_job_id, 0x62)] {
        let record = coordinator
            .exported_job_record(job_id)
            .expect("every live export remains addressable by JobId");
        assert!(matches!(
            record.state,
            PinStateV1::Exported {
                job_id: current,
                export,
                ..
            } if current == job_id
                && export.manifest_hash == B256::repeat_byte(manifest_byte)
        ));
    }
}

#[test]
fn ocm_pin_001_exported_record_reloads_every_live_export_by_job_id() {
    let limits = poc_schema_limits();
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let second_request = block(221, B256::repeat_byte(0x32), 2);
    let first_intent = production_intent(first_request.number());
    let mut second_intent = production_intent(second_request.number());
    second_intent.pending_nonce = 2;
    second_intent.attempt = 2;
    second_intent
        .activation_preconditions
        .metadosis
        .pending_nonce = 2;

    let make_candidate = |request: &ConsensusBlock, intent: &JobIntentV1| {
        let intent_id = intent.intent_id(&limits).expect("fixture IntentId");
        CandidatePinV1 {
            block_number: request.number(),
            block_hash: request.block_hash(),
            state_root: request.header().inner.state_root,
            intent_id,
            wwd: intent.wwd,
            ce_sealed_root: intent.ce_sealed_root,
            protocol_bundle_hash: intent.protocol_bundle_hash,
            input_lease_id: intent.input_lease_id().expect("fixture input lease"),
        }
    };
    let first_candidate = make_candidate(&first_request, &first_intent);
    let second_candidate = make_candidate(&second_request, &second_intent);
    let first_job_id = first_intent
        .job_id(
            first_candidate.block_hash,
            first_candidate.state_root,
            &limits,
        )
        .expect("first JobId");
    let second_job_id = second_intent
        .job_id(
            second_candidate.block_hash,
            second_candidate.state_root,
            &limits,
        )
        .expect("second JobId");
    let inner = DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (second_candidate, second_job_id),
    ]);
    let source = Arc::new(ProofReturningSource {
        inner: inner.clone(),
        proofs: BTreeMap::from([
            (
                first_candidate.block_hash,
                unverified_proof(first_candidate, &first_intent),
            ),
            (
                second_candidate.block_hash,
                unverified_proof(second_candidate, &second_intent),
            ),
        ]),
    });
    let root = tempfile::tempdir().expect("multi-export journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);

    for (request, job_id, manifest_byte) in [
        (&first_request, first_job_id, 0x61),
        (&second_request, second_job_id, 0x62),
    ] {
        coordinator
            .prepare_candidate(request)
            .expect("candidate pin must become durable");
        DeterministicConsensusDriver::finalize(&inner, &coordinator, request);
        let (generation, _) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(manifest_byte))
            .expect("job reaches exported state");
    }

    for job_id in [first_job_id, second_job_id] {
        let record = coordinator
            .exported_job_record(job_id)
            .expect("every live Exported job must remain independently addressable");
        assert!(matches!(
            record.state,
            PinStateV1::Exported {
                job_id: current,
                ..
            } if current == job_id
        ));
    }
}

#[test]
fn ocm_pin_001_finalized_state_drives_terminal_retention_after_export_ack() {
    let request = block(151, B256::repeat_byte(0x35), 5);
    let candidate = candidate(&request, B256::repeat_byte(0x45));
    let job_id = B256::repeat_byte(0x55);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("terminal observation journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x46))
        .expect("export ACK is durable before terminal retention");
    source.observe_terminal(job_id, 160);
    let terminal_block = block(160, B256::repeat_byte(0x36), 6);
    source.observe_finalized(&terminal_block);
    coordinator
        .reconcile_finalized(&terminal_block)
        .expect("finalized terminal state is node-local authority");

    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 160,
            release_height: 224,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_terminal_state_never_releases_unexported_input() {
    let request = block(151, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("unexported terminal journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    source.observe_terminal(job_id, 160);
    coordinator
        .reconcile_finalized(&block(400, B256::repeat_byte(0x38), 8))
        .expect("terminal observation is retryable while exporter is unavailable");

    assert_eq!(ready_record(&coordinator), finalized);
    assert!(coordinator
        .release_due(10_000)
        .expect("release scan")
        .is_none());
    assert!(coordinator.is_exportable(job_id));
}

#[test]
fn ocm_pin_001_production_open_automatically_releases_due_terminal_across_restart() {
    let request = block(151, B256::repeat_byte(0x6A), 0x6B);
    let candidate = candidate(&request, B256::repeat_byte(0x6C));
    let job_id = B256::repeat_byte(0x6D);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("production-open journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x6F))
        .expect("export ACK is durable before terminal retention");
    source.observe_terminal(job_id, 160);
    coordinator
        .reconcile_finalized(&block(160, B256::repeat_byte(0x6E), 0x6F))
        .expect("canonical terminal state is observed");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 160,
            release_height: 224,
            ..
        } if current == job_id
    ));

    coordinator
        .reconcile_finalized(&block(224, B256::repeat_byte(0x70), 0x71))
        .expect("production open drains every due terminal record");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: 224,
            ..
        } if current == job_id
    ));
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: Some(current),
            reason: PinReleaseReason::RetentionSatisfied,
            observed_height: 224,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_quorum_status_is_not_retention_terminal_before_response_deadline() {
    for status in [OcompJobStatus::Completed, OcompJobStatus::Conflicted] {
        assert_eq!(
            retention_terminal_height_for_status(status, 160, 165, 160).unwrap(),
            None,
            "quorum at 160 must not hide a still-open job before deadline 165"
        );
        assert_eq!(
            retention_terminal_height_for_status(status, 165, 165, 160).unwrap(),
            Some(165),
            "deadline closure, not quorum formation, terminalizes retention"
        );
    }
}

#[test]
fn ocm_pin_001_restart_keeps_quorum_complete_export_live_until_deadline() {
    let request = block(100, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request, B256::repeat_byte(0x47));
    let job_id = B256::repeat_byte(0x57);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    let source = Arc::new(ResponseWindowCompletedSource {
        inner: inner.clone(),
    });
    let root = tempfile::tempdir().expect("response-window journal root");

    {
        let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert_eq!(
            DeterministicConsensusDriver::vote(&coordinator, &request),
            VoteOutcome::Positive
        );
        DeterministicConsensusDriver::finalize(&inner, &coordinator, &request);
        let (generation, finalized) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        assert_eq!(finalized.deadline_height, 114);
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(0x67))
            .expect("job reaches exported state");

        inner.observe_terminal(job_id, 110);
        coordinator
            .reconcile_finalized(&block(110, B256::repeat_byte(0x38), 8))
            .expect("quorum block reconciles without terminalizing retention");
        assert_eq!(coordinator.finalized_live_jobs().unwrap().len(), 1);
        coordinator
            .exported_job_record(job_id)
            .expect("quorum-complete job remains exported before deadline");
    }

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    restarted
        .reconcile_finalized(&block(113, B256::repeat_byte(0x39), 9))
        .expect("restart before deadline restores the live export");
    assert_eq!(restarted.finalized_live_jobs().unwrap().len(), 1);
    restarted
        .exported_job_record(job_id)
        .expect("restarted node can still serve the pinned job");

    restarted
        .reconcile_finalized(&block(114, B256::repeat_byte(0x3A), 10))
        .expect("deadline closure terminalizes retention");
    assert!(restarted.finalized_live_jobs().unwrap().is_empty());
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Terminal {
            job_id: current,
            terminal_height: 114,
            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_shared_input_lease_is_collected_after_its_last_job_reference() {
    let first_request = block(151, B256::repeat_byte(0x33), 3);
    let retry_request = block(221, B256::repeat_byte(0x34), 4);
    let first_candidate = candidate(&first_request, B256::repeat_byte(0x43));
    let retry_candidate = candidate(&retry_request, B256::repeat_byte(0x44));
    assert_eq!(
        first_candidate.input_lease_id, retry_candidate.input_lease_id,
        "fixture models a retry over byte-identical authenticated input"
    );
    let first_job_id = B256::repeat_byte(0x53);
    let retry_job_id = B256::repeat_byte(0x54);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (retry_candidate, retry_job_id),
    ]));
    let root = tempfile::tempdir().expect("shared-lease journal root");
    let storage = Arc::new(MemoryStorage::default());
    let day = WorldwideDay::new(first_candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x61; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x62),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: first_candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained input lease");
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &first_request);
    let first_finalized = ready_record(&coordinator);
    let first_exported = coordinator
        .record_exported(
            first_job_id,
            first_finalized.generation,
            9,
            B256::repeat_byte(0x91),
        )
        .expect("first export");
    coordinator
        .observe_terminal(first_job_id, first_exported.generation, 219)
        .expect("first terminal");
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &retry_request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &retry_request);
    let retry_finalized = ready_record(&coordinator);
    let retry_exported = coordinator
        .record_exported(
            retry_job_id,
            retry_finalized.generation,
            10,
            B256::repeat_byte(0x92),
        )
        .expect("retry export");

    coordinator
        .release_due(283)
        .expect("first release")
        .expect("first reference is due");
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 1)
            .expect("shared lease remains readable")
            .records
            .len(),
        1,
        "the predecessor cannot collect input still referenced by its retry"
    );

    coordinator
        .observe_terminal(retry_job_id, retry_exported.generation, 300)
        .expect("retry terminal");
    coordinator
        .release_due(364)
        .expect("last release")
        .expect("last reference is due");
    assert!(retained_reader
        .list_by_day(pin, None, 1)
        .expect("lease after last-reference GC")
        .records
        .is_empty());
}

#[test]
fn ocm_pin_001_journal_bytes_are_stable_and_corruption_quarantines() {
    let request = block(100, B256::repeat_byte(0x37), 7);
    let candidate = candidate(&request, B256::repeat_byte(0x46));
    let job_id = B256::repeat_byte(0x56);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    let record = ready_record(&coordinator);
    let bytes = fs::read(root.path().join("pin.v1")).unwrap();
    assert_eq!(
        keccak256_for_test(&bytes),
        b256!("95f4424d2a191ddcde10b4339caeee832312d9ebdef1240402183eb3333cac71"),
        "update only when the intentional journal wire format changes"
    );
    assert_eq!(record.generation, 1);
    drop(coordinator);

    let mut unsupported = bytes;
    unsupported[8..10].copy_from_slice(&6_u16.to_be_bytes());
    let body_len = unsupported.len() - 32;
    let checksum = alloy_primitives::keccak256(&unsupported[..body_len]);
    unsupported[body_len..].copy_from_slice(checksum.as_slice());
    fs::write(root.path().join("pin.v1"), unsupported).unwrap();
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(
        restarted.status(),
        RetentionStatus::Quarantined { .. }
    ));
    assert!(!restarted.is_signable(job_id));
}

fn keccak256_for_test(bytes: &[u8]) -> B256 {
    alloy_primitives::keccak256(bytes)
}

fn eligibility_registration(
    validator: Address,
    consensus_pubkey: &[u8; 48],
    key_seed: u8,
) -> (OcompKeyRegistrationV1, Vec<u8>) {
    let signing_key = SigningKey::from_bytes((&[key_seed; 32]).into()).unwrap();
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: 42,
            genesis_hash: B256::ZERO,
            validator_identity_hash: validator_identity_hash_v1(validator, consensus_pubkey)
                .unwrap(),
            ocomp_public_key_sec1: signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
            key_epoch: POC_KEY_EPOCH,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    let limits = poc_schema_limits();
    let digest = registration.proof_of_possession_digest(&limits).unwrap();
    let signature: Signature = signing_key.sign_prehash(digest.as_slice()).unwrap();
    registration.proof_of_possession = signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into();
    let encoded = registration.encode_canonical(&limits).unwrap();
    (registration, encoded)
}

fn eligibility_snapshot_fixture(
    epoch: u64,
    member_count: u8,
    marker: u8,
) -> (CandidateProvider, ConsensusBlock, JobIntentV1, Vec<B256>) {
    let request = block(100 + epoch, B256::repeat_byte(marker), marker);
    let mut storage = HashMapStorageProvider::new(42);
    let (committee_set_hash, ocomp_binding_hash, key_hashes) =
        StorageHandle::enter(&mut storage, |storage| {
            let mut validator_set = ValidatorSet::new(storage.clone());
            validator_set.config_owner.write(Address::ZERO).unwrap();
            validator_set.set_config_max_validators(128).unwrap();

            let mut committee = Vec::with_capacity(usize::from(member_count));
            let mut key_hashes = Vec::with_capacity(usize::from(member_count));
            for index in 0..member_count {
                let validator = Address::repeat_byte(index.saturating_add(1));
                let consensus_pubkey = [index.saturating_add(0x31); 48];
                validator_set
                    .register_validator(Address::ZERO, validator, &consensus_pubkey)
                    .unwrap();
                validator_set.mark_pending(validator).unwrap();
                let (registration, encoded) = eligibility_registration(
                    validator,
                    &consensus_pubkey,
                    index.saturating_add(0x11),
                );
                validator_set
                    .confirm_validator_ready(validator, &encoded)
                    .unwrap();
                key_hashes.push(keccak256(registration.core.ocomp_public_key_sec1));
                committee.push(CommitteeEntry {
                    address: validator,
                    consensus_pubkey,
                });
            }
            let snapshot = CommitteeSnapshot {
                committee,
                vrf_material_version: epoch.saturating_add(1),
                vrf_group_public_key_bytes: vec![marker; 48],
                vrf_public_polynomial_hash: B256::ZERO,
            };
            let (committee_set_hash, snapshot_key) =
                write_committee_snapshot(storage.clone(), epoch, &snapshot).unwrap();
            let extension = read_ocomp_snapshot_extension(storage, snapshot_key)
                .unwrap()
                .expect("OCOMP snapshot extension");
            (committee_set_hash, extension.ocomp_binding_hash, key_hashes)
        });

    let chain_spec: ChainSpec<OutbeHeader> = ChainSpecBuilder::mainnet()
        .build()
        .map_header(OutbeHeader::new);
    let provider = MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec);
    provider.add_header(request.block_hash(), request.header().clone());
    let validator_set_storage = storage
        .storage
        .into_iter()
        .filter_map(|((address, slot), value)| {
            (address == VALIDATOR_SET_ADDRESS)
                .then_some((B256::new(slot.to_be_bytes::<32>()), value))
        })
        .collect::<Vec<_>>();
    provider.add_account(
        VALIDATOR_SET_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO)
            .with_bytecode(Bytes::from_static(&[0xef]))
            .extend_storage(validator_set_storage),
    );

    let mut intent = production_intent(request.number());
    intent.result_validator_set_epoch = epoch;
    intent.result_committee_set_hash = committee_set_hash;
    intent.result_ocomp_binding_hash = ocomp_binding_hash;
    intent.result_member_count = u16::from(member_count);
    intent.result_quorum_threshold = u16::from(member_count.saturating_mul(2) / 3 + 1);
    (provider, request, intent, key_hashes)
}

#[test]
fn ocomp_exact_request_eligibility_tracks_both_sides_of_activation_boundary() {
    let (old_provider, old_request, old_intent, old_keys) =
        eligibility_snapshot_fixture(0, 4, 0xa1);
    for key_hash in &old_keys {
        assert_eq!(
            ocomp_snapshot_contains_key_at(
                &old_provider,
                old_request.block_hash(),
                &old_intent,
                *key_hash,
            ),
            OcompSnapshotEligibilityV1::Eligible
        );
    }

    let (new_provider, new_request, new_intent, new_keys) =
        eligibility_snapshot_fixture(1, 5, 0xb1);
    assert_eq!(old_intent.result_member_count, 4);
    assert_eq!(new_intent.result_member_count, 5);
    assert_eq!(
        ocomp_snapshot_contains_key_at(
            &old_provider,
            old_request.block_hash(),
            &old_intent,
            new_keys[4],
        ),
        OcompSnapshotEligibilityV1::NotMember,
        "a request pinned before activation must exclude the incoming validator"
    );
    for key_hash in &new_keys {
        assert_eq!(
            ocomp_snapshot_contains_key_at(
                &new_provider,
                new_request.block_hash(),
                &new_intent,
                *key_hash,
            ),
            OcompSnapshotEligibilityV1::Eligible,
            "a request pinned after activation must include every advertised validator"
        );
    }
}
