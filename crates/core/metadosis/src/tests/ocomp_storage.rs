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
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, types::StorageKey, StorageHandle},
};
use outbe_promislimit::schema::PromisLimitContract;

use crate::precompile::IMetadosis;
use crate::{
    ocomp::{
        schema::{poc_schema_limits, OcompRequestProfile},
        state::{DayPhase, JobFsmLimits},
    },
    schema::{
        day_type, status, MetadosisContract, WorldwideDay as WorldwideDayRecord,
        WorldwideDayEntryExt, OCOMP_JOB_RECORDS_BASE_SLOT,
    },
};

use super::with_storage;

const WWD: WorldwideDay = WorldwideDay::new(20_260_723);
const REQUEST_HEIGHT: u64 = 10;
const DEADLINE_HEIGHT: u64 = 74;
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
        result_committee_snapshot_hash: B256::repeat_byte(0x36),
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

fn create_ready_day(contract: &mut MetadosisContract<'_>, wwd: WorldwideDay) {
    contract
        .worldwide_days
        .create(&WorldwideDayRecord {
            wwd,
            status: status::READY,
            day_type: day_type::GREEN,
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_amount: DAY_LIMIT,
            previous_vwap: U256::from(50),
            current_vwap: U256::from(55),
        })
        .unwrap();
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
    deadline_height: u64,
    receipt_hash: B256,
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
                series_id: WWD.value(),
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
        result_committee_snapshot_hash: B256::repeat_byte(0x36),
        custody_committee_epoch_hash: None,
        deadline_height,
    }
}

#[test]
fn persisted_request_and_expiry_keep_job_indexes_status_and_budget_equivalent() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 2,
        };
        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        let receipt = receipt();
        let receipt_hash = receipt.receipt_hash(&limits).unwrap();
        let first_intent = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash);
        let first_intent_id = first_intent.intent_id(&limits).unwrap();
        contract
            .commit_ocomp_request(&first_intent, &receipt, &limits, fsm_limits)
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
        assert_eq!(live_record.status, OcompJobStatus::OffchainPending);
        assert_eq!(live_record.intent, first_intent);

        contract
            .expire_ocomp_job(DEADLINE_HEIGHT, REQUEST_TIME + 64, &limits, fsm_limits)
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
        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();

        let request_receipt = receipt();
        let request_receipt_hash = request_receipt.receipt_hash(&limits).unwrap();
        let requested = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, request_receipt_hash);
        let intent_id = requested.intent_id(&limits).unwrap();
        contract
            .commit_ocomp_request(&requested, &request_receipt, &limits, fsm_limits)
            .unwrap();

        let job_id = B256::repeat_byte(0x51);
        let result_digest = B256::repeat_byte(0x52);
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
            result_evidence_hash: B256::repeat_byte(0x54),
            terminal_receipt_hash: terminal_receipt.terminal_receipt_hash(&limits).unwrap(),
            terminal_receipt,
        };

        assert_eq!(
            contract
                .commit_ocomp_conflict(
                    intent_id,
                    completed_binding.clone(),
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
    let requested = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash);
    let intent_id = requested.intent_id(&limits).unwrap();
    let protocol_key = intent_storage_key(intent_id).unwrap();
    let records_base_slot = U256::from(OCOMP_JOB_RECORDS_BASE_SLOT);
    let protocol_slot = protocol_key.mapping_slot(records_base_slot);
    let raw_intent_slot = intent_id.mapping_slot(records_base_slot);
    assert_ne!(protocol_slot, raw_intent_slot);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = MetadosisContract::new(storage.clone());
        assert_eq!(contract.ocomp_job_records.base_slot(), records_base_slot);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        contract
            .commit_ocomp_request(&requested, &receipt, &limits, fsm_limits)
            .unwrap();

        assert_eq!(
            contract.ocomp_job_record(intent_id, &limits).unwrap(),
            Some(outbe_ocomp_protocol::state::OcompJobRecordV1 {
                intent: requested.clone(),
                status: OcompJobStatus::OffchainPending,
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
        let receipt = receipt();
        let receipt_hash = receipt.receipt_hash(&limits).unwrap();
        let requested = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash);
        let intent_id = requested.intent_id(&limits).unwrap();
        let protocol_key = intent_storage_key(intent_id).unwrap();
        let original = OcompJobRecordV1 {
            intent: requested.clone(),
            status: OcompJobStatus::OffchainPending,
            terminal: None,
        };
        let original_bytes = original.encode_canonical(&limits).unwrap();

        let mut contract = MetadosisContract::new(storage);
        create_ready_day(&mut contract, WWD);
        contract
            .enqueue_ocomp_ready(WWD, REQUEST_HEIGHT, fsm_limits)
            .unwrap();
        contract
            .ocomp_job_records
            .get_bytes(&protocol_key)
            .write(&original_bytes)
            .unwrap();

        assert!(contract
            .commit_ocomp_request(&requested, &receipt, &limits, fsm_limits)
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
fn final_allowed_expiry_credits_full_lysis_budget_once_and_does_not_requeue() {
    with_storage(|storage| {
        let limits = poc_schema_limits();
        let fsm_limits = JobFsmLimits {
            max_terminal_records: 1,
        };
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
        let first_intent = intent(0, REQUEST_HEIGHT, DEADLINE_HEIGHT, receipt_hash);
        let first_intent_id = first_intent.intent_id(&limits).unwrap();
        contract
            .commit_ocomp_request(&first_intent, &receipt, &limits, fsm_limits)
            .unwrap();

        contract
            .expire_ocomp_job(DEADLINE_HEIGHT, REQUEST_TIME + 64, &limits, fsm_limits)
            .unwrap();

        assert_eq!(contract.get_wwd_status(WWD).unwrap(), status::FAILED);
        assert!(contract.ocomp_scheduler.is_empty().unwrap());
        assert!(contract
            .next_ocomp_ready(&limits, fsm_limits)
            .unwrap()
            .is_none());
        assert!(contract.ocomp_fsm_state(WWD, &limits, fsm_limits).is_err());
        assert_eq!(
            promis_limit.get_total_unallocated().unwrap(),
            existing_carry_over + LYSIS_BUDGET
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
            .expire_ocomp_job(DEADLINE_HEIGHT + 1, REQUEST_TIME + 65, &limits, fsm_limits,)
            .is_err());
        assert_eq!(
            promis_limit.get_total_unallocated().unwrap(),
            existing_carry_over + LYSIS_BUDGET
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
