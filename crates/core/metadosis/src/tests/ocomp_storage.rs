use alloy_primitives::{B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_ocomp_protocol::{
    hash::hash_framed,
    intent::{
        intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::CapacityProfileV1,
    receipts::{
        desis_request_brief_hash, empty_apply_event_summary_hash, ActivationOutcome,
        AggregateActivationReceiptV1, BudgetSplitDestination, EffectBindingV1,
        RequestBudgetSplitReceiptV1,
    },
    registry::HashDomain,
    state::{OcompCompletedBindingV1, OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    vote::OcompQuorumV1,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, types::StorageKey, StorageHandle},
};
use outbe_promislimit::schema::PromisLimitContract;

use crate::precompile::IMetadosis;
use crate::{
    fixture_kernel::FixtureKernelExt,
    ocomp::{
        fork::OcompForkInstallClassification,
        schema::{poc_schema_limits, OcompExpiryDisposition, OcompRequestProfile},
        state::{DayPhase, JobFsmLimits},
    },
    reducer::{OcompRetryCause, OuterWwdEvent, OuterWwdTransition},
    schema::{status, MetadosisContract, WorldwideDayEntryExt, OCOMP_JOB_RECORDS_BASE_SLOT},
};

use super::with_storage;

const WWD: WorldwideDay = WorldwideDay::new(20_260_723);
const REQUEST_HEIGHT: u64 = 10;
const DEADLINE_HEIGHT: u64 = REQUEST_HEIGHT
    + outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH
    + outbe_chain_constants::DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS;
const REQUEST_TIME: u64 = 1_753_315_200;
const DAY_LIMIT: U256 = U256::from_limbs([1_000, 0, 0, 0]);
const LYSIS_BUDGET: U256 = U256::from_limbs([700, 0, 0, 0]);
const AUCTION_BASE: U256 = U256::from_limbs([300, 0, 0, 0]);
const AUCTION_ENTRY_PRICE: U256 = U256::from_limbs([55, 0, 0, 0]);

pub(super) fn capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(0x22),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        retry_backoff_blocks: 1,
        max_terminal_job_records: 365,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: outbe_chain_constants::DEFAULT_OCOMP_COMPUTE_VOTE_WINDOW_BLOCKS,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: B256::repeat_byte(0x23),
    }
}

pub(super) fn request_profile() -> OcompRequestProfile {
    OcompRequestProfile {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        fork_id: B256::repeat_byte(0x21),
        protocol_bundle_hash: B256::repeat_byte(0x41),
        correctness_profile_id: B256::repeat_byte(0x24),
        capacity_profile: capacity_profile(),
        source_availability_policy_id: B256::repeat_byte(0x35),
    }
}

#[test]
fn request_profile_initialization_is_exact_idempotent_and_chain_bound() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let mut contract = MetadosisContract::new(storage);
        let profile = request_profile();

        contract
            .initialize_ocomp_request_profile(&profile, &limits)
            .unwrap();
        assert_eq!(
            contract.read_ocomp_request_profile(&limits).unwrap(),
            Some(profile.clone())
        );
        contract
            .initialize_ocomp_request_profile(&profile, &limits)
            .unwrap();

        let mut changed = profile;
        changed.protocol_bundle_hash = B256::repeat_byte(0x42);
        assert!(contract
            .initialize_ocomp_request_profile(&changed, &limits)
            .is_err());
        assert_eq!(
            contract.read_ocomp_request_profile(&limits).unwrap(),
            Some(request_profile())
        );
    });
}

#[test]
fn fork_install_is_exactly_profile_plus_bundle_and_keeps_reserved_slot_zero() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let install = crate::fixture_kernel::fork_install_fixture(
            OcompForkInstallClassification::Measurement,
            1,
            1,
            B256::repeat_byte(0x11),
        );
        let encoded = install.encode_canonical(&limits).unwrap();
        let nested_bytes = install
            .request_profile
            .encode_canonical(&limits)
            .unwrap()
            .len()
            + install
                .protocol_bundle
                .encode_canonical(&limits)
                .unwrap()
                .len();
        let founder_bytes = install
            .founder_registrations
            .iter()
            .map(|registration| 4 + registration.encode_canonical(&limits).unwrap().len())
            .sum::<usize>();
        assert_eq!(
            encoded.len() - nested_bytes,
            4 + 2 + 1 + 8 + 4 + 4 + 4 + founder_bytes
        );
        assert_eq!(
            crate::config::OcompForkInstallV1::decode_canonical(&encoded, &limits).unwrap(),
            install
        );

        let mut contract = MetadosisContract::new(storage);
        contract
            .initialize_ocomp_fork_install(&install, 1, &limits)
            .unwrap();
        assert!(contract.ocomp_result_committee_snapshot.is_empty().unwrap());
        contract
            .initialize_ocomp_fork_install(&install, 1, &limits)
            .unwrap();
        assert!(contract.ocomp_result_committee_snapshot.is_empty().unwrap());
    });
}

