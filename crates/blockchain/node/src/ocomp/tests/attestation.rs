use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt as _,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use alloy_primitives::{Address, Bytes, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_consensus::block::ConsensusBlock;
use outbe_metadosis::config::poc_schema_limits;
use outbe_ocomp_protocol::{
    committee::{
        OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::BoundedBytes,
    hash::hash_framed,
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    local_control::{effective_uid, ClientPolicy, ControlClientSession, EndpointIdentity},
    profile::poc_schema_limits as protocol_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    vote::ResultVoteV1,
    AttestationResponseV1, CommitLocalResultV1, ListFinalizedJobsResponseV1, ListFinalizedJobsV1,
    LocalErrorCode, LocalErrorV1, LocalResultCommittedV1, NodeMessageKind, ProtocolError,
    RequestAttestationV1, SchemaLimits, MAX_FINALIZED_JOBS_PER_RESPONSE,
};
use outbe_primitives::{
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    OutbePrimitives,
};
use outbe_validatorset::{
    ocomp_binding_hash_v1, read_committee_snapshot, read_ocomp_snapshot_extension,
    write_committee_snapshot, OcompSnapshotMemberV1, COMMITTEE_SNAPSHOT_RETAIN_EPOCHS,
};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Metadata, Subscriber,
};

use crate::ocomp::{
    attestation::{
        AtomicHeightSource, AttestationAuthorityError, AttestationError,
        ExportedAttestationAuthorityV1, FinalizedAttestationAuthority,
        HistoricalOcompSnapshotSource, HistoricalOcompSnapshotV1, OcompAttestationConfig,
        OcompAttestationGate, ProviderHistoricalOcompSnapshotSource, SnapshotSourceError,
    },
    control::OcompControlServer,
    local_result::LocalLysisResultStore,
    retention::{
        CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, OcompRetentionCoordinator,
        RetentionError,
    },
    sign_once::SignOnceStore,
    signer::OcompSigner,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index.saturating_add(1); 32]).into()).expect("test signing key")
}

fn sign(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key.sign_prehash(digest.as_slice()).expect("test signature");
    signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into()
}

#[derive(Clone, Default)]
struct CapturedEvents {
    fields: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}

struct CapturedEventVisitor {
    fields: BTreeMap<String, String>,
}

impl Visit for CapturedEventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl Subscriber for CapturedEvents {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = CapturedEventVisitor {
            fields: BTreeMap::new(),
        };
        event.record(&mut visitor);
        self.fields
            .lock()
            .expect("captured event mutex")
            .push(visitor.fields);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct CapturedMetrics {
    value: Arc<AtomicU64>,
    keys: Mutex<Vec<String>>,
}

impl metrics::Recorder for CapturedMetrics {
    fn describe_counter(
        &self,
        _key: metrics::KeyName,
        _unit: Option<metrics::Unit>,
        _description: metrics::SharedString,
    ) {
    }

    fn describe_gauge(
        &self,
        _key: metrics::KeyName,
        _unit: Option<metrics::Unit>,
        _description: metrics::SharedString,
    ) {
    }

    fn describe_histogram(
        &self,
        _key: metrics::KeyName,
        _unit: Option<metrics::Unit>,
        _description: metrics::SharedString,
    ) {
    }

    fn register_counter(
        &self,
        key: &metrics::Key,
        _metadata: &metrics::Metadata<'_>,
    ) -> metrics::Counter {
        self.keys
            .lock()
            .expect("captured metrics mutex")
            .push(key.to_string());
        metrics::Counter::from_arc(self.value.clone())
    }

    fn register_gauge(
        &self,
        _key: &metrics::Key,
        _metadata: &metrics::Metadata<'_>,
    ) -> metrics::Gauge {
        metrics::Gauge::noop()
    }

    fn register_histogram(
        &self,
        _key: &metrics::Key,
        _metadata: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::noop()
    }
}

fn registration(index: u8, limits: &SchemaLimits) -> OcompKeyRegistrationV1 {
    let key = signing_key(index);
    let public_key: [u8; 33] = key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .expect("compressed test key");
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: 42,
            genesis_hash: hash(40),
            validator_identity_hash: hash(70 + index),
            ocomp_public_key_sec1: public_key,
            key_epoch: 1,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    let digest = registration
        .proof_of_possession_digest(limits)
        .expect("PoP digest");
    registration.proof_of_possession = sign(&key, digest);
    registration
}

#[derive(Clone)]
pub(super) struct TestHistoricalCommittee {
    fork_id: B256,
    snapshot: HistoricalOcompSnapshotV1,
}

fn committee(limits: &SchemaLimits) -> TestHistoricalCommittee {
    let epoch = 1;
    let committee_set_hash = hash(45);
    let ordered_members = (0..4)
        .map(|index| {
            let registration = registration(index, limits);
            OcompSnapshotMemberV1 {
                validator_address: Address::repeat_byte(index.saturating_add(1)),
                ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
                key_epoch: registration.core.key_epoch,
            }
        })
        .collect::<Vec<_>>();
    let ocomp_binding_hash = ocomp_binding_hash_v1(epoch, committee_set_hash, &ordered_members);
    TestHistoricalCommittee {
        fork_id: hash(1),
        snapshot: HistoricalOcompSnapshotV1 {
            epoch,
            committee_set_hash,
            ocomp_binding_hash,
            ordered_members,
        },
    }
}

fn preconditions() -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: TributeInputBindingV1 {
            wwd: 7,
            source_generation: 3,
            collection_key: hash(30),
            sealed_collection_root: hash(31),
            exact_count: 1,
            exact_nominal_total: U256::ZERO,
        },
        nod: NodTargetPreconditionV1 {
            wwd: 7,
            target_generation: 5,
            namespace_root_before: hash(32),
            max_nod_count: 1,
        },
        contributors: ContributorTargetPreconditionV1 {
            series_id: 7,
            expected_series_version: 8,
            max_contributor_count: 1,
            max_eligible_nominal_total: U256::ZERO,
        },
        metadosis: MetadosisAttemptPreconditionV1 {
            wwd: 7,
            pending_nonce: 1,
            expected_status: MetadosisExpectedStatus::OffchainPending,
            state_version: 12,
        },
    }
}

