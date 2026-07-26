use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolCall;
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, partition_collection_key, AuthenticatedParentTree, CeWorkConfig, EntityRef,
    ExecutionScope, FinalLeafMutation, PartitionRef, ProvisionalTreeBatch,
};
use outbe_ocomp_protocol::{
    activation::PoCActivationV1,
    certificate::{ExecutionCertificateV1, OrderedSignatureV1},
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
    state::OcompJobStatus,
    SchemaLimits,
};
use outbe_primitives::{
    addresses::{
        COMPRESSED_ENTITIES_ADDRESS, INTEX_ADDRESS, METADOSIS_ADDRESS, NOD_ADDRESS,
        PROMIS_LIMIT_ADDRESS, TRIBUTE_ADDRESS,
    },
    error::{PrecompileError, Result as PrecompileResult},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_tribute::{DayPreAdmission, DayTotals, TributeContract};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::{
    ocomp::{
        activation::{
            dispatch_public_activation, OcompFinalityAuthorityError, OcompFinalizedIntentAuthority,
        },
        schema::{poc_schema_limits, OcompRequestProfile},
        state::JobFsmLimits,
    },
    precompile::IMetadosis,
    schema::{day_type, status, MetadosisContract, WorldwideDay as WorldwideDayRecord},
};

const WWD: WorldwideDay = WorldwideDay::new(20_260_723);
const REQUEST_HEIGHT: u64 = 10;
const ACTIVATION_HEIGHT: u64 = 20;
const LOGICAL_TIME: u64 = 1_000;
const ACTIVATION_TIME: u64 = 1_010;

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn tribute_collection_key() -> B256 {
    let (_, key) = partition_collection_key(PartitionRef::TributeWwd(WWD)).unwrap();
    B256::from(*key.as_bytes())
}

fn capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: hash(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_pending_jobs: 1,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        retry_backoff_blocks: 1,
        max_terminal_job_records: 365,
        max_reference_currencies: 256,
        max_fidelity_cohorts_per_owner: 64,
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

fn committee(bundle_hash: B256, limits: &SchemaLimits) -> OcompCommitteeSnapshotV1 {
    let registrations = (0..4)
        .map(|index| {
            let key = signing_key(index);
            let mut registration = OcompKeyRegistrationV1 {
                core: OcompKeyRegistrationCoreV1 {
                    chain_id: 1,
                    genesis_hash: hash(17),
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
        chain_id: 1,
        genesis_hash: hash(17),
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

fn request_receipt(bundle_hash: B256) -> RequestBudgetSplitReceiptV1 {
    RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash: bundle_hash,
        wwd: WWD.value(),
        pending_nonce: 0,
        day_type: DayType::Green,
        day_limit: U256::from(100),
        lysis_budget: U256::from(60),
        auction_base: U256::from(40),
        destination: BudgetSplitDestination::DesisAuction,
        desis_brief_hash: Some(
            desis_request_brief_hash(
                bundle_hash,
                WWD.value(),
                U256::from(40),
                U256::from(9),
                LOGICAL_TIME,
            )
            .unwrap(),
        ),
        carry_over_credit: U256::ZERO,
        auction_entry_price: U256::from(9),
        logical_anchor: LOGICAL_TIME,
    }
}

fn intent(bundle_hash: B256, committee_hash: B256, request_receipt_hash: B256) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: 1,
        genesis_hash: hash(17),
        fork_id: hash(21),
        wwd: WWD.value(),
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
        logical_evaluation_height: REQUEST_HEIGHT,
        logical_evaluation_time: LOGICAL_TIME,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: WWD.value(),
                source_generation: 0,
                collection_key: tribute_collection_key(),
                sealed_collection_root: hash(31),
                exact_count: 2,
                exact_nominal_total: U256::from(1_000),
            },
            nod: NodTargetPreconditionV1 {
                wwd: WWD.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 2,
            },
            contributors: ContributorTargetPreconditionV1 {
                series_id: WWD.value(),
                expected_series_version: 0,
                max_contributor_count: 2,
                max_eligible_nominal_total: U256::from(1_000),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: WWD.value(),
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_committee_snapshot_hash: committee_hash,
        custody_committee_epoch_hash: None,
        deadline_height: 74,
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
            source_wwd: WWD.value(),
            reason: CarryOverReason::UnusedLysis,
            amount: U256::from(15),
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: WWD.value(),
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
            logical_evaluation_height: REQUEST_HEIGHT,
            logical_evaluation_time: LOGICAL_TIME,
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
struct FixedFinality {
    expected: ExpectedFinalizedIntentBindingV1,
    verified: VerifiedFinalizedIntentV1,
    calls: Arc<AtomicUsize>,
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

struct Fixture {
    activation: PoCActivationV1,
    finality: FixedFinality,
    intent_id: B256,
}

fn setup(provider: &mut HashMapStorageProvider, seed_targets: bool) -> Fixture {
    let limits = poc_schema_limits();
    let bundle = bundle();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let committee = committee(bundle_hash, &limits);
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
    let activation_payload = result.activation_payload(&limits).unwrap();
    let result_digest = activation_payload.result_digest(&limits).unwrap();
    let certificate = ExecutionCertificateV1 {
        result_committee_snapshot_hash: committee_hash,
        signer_bitmap: 0b0111,
        ordered_signatures: (0..3)
            .map(|index| OrderedSignatureV1 {
                validator_index: index,
                signature_rs: sign(&signing_key(index), result_digest),
            })
            .collect(),
        result_digest,
    };
    let activation = PoCActivationV1 {
        intent_id,
        finalized_intent_proof: proof,
        activation_payload,
        result,
        certificate,
    };
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

    StorageHandle::enter(provider, |storage| {
        if seed_targets {
            let tribute = TributeContract::new(storage.clone());
            tribute.ocomp_profile_ready.write(true).unwrap();
            tribute.total_supply.write(2).unwrap();
            let mut totals = DayTotals::with_key(WWD);
            totals.initialized = true;
            totals.is_sealed = true;
            totals.tribute_count = 2;
            totals.tribute_nominal_amount = U256::from(1_000);
            tribute.day_totals.create(&totals).unwrap();
            let mut admission = DayPreAdmission::with_key(WWD);
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
            contract.initialize_ocomp_pre_admission(WWD).unwrap();
        }
        contract
            .worldwide_days
            .create(&WorldwideDayRecord {
                wwd: WWD,
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
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
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
    });

    Fixture {
        activation,
        finality,
        intent_id,
    }
}

fn calldata(activation: &PoCActivationV1, limits: &SchemaLimits) -> Vec<u8> {
    IMetadosis::activateLysisCall {
        pocActivationV1: Bytes::from(activation.encode_canonical(limits).unwrap()),
    }
    .abi_encode()
}

fn rejection_code(error: PrecompileError) -> u16 {
    let PrecompileError::RevertBytes(bytes) = error else {
        panic!("expected typed activation rejection");
    };
    u16::try_from(U256::from_be_slice(&bytes[4..36])).unwrap()
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
        if expected_parent_root != self.parent_root || partition != PartitionRef::TributeWwd(WWD) {
            return Err(PrecompileError::Fatal(
                "activation test parent partition binding mismatch".into(),
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
            "activation test does not seal the synthetic parent".into(),
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

#[test]
fn certified_conflict_requires_valid_evidence_and_mutates_no_owner_state() {
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider.set_timestamp(U256::from(ACTIVATION_TIME));
    let fixture = setup(&mut provider, false);
    provider.clear_events(METADOSIS_ADDRESS);

    let mut invalid = fixture.activation.clone();
    invalid.certificate.ordered_signatures[0].signature_rs[0] ^= 1;
    StorageHandle::enter(&mut provider, |storage| {
        let error = dispatch_public_activation(
            storage.clone(),
            &outbe_compressed_entities::ExecutionScope::new(),
            Some(&fixture.finality),
            &calldata(&invalid, &limits),
            U256::ZERO,
            false,
        )
        .unwrap_err();
        assert_eq!(rejection_code(error), 11);
        let job = MetadosisContract::new(storage)
            .ocomp_job_record(fixture.intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, OcompJobStatus::OffchainPending);
    });
    assert!(provider.get_events(METADOSIS_ADDRESS).is_empty());

    let output = StorageHandle::enter(&mut provider, |storage| {
        dispatch_public_activation(
            storage,
            &outbe_compressed_entities::ExecutionScope::new(),
            Some(&fixture.finality),
            &calldata(&fixture.activation, &limits),
            U256::ZERO,
            false,
        )
        .unwrap()
    });
    let decoded = IMetadosis::activateLysisCall::abi_decode_returns(&output).unwrap();
    assert_eq!(decoded.outcome, ActivationOutcome::ConflictResolved as u8);

    StorageHandle::enter(&mut provider, |storage| {
        let contract = MetadosisContract::new(storage.clone());
        let job = contract
            .ocomp_job_record(fixture.intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, OcompJobStatus::Conflicted);
        assert_eq!(contract.get_wwd_status(WWD).unwrap(), status::READY);
        let nod = outbe_nod::schema::NodContract::new(storage.clone())
            .ocomp_target_projection(WWD)
            .unwrap();
        assert_eq!(nod.target_generation, 0);
        assert!(nod.namespace_root_before.is_zero());
        assert!(
            outbe_intex::api::ocomp_contributor_target_projection(&storage, WWD.value(),)
                .unwrap()
                .contributor_total
                .is_zero()
        );
        assert_eq!(
            outbe_tribute::TributeContract::new(storage.clone())
                .pre_admission_projection(WWD)
                .unwrap()
                .source_generation,
            0
        );
        assert!(outbe_promislimit::PromisLimitContract::new(storage)
            .total_unallocated
            .read()
            .unwrap()
            .is_zero());
    });
    assert_eq!(provider.get_events(METADOSIS_ADDRESS).len(), 1);
}

#[test]
fn certified_apply_commits_four_owners_once_and_exact_retry_is_read_only() {
    let limits = poc_schema_limits();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(ACTIVATION_HEIGHT);
    provider.set_timestamp(U256::from(ACTIVATION_TIME));
    let fixture = setup(&mut provider, true);
    let scope = begin_activation_scope(&mut provider);
    provider.clear_events(METADOSIS_ADDRESS);
    provider.enable_lysis_activation_frame();

    let output = StorageHandle::enter(&mut provider, |storage| {
        dispatch_public_activation(
            storage,
            &scope,
            Some(&fixture.finality),
            &calldata(&fixture.activation, &limits),
            U256::ZERO,
            false,
        )
        .unwrap()
    });
    let decoded = IMetadosis::activateLysisCall::abi_decode_returns(&output).unwrap();
    assert_eq!(decoded.outcome, ActivationOutcome::Applied as u8);
    assert_eq!(fixture.finality.calls.load(Ordering::Relaxed), 1);

    StorageHandle::enter(&mut provider, |storage| {
        let contract = MetadosisContract::new(storage.clone());
        let job = contract
            .ocomp_job_record(fixture.intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, OcompJobStatus::Completed);
        let terminal = job
            .terminal
            .as_ref()
            .unwrap()
            .completed_binding
            .as_ref()
            .unwrap();
        assert_eq!(
            terminal.terminal_receipt.activated_at_height,
            ACTIVATION_HEIGHT
        );
        assert_eq!(terminal.terminal_receipt.activated_at_time, ACTIVATION_TIME);
        assert_eq!(contract.get_wwd_status(WWD).unwrap(), status::COMPLETED);
        let active = contract
            .active_lysis_generation(WWD, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(active.job_id, fixture.activation.result.job_id);
        assert_eq!(active.nod_root, fixture.activation.result.roots.nod_root);

        let nod = outbe_nod::schema::NodContract::new(storage.clone())
            .ocomp_target_projection(WWD)
            .unwrap();
        assert_eq!(nod.target_generation, 1);
        assert_eq!(
            nod.namespace_root_before,
            fixture.activation.result.roots.nod_root
        );
        let nod_generation = outbe_nod::schema::NodContract::new(storage.clone())
            .ocomp_certified_generation(WWD)
            .unwrap()
            .unwrap();
        assert_eq!(nod_generation.issued_at, LOGICAL_TIME);
        let contributors =
            outbe_intex::api::certified_contributor_generation(&storage, WWD.value())
                .unwrap()
                .unwrap();
        assert_eq!(contributors.series_version, 1);
        assert_eq!(
            contributors.contributor_root,
            fixture.activation.result.roots.contributor_root
        );
        let tribute = TributeContract::new(storage.clone());
        let admission = tribute.pre_admission_projection(WWD).unwrap();
        let totals = tribute.get_day_totals(WWD).unwrap();
        assert_eq!(admission.source_generation, 1);
        assert_eq!(totals.tribute_count, 0);
        assert!(totals.tribute_nominal_amount.is_zero());
        assert_eq!(tribute.total_supply.read().unwrap(), 0);
        assert_eq!(
            outbe_promislimit::PromisLimitContract::new(storage)
                .total_unallocated
                .read()
                .unwrap(),
            U256::from(15)
        );
    });

    let events_after_apply = provider.get_ordered_events().len();
    let retry = StorageHandle::enter(&mut provider, |storage| {
        dispatch_public_activation(
            storage,
            &scope,
            Some(&fixture.finality),
            &calldata(&fixture.activation, &limits),
            U256::ZERO,
            false,
        )
        .unwrap()
    });
    assert_eq!(retry, output);
    assert_eq!(fixture.finality.calls.load(Ordering::Relaxed), 1);
    assert_eq!(provider.get_ordered_events().len(), events_after_apply);

    let mut wrong_job = fixture.activation.clone();
    wrong_job.result.job_id = hash(200);
    let mut changed_proof = fixture.activation.clone();
    changed_proof
        .finalized_intent_proof
        .intent_account_proof
        .0
        .push(0xAA);
    let mut changed_certificate = fixture.activation.clone();
    changed_certificate.certificate.ordered_signatures[0].signature_rs[0] ^= 1;
    for mismatch in [wrong_job, changed_proof, changed_certificate] {
        let error = StorageHandle::enter(&mut provider, |storage| {
            dispatch_public_activation(
                storage,
                &scope,
                Some(&fixture.finality),
                &calldata(&mismatch, &limits),
                U256::ZERO,
                false,
            )
            .unwrap_err()
        });
        assert_eq!(rejection_code(error), 6);
        assert_eq!(fixture.finality.calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.get_ordered_events().len(), events_after_apply);
    }
}

fn assert_pending_owner_state(
    provider: &mut HashMapStorageProvider,
    fixture: &Fixture,
    limits: &SchemaLimits,
) {
    StorageHandle::enter(provider, |storage| {
        let contract = MetadosisContract::new(storage.clone());
        let job = contract
            .ocomp_job_record(fixture.intent_id, limits)
            .unwrap()
            .unwrap();
        assert_eq!(job.status, OcompJobStatus::OffchainPending);
        assert_eq!(
            contract.get_wwd_status(WWD).unwrap(),
            status::OFFCHAIN_PENDING
        );
        assert!(contract
            .active_lysis_generation(WWD, limits)
            .unwrap()
            .is_none());

        let nod = outbe_nod::schema::NodContract::new(storage.clone())
            .ocomp_target_projection(WWD)
            .unwrap();
        assert_eq!(nod.target_generation, 0);
        assert!(nod.namespace_root_before.is_zero());
        assert!(
            outbe_intex::api::certified_contributor_generation(&storage, WWD.value())
                .unwrap()
                .is_none()
        );
        let tribute = TributeContract::new(storage.clone());
        let admission = tribute.pre_admission_projection(WWD).unwrap();
        let totals = tribute.get_day_totals(WWD).unwrap();
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

#[test]
fn every_owner_failure_rolls_back_job_four_owners_events_and_ce_work() {
    let limits = poc_schema_limits();
    for (owner, address) in [
        ("Nod", NOD_ADDRESS),
        ("Contributor", INTEX_ADDRESS),
        ("Tribute", TRIBUTE_ADDRESS),
        ("CarryOver", PROMIS_LIMIT_ADDRESS),
    ] {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(ACTIVATION_HEIGHT);
        provider.set_timestamp(U256::from(ACTIVATION_TIME));
        let fixture = setup(&mut provider, true);
        let scope = begin_activation_scope(&mut provider);
        let events_before = provider.get_ordered_events().len();
        let ce_expected = scope.ce_work_checkpoint().unwrap();
        provider.fail_mutation_at_address(address);
        provider.enable_lysis_activation_frame();

        let error = StorageHandle::enter(&mut provider, |storage| {
            dispatch_public_activation(
                storage,
                &scope,
                Some(&fixture.finality),
                &calldata(&fixture.activation, &limits),
                U256::ZERO,
                false,
            )
            .unwrap_err()
        });
        assert!(
            matches!(
                error,
                PrecompileError::Storage(_)
                    | PrecompileError::Fatal(_)
                    | PrecompileError::Revert(_)
                    | PrecompileError::RevertBytes(_)
            ),
            "{owner} fault must abort the activation"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            ce_expected,
            "{owner} fault must restore CE work"
        );
        provider.clear_mutation_failure();

        assert_pending_owner_state(&mut provider, &fixture, &limits);
        assert_eq!(
            provider.get_ordered_events().len(),
            events_before,
            "{owner} fault must roll back every event"
        );
    }
}