#[test]
fn fork_install_round_trips_founder_keys_without_defining_membership() {
    let limits = poc_schema_limits();
    let install = crate::fixture_kernel::fork_install_fixture(
        OcompForkInstallClassification::Measurement,
        1,
        1,
        B256::repeat_byte(0x11),
    );
    assert!(
        !install.founder_registrations.is_empty(),
        "an OCOMP-enabled fresh network must bootstrap every founder key"
    );

    let encoded = install.encode_canonical(&limits).unwrap();
    let decoded = crate::config::OcompForkInstallV1::decode_canonical(&encoded, &limits).unwrap();
    assert_eq!(decoded.founder_registrations, install.founder_registrations);
    assert_eq!(decoded, install);
}

fn create_ready_day(contract: &mut MetadosisContract<'_>, wwd: WorldwideDay) {
    contract
        .fixture_create_ready_day(wwd, DAY_LIMIT, U256::from(50), U256::from(55))
        .unwrap();
}

fn outer_transition(contract: &MetadosisContract<'_>, event: OuterWwdEvent) -> OuterWwdTransition {
    crate::commit::plan_outer_transition_for_test_fixture(contract, WWD, event).unwrap()
}

fn receipt() -> RequestBudgetSplitReceiptV1 {
    let protocol_bundle_hash = B256::repeat_byte(0x41);
    RequestBudgetSplitReceiptV1 {
        protocol_bundle_hash,
        wwd: WWD.value(),
        pending_nonce: 0,
        day_type: DayType::Green,
        day_limit: DAY_LIMIT,
        lysis_budget: LYSIS_BUDGET,
        auction_base: AUCTION_BASE,
        destination: BudgetSplitDestination::DesisAuction,
        desis_brief_hash: Some(
            desis_request_brief_hash(
                protocol_bundle_hash,
                WWD.value(),
                AUCTION_BASE,
                AUCTION_ENTRY_PRICE,
                REQUEST_TIME,
            )
            .unwrap(),
        ),
        carry_over_credit: U256::ZERO,
        auction_entry_price: AUCTION_ENTRY_PRICE,
        logical_anchor: REQUEST_TIME,
    }
}

fn intent(
    pending_nonce: u64,
    request_height: u64,
    _deadline_height: u64,
    receipt_hash: B256,
    snapshot: &outbe_validatorset::OcompSnapshotExtensionV1,
) -> JobIntentV1 {
    let attempt = u32::try_from(pending_nonce).unwrap();
    JobIntentV1 {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x11),
        fork_id: B256::repeat_byte(0x21),
        wwd: WWD.value(),
        pending_nonce,
        attempt,
        protocol_bundle_hash: B256::repeat_byte(0x41),
        ce_sealed_root: B256::repeat_byte(0x31),
        sealed_tribute_collection_key: B256::repeat_byte(0x32),
        sealed_tribute_collection_root: B256::repeat_byte(0x33),
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::from(10),
        pre_admission_envelope_hash: B256::repeat_byte(0x34),
        source_availability_policy_id: B256::repeat_byte(0x35),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: DAY_LIMIT,
            previous_vwap: U256::from(50),
            current_vwap: U256::from(55),
            gratis_demand: LYSIS_BUDGET,
            gratis_supply: DAY_LIMIT,
            lysis_budget: LYSIS_BUDGET,
            auction_base: AUCTION_BASE,
            auction_entry_price: AUCTION_ENTRY_PRICE,
            request_budget_split_receipt_hash: receipt_hash,
        },
        logical_evaluation_height: request_height,
        logical_evaluation_time: REQUEST_TIME,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: WWD.value(),
                source_generation: 0,
                collection_key: B256::repeat_byte(0x32),
                sealed_collection_root: B256::repeat_byte(0x33),
                exact_count: 1,
                exact_nominal_total: U256::from(10),
            },
            nod: NodTargetPreconditionV1 {
                wwd: WWD.value(),
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 1,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: WWD.value(),
                expected_series_version: 0,
                max_contributor_count: 1,
                max_eligible_nominal_total: U256::from(10),
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: WWD.value(),
                pending_nonce,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 2,
            },
        },
        result_validator_set_epoch: snapshot.epoch,
        result_committee_set_hash: snapshot.committee_set_hash,
        result_ocomp_binding_hash: snapshot.ocomp_binding_hash,
        result_member_count: snapshot.member_count,
        result_quorum_threshold: u16::try_from(outbe_consensus::proof::simplex_n3f1_quorum(
            usize::from(snapshot.member_count),
        ))
        .unwrap(),
        custody_committee_epoch_hash: None,
    }
}