pub(super) fn fixture() -> (
    TestHistoricalCommittee,
    ExportedAttestationAuthorityV1,
    LysisResultV1,
) {
    let limits = protocol_limits();
    assert_eq!(limits, poc_schema_limits());
    let committee = committee(&limits);
    let intent = JobIntentV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        wwd: 7,
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: hash(41),
        ce_sealed_root: hash(42),
        sealed_tribute_collection_key: hash(30),
        sealed_tribute_collection_root: hash(31),
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: hash(43),
        source_availability_policy_id: hash(44),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::ZERO,
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
            auction_entry_price: U256::ZERO,
            request_budget_split_receipt_hash: hash(113),
        },
        logical_evaluation_height: 100,
        logical_evaluation_time: 1_000,
        activation_preconditions: preconditions(),
        result_validator_set_epoch: committee.snapshot.epoch,
        result_committee_set_hash: committee.snapshot.committee_set_hash,
        result_ocomp_binding_hash: committee.snapshot.ocomp_binding_hash,
        result_member_count: committee.snapshot.ordered_members.len() as u16,
        result_quorum_threshold: outbe_consensus::proof::simplex_n3f1_quorum(
            committee.snapshot.ordered_members.len(),
        ) as u16,
        custody_committee_epoch_hash: None,
    };
    intent.validate_semantics().expect("valid test intent");
    let candidate = CandidatePinV1 {
        block_number: 99,
        block_hash: hash(90),
        state_root: hash(91),
        intent_id: intent.intent_id(&limits).expect("intent id"),
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    let job_id = intent
        .job_id(candidate.block_hash, candidate.state_root, &limits)
        .expect("job id");

    let carry_over_credit = CarryOverCreditActionV1 {
        source_wwd: 7,
        reason: CarryOverReason::UnusedLysis,
        amount: U256::ZERO,
    };
    let completion = MetadosisCompletionSummaryV1 {
        wwd: 7,
        pending_nonce: 1,
        day_type: DayType::Green,
        tribute_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        gratis_supply: U256::ZERO,
        lysis_budget: U256::ZERO,
        auction_base: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        unused_lysis: U256::ZERO,
        carry_over_credit: U256::ZERO,
        status: CompletionStatus::Completed,
        logical_evaluation_height: 100,
        logical_evaluation_time: 1_000,
    };
    let roots = ResultRootsV1 {
        nod_root: hash(50),
        bucket_root: hash(51),
        contributor_root: hash(52),
        output_manifest_root: hash(53),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        gratis_supply: U256::ZERO,
        lysis_budget: U256::ZERO,
        auction_base: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        unused_lysis: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    };
    let manifest_hash = hash(54);
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: manifest_hash,
        plan_hash: hash(55),
        unit_artifact_root: hash(56),
        fidelity_fraction_root: hash(57),
        gratis_prefix_root: hash(58),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    let arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &summary.encode_canonical(&limits).expect("summary encoding"),
    )
    .expect("arithmetic commitment");
    let result = LysisResultV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id,
        attempt: intent.attempt,
        input_manifest_hash: manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: hash(61),
        carry_over_credit,
        metadosis_completion_summary: completion,
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment,
        event_summary_hash: lysis_v1_empty_semantic_event_root().expect("empty event root"),
    };
    result.validate_semantics(&limits).expect("valid result");
    (
        committee,
        ExportedAttestationAuthorityV1 {
            candidate,
            job_id,
            manifest_hash,
            finalized_intent: intent,
            finality_recorded_height: 100,
            open_height: 104,
            deadline_height: 110,
        },
        result,
    )
}

struct StaticAuthority {
    value: Mutex<ExportedAttestationAuthorityV1>,
    reloads: AtomicUsize,
}

struct ChangingAuthority {
    first: ExportedAttestationAuthorityV1,
    second: ExportedAttestationAuthorityV1,
    reloads: AtomicUsize,
}

struct MultiAuthority {
    values: BTreeMap<B256, ExportedAttestationAuthorityV1>,
}

impl FinalizedAttestationAuthority for MultiAuthority {
    fn reload_exported(
        &self,
        job_id: B256,
        _limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        self.values
            .get(&job_id)
            .cloned()
            .ok_or(AttestationAuthorityError::NotExported(job_id))
    }
}

struct MultiHistoricalSnapshotSource {
    snapshots: Vec<HistoricalOcompSnapshotV1>,
}

