//! Rust-only fixture for cross-crate OCOMP activation tests.
//!
//! This module is available only through the explicit `test-utils` feature.
//! It builds consensus-valid activation bytes and seeds the real Metadosis and
//! owner storage APIs; it does not provide a production activation shortcut.

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::{Address, Bytes, Log, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, partition_collection_key, AuthenticatedParentTree, CeWorkCheckpoint, CeWorkConfig,
    EntityRef, ExecutionScope, FinalLeafMutation, PartitionRef, ProvisionalTreeBatch,
};
use outbe_ocomp_protocol::{
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, ProofBytes},
    hash::hash_framed,
    intent::{
        intent_storage_key, ActivationPreconditionsV1, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, ExpectedFinalizedIntentBindingV1,
        FinalizedIntentProofV1, FinalizedIntentVerificationError, FinalizedRequestBindingV1,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, ParentProofKind, TributeInputBindingV1,
        VerifiedFinalizedIntentV1,
    },
    profile::{CapacityProfileV1, ProtocolBundleV1},
    receipts::{
        desis_request_brief_hash, ActivationOutcome, BudgetSplitDestination,
        RequestBudgetSplitReceiptV1,
    },
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    state::{OcompJobStatus, RESULT_VOTE_MIN_FINALITY_DEPTH},
    vote::ResultVoteV1,
    SchemaLimits,
};
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, METADOSIS_ADDRESS},
    error::{PrecompileError, Result as PrecompileResult},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tribute::{DayPreAdmission, DayTotals, TributeContract};

use crate::{
    ocomp::{
        activation::{OcompFinalityAuthorityError, OcompFinalizedIntentAuthority},
        fork::{OcompForkInstallClassification, OcompForkInstallV1},
        schema::{poc_schema_limits, OcompRequestProfile},
        state::JobFsmLimits,
        vote::dispatch_public_result_vote,
    },
    schema::{day_type, status, MetadosisContract, WorldwideDay as WorldwideDayRecord},
};
use outbe_lysis::activation_v1::LysisOwnerReceiptsV1;

pub const TEST_WWD: WorldwideDay = WorldwideDay::new(20_260_723);
pub const TEST_REQUEST_HEIGHT: u64 = 10;
pub const TEST_LOGICAL_TIME: u64 = 1_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationReceiptFault {
    Nod,
    Contributor,
    Tribute,
    CarryOver,
    RequestSplit,
}

thread_local! {
    static ACTIVATION_RECEIPT_FAULT: Cell<Option<ActivationReceiptFault>> =
        const { Cell::new(None) };
}

pub(crate) fn inject_receipt_fault(
    request_receipt: &mut RequestBudgetSplitReceiptV1,
    receipts: &mut LysisOwnerReceiptsV1,
) {
    let fault = ACTIVATION_RECEIPT_FAULT.with(Cell::take);
    match fault {
        Some(ActivationReceiptFault::Nod) => {
            receipts.nod.nod_root = hash(210);
        }
        Some(ActivationReceiptFault::Contributor) => {
            receipts.contributor.contributor_count += 1;
        }
        Some(ActivationReceiptFault::Tribute) => {
            receipts.tribute.retired_generation += 1;
        }
        Some(ActivationReceiptFault::CarryOver) => {
            receipts.carry_over.credited_unused_lysis += U256::from(1);
            receipts.carry_over.after_value += U256::from(1);
        }
        Some(ActivationReceiptFault::RequestSplit) => {
            request_receipt.logical_anchor += 1;
        }
        None => {}
    }
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn tribute_collection_key() -> B256 {
    let (_, key) = partition_collection_key(PartitionRef::TributeWwd(TEST_WWD)).unwrap();
    B256::from(*key.as_bytes())
}

fn capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: hash(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_pending_jobs: 2,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        retry_backoff_blocks: 1,
        max_terminal_job_records: 365,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: 64,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: hash(23),
    }
}

fn bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(21),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: outbe_ocomp_protocol::registry::TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: outbe_ocomp_protocol::registry::FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: outbe_ocomp_protocol::registry::ORACLE_OPENING_CODEC_ID,
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(22),
        release_approval_policy_hash: hash(24),
        release_validator_command_artifact_hash: hash(25),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(26),
        required_upgrade_handler_set_hash: hash(27),
    }
}

fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index + 1; 32]).into()).unwrap()
}

fn sign(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key.sign_prehash(digest.as_slice()).unwrap();
    signature.to_bytes().into()
}

fn committee(
    chain_id: u64,
    genesis_hash: B256,
    bundle_hash: B256,
    limits: &SchemaLimits,
) -> OcompCommitteeSnapshotV1 {
    let registrations = (0..4)
        .map(|index| {
            let key = signing_key(index);
            let mut registration = OcompKeyRegistrationV1 {
                core: OcompKeyRegistrationCoreV1 {
                    chain_id,
                    genesis_hash,
                    fork_id: hash(21),
                    protocol_bundle_hash: bundle_hash,
                    validator_index: index,
                    validator_identity_hash: hash(70 + index),
                    ocomp_public_key_sec1: key
                        .verifying_key()
                        .to_encoded_point(true)
                        .as_bytes()
                        .try_into()
                        .unwrap(),
                    key_epoch: 1,
                    allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
                    valid_from_height: 1,
                    valid_until_height_exclusive: 1_000,
                },
                proof_of_possession: [0; 64],
            };
            let digest = registration.proof_of_possession_digest(limits).unwrap();
            registration.proof_of_possession = sign(&key, digest);
            registration
        })
        .collect::<Vec<_>>();
    OcompCommitteeSnapshotV1 {
        chain_id,
        genesis_hash,
        fork_id: hash(21),
        protocol_bundle_hash: bundle_hash,
        snapshot_epoch: 1,
        threshold: 3,
        ordered_members: registrations
            .into_iter()
            .map(|registration| OcompMemberV1 {
                validator_index: registration.core.validator_index,
                validator_identity_hash: registration.core.validator_identity_hash,
                ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
                key_epoch: registration.core.key_epoch,
                allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
                valid_from_height: registration.core.valid_from_height,
                valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
                proof_of_possession: registration.proof_of_possession,
            })
            .collect(),
    }
}

/// Builds a fully valid immutable fork-install artifact for behavioral tests.
///
/// This creates only canonical manifest bytes. It does not seed chain state or
/// bypass the production lifecycle.
pub fn fork_install_fixture(
    classification: OcompForkInstallClassification,
    activation_height: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> OcompForkInstallV1 {
    let limits = poc_schema_limits();
    let protocol_bundle = bundle();
    let bundle_hash = protocol_bundle.protocol_bundle_hash(&limits).unwrap();
    let result_committee = committee(chain_id, genesis_hash, bundle_hash, &limits);
    let committee_hash = result_committee.snapshot_hash(&limits).unwrap();
    OcompForkInstallV1 {
        classification,
        activation_height,
        request_profile: OcompRequestProfile {
            chain_id,
            genesis_hash,
            fork_id: hash(21),
            protocol_bundle_hash: bundle_hash,
            correctness_profile_id: hash(12),
            capacity_profile: capacity_profile(),
            source_availability_policy_id: hash(44),
            result_committee_snapshot_hash: committee_hash,
        },
        protocol_bundle,
        result_committee,
    }
}

fn request_receipt(bundle_hash: B256) -> RequestBudgetSplitReceiptV1 {
    RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash: bundle_hash,
        wwd: TEST_WWD.value(),
        pending_nonce: 0,
        day_type: DayType::Green,
        day_limit: U256::from(100),
        lysis_budget: U256::from(60),
        auction_base: U256::from(40),
        destination: BudgetSplitDestination::DesisAuction,
        desis_brief_hash: Some(
            desis_request_brief_hash(
                bundle_hash,
                TEST_WWD.value(),
                U256::from(40),
                U256::from(9),
                TEST_LOGICAL_TIME,
            )
            .unwrap(),
        ),
        carry_over_credit: U256::ZERO,
        auction_entry_price: U256::from(9),
        logical_anchor: TEST_LOGICAL_TIME,
    }
}