fn open_job(
    contract: &mut MetadosisContract<'_>,
    intent_id: B256,
    limits: &outbe_ocomp_protocol::SchemaLimits,
    fsm_limits: JobFsmLimits,
) -> outbe_ocomp_protocol::state::OcompFinalizedJobV1 {
    // Production installs this immutable profile at genesis activation. These
    // storage-focused tests construct records directly, so mirror that
    // prerequisite before exercising finality.
    contract
        .initialize_ocomp_request_profile(&request_profile(), limits)
        .unwrap();
    let finalized = contract
        .record_ocomp_finality(
            intent_id,
            B256::repeat_byte(0x46),
            B256::repeat_byte(0x98),
            REQUEST_HEIGHT,
            DEADLINE_HEIGHT - REQUEST_HEIGHT - 4,
            limits,
        )
        .unwrap();
    contract
        .open_due_ocomp_voting(finalized.open_height, limits, fsm_limits)
        .unwrap();
    finalized
}

#[test]
fn certified_parent_finality_records_only_the_exact_live_request_and_fails_closed_without_root() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let snapshot = crate::fixture_kernel::seed_validator_snapshot(storage.clone(), &limits, 4);
        let receipt = receipt();
        let requested = intent(
            0,
            REQUEST_HEIGHT,
            DEADLINE_HEIGHT,
            receipt.receipt_hash(&limits).unwrap(),
            &snapshot,
        );
        let intent_id = requested.intent_id(&limits).unwrap();
        let mut contract = MetadosisContract::new(storage.clone());
        contract
            .initialize_ocomp_request_profile(&request_profile(), &limits)
            .unwrap();
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        contract
            .commit_ocomp_request(
                &request_transition,
                &requested,
                &receipt,
                &limits,
                fsm_limits,
            )
            .unwrap();

        let finality_height = REQUEST_HEIGHT + 1;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(finality_height, REQUEST_TIME + 1, 1),
            storage,
        );
        assert!(
            !crate::ocomp::expiry::record_certified_parent_finality(
                &ctx,
                REQUEST_HEIGHT - 1,
                B256::repeat_byte(0x45),
                B256::ZERO,
            )
            .unwrap(),
            "an unrelated certified parent must not mutate the one live job"
        );
        assert!(MetadosisContract::new(ctx.storage.clone())
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap()
            .finalized
            .is_none());

        assert!(
            crate::ocomp::expiry::record_certified_parent_finality(
                &ctx,
                REQUEST_HEIGHT,
                B256::repeat_byte(0x46),
                B256::ZERO,
            )
            .is_err(),
            "the exact request cannot become final without its authenticated state root"
        );
        assert!(MetadosisContract::new(ctx.storage.clone())
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap()
            .finalized
            .is_none());

        let request_hash = B256::repeat_byte(0x46);
        let request_state_root = B256::repeat_byte(0x98);
        assert!(crate::ocomp::expiry::record_certified_parent_finality(
            &ctx,
            REQUEST_HEIGHT,
            request_hash,
            request_state_root,
        )
        .unwrap());
        let finalized = MetadosisContract::new(ctx.storage.clone())
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap()
            .finalized
            .unwrap();
        assert_eq!(finalized.finalized_request_block_hash, request_hash);
        assert_eq!(finalized.finalized_request_state_root, request_state_root);
        assert_eq!(finalized.finality_recorded_height, finality_height);
        assert_eq!(finalized.open_height, finality_height + 4);
        assert_eq!(
            finalized.deadline_height,
            finality_height + 4 + capacity_profile().result_deadline_blocks
        );

        assert!(
            crate::ocomp::expiry::record_certified_parent_finality(
                &ctx,
                REQUEST_HEIGHT,
                request_hash,
                request_state_root,
            )
            .unwrap(),
            "an exact consensus replay must be idempotent"
        );
    });
}