impl HistoricalOcompSnapshotSource for MultiHistoricalSnapshotSource {
    fn load_snapshot(
        &self,
        epoch: u64,
        committee_set_hash: B256,
        ocomp_binding_hash: B256,
    ) -> Result<Option<HistoricalOcompSnapshotV1>, SnapshotSourceError> {
        Ok(self
            .snapshots
            .iter()
            .find(|snapshot| {
                snapshot.epoch == epoch
                    && snapshot.committee_set_hash == committee_set_hash
                    && snapshot.ocomp_binding_hash == ocomp_binding_hash
            })
            .cloned())
    }
}

impl FinalizedAttestationAuthority for ChangingAuthority {
    fn reload_exported(
        &self,
        job_id: B256,
        _limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        let reload = self.reloads.fetch_add(1, Ordering::SeqCst);
        let value = if reload == 0 {
            self.first.clone()
        } else {
            self.second.clone()
        };
        if value.job_id != job_id {
            return Err(AttestationAuthorityError::NotExported(job_id));
        }
        Ok(value)
    }
}

impl FinalizedAttestationAuthority for StaticAuthority {
    fn reload_exported(
        &self,
        job_id: B256,
        _limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        self.reloads.fetch_add(1, Ordering::SeqCst);
        let value = self.value.lock().expect("authority mutex").clone();
        if value.job_id != job_id {
            return Err(AttestationAuthorityError::NotExported(job_id));
        }
        Ok(value)
    }
}

struct EmptyProofSource;

struct StaticHistoricalSnapshotSource {
    snapshot: Option<HistoricalOcompSnapshotV1>,
}

impl HistoricalOcompSnapshotSource for StaticHistoricalSnapshotSource {
    fn load_snapshot(
        &self,
        epoch: u64,
        committee_set_hash: B256,
        ocomp_binding_hash: B256,
    ) -> Result<Option<HistoricalOcompSnapshotV1>, SnapshotSourceError> {
        Ok(self.snapshot.as_ref().and_then(|snapshot| {
            (snapshot.epoch == epoch
                && snapshot.committee_set_hash == committee_set_hash
                && snapshot.ocomp_binding_hash == ocomp_binding_hash)
                .then(|| snapshot.clone())
        }))
    }
}

impl FinalizedInputProofSource for EmptyProofSource {
    fn candidate_for_block(
        &self,
        _block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok(None)
    }

    fn resolve_finality(
        &self,
        _candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        Err(RetentionError::Source(
            "unused attestation control fixture".to_owned(),
        ))
    }
}

pub(super) fn attestation_gate(
    directory: &tempfile::TempDir,
    authority: Arc<dyn FinalizedAttestationAuthority>,
    height: Arc<AtomicHeightSource>,
    committee: TestHistoricalCommittee,
) -> OcompAttestationGate {
    let limits = protocol_limits();
    let snapshots = Arc::new(StaticHistoricalSnapshotSource {
        snapshot: Some(committee.snapshot),
    });
    let signer = OcompSigner::from_secret([1; 32]).expect("local OCOMP signer");
    let root = directory.path().join("sign-once");
    let owner_uid = fs::metadata(directory.path())
        .expect("temporary directory metadata")
        .uid();
    let store = SignOnceStore::open(root, owner_uid, limits).expect("sign-once store");
    OcompAttestationGate::new(
        authority,
        height,
        snapshots,
        OcompAttestationConfig {
            identity: EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            fork_id: committee.fork_id,
            validator_address: Address::repeat_byte(1),
        },
        signer,
        store,
        limits,
    )
    .expect("attestation gate")
}

fn assert_historical_vote(
    vote: &ResultVoteV1,
    exported: &ExportedAttestationAuthorityV1,
    committee: &TestHistoricalCommittee,
    inclusion_height: u64,
    limits: &SchemaLimits,
) {
    let member = committee
        .snapshot
        .ordered_members
        .get(usize::from(vote.validator_index))
        .expect("vote member in historical ValidatorSet snapshot");
    vote.verify_historical_member(
        &exported.finalized_intent,
        exported.job_id,
        u16::try_from(committee.snapshot.ordered_members.len())
            .expect("historical ValidatorSet member count fits u16"),
        member.key_epoch,
        &member.ocomp_public_key_sec1,
        inclusion_height,
        exported.open_height,
        exported.deadline_height,
        limits,
    )
    .expect("historical ValidatorSet vote verification");
}

fn control_server(
    directory: &tempfile::TempDir,
    gate: OcompAttestationGate,
) -> Arc<OcompControlServer> {
    Arc::new(
        base_control_server(directory)
            .with_local_result_store(local_result_store(directory))
            .with_attestation(Arc::new(gate)),
    )
}

fn local_result_store(directory: &tempfile::TempDir) -> Arc<LocalLysisResultStore> {
    Arc::new(
        LocalLysisResultStore::open(directory.path().join("local-results"), protocol_limits())
            .expect("local Lysis result store"),
    )
}

fn base_control_server(directory: &tempfile::TempDir) -> OcompControlServer {
    let limits = protocol_limits();
    let retention = Arc::new(OcompRetentionCoordinator::open(
        directory.path().join("retention"),
        Arc::new(EmptyProofSource),
    ));
    OcompControlServer::new(
        retention,
        effective_uid().expect("effective uid"),
        EndpointIdentity {
            chain_id: 42,
            genesis_hash: hash(40),
            boot_nonce: hash(99),
            protocol_bundle_hash: hash(41),
        },
        1,
        limits,
    )
    .expect("node control server")
}

