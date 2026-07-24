//! OCM-PIN-001: production retention notifications and journal on a real filesystem.
// OCOMP-TEST-ID: OCM-PIN-001

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{b256, keccak256, Bytes, Log, B256, U256};
use alloy_sol_types::SolEvent as _;
use outbe_consensus::{
    block::ConsensusBlock, finalization::parent_cert_store::FinalizedParentCertStore,
    ocomp_retention::OcompRetentionHook,
};
use outbe_metadosis::{
    ocomp::schema::poc_schema_limits, precompile::IMetadosis, schema::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    intent::{
        intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    state::{OcompJobRecordV1, OcompJobStatus},
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS, storage::types::StorageKey as _, OutbeExecutionData, OutbeHeader,
    OutbePayloadTypes, OutbePrimitives,
};
use reth_chainspec::{ChainSpec, ChainSpecBuilder};
use reth_ethereum::{primitives::SealedBlock, Block, Receipt, TxType};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};

use crate::ocomp::retention::{
    CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, FinalizedJobPinV1,
    JournalDurability, OcompRetentionCoordinator, OcompRetentionService, PinRecordV1,
    PinReleaseReason, PinStateV1, RetentionError, RetentionStatus, RethFinalizedInputProofSource,
};

#[derive(Clone, Default)]
struct DeterministicProofSource {
    jobs: Arc<Mutex<BTreeMap<B256, (CandidatePinV1, B256)>>>,
    finalized: Arc<Mutex<BTreeMap<u64, (B256, B256)>>>,
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
        let finalized_hash = finalized
            .get(&candidate.block_number)
            .map(|(hash, _)| *hash)
            .or_else(|| {
                finalized
                    .get(&candidate.block_number.checked_add(1)?)
                    .map(|(_, parent_hash)| *parent_hash)
            })
            .ok_or_else(|| {
                RetentionError::Source("candidate-height finality is unavailable".to_owned())
            })?;
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
        }))
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
        deadline_height: block.number() + 10,
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
            auction_entry_price: U256::from(95),
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
                series_id: 7,
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
        result_committee_snapshot_hash: B256::repeat_byte(11),
        custody_committee_epoch_hash: None,
        deadline_height: block_number + 10,
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
        status: OcompJobStatus::OffchainPending,
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
        deadlineHeight: intent.deadline_height,
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
        deadline_height: intent.deadline_height,
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
    ));
    ProductionCandidateFixture {
        request,
        candidate: expected,
        source,
        pending_receipts,
    }
}

fn ready_record(coordinator: &OcompRetentionCoordinator) -> PinRecordV1 {
    match coordinator.status() {
        RetentionStatus::Ready(record) => record,
        other => panic!("expected ready pin record, got {other:?}"),
    }
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
            state: PinStateV1::Finalized { candidate, job_id },
        }
    );
    assert!(coordinator.is_signable(job_id));
}

#[test]
fn ocm_pin_001_restart_reconciles_a_canonical_tentative_from_the_next_finalization() {
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

    let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
    DeterministicConsensusDriver::finalize(source.as_ref(), &restarted, &successor);
    assert_eq!(
        ready_record(&restarted),
        PinRecordV1 {
            generation: 2,
            state: PinStateV1::Finalized { candidate, job_id },
        }
    );
    assert!(restarted.is_signable(job_id));
}

#[test]
fn ocm_pin_001_production_source_opens_typed_post_state_before_four_positive_votes() {
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
    let (service, handle) = OcompRetentionService::new(coordinator.clone());

    handle
        .reconcile_finalized(&request)
        .expect("consensus only queues the node-local finality notification");
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
            state: PinStateV1::Finalized { candidate, job_id },
        }
    );
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
            state: PinStateV1::Finalized { candidate, job_id },
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
fn ocm_pin_001_tentative_is_durable_before_four_positive_votes_and_finalizes_exactly() {
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
                state: PinStateV1::Finalized { candidate, job_id },
            }
        );
        assert!(coordinator.is_signable(job_id));
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
                },
            }
        );
        assert!(!coordinator.is_signable(job_id));
        drop(coordinator);

        let restarted = OcompRetentionCoordinator::open(root.path(), source.clone());
        assert!(!restarted.is_signable(job_id));
        assert!(!restarted.is_exportable(job_id));
        assert_eq!(
            DeterministicConsensusDriver::vote(&restarted, &request),
            VoteOutcome::Abstained,
            "the orphaned candidate must not become live after restart"
        );
    }
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
fn ocm_pin_001_fsync_failure_conflict_and_ambiguous_restart_abstain() {
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
        RetentionStatus::Quarantined { .. }
    ));

    let root = tempfile::tempdir().expect("conflict journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &first),
        VoteOutcome::Positive
    );
    let before_conflict = fs::read(root.path().join("pin.v1")).unwrap();
    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &second),
        VoteOutcome::Abstained
    );
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        before_conflict,
        "a conflicting candidate must not overwrite the active pin"
    );
    drop(coordinator);

    fs::write(root.path().join("pin.v1.tmp"), b"torn").expect("inject torn write");
    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(matches!(
        restarted.status(),
        RetentionStatus::Quarantined { .. }
    ));
    assert_eq!(
        DeterministicConsensusDriver::vote(&restarted, &first),
        VoteOutcome::Abstained
    );
    assert_eq!(
        fs::read(root.path().join("pin.v1")).unwrap(),
        before_conflict,
        "quarantine must preserve the last durable record"
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
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert_eq!(
        DeterministicConsensusDriver::vote(&coordinator, &request),
        VoteOutcome::Positive
    );
    DeterministicConsensusDriver::finalize(source.as_ref(), &coordinator, &request);
    let finalized = ready_record(&coordinator);
    assert!(matches!(
        coordinator.record_exported(job_id, finalized.generation - 1, 9, B256::repeat_byte(0x65),),
        Err(RetentionError::StaleGeneration { .. })
    ));
    let exported = coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x65))
        .expect("exact exporter CAS");
    assert!(coordinator.is_signable(job_id));
    let terminal = coordinator
        .observe_terminal(job_id, exported.generation, 120, 184)
        .expect("terminal finality");
    assert!(!coordinator.is_signable(job_id));
    assert_eq!(coordinator.release_due(183).unwrap(), None);
    let released = coordinator
        .release_due(184)
        .expect("release transition")
        .expect("release is due");
    assert_eq!(released.generation, terminal.generation + 1);
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
        b256!("d633aa7e6876c26f584a282da1536bb1d885795ed1d0550e63554fcbbe5a8836"),
        "update only when the intentional journal wire format changes"
    );
    assert_eq!(record.generation, 1);
    drop(coordinator);

    let mut unsupported = bytes;
    unsupported[8..10].copy_from_slice(&2_u16.to_be_bytes());
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