#[test]
fn persisted_request_and_expiry_keep_job_indexes_status_and_budget_equivalent() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let snapshot = crate::fixture_kernel::seed_validator_snapshot(storage.clone(), &limits, 4);
        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        let receipt = receipt();
        let receipt_hash = receipt.receipt_hash(&limits).unwrap();
        let first_intent = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash, &snapshot);
        let first_intent_id = first_intent.intent_id(&limits).unwrap();
        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        contract
            .commit_ocomp_request(
                &request_transition,
                &first_intent,
                &receipt,
                &limits,
                fsm_limits,
            )
            .unwrap();

        assert_eq!(
            contract.worldwide_days.entry(WWD).status().read().unwrap(),
            status::OFFCHAIN_PENDING
        );
        let pending = contract.ocomp_fsm_state(WWD, &limits, fsm_limits).unwrap();
        let pending_projection = pending.projection();
        assert_eq!(pending_projection.phase, DayPhase::OffchainPending);
        assert_eq!(pending_projection.live_intent_id, Some(first_intent_id));
        assert_eq!(
            contract.request_budget_receipt(WWD, &limits).unwrap(),
            Some(receipt.clone())
        );
        let live_record = contract
            .ocomp_job_record(first_intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(live_record.status, OcompJobStatus::AwaitingFinality);
        assert_eq!(live_record.intent, first_intent);

        open_job(&mut contract, first_intent_id, &limits, fsm_limits);
        let retry_transition = outer_transition(
            &contract,
            OuterWwdEvent::OcompRetryScheduled(OcompRetryCause::Expired),
        );
        contract
            .expire_ocomp_job(
                &retry_transition,
                first_intent_id,
                DEADLINE_HEIGHT,
                REQUEST_TIME + 64,
                &limits,
                fsm_limits,
            )
            .unwrap();

        assert_eq!(
            contract.worldwide_days.entry(WWD).status().read().unwrap(),
            status::READY
        );
        let ready = contract.ocomp_fsm_state(WWD, &limits, fsm_limits).unwrap();
        let ready_projection = ready.projection();
        assert_eq!(ready_projection.phase, DayPhase::Ready);
        assert_eq!(ready_projection.pending_nonce, 1);
        assert_eq!(
            ready_projection.next_check_height,
            Some(DEADLINE_HEIGHT + 1)
        );
        assert_eq!(ready_projection.retained_lysis_budget, Some(LYSIS_BUDGET));
        assert_eq!(ready_projection.terminal_records, 1);

        let terminal_record = contract
            .ocomp_job_record(first_intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(terminal_record.status, OcompJobStatus::Expired);
        let terminal = terminal_record.terminal.unwrap();
        assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
        assert_eq!(terminal.next_pending_nonce, Some(1));
        assert_eq!(terminal.completed_binding, None);
    });
}