fn recompute_arithmetic(result: &mut LysisResultV1, limits: &SchemaLimits) {
    result.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &result
            .arithmetic_summary()
            .encode_canonical(limits)
            .expect("arithmetic summary encoding"),
    )
    .expect("arithmetic commitment");
}

fn assert_no_sign_once_record(directory: &tempfile::TempDir) {
    assert_eq!(
        fs::read_dir(directory.path().join("sign-once"))
            .expect("read sign-once directory")
            .count(),
        0,
        "a rejected candidate must not consume the sign-once slot"
    );
}

fn mock_reth_provider(seed: &HashMapStorageProvider) -> MockEthProvider<OutbePrimitives> {
    let provider = MockEthProvider::<OutbePrimitives>::new();
    let mut accounts: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for ((account, slot), value) in &seed.storage {
        accounts
            .entry(*account)
            .or_default()
            .push((B256::from(slot.to_be_bytes::<32>()), *value));
    }
    for (account, storage) in accounts {
        provider.add_account(
            account,
            ExtendedAccount::new(0, U256::ZERO)
                .with_bytecode(Bytes::from_static(&[0xef]))
                .extend_storage(storage),
        );
    }
    provider
}

#[test]
fn ocm_sig_001_provider_source_reads_only_the_exact_retained_ring_snapshot() {
    let owner = Address::ZERO;
    let first = Address::repeat_byte(0x61);
    let second = Address::repeat_byte(0x62);
    let first_bls = [0x71; 48];
    let second_bls = [0x72; 48];
    let initial_epoch = 3u64;
    let mut seed = HashMapStorageProvider::new(42);
    let (snapshot, initial_consensus_hash, initial_binding_hash) =
        StorageHandle::enter(&mut seed, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            validators.config_owner.write(owner).unwrap();
            validators.config_is_initialized.write(true).unwrap();
            validators.set_config_max_validators(128).unwrap();
            validators
                .epoch_number
                .write(U256::from(initial_epoch))
                .unwrap();
            validators
                .register_validator(owner, first, &first_bls)
                .unwrap();
            validators
                .register_validator(owner, second, &second_bls)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(first)
                .unwrap();
            let snapshot_key = validators
                .activate_validator_via_boundary_for_test(second)
                .unwrap();
            let snapshot = read_committee_snapshot(storage.clone(), snapshot_key)
                .unwrap()
                .expect("current consensus snapshot");
            let extension = read_ocomp_snapshot_extension(storage, snapshot_key)
                .unwrap()
                .expect("current OCOMP extension");
            (
                snapshot,
                extension.committee_set_hash,
                extension.ocomp_binding_hash,
            )
        });

    let initial_source = ProviderHistoricalOcompSnapshotSource::new(mock_reth_provider(&seed));
    let initial = initial_source
        .load_snapshot(initial_epoch, initial_consensus_hash, initial_binding_hash)
        .unwrap()
        .expect("exact retained snapshot");
    assert_eq!(initial.ordered_members.len(), 2);
    assert!(initial_source
        .load_snapshot(
            initial_epoch,
            initial_consensus_hash,
            B256::repeat_byte(0xFF),
        )
        .unwrap()
        .is_none());

    let replacement_epoch = initial_epoch + COMMITTEE_SNAPSHOT_RETAIN_EPOCHS;
    let (replacement_key, replacement_consensus_hash, replacement_binding_hash) =
        StorageHandle::enter(&mut seed, |storage| {
            let (consensus_hash, snapshot_key) =
                write_committee_snapshot(storage.clone(), replacement_epoch, &snapshot).unwrap();
            let extension = read_ocomp_snapshot_extension(storage, snapshot_key)
                .unwrap()
                .expect("replacement OCOMP extension");
            (snapshot_key, consensus_hash, extension.ocomp_binding_hash)
        });
    let replacement_source = ProviderHistoricalOcompSnapshotSource::new(mock_reth_provider(&seed));
    assert!(replacement_source
        .load_snapshot(initial_epoch, initial_consensus_hash, initial_binding_hash,)
        .unwrap()
        .is_none());
    assert_eq!(
        replacement_source
            .load_snapshot(
                replacement_epoch,
                replacement_consensus_hash,
                replacement_binding_hash,
            )
            .unwrap()
            .expect("replacement retained snapshot")
            .ordered_members
            .len(),
        2
    );

    StorageHandle::enter(&mut seed, |storage| {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        validators
            .committee_snapshot_ocomp_key_lo_at
            .get_nested(&replacement_key)
            .write(&0, B256::ZERO)
            .unwrap();
        validators
            .committee_snapshot_ocomp_key_hi_at
            .get_nested(&replacement_key)
            .write(&0, B256::ZERO)
            .unwrap();
    });
    let corrupt_source = ProviderHistoricalOcompSnapshotSource::new(mock_reth_provider(&seed));
    assert!(corrupt_source
        .load_snapshot(
            replacement_epoch,
            replacement_consensus_hash,
            replacement_binding_hash,
        )
        .unwrap()
        .is_none());
}