fn intent(bundle_hash: B256, committee_hash: B256, request_receipt_hash: B256) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 1,
        genesis_hash: hash(17),
        fork_id: hash(21),
        wwd: TEST_WWD.value(),
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: bundle_hash,
        ce_sealed_root: hash(42),
        sealed_tribute_collection_key: tribute_collection_key(),
        sealed_tribute_collection_root: hash(31),
        authenticated_day_count: 2,
        authenticated_day_nominal: U256::from(1_000),
        pre_admission_envelope_hash: hash(43),
        source_availability_policy_id: hash(44),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::from(100),
            previous_vwap: U256::from(8),
            current_vwap: U256::from(10),
            gratis_demand: U256::from(60),
            gratis_supply: U256::from(60),
            lysis_budget: U256::from(60),
            auction_base: U256::from(40),
            auction_entry_price: U256::from(9),
            request_budget_split_receipt_hash: request_receipt_hash,
        },
        logical_evaluation_height: TEST_REQUEST_HEIGHT,
        logical_evaluation_time: TEST_LOGICAL_TIME,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: TEST_WWD.value(),
                source_generation: 0,
                collection_key: tribute_collection_key(),
                sealed_collection_root: hash(31),
                exact_count: 2,
                exact_nominal_total: U256::from(1_000),
            },
            nod: NodTargetPreconditionV1 {
                wwd: TEST_WWD.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 2,
            },
            contributors: ContributorTargetPreconditionV1 {
                series_id: TEST_WWD.value(),
                expected_series_version: 0,
                max_contributor_count: 2,
                max_eligible_nominal_total: U256::from(1_000),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: TEST_WWD.value(),
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_committee_snapshot_hash: committee_hash,
        custody_committee_epoch_hash: None,
    }
}

fn result(bundle_hash: B256, job_id: B256, limits: &SchemaLimits) -> LysisResultV1 {
    let roots = ResultRootsV1 {
        nod_root: hash(50),
        bucket_root: hash(51),
        contributor_root: hash(52),
        output_manifest_root: hash(53),
    };
    let counts = ExactCountsV1 {
        tribute_count: 2,
        nod_count: 2,
        bucket_count: 1,
        contributor_count: 1,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::from(1_000),
        eligible_nominal_total: U256::from(600),
        day_limit: U256::from(100),
        gratis_demand: U256::from(60),
        gratis_supply: U256::from(60),
        lysis_budget: U256::from(60),
        auction_base: U256::from(40),
        nod_gratis_consumed: U256::from(45),
        unused_lysis: U256::from(15),
        carry_over_credit: U256::from(15),
        nod_cost_total: U256::from(300),
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: hash(54),
        plan_hash: hash(55),
        unit_artifact_root: hash(56),
        fidelity_fraction_root: hash(57),
        gratis_prefix_root: hash(58),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    LysisResultV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 2,
        result_chunk_list_root: hash(61),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: TEST_WWD.value(),
            reason: CarryOverReason::UnusedLysis,
            amount: U256::from(15),
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: TEST_WWD.value(),
            pending_nonce: 0,
            day_type: DayType::Green,
            tribute_nominal_total: U256::from(1_000),
            day_limit: U256::from(100),
            gratis_demand: U256::from(60),
            gratis_supply: U256::from(60),
            lysis_budget: U256::from(60),
            auction_base: U256::from(40),
            nod_gratis_consumed: U256::from(45),
            unused_lysis: U256::from(15),
            carry_over_credit: U256::from(15),
            status: CompletionStatus::Completed,
            logical_evaluation_height: TEST_REQUEST_HEIGHT,
            logical_evaluation_time: TEST_LOGICAL_TIME,
        },
        tribute_count: 2,
        tribute_nominal_total: U256::from(1_000),
        unused_lysis: U256::from(15),
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    }
}

fn finality_proof(intent: &JobIntentV1, limits: &SchemaLimits) -> FinalizedIntentProofV1 {
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(vec![1, 2]),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: 9,
            finalized_block_hash: hash(46),
            finalized_epoch: 2,
            finalized_view: 3,
            parent_view: 2,
            ordered_committee: vec![BoundedBytes(vec![1])],
            signer_bitmap: BoundedBytes(vec![1]),
            canonical_commonware_finalization_proof: ProofBytes(vec![2]),
            committee_set_hash: hash(47),
            vrf_material_version: 1,
            vrf_group_public_key_hash: hash(48),
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(vec![3]),
        canonical_job_intent: BoundedBytes(intent.encode_canonical(limits).unwrap()),
        intent_account_proof: ProofBytes(vec![4]),
        intent_storage_proof: ProofBytes(vec![5]),
    }
}