#[test]
fn certified_conflict_is_terminal_for_the_old_job_and_requeues_the_same_budget() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let snapshot = crate::fixture_kernel::seed_validator_snapshot(storage.clone(), &limits, 4);
        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        let request_receipt = receipt();
        let request_receipt_hash = request_receipt.receipt_hash(&limits).unwrap();
        let requested = intent(
            0,
            REQUEST_HEIGHT,
            DEADLINE_HEIGHT,
            request_receipt_hash,
            &snapshot,
        );
        let intent_id = requested.intent_id(&limits).unwrap();
        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        contract
            .commit_ocomp_request(
                &request_transition,
                &requested,
                &request_receipt,
                &limits,
                fsm_limits,
            )
            .unwrap();

        let finalized = open_job(&mut contract, intent_id, &limits, fsm_limits);
        let job_id = finalized.job_id;
        let result_digest = B256::repeat_byte(0x52);
        let quorum = OcompQuorumV1 {
            result_digest,
            quorum_height: finalized.open_height,
            member_count: 4,
            quorum_threshold: 3,
            signer_bitmap: vec![0b0111],
            evidence_hash: B256::repeat_byte(0x55),
        };
        let activation_call_id = B256::repeat_byte(0x53);
        let binding = EffectBindingV1 {
            intent_id,
            job_id,
            attempt: requested.attempt,
            protocol_bundle_hash: requested.protocol_bundle_hash,
            result_digest,
            activation_preconditions_hash: requested
                .activation_preconditions
                .activation_preconditions_hash(&limits)
                .unwrap(),
            activation_call_id,
        };
        let activation_height = REQUEST_HEIGHT + 5;
        let activation_time = REQUEST_TIME + 5;
        let terminal_receipt = AggregateActivationReceiptV1 {
            binding,
            outcome: ActivationOutcome::ConflictResolved,
            nod_receipt_hash: None,
            contributor_receipt_hash: None,
            tribute_receipt_hash: None,
            carry_over_receipt_hash: None,
            request_budget_split_receipt_hash: request_receipt_hash,
            active_generation_hash: None,
            effect_commitment: hash_framed(HashDomain::Effects, &[]).unwrap(),
            event_summary_hash: empty_apply_event_summary_hash().unwrap(),
            activated_at_height: activation_height,
            activated_at_time: activation_time,
        };
        let completed_binding = OcompCompletedBindingV1 {
            job_id,
            activation_call_id,
            result_digest,
            quorum_height: quorum.quorum_height,
            quorum_signer_bitmap: quorum.signer_bitmap.clone(),
            quorum_evidence_hash: quorum.evidence_hash,
            result_evidence_hash: B256::repeat_byte(0x54),
            terminal_receipt_hash: terminal_receipt.terminal_receipt_hash(&limits).unwrap(),
            terminal_receipt,
        };

        let conflict_transition = outer_transition(
            &contract,
            OuterWwdEvent::OcompRetryScheduled(OcompRetryCause::Conflicted),
        );
        assert_eq!(
            contract
                .commit_ocomp_conflict(
                    &conflict_transition,
                    intent_id,
                    completed_binding.clone(),
                    &quorum,
                    activation_height,
                    activation_time,
                    &limits,
                    fsm_limits,
                )
                .unwrap(),
            1
        );

        assert_eq!(contract.get_wwd_status(WWD).unwrap(), status::READY);
        let projection = contract
            .ocomp_fsm_state(WWD, &limits, fsm_limits)
            .unwrap()
            .projection();
        assert_eq!(projection.phase, DayPhase::Ready);
        assert_eq!(projection.pending_nonce, 1);
        assert_eq!(projection.next_check_height, Some(activation_height + 1));
        assert_eq!(projection.retained_lysis_budget, Some(LYSIS_BUDGET));
        let terminal = contract
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(terminal.status, OcompJobStatus::Conflicted);
        assert_eq!(
            terminal.terminal.unwrap().completed_binding,
            Some(completed_binding.clone())
        );
        assert!(contract.ocomp_scheduler.is_empty().unwrap());

        assert_eq!(
            IMetadosis::getLysisTerminalReceiptCall::SELECTOR,
            [0x20, 0xf4, 0x6b, 0xe7]
        );
        let encoded = crate::precompile::dispatch(
            contract.storage.clone(),
            &IMetadosis::getLysisTerminalReceiptCall {
                intentId: intent_id,
            }
            .abi_encode(),
            alloy_primitives::Address::repeat_byte(0x61),
            U256::ZERO,
        )
        .unwrap();
        let public_receipt =
            IMetadosis::getLysisTerminalReceiptCall::abi_decode_returns(&encoded).unwrap();
        assert_eq!(
            AggregateActivationReceiptV1::decode_canonical(public_receipt.as_ref(), &limits)
                .unwrap(),
            completed_binding.terminal_receipt
        );
    });
}