#[test]
fn ocm_sig_001_derives_the_local_u16_index_from_the_job_snapshot() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let local_address = Address::repeat_byte(0xA1);
    let mut members = committee.snapshot.ordered_members.clone();
    members.swap(0, 2);
    members[2].validator_address = local_address;
    let snapshots = Arc::new(StaticHistoricalSnapshotSource {
        snapshot: Some(HistoricalOcompSnapshotV1 {
            epoch: exported.finalized_intent.result_validator_set_epoch,
            committee_set_hash: exported.finalized_intent.result_committee_set_hash,
            ocomp_binding_hash: exported.finalized_intent.result_ocomp_binding_hash,
            ordered_members: members,
        }),
    });
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let owner_uid = fs::metadata(directory.path())
        .expect("temporary directory metadata")
        .uid();
    let gate = OcompAttestationGate::new(
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        snapshots,
        OcompAttestationConfig {
            identity: EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            fork_id: committee.fork_id,
            validator_address: local_address,
        },
        OcompSigner::from_secret([1; 32]).expect("local OCOMP signer"),
        SignOnceStore::open(directory.path().join("sign-once"), owner_uid, limits)
            .expect("sign-once store"),
        limits,
    )
    .expect("dynamic attestation gate");

    let vote = gate.attest(result).expect("historical member attestation");
    assert_eq!(vote.validator_index, 2);
    vote.verify_historical_member(
        &exported.finalized_intent,
        exported.job_id,
        4,
        1,
        &OcompSigner::from_secret([1; 32])
            .expect("local OCOMP signer")
            .public_key_sec1(),
        105,
        exported.open_height,
        exported.deadline_height,
        &limits,
    )
    .expect("dynamic historical vote verifies");
}