#[derive(Clone)]
pub struct FixedFinality {
    expected: ExpectedFinalizedIntentBindingV1,
    verified: VerifiedFinalizedIntentV1,
    calls: Arc<AtomicUsize>,
}

impl FixedFinality {
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl OcompFinalizedIntentAuthority for FixedFinality {
    fn verify(
        &self,
        _proof: &FinalizedIntentProofV1,
        expected: ExpectedFinalizedIntentBindingV1,
        _limits: &SchemaLimits,
    ) -> Result<VerifiedFinalizedIntentV1, OcompFinalityAuthorityError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if expected != self.expected {
            return Err(FinalizedIntentVerificationError::WrongProtocolBundle.into());
        }
        Ok(self.verified.clone())
    }
}

#[derive(Debug)]
struct TributePartitionTree {
    parent_root: B256,
}

impl AuthenticatedParentTree for TributePartitionTree {
    fn parent_block_hash(&self) -> B256 {
        hash(90)
    }

    fn parent_root(&self) -> B256 {
        self.parent_root
    }

    fn read_leaf_verified(
        &self,
        _entity: EntityRef,
        _expected_parent_root: B256,
    ) -> PrecompileResult<Option<outbe_compressed_entities::Commitment>> {
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> PrecompileResult<bool> {
        if expected_parent_root != self.parent_root
            || partition != PartitionRef::TributeWwd(TEST_WWD)
        {
            return Err(PrecompileError::Fatal(
                "activation fixture parent partition binding mismatch".into(),
            ));
        }
        Ok(true)
    }

    fn prepare_seal(
        &self,
        _block_number: u64,
        _mutations: &[FinalLeafMutation],
        _retirements: &[PartitionRef],
    ) -> PrecompileResult<ProvisionalTreeBatch> {
        Err(PrecompileError::Fatal(
            "activation fixture does not seal its synthetic parent".into(),
        ))
    }
}

fn begin_activation_scope(provider: &mut HashMapStorageProvider) -> ExecutionScope {
    let parent_root = hash(80);
    let scope = ExecutionScope::with_parent_tree(
        Arc::new(TributePartitionTree { parent_root }),
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    StorageHandle::enter(provider, |storage| {
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(parent_root.as_slice()),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    scope
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackSnapshot {
    storage: HashMap<(Address, U256), U256>,
    events: Vec<Log>,
    ce_work: CeWorkCheckpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSnapshot {
    pub job_status: OcompJobStatus,
    pub worldwide_day_status: u8,
    pub active_job_id: B256,
    pub active_nod_root: B256,
    pub nod: outbe_nod::schema::NodCertifiedGenerationProjection,
    pub contributor: outbe_intex::schema::CertifiedContributorGenerationProjection,
    pub tribute_generation: u64,
    pub tribute_count: u32,
    pub tribute_nominal: U256,
    pub tribute_total_supply: u64,
    pub carry_over: U256,
    pub owner_events: Vec<Log>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationMetadata {
    pub activated_at_height: u64,
    pub activated_at_time: u64,
}

pub struct ActivationFixture {
    pub provider: HashMapStorageProvider,
    pub scope: ExecutionScope,
    pub result: LysisResultV1,
    pub finality: FixedFinality,
    pub intent_id: B256,
    pub limits: SchemaLimits,
    pub request_receipt: RequestBudgetSplitReceiptV1,
}

impl ActivationFixture {
    #[must_use]
    pub fn new(current_height: u64, current_time: u64, seed_targets: bool) -> Self {
        Self::new_with_initial_votes(current_height, current_time, seed_targets, 2)
    }

    /// Builds the same production-valid fixture before any validator vote is
    /// recorded, so public result-vote dispatch tests can exercise all four
    /// consensus slots instead of injecting quorum state.
    #[must_use]
    pub fn new_voting(current_height: u64, current_time: u64, seed_targets: bool) -> Self {
        Self::new_with_initial_votes(current_height, current_time, seed_targets, 0)
    }

    fn new_with_initial_votes(
        current_height: u64,
        current_time: u64,
        seed_targets: bool,
        initial_vote_count: u8,
    ) -> Self {
        assert!(initial_vote_count <= 3);
        let limits = poc_schema_limits();
        let bundle = bundle();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let committee = committee(1, hash(17), bundle_hash, &limits);
        let committee_hash = committee.snapshot_hash(&limits).unwrap();
        let request_receipt = request_receipt(bundle_hash);
        let request_receipt_hash = request_receipt.receipt_hash(&limits).unwrap();
        let intent = intent(bundle_hash, committee_hash, request_receipt_hash);
        let intent_id = intent.intent_id(&limits).unwrap();
        let proof = finality_proof(&intent, &limits);
        let request_state_root = hash(98);
        let job_id = intent
            .job_id(
                proof.parent_accounting.finalized_block_hash,
                request_state_root,
                &limits,
            )
            .unwrap();
        let result = result(bundle_hash, job_id, &limits);
        let expected = ExpectedFinalizedIntentBindingV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            fork_id: intent.fork_id,
            protocol_bundle_hash: intent.protocol_bundle_hash,
        };
        let finality = FixedFinality {
            expected,
            verified: VerifiedFinalizedIntentV1 {
                intent: intent.clone(),
                intent_id,
                intent_storage_key: intent_storage_key(intent_id).unwrap(),
                job_id,
                request: FinalizedRequestBindingV1 {
                    block_number: 9,
                    block_hash: hash(46),
                    state_root: request_state_root,
                },
            },
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(current_height);
        provider.set_timestamp(U256::from(current_time));
        let scope = begin_activation_scope(&mut provider);
        StorageHandle::enter(&mut provider, |storage| {
            if seed_targets {
                let tribute = TributeContract::new(storage.clone());
                tribute.ocomp_profile_ready.write(true).unwrap();
                tribute.total_supply.write(2).unwrap();
                let mut totals = DayTotals::with_key(TEST_WWD);
                totals.initialized = true;
                totals.is_sealed = true;
                totals.tribute_count = 2;
                totals.tribute_nominal_amount = U256::from(1_000);
                tribute.day_totals.create(&totals).unwrap();
                let mut admission = DayPreAdmission::with_key(TEST_WWD);
                admission.initialized = true;
                admission.is_sealed = true;
                admission.sealed_collection_root = hash(31);
                admission.sealed_tribute_count = 2;
                admission.sealed_tribute_nominal_amount = U256::from(1_000);
                admission.source_generation = 0;
                tribute.day_pre_admission.create(&admission).unwrap();
            }

            let mut contract = MetadosisContract::new(storage);
            if seed_targets {
                contract.initialize_ocomp_pre_admission(TEST_WWD).unwrap();
            }
            contract
                .worldwide_days
                .create(&WorldwideDayRecord {
                    wwd: TEST_WWD,
                    status: status::READY,
                    day_type: day_type::GREEN,
                    forming_start: 1,
                    forming_end: 2,
                    lookback_end: 3,
                    offering_end: 4,
                    scheduled_process_time: 5,
                    metadosis_limit_amount: U256::from(100),
                    previous_vwap: U256::from(8),
                    current_vwap: U256::from(10),
                })
                .unwrap();
            let fsm_limits = JobFsmLimits {
                max_terminal_records: 365,
            };
            contract
                .enqueue_ocomp_ready(TEST_WWD, TEST_REQUEST_HEIGHT, fsm_limits)
                .unwrap();
            let profile = OcompRequestProfile {
                chain_id: 1,
                genesis_hash: hash(17),
                fork_id: hash(21),
                protocol_bundle_hash: bundle_hash,
                correctness_profile_id: hash(12),
                capacity_profile: capacity_profile(),
                source_availability_policy_id: hash(44),
                result_committee_snapshot_hash: committee_hash,
            };
            contract
                .initialize_ocomp_request_profile(&profile, &limits)
                .unwrap();
            contract
                .initialize_ocomp_activation_authority(&bundle, &committee, &limits)
                .unwrap();
            contract
                .commit_ocomp_request(&intent, &request_receipt, &limits, fsm_limits)
                .unwrap();
            let finalized = contract
                .record_ocomp_finality(
                    intent_id,
                    hash(46),
                    request_state_root,
                    TEST_REQUEST_HEIGHT,
                    capacity_profile().result_deadline_blocks,
                    &limits,
                )
                .unwrap();
            assert_eq!(
                finalized.open_height,
                TEST_REQUEST_HEIGHT + RESULT_VOTE_MIN_FINALITY_DEPTH
            );
            contract
                .open_due_ocomp_voting(finalized.open_height, &limits, fsm_limits)
                .unwrap();
            for index in 0..initial_vote_count {
                let mut vote = ResultVoteV1 {
                    protocol_bundle_hash: bundle_hash,
                    job_id,
                    attempt: intent.attempt,
                    result_committee_snapshot_hash: committee_hash,
                    validator_index: index,
                    key_epoch: 1,
                    result: result.clone(),
                    signature_rs: [0; 64],
                };
                let signing_digest = vote.signing_digest(&intent, &limits).unwrap();
                vote.signature_rs = sign(&signing_key(index), signing_digest);
                contract
                    .record_ocomp_result_vote(
                        &vote,
                        finalized.open_height + u64::from(index),
                        &scope,
                        &limits,
                    )
                    .unwrap();
            }
        });
        provider.clear_events(METADOSIS_ADDRESS);

        Self {
            provider,
            scope,
            result,
            finality,
            intent_id,
            limits,
            request_receipt,
        }
    }

    /// Creates the canonical node-attested vote for one fixture committee
    /// member. The returned bytes still have to pass the production public
    /// dispatch and consensus transition.
    #[must_use]
    pub fn signed_result_vote(&self, validator_index: u8) -> ResultVoteV1 {
        let intent = &self.finality.verified.intent;
        let mut vote = ResultVoteV1 {
            protocol_bundle_hash: intent.protocol_bundle_hash,
            job_id: self.result.job_id,
            attempt: intent.attempt,
            result_committee_snapshot_hash: intent.result_committee_snapshot_hash,
            validator_index,
            key_epoch: 1,
            result: self.result.clone(),
            signature_rs: [0; 64],
        };
        vote.signature_rs = sign(
            &signing_key(validator_index),
            vote.signing_digest(intent, &self.limits).unwrap(),
        );
        vote
    }

    pub fn apply(&mut self) -> PrecompileResult<Bytes> {
        self.provider.enable_lysis_activation_frame();
        self.dispatch_current()
    }

    pub fn apply_with_receipt_fault(
        &mut self,
        fault: ActivationReceiptFault,
    ) -> PrecompileResult<Bytes> {
        ACTIVATION_RECEIPT_FAULT.with(|slot| {
            assert!(
                slot.replace(Some(fault)).is_none(),
                "nested activation receipt fault"
            );
        });
        let result = self.apply();
        ACTIVATION_RECEIPT_FAULT.with(|slot| slot.set(None));
        result
    }

    #[must_use]
    pub fn calldata(&self) -> Bytes {
        Bytes::from(
            outbe_ocomp_protocol::abi::encode_submit_lysis_result_calldata(
                &self.signed_result_vote(2),
                &self.limits,
            )
            .unwrap(),
        )
    }

    pub fn dispatch_current(&mut self) -> PrecompileResult<Bytes> {
        let calldata = self.calldata();
        StorageHandle::enter(&mut self.provider, |storage| {
            dispatch_public_result_vote(storage, &self.scope, calldata.as_ref(), U256::ZERO, false)
        })
    }

    pub fn terminal_outcome(&mut self) -> ActivationOutcome {
        StorageHandle::enter(&mut self.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap()
                .terminal
                .unwrap()
                .completed_binding
                .unwrap()
                .terminal_receipt
                .outcome
        })
    }

    pub fn replace_request_receipt(&mut self, receipt: &RequestBudgetSplitReceiptV1) {
        let encoded = receipt.encode_canonical(&self.limits).unwrap();
        StorageHandle::enter(&mut self.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_request_budget_receipts
                .get_bytes(&TEST_WWD)
                .write(&encoded)
                .unwrap();
        });
    }

    pub fn rollback_snapshot(&mut self) -> RollbackSnapshot {
        RollbackSnapshot {
            storage: self.provider.storage.clone(),
            events: self.provider.get_ordered_events().to_vec(),
            ce_work: self.scope.ce_work_checkpoint().unwrap(),
        }
    }

    pub fn semantic_snapshot(&mut self) -> SemanticSnapshot {
        let owner_events = self
            .provider
            .get_ordered_events()
            .iter()
            .filter(|event| event.address != METADOSIS_ADDRESS)
            .cloned()
            .collect();
        StorageHandle::enter(&mut self.provider, |storage| {
            let contract = MetadosisContract::new(storage.clone());
            let job = contract
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap();
            let active = contract
                .active_lysis_generation(TEST_WWD, &self.limits)
                .unwrap()
                .unwrap();
            let nod = outbe_nod::schema::NodContract::new(storage.clone())
                .ocomp_certified_generation(TEST_WWD)
                .unwrap()
                .unwrap();
            let contributor =
                outbe_intex::api::certified_contributor_generation(&storage, TEST_WWD.value())
                    .unwrap()
                    .unwrap();
            let tribute = TributeContract::new(storage.clone());
            let admission = tribute.pre_admission_projection(TEST_WWD).unwrap();
            let totals = tribute.get_day_totals(TEST_WWD).unwrap();
            SemanticSnapshot {
                job_status: job.status,
                worldwide_day_status: contract.get_wwd_status(TEST_WWD).unwrap(),
                active_job_id: active.job_id,
                active_nod_root: active.nod_root,
                nod,
                contributor,
                tribute_generation: admission.source_generation,
                tribute_count: totals.tribute_count,
                tribute_nominal: totals.tribute_nominal_amount,
                tribute_total_supply: tribute.total_supply.read().unwrap(),
                carry_over: outbe_promislimit::PromisLimitContract::new(storage)
                    .total_unallocated
                    .read()
                    .unwrap(),
                owner_events,
            }
        })
    }

    pub fn activation_metadata(&mut self) -> ActivationMetadata {
        StorageHandle::enter(&mut self.provider, |storage| {
            let job = MetadosisContract::new(storage)
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap();
            let receipt = &job
                .terminal
                .unwrap()
                .completed_binding
                .unwrap()
                .terminal_receipt;
            ActivationMetadata {
                activated_at_height: receipt.activated_at_height,
                activated_at_time: receipt.activated_at_time,
            }
        })
    }

    pub fn assert_pending(&mut self) {
        StorageHandle::enter(&mut self.provider, |storage| {
            let contract = MetadosisContract::new(storage.clone());
            let job = contract
                .ocomp_job_record(self.intent_id, &self.limits)
                .unwrap()
                .unwrap();
            assert_eq!(job.status, OcompJobStatus::VotingOpen);
            assert!(job.finalized.unwrap().quorum.is_none());
            assert_eq!(
                contract.get_wwd_status(TEST_WWD).unwrap(),
                status::OFFCHAIN_PENDING
            );
            assert!(contract
                .active_lysis_generation(TEST_WWD, &self.limits)
                .unwrap()
                .is_none());
            assert!(outbe_nod::schema::NodContract::new(storage.clone())
                .ocomp_certified_generation(TEST_WWD)
                .unwrap()
                .is_none());
            assert!(
                outbe_intex::api::certified_contributor_generation(&storage, TEST_WWD.value())
                    .unwrap()
                    .is_none()
            );
            let tribute = TributeContract::new(storage.clone());
            let admission = tribute.pre_admission_projection(TEST_WWD).unwrap();
            let totals = tribute.get_day_totals(TEST_WWD).unwrap();
            assert_eq!(admission.source_generation, 0);
            assert_eq!(totals.tribute_count, 2);
            assert_eq!(totals.tribute_nominal_amount, U256::from(1_000));
            assert_eq!(tribute.total_supply.read().unwrap(), 2);
            assert!(outbe_promislimit::PromisLimitContract::new(storage)
                .total_unallocated
                .read()
                .unwrap()
                .is_zero());
        });
    }
}