#[test]
fn job_record_is_physically_bound_to_the_protocol_intent_slot_key() {
    let limits = poc_schema_limits();
    let fsm_limits = JobFsmLimits {
        max_terminal_records: 2,
    };
    let receipt = receipt();
    let receipt_hash = receipt.receipt_hash(&limits).unwrap();
    let mut provider = HashMapStorageProvider::new(1);
    let snapshot = StorageHandle::enter(&mut provider, |storage| {
        crate::fixture_kernel::seed_validator_snapshot(storage, &limits, 4)
    });
    let requested = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash, &snapshot);
    let intent_id = requested.intent_id(&limits).unwrap();
    let protocol_key = intent_storage_key(intent_id).unwrap();
    let records_base_slot = U256::from(OCOMP_JOB_RECORDS_BASE_SLOT);
    let protocol_slot = protocol_key.mapping_slot(records_base_slot);
    let raw_intent_slot = intent_id.mapping_slot(records_base_slot);
    assert_ne!(protocol_slot, raw_intent_slot);

    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = MetadosisContract::new(storage.clone());
        assert_eq!(contract.ocomp_job_records.base_slot(), records_base_slot);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        contract
            .commit_ocomp_request(
                &request_transition,
                &requested,
                &receipt,
                &limits,
                fsm_limits,
            )
            .unwrap();

        assert_eq!(
            contract.ocomp_job_record(intent_id, &limits).unwrap(),
            Some(outbe_ocomp_protocol::state::OcompJobRecordV1 {
                intent: requested.clone(),
                intent_height: requested.logical_evaluation_height,
                status: OcompJobStatus::AwaitingFinality,
                finalized: None,
                terminal: None,
            })
        );
    });

    assert!(
        provider
            .storage
            .get(&(METADOSIS_ADDRESS, protocol_slot))
            .is_some_and(|word| !word.is_zero()),
        "the canonical job record must occupy the protocol-derived slot"
    );
    assert!(
        provider
            .storage
            .get(&(METADOSIS_ADDRESS, raw_intent_slot))
            .is_none_or(U256::is_zero),
        "raw IntentId must not be an authoritative storage key"
    );
}

#[test]
fn duplicate_request_cannot_replace_a_record_at_the_protocol_intent_slot_key() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let snapshot = crate::fixture_kernel::seed_validator_snapshot(storage.clone(), &limits, 4);
        let receipt = receipt();
        let receipt_hash = receipt.receipt_hash(&limits).unwrap();
        let requested = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash, &snapshot);
        let intent_id = requested.intent_id(&limits).unwrap();
        let protocol_key = intent_storage_key(intent_id).unwrap();
        let original = OcompJobRecordV1 {
            intent: requested.clone(),
            intent_height: requested.logical_evaluation_height,
            status: OcompJobStatus::AwaitingFinality,
            finalized: None,
            terminal: None,
        };
        let original_bytes = original.encode_canonical(&limits).unwrap();

        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        contract
            .corrupt_ocomp_job_record(intent_id, &original_bytes)
            .unwrap();

        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        assert!(contract
            .commit_ocomp_request(
                &request_transition,
                &requested,
                &receipt,
                &limits,
                fsm_limits,
            )
            .is_err());
        assert_eq!(
            contract
                .ocomp_job_records
                .get_bytes(&protocol_key)
                .read()
                .unwrap(),
            original_bytes
        );
        assert_eq!(
            contract.ocomp_job_record(intent_id, &limits).unwrap(),
            Some(original)
        );
    });
}