#[test]
fn ocm_sig_001_two_live_membership_epochs_sign_with_their_own_indices() {
    let limits = protocol_limits();
    let (committee, first_exported, first_result) = fixture();
    let local_address = Address::repeat_byte(1);
    let first_members = committee.snapshot.ordered_members.clone();
    let first_snapshot = HistoricalOcompSnapshotV1 {
        epoch: first_exported.finalized_intent.result_validator_set_epoch,
        committee_set_hash: first_exported.finalized_intent.result_committee_set_hash,
        ocomp_binding_hash: first_exported.finalized_intent.result_ocomp_binding_hash,
        ordered_members: first_members.clone(),
    };

    let mut second_exported = first_exported.clone();
    second_exported.candidate.block_number += 100;
    second_exported.candidate.block_hash = hash(0xD1);
    second_exported.candidate.state_root = hash(0xD2);
    second_exported.finalized_intent.result_validator_set_epoch = 2;
    second_exported.finalized_intent.result_committee_set_hash = hash(0xD3);
    second_exported.finalized_intent.result_ocomp_binding_hash = hash(0xD4);
    second_exported.finalized_intent.result_member_count = 5;
    second_exported.finalized_intent.result_quorum_threshold = 4;
    second_exported.candidate.intent_id = second_exported
        .finalized_intent
        .intent_id(&limits)
        .expect("second IntentId");
    second_exported.job_id = second_exported
        .finalized_intent
        .job_id(
            second_exported.candidate.block_hash,
            second_exported.candidate.state_root,
            &limits,
        )
        .expect("second JobId");
    let mut second_result = first_result.clone();
    second_result.job_id = second_exported.job_id;

    let mut second_members = first_members[1..].to_vec();
    let fifth_registration = registration(4, &limits);
    second_members.push(outbe_validatorset::OcompSnapshotMemberV1 {
        validator_address: Address::repeat_byte(5),
        ocomp_public_key_sec1: fifth_registration.core.ocomp_public_key_sec1,
        key_epoch: fifth_registration.core.key_epoch,
    });
    second_members.push(outbe_validatorset::OcompSnapshotMemberV1 {
        validator_address: local_address,
        ocomp_public_key_sec1: first_members[0].ocomp_public_key_sec1,
        key_epoch: first_members[0].key_epoch,
    });
    let second_snapshot = HistoricalOcompSnapshotV1 {
        epoch: second_exported.finalized_intent.result_validator_set_epoch,
        committee_set_hash: second_exported.finalized_intent.result_committee_set_hash,
        ocomp_binding_hash: second_exported.finalized_intent.result_ocomp_binding_hash,
        ordered_members: second_members,
    };
    let authority = Arc::new(MultiAuthority {
        values: BTreeMap::from([
            (first_exported.job_id, first_exported.clone()),
            (second_exported.job_id, second_exported.clone()),
        ]),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let owner_uid = fs::metadata(directory.path())
        .expect("temporary directory metadata")
        .uid();
    let gate = OcompAttestationGate::new(
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        Arc::new(MultiHistoricalSnapshotSource {
            snapshots: vec![first_snapshot, second_snapshot],
        }),
        OcompAttestationConfig {
            identity: EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            fork_id: committee.fork_id,
            validator_address: local_address,
        },
        OcompSigner::from_secret([1; 32]).expect("local OCOMP signer"),
        SignOnceStore::open(directory.path().join("sign-once"), owner_uid, limits)
            .expect("sign-once store"),
        limits,
    )
    .expect("dynamic attestation gate");

    assert_eq!(
        gate.attest(first_result)
            .expect("first membership vote")
            .validator_index,
        0
    );
    assert_eq!(
        gate.attest(second_result)
            .expect("second membership vote")
            .validator_index,
        4
    );
}

#[test]
fn ocm_sig_001_unusable_historical_membership_abstains_observably_without_signing() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let local_address = Address::repeat_byte(1);
    let captured_metrics = CapturedMetrics::default();
    let _metrics_guard = metrics::set_default_local_recorder(&captured_metrics);
    let captured_events = CapturedEvents::default();
    let dispatch = tracing::Dispatch::new(captured_events.clone());
    let _tracing_guard = tracing::dispatcher::set_default(&dispatch);
    let make_gate = |directory: &tempfile::TempDir, snapshot: Option<HistoricalOcompSnapshotV1>| {
        let owner_uid = fs::metadata(directory.path())
            .expect("temporary directory metadata")
            .uid();
        OcompAttestationGate::new(
            Arc::new(StaticAuthority {
                value: Mutex::new(exported.clone()),
                reloads: AtomicUsize::new(0),
            }),
            Arc::new(AtomicHeightSource::new(105)),
            Arc::new(StaticHistoricalSnapshotSource { snapshot }),
            OcompAttestationConfig {
                identity: EndpointIdentity {
                    chain_id: 42,
                    genesis_hash: hash(40),
                    boot_nonce: hash(99),
                    protocol_bundle_hash: hash(41),
                },
                fork_id: committee.fork_id,
                validator_address: local_address,
            },
            OcompSigner::from_secret([1; 32]).expect("local OCOMP signer"),
            SignOnceStore::open(directory.path().join("sign-once"), owner_uid, limits)
                .expect("sign-once store"),
            limits,
        )
        .expect("dynamic attestation gate")
    };

    let missing_directory = tempfile::tempdir().expect("missing snapshot directory");
    let missing = make_gate(&missing_directory, None);
    assert!(matches!(
        missing.attest(result.clone()),
        Err(AttestationError::HistoricalSnapshotUnavailable)
    ));
    assert_eq!(missing.abstention_count(), 1);
    assert_no_sign_once_record(&missing_directory);

    let members = committee.snapshot.ordered_members.clone();

    let binding_mismatch_directory = tempfile::tempdir().expect("binding mismatch directory");
    let binding_mismatch = make_gate(
        &binding_mismatch_directory,
        Some(HistoricalOcompSnapshotV1 {
            epoch: exported.finalized_intent.result_validator_set_epoch + 1,
            committee_set_hash: exported.finalized_intent.result_committee_set_hash,
            ocomp_binding_hash: exported.finalized_intent.result_ocomp_binding_hash,
            ordered_members: members.clone(),
        }),
    );
    assert!(matches!(
        binding_mismatch.attest(result.clone()),
        Err(AttestationError::HistoricalSnapshotUnavailable)
    ));
    assert_eq!(binding_mismatch.abstention_count(), 1);
    assert_no_sign_once_record(&binding_mismatch_directory);

    let mut absent_members = members.clone();
    absent_members[0].validator_address = Address::repeat_byte(0xEE);
    let absent_directory = tempfile::tempdir().expect("local member absent directory");
    let absent = make_gate(
        &absent_directory,
        Some(HistoricalOcompSnapshotV1 {
            epoch: exported.finalized_intent.result_validator_set_epoch,
            committee_set_hash: exported.finalized_intent.result_committee_set_hash,
            ocomp_binding_hash: exported.finalized_intent.result_ocomp_binding_hash,
            ordered_members: absent_members,
        }),
    );
    assert!(matches!(
        absent.attest(result.clone()),
        Err(AttestationError::LocalMemberAbsent)
    ));
    assert_eq!(absent.abstention_count(), 1);
    assert_no_sign_once_record(&absent_directory);

    let mut members = members;
    members[0].ocomp_public_key_sec1 = members[1].ocomp_public_key_sec1;
    let mismatch_directory = tempfile::tempdir().expect("key mismatch directory");
    let mismatch = make_gate(
        &mismatch_directory,
        Some(HistoricalOcompSnapshotV1 {
            epoch: exported.finalized_intent.result_validator_set_epoch,
            committee_set_hash: exported.finalized_intent.result_committee_set_hash,
            ocomp_binding_hash: exported.finalized_intent.result_ocomp_binding_hash,
            ordered_members: members,
        }),
    );
    assert!(matches!(
        mismatch.attest(result),
        Err(AttestationError::LocalKeyMismatch)
    ));
    assert_eq!(mismatch.abstention_count(), 1);
    assert_no_sign_once_record(&mismatch_directory);

    assert_eq!(captured_metrics.value.load(Ordering::Relaxed), 4);
    let metric_keys = captured_metrics
        .keys
        .lock()
        .expect("captured metrics mutex")
        .join("\n");
    for reason in [
        "missing_snapshot",
        "local_member_absent",
        "local_key_mismatch",
    ] {
        assert!(
            metric_keys.contains("outbe_ocomp_attestation_abstentions_total")
                && metric_keys.contains(reason),
            "missing metric reason {reason}: {metric_keys}"
        );
    }
    assert_eq!(metric_keys.matches("reason = missing_snapshot").count(), 2);
    let events = captured_events.fields.lock().expect("captured event mutex");
    let abstentions = events
        .iter()
        .filter(|fields| fields.contains_key("reason"))
        .collect::<Vec<_>>();
    assert_eq!(abstentions.len(), 4, "captured fields: {events:?}");
    for fields in abstentions {
        for required in [
            "job_id",
            "result_validator_set_epoch",
            "result_committee_set_hash",
            "result_ocomp_binding_hash",
            "reason",
        ] {
            assert!(
                fields.contains_key(required),
                "missing log field {required}: {fields:?}"
            );
        }
    }
}

#[test]
fn ocm_sig_001_node_reconstructs_digest_replays_exactly_and_refuses_second_result() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let height = Arc::new(AtomicHeightSource::new(105));
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority.clone(),
        height.clone(),
        committee.clone(),
    );
    let canonical = result.encode_canonical(&limits).expect("canonical result");

    let first = gate
        .attest_canonical_result(&canonical)
        .expect("first attestation");
    assert_historical_vote(&first, &exported, &committee, 105, &limits);
    let replay = gate
        .attest_canonical_result(&canonical)
        .expect("exact replay");
    assert_eq!(replay, first);
    assert_eq!(authority.reloads.load(Ordering::SeqCst), 4);

    let mut conflicting = result.clone();
    conflicting.roots.nod_root = hash(62);
    recompute_arithmetic(&mut conflicting, &limits);
    assert!(matches!(
        gate.attest(conflicting.clone()),
        Err(AttestationError::SignOnce(_))
    ));

    drop(gate);
    let restarted = attestation_gate(&directory, authority, height, committee);
    assert!(matches!(
        restarted.attest(conflicting),
        Err(AttestationError::SignOnce(_))
    ));
}