#[test]
fn final_allowed_expiry_prepares_terminal_evidence_for_the_scoped_failure_commit() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 1,
        };
        let snapshot = crate::fixture_kernel::seed_validator_snapshot(storage.clone(), &limits, 4);
        let mut promis_limit = PromisLimitContract::new(storage.clone());
        let existing_carry_over = U256::from(17);
        promis_limit
            .checked_add_carry_over(existing_carry_over)
            .unwrap();

        let mut contract = MetadosisContract::new(storage.clone());
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        let receipt = receipt();
        let receipt_hash = receipt.receipt_hash(&limits).unwrap();
        let first_intent = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash, &snapshot);
        let first_intent_id = first_intent.intent_id(&limits).unwrap();
        let request_transition = outer_transition(&contract, OuterWwdEvent::OcompRequestCommitted);
        contract
            .commit_ocomp_request(
                &request_transition,
                &first_intent,
                &receipt,
                &limits,
                fsm_limits,
            )
            .unwrap();

        open_job(&mut contract, first_intent_id, &limits, fsm_limits);
        let exhausted_transition =
            outer_transition(&contract, OuterWwdEvent::OcompAttemptsExhausted);
        let disposition = contract
            .expire_ocomp_job(
                &exhausted_transition,
                first_intent_id,
                DEADLINE_HEIGHT,
                REQUEST_TIME + 64,
                &limits,
                fsm_limits,
            )
            .unwrap();

        assert_eq!(
            disposition,
            OcompExpiryDisposition::TerminalNoRetry {
                next_pending_nonce: 1,
                retained_lysis_budget: LYSIS_BUDGET,
            }
        );
        assert_eq!(
            contract.get_wwd_status(WWD).unwrap(),
            status::OFFCHAIN_PENDING
        );
        assert!(!contract.ocomp_scheduler.is_empty().unwrap());
        assert!(contract
            .next_ocomp_ready(&limits, fsm_limits)
            .unwrap()
            .is_none());
        assert_eq!(
            promis_limit.get_total_unallocated().unwrap(),
            existing_carry_over
        );

        let terminal_record = contract
            .ocomp_job_record(first_intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(terminal_record.status, OcompJobStatus::Expired);
        let terminal = terminal_record.terminal.unwrap();
        assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
        assert_eq!(terminal.next_pending_nonce, Some(1));

        assert!(contract
            .expire_ocomp_job(
                &exhausted_transition,
                first_intent_id,
                DEADLINE_HEIGHT + 1,
                REQUEST_TIME + 65,
                &limits,
                fsm_limits,
            )
            .is_err());
        assert_eq!(
            promis_limit.get_total_unallocated().unwrap(),
            existing_carry_over
        );
    });
}

#[test]
fn deferred_ready_day_does_not_starve_the_next_due_day() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let later_wwd = WorldwideDay::new(20_260_724);
        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        create_ready_day(&mut contract, later_wwd);

        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        contract
            .enqueue_ocomp_ready(later_wwd, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        assert_eq!(
            contract
                .next_ocomp_ready(&limits, fsm_limits)
                .unwrap()
                .unwrap()
                .worldwide_day,
            WWD
        );

        contract
            .defer_ocomp_ready(WWD, REQUEST_HEIGHT, REQUEST_HEIGHT + 1, &limits, fsm_limits)
            .unwrap();

        let next = contract
            .next_ocomp_ready(&limits, fsm_limits)
            .unwrap()
            .unwrap();
        assert_eq!(next.worldwide_day, later_wwd);
        assert_eq!(next.next_check_height, Some(REQUEST_HEIGHT));

        let deferred = contract
            .ocomp_fsm_state(WWD, &limits, fsm_limits)
            .unwrap()
            .projection();
        assert_eq!(deferred.next_check_height, Some(REQUEST_HEIGHT + 1));
        assert_eq!(deferred.pending_nonce, 0);
    });
}

#[test]
fn canonical_storage_reads_fail_closed_when_the_declared_byte_cap_overflows() {
    with_storage(|storage| {
        let mut limits = poc_schema_limits();
        limits.codec.max_body_bytes = usize::MAX;
        let contract = MetadosisContract::new(storage);

        for result in [
            contract
                .read_pre_admission_envelope(WWD, &limits)
                .map(|_| ()),
            contract.request_budget_receipt(WWD, &limits).map(|_| ()),
            contract
                .ocomp_job_record(B256::repeat_byte(0x91), &limits)
                .map(|_| ()),
        ] {
            assert!(matches!(
                result,
                Err(outbe_primitives::error::PrecompileError::Fatal(_))
            ));
        }
    });
}

#[test]
fn terminal_index_push_rejects_zero_overwrite_and_exact_cap() {
    with_storage(|storage| {
        let mut metadosis = MetadosisContract::new(storage);
        let wwd = WorldwideDay::new(20270401);
        let cap = 3_u16;

        assert!(matches!(
            metadosis.push_terminal_intent(wwd, B256::ZERO, cap),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));

        for index in 0..cap {
            metadosis
                .push_terminal_intent(wwd, B256::repeat_byte(0x10 + index as u8), cap)
                .unwrap();
            assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), index + 1);
        }

        // The day is exactly at its cap: the next push must be refused.
        assert!(matches!(
            metadosis.push_terminal_intent(wwd, B256::repeat_byte(0x77), cap),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));

        // An occupied position must never be overwritten, even if the count
        // word is corrupted backwards.
        metadosis.ocomp_terminal_counts.write(&wwd, 1).unwrap();
        assert!(matches!(
            metadosis.push_terminal_intent(wwd, B256::repeat_byte(0x78), cap),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn terminal_index_round_trips_and_fails_closed_on_sparse_or_over_cap_counts() {
    with_storage(|storage| {
        let mut metadosis = MetadosisContract::new(storage);
        let wwd = WorldwideDay::new(20270402);
        let cap = 365_u16;

        assert_eq!(
            metadosis.terminal_intents_for(wwd, cap).unwrap(),
            Vec::<B256>::new()
        );

        let mut expected = Vec::new();
        for index in 0..cap {
            let id = crate::ocomp::terminal_entry_key(wwd, index);
            metadosis.push_terminal_intent(wwd, id, cap).unwrap();
            expected.push(id);
            if matches!(index + 1, 1 | 364 | 365) {
                assert_eq!(
                    metadosis.terminal_intents_for(wwd, cap).unwrap(),
                    expected[..usize::from(index + 1)]
                );
            }
        }
        assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), cap);

        // A count above the per-day cap fails closed on read.
        metadosis
            .ocomp_terminal_counts
            .write(&wwd, cap + 1)
            .unwrap();
        assert!(matches!(
            metadosis.terminal_intents_for(wwd, cap),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));

        // A count pointing past the last occupied position fails closed.
        let sparse = WorldwideDay::new(20270403);
        metadosis
            .push_terminal_intent(sparse, B256::repeat_byte(0x31), cap)
            .unwrap();
        metadosis.ocomp_terminal_counts.write(&sparse, 2).unwrap();
        assert!(matches!(
            metadosis.terminal_intents_for(sparse, cap),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn terminal_index_keys_are_domain_separated() {
    let day_a = WorldwideDay::new(20270404);
    let day_b = WorldwideDay::new(20270405);
    assert_ne!(
        crate::ocomp::terminal_entry_key(day_a, 0),
        crate::ocomp::terminal_entry_key(day_b, 0)
    );
    assert_ne!(
        crate::ocomp::terminal_entry_key(day_a, 0),
        crate::ocomp::terminal_entry_key(day_a, 1)
    );
    // Byte-for-byte pin of the key derivation: a silent change to the domain
    // string, field widths, or endianness must fail this assertion.
    assert_eq!(
        crate::ocomp::terminal_entry_key(day_a, 0),
        alloy_primitives::keccak256({
            let mut preimage = Vec::new();
            preimage.extend_from_slice(b"OUTBE_OCOMP_TERMINAL_INDEX_V1");
            preimage.extend_from_slice(&20270404_u32.to_be_bytes());
            preimage.extend_from_slice(&0_u16.to_be_bytes());
            preimage
        })
    );
}

#[test]
fn terminal_index_is_deleted_with_its_worldwide_day() {
    with_storage(|storage| {
        let mut metadosis = MetadosisContract::new(storage);
        let wwd = WorldwideDay::new(20270406);
        let cap = 4_u16;
        for index in 0..cap {
            metadosis
                .push_terminal_intent(wwd, B256::repeat_byte(0x40 + index as u8), cap)
                .unwrap();
        }

        metadosis.delete_terminal_index(wwd).unwrap();

        assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), 0);
        for index in 0..cap {
            assert_eq!(metadosis.terminal_intent_at(wwd, index).unwrap(), None);
        }
        // The vacated positions accept a fresh history.
        metadosis
            .push_terminal_intent(wwd, B256::repeat_byte(0x50), cap)
            .unwrap();
        assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), 1);
    });
}

#[test]
fn aggregate_rejects_closed_wwd_population_above_max_records_kept() {
    with_storage(|storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        for offset in 0..=u32::try_from(crate::constants::MAX_RECORDS_KEPT).unwrap() {
            metadosis
                .closed_wwd
                .push_back(WorldwideDay::new(20000101 + offset))
                .unwrap();
        }
        let error = crate::aggregate::ValidatedWwdAggregate::load_and_validate(storage)
            .expect_err("closed population above the retention cap must fail closed");
        assert!(
            error.to_string().contains("exceeds retention cap"),
            "unexpected error: {error}"
        );
    });
}