#[test]
fn ocm_sig_001_real_control_socket_attests_and_returns_typed_conflict_without_dropping_session() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        committee.clone(),
    );
    let server = control_server(&directory, gate);
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));

    let identity = EndpointIdentity {
        chain_id: 42,
        genesis_hash: hash(40),
        boot_nonce: hash(99),
        protocol_bundle_hash: hash(41),
    };
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(effective_uid().expect("effective uid"), identity, limits),
    )
    .expect("connect supervisor control session");
    client.handshake().expect("control handshake");
    let canonical = result.encode_canonical(&limits).expect("canonical result");
    let request = RequestAttestationV1 {
        canonical_result: BoundedBytes(canonical.clone()),
    };
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            request.encode_body(&limits).expect("attestation request"),
        )
        .expect("send first attestation request");
    let first_frame = client.receive_response().expect("first response");
    assert_eq!(first_frame.message_kind, NodeMessageKind::Response as u16);
    let first = AttestationResponseV1::decode_body(&first_frame.body, &limits)
        .expect("attestation response");
    let vote = ResultVoteV1::decode_canonical(&first.canonical_vote.0, &limits)
        .expect("canonical result vote");
    assert_historical_vote(&vote, &exported, &committee, 105, &limits);
    local_result_store(&directory)
        .verify_exact(result.job_id, &result)
        .expect("attestation persists the exact local result before signing");

    let mut conflicting = result;
    conflicting.roots.nod_root = hash(62);
    recompute_arithmetic(&mut conflicting, &limits);
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            RequestAttestationV1 {
                canonical_result: BoundedBytes(
                    conflicting
                        .encode_canonical(&limits)
                        .expect("conflicting result"),
                ),
            }
            .encode_body(&limits)
            .expect("conflicting request"),
        )
        .expect("send conflicting request");
    let conflict_frame = client.receive_response().expect("conflict response");
    assert_eq!(conflict_frame.message_kind, NodeMessageKind::Error as u16);
    let conflict =
        LocalErrorV1::decode_body(&conflict_frame.body, &limits).expect("typed local error");
    assert_eq!(
        conflict.rejected_kind,
        NodeMessageKind::RequestAttestation as u16
    );
    assert_eq!(conflict.error_code, LocalErrorCode::Conflict as u16);
    assert!(!conflict.retryable);

    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            request.encode_body(&limits).expect("replay request"),
        )
        .expect("send exact replay");
    let replay_frame = client.receive_response().expect("replay response");
    assert_eq!(replay_frame.message_kind, NodeMessageKind::Response as u16);
    let replay = AttestationResponseV1::decode_body(&replay_frame.body, &limits)
        .expect("replayed attestation");
    assert_eq!(replay, first);

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn keyless_control_commits_exact_local_result_and_rejects_conflict_without_voting() {
    let limits = protocol_limits();
    let (_, _, result) = fixture();
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = local_result_store(&directory);
    let server = Arc::new(base_control_server(&directory).with_local_result_store(store.clone()));
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(
            effective_uid().expect("effective uid"),
            EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            limits,
        ),
    )
    .expect("connect keyless follower control session");
    client.handshake().expect("control handshake");
    let canonical = result.encode_canonical(&limits).expect("canonical result");
    let request = CommitLocalResultV1 {
        canonical_result: BoundedBytes(canonical),
    };

    for _ in 0..2 {
        client
            .send_request(
                NodeMessageKind::CommitLocalResult as u16,
                request.encode_body(&limits).expect("commit request"),
            )
            .expect("send local result commit");
        let frame = client.receive_response().expect("commit response");
        assert_eq!(frame.message_kind, NodeMessageKind::Response as u16);
        let committed = LocalResultCommittedV1::decode_body(&frame.body, &limits)
            .expect("typed committed response");
        assert_eq!(committed.job_id, result.job_id);
        assert_eq!(
            committed.result_digest,
            result.result_digest(&limits).expect("result digest")
        );
    }
    store
        .verify_exact(result.job_id, &result)
        .expect("keyless follower result is durable");

    let mut conflicting = result.clone();
    conflicting.roots.nod_root = hash(62);
    recompute_arithmetic(&mut conflicting, &limits);
    client
        .send_request(
            NodeMessageKind::CommitLocalResult as u16,
            CommitLocalResultV1 {
                canonical_result: BoundedBytes(
                    conflicting
                        .encode_canonical(&limits)
                        .expect("conflicting canonical result"),
                ),
            }
            .encode_body(&limits)
            .expect("conflicting commit request"),
        )
        .expect("send conflicting local result");
    let conflict_frame = client.receive_response().expect("conflict response");
    assert_eq!(conflict_frame.message_kind, NodeMessageKind::Error as u16);
    let conflict =
        LocalErrorV1::decode_body(&conflict_frame.body, &limits).expect("typed local error");
    assert_eq!(
        conflict.rejected_kind,
        NodeMessageKind::CommitLocalResult as u16
    );
    assert_eq!(conflict.error_code, LocalErrorCode::Conflict as u16);
    assert!(!conflict.retryable);

    client
        .send_request(
            NodeMessageKind::ListFinalizedJobs as u16,
            ListFinalizedJobsV1 {
                after_cursor: 0,
                limit: MAX_FINALIZED_JOBS_PER_RESPONSE,
            }
            .encode_body(&limits)
            .expect("list request after conflict"),
        )
        .expect("control session remains usable after conflict");
    assert_eq!(
        client
            .receive_response()
            .expect("list response after conflict")
            .message_kind,
        NodeMessageKind::Response as u16
    );

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn ocm_sig_001_unavailable_signing_disables_only_attestation_on_the_node_control_session() {
    let limits = protocol_limits();
    let (_, _, result) = fixture();
    let directory = tempfile::tempdir().expect("temporary directory");
    let server = Arc::new(base_control_server(&directory));
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(
            effective_uid().expect("effective uid"),
            EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            limits,
        ),
    )
    .expect("connect supervisor control session");
    client.handshake().expect("control handshake");
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            RequestAttestationV1 {
                canonical_result: BoundedBytes(
                    result.encode_canonical(&limits).expect("canonical result"),
                ),
            }
            .encode_body(&limits)
            .expect("attestation request"),
        )
        .expect("send unavailable attestation request");
    let unavailable_frame = client.receive_response().expect("unavailable response");
    assert_eq!(
        unavailable_frame.message_kind,
        NodeMessageKind::Error as u16
    );
    let unavailable = LocalErrorV1::decode_body(&unavailable_frame.body, &limits)
        .expect("typed unavailable error");
    assert_eq!(
        unavailable.error_code,
        LocalErrorCode::InternalOcompUnavailable as u16
    );
    assert!(unavailable.retryable);

    client
        .send_request(
            NodeMessageKind::ListFinalizedJobs as u16,
            ListFinalizedJobsV1 {
                after_cursor: 0,
                limit: MAX_FINALIZED_JOBS_PER_RESPONSE,
            }
            .encode_body(&limits)
            .expect("list request"),
        )
        .expect("send unaffected control request");
    let list_frame = client.receive_response().expect("list response");
    assert_eq!(list_frame.message_kind, NodeMessageKind::Response as u16);
    let listing = ListFinalizedJobsResponseV1::decode_body(&list_frame.body, &limits)
        .expect("list finalized jobs response");
    assert!(listing.jobs.is_empty());

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn ocm_sig_001_node_refuses_wrong_export_deadline_bundle_committee_and_arithmetic() {
    let (committee, exported, result) = fixture();

    let wrong_export = Arc::new(StaticAuthority {
        value: Mutex::new(ExportedAttestationAuthorityV1 {
            manifest_hash: hash(200),
            ..exported.clone()
        }),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            wrong_export,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("exported result authority"))
    ));
    assert_no_sign_once_record(&directory);

    let stale_authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            stale_authority,
            Arc::new(AtomicHeightSource::new(110)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::DeadlineReached { .. })
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_bundle = result.clone();
    wrong_bundle.protocol_bundle_hash = hash(201);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(wrong_bundle),
        Err(AttestationError::Binding("exported result authority"))
    ));
    assert_no_sign_once_record(&directory);

    let mut wrong_committee_export = exported.clone();
    wrong_committee_export
        .finalized_intent
        .result_ocomp_binding_hash = hash(202);
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_committee_export),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent pin"))
    ));
    assert_no_sign_once_record(&directory);

    let mut wrong_open_height = exported.clone();
    wrong_open_height.open_height = wrong_open_height
        .finality_recorded_height
        .checked_add(3)
        .expect("fixture height");
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_open_height),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent pin"))
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_arithmetic = result;
    wrong_arithmetic.arithmetic_commitment = hash(203);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee,
        )
        .attest(wrong_arithmetic),
        Err(AttestationError::Protocol(ProtocolError::InvalidInvariant(
            "arithmetic commitment"
        )))
    ));
    assert_no_sign_once_record(&directory);
}

#[test]
fn ocm_sig_001_node_refuses_wrong_intent_result_and_changed_authority_before_signing() {
    let (committee, exported, result) = fixture();

    let mut wrong_pin = exported.clone();
    wrong_pin.candidate.intent_id = hash(210);
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_pin),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent pin"))
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_result = result.clone();
    wrong_result.attempt = wrong_result.attempt.saturating_add(1);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(wrong_result),
        Err(AttestationError::Protocol(ProtocolError::InvalidInvariant(
            "result finalized intent binding"
        )))
    ));
    assert_no_sign_once_record(&directory);

    let mut changed = exported.clone();
    changed.manifest_hash = hash(211);
    let authority = Arc::new(ChangingAuthority {
        first: exported,
        second: changed,
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee,
        )
        .attest(result),
        Err(AttestationError::AuthorityChanged)
    ));
    assert_no_sign_once_record(&directory);
}
