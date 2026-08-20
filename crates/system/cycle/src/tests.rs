//! Cycle dispatcher tests.
//!
//! `next_fire_at` is asserted in `schedule_math_*`. Integration tests
//! exercise the dispatcher loop against the `HashMapStorageProvider`
//! so they cover the storage round-trip (`Cycle.last_executed_at`)
//! and the genesis-anchor interaction with Rewards.
//!
//! The dispatcher uses a lazy first-encounter anchor: on the very
//! first block it sees a trigger, it writes
//! `last_executed_at = block_ts` instead of firing. This anchors the
//! schedule at the chain's deployment instant so the first real fire
//! happens at the *next* slot strictly after that anchor. Without
//! this, every chain would fire its daily trigger on block 1 because
//! `block_ts >> 86_400` is always true on a real chain.

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    CompressedEntitiesLifecycle, CompressedEntitiesLifecycleContext, ExecutionScope,
};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MetadosisMutationPurposeTag, StorageHandle};
use outbe_tribute::TributeRepositoryReader;
use outbe_validatorset::contract::ValidatorSet;
use std::sync::Arc;

use crate::lifecycle::{CycleLifecycle, CycleLifecycleContext};
use crate::schema::Cycle;
use crate::triggers::{next_fire_at, TriggerId, ACTIVE_TRIGGERS};

mod model;

const CHAIN_ID: u64 = 1;
/// Genesis at midnight UTC of 2024-01-01.
const GENESIS_TS: u64 = 1_704_067_200;
const SECONDS_PER_DAY: u64 = 86_400;
const EMISSION_LIMIT_1_ID: u32 = TriggerId::EmissionLimit1.as_u32();
const METADOSIS_HOURLY_ID: u32 = TriggerId::MetadosisHourly.as_u32();

fn retained_days_before(
    victim: outbe_common::WorldwideDay,
    count: usize,
) -> Vec<outbe_common::WorldwideDay> {
    (0..count)
        .map(|offset| {
            let days_before = count - offset;
            let seconds_before = u64::try_from(days_before)
                .unwrap()
                .checked_mul(SECONDS_PER_DAY)
                .unwrap();
            outbe_common::WorldwideDay::from_timestamp(
                victim
                    .start_timestamp()
                    .checked_sub(seconds_before)
                    .unwrap(),
            )
        })
        .collect()
}

#[test]
fn cycle_tick_metadosis_authority_budget_matches_all_fixed_handlers() {
    assert_eq!(
        crate::triggers::metadosis_mutation_lease_budget_per_tick(false),
        3
    );
    assert_eq!(
        crate::triggers::metadosis_mutation_lease_budget_per_tick(true),
        2
    );
}

fn cycle_storage() -> HashMapStorageProvider {
    cycle_storage_for(CHAIN_ID)
}

fn cycle_storage_for(chain_id: u64) -> HashMapStorageProvider {
    let genesis_hash = B256::repeat_byte(0x11);
    let mut storage = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    storage.set_block_number(1);
    storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::ForkProfile);
    let install = outbe_metadosis::test_support::ForkInstallScenario::measurement_at(
        1,
        chain_id,
        genesis_hash,
    )
    .unwrap()
    .into_install();
    StorageHandle::enter(&mut storage, |handle| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, GENESIS_TS, chain_id),
            handle,
        );
        let owner = Address::repeat_byte(0xA0);
        let founder = Address::repeat_byte(0xB0);
        let consensus_key = [0x30; 48];
        let mut validators = ValidatorSet::new(ctx.storage.clone());
        validators.config_owner.write(owner).unwrap();
        validators.set_config_max_validators(1).unwrap();
        validators
            .register_validator(owner, founder, &consensus_key)
            .unwrap();
        validators.mark_pending(founder).unwrap();
        let registration = install.founder_registrations[0]
            .encode_canonical(&outbe_metadosis::config::poc_schema_limits())
            .unwrap();
        validators
            .confirm_validator_ready(founder, &registration)
            .unwrap();
        validators
            .activate_validator_via_boundary_for_test(founder)
            .unwrap();
        outbe_oracle::api::register_pair(ctx.storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_metadosis::commands::install_fork_profile(&ctx, &install).unwrap();
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage
}

fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

fn anchor_genesis(ctx: &BlockRuntimeContext) {
    outbe_rewards::runtime::ensure_genesis_anchor(ctx).unwrap();
}

/// seed V2 Phase 1 accounting progress so the dispatcher's
/// new gate (`last_accounted_block_number >= block_number - 1`) is
/// satisfied for tests that fire the trigger at `block_number >= 2`.
/// Mirrors what `apply_phase1_commit_in_preexec` records in production.
fn account_parent(ctx: &BlockRuntimeContext, block_number: u64) {
    if block_number >= 2 {
        outbe_accounting::record_phase1_progress(ctx, block_number - 1).unwrap();
    }
}

fn with_execution_scope(
    ctx: &BlockRuntimeContext,
    f: impl FnOnce(&ExecutionScope, &TributeRepositoryReader) -> outbe_primitives::error::Result<()>,
) -> outbe_primitives::error::Result<()> {
    ctx.storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))?;
    ctx.storage.sstore(
        COMPRESSED_ENTITIES_ADDRESS,
        U256::from(1),
        U256::from_be_slice(
            outbe_compressed_entities::sealed_root(B256::ZERO)
                .unwrap()
                .as_slice(),
        ),
    )?;
    let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let parent = TributeRepositoryReader::new(storage);
    let scope = ExecutionScope::new();
    let lifecycle = CompressedEntitiesLifecycleContext::new(ctx.clone(), &scope);
    <CompressedEntitiesLifecycle as BlockLifecycle>::begin_block(&lifecycle)?;
    let result = f(&scope, &parent);
    let cleanup =
        <CompressedEntitiesLifecycle as BlockLifecycle>::end_block(&lifecycle).map(|_| ());
    result.and(cleanup)
}

fn dispatch_triggers(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        crate::runtime::dispatch_triggers(ctx, scope, parent)
    })
}

fn run_cycle_lifecycle(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    run_cycle_lifecycle_at_activation(ctx, 1)
}

fn run_cycle_lifecycle_at_activation(
    ctx: &BlockRuntimeContext,
    metadosis_genesis_activation_height: u64,
) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        let lifecycle = CycleLifecycleContext::new(ctx.clone(), scope, parent)
            .with_metadosis_genesis_activation_height(metadosis_genesis_activation_height);
        <CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)
    })
}

fn run_cycle_lifecycle_with_hourly_activation(
    ctx: &BlockRuntimeContext,
    metadosis_hourly_activation_height: u64,
) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        let lifecycle = CycleLifecycleContext::new(ctx.clone(), scope, parent)
            .with_metadosis_hourly_activation_height(Some(metadosis_hourly_activation_height));
        <CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)
    })
}

fn run_emission_limit_daily(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        crate::handler::run_emission_limit_daily(ctx, scope, parent)
    })
}

fn run_emission_limit_hourly(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        crate::handler::run_metadosis_hourly(ctx, scope, parent)
    })
}

fn advance_metadosis_only(
    storage: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(storage, |handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(block_number, timestamp), handle);
        with_execution_scope(&ctx, |scope, _| {
            outbe_metadosis::commands::advance_active_worldwide_days(&ctx, scope)
        })
    })
}

// ---------------------------------------------------------------------------
// next_fire_at — pure scheduling math
// ---------------------------------------------------------------------------

#[test]
fn schedule_math_pinned_values() {
    // Daily, offset = 0 => first slot is `period_seconds`.
    assert_eq!(next_fire_at(86_400, 0, 0), 86_400);
    // Hourly @ :30 (offset = 1800), first slot at 1800.
    assert_eq!(next_fire_at(3_600, 1_800, 0), 1_800);
    // Hourly @ :30, last fired at 1800 => next at 5400.
    assert_eq!(next_fire_at(3_600, 1_800, 1_800), 5_400);
    // 5-minute, offset = 0, last fired at 299 => next at 300.
    assert_eq!(next_fire_at(300, 0, 299), 300);
    // 5-minute, offset = 0, last fired at 300 => next at 600.
    assert_eq!(next_fire_at(300, 0, 300), 600);
    // last well past first slot.
    assert_eq!(next_fire_at(86_400, 0, 86_400 * 5), 86_400 * 6);
}

#[test]
fn schedule_math_aligned_property() {
    // (next - offset) % period == 0 for arbitrary inputs.
    for &period in &[60u64, 300, 3_600, 86_400] {
        for &offset in &[0u64, 1, 7, period - 1] {
            for &last in &[0u64, 1, 100, 86_400, 86_400 * 365] {
                let next = next_fire_at(period, offset, last);
                assert!(next > last, "p={period} o={offset} l={last} n={next}");
                assert!(next >= offset);
                assert_eq!((next - offset) % period, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatcher: lazy first-encounter anchor + slot-based fire
// ---------------------------------------------------------------------------

#[test]
fn first_encounter_anchors_without_firing() {
    // First time the dispatcher sees the trigger, it anchors
    // `last_executed_at = block_ts` and skips firing. No event, no
    // handler invocation, no settle.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        dispatch_triggers(&ctx).unwrap();

        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts,
            "first encounter anchors at block timestamp"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            0,
            "no fire = no last_executed_block_number write"
        );
    });
}

#[test]
fn hourly_trigger_is_untouched_before_activation_and_only_anchors_at_activation() {
    const ACTIVATION_HEIGHT: u64 = 10;
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&anchor);
        run_cycle_lifecycle_with_hourly_activation(&anchor, ACTIVATION_HEIGHT).unwrap();

        let before = BlockRuntimeContext::new(
            block_ctx(9, GENESIS_TS + SECONDS_PER_DAY + 60),
            handle.clone(),
        );
        account_parent(&before, 9);
        run_cycle_lifecycle_with_hourly_activation(&before, ACTIVATION_HEIGHT).unwrap();

        let cycle: Cycle<'_> = before.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&METADOSIS_HOURLY_ID).unwrap(),
            0
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&METADOSIS_HOURLY_ID)
                .unwrap(),
            0
        );
        assert!(
            before
                .storage
                .contract::<outbe_rewards::schema::Rewards<'_>>()
                .daily_settled
                .read(&20_240_101)
                .unwrap(),
            "a staged future activation must preserve legacy settlement before X"
        );

        let activation_timestamp = GENESIS_TS + SECONDS_PER_DAY + 61 * 60;
        let activation =
            BlockRuntimeContext::new(block_ctx(ACTIVATION_HEIGHT, activation_timestamp), handle);
        account_parent(&activation, ACTIVATION_HEIGHT);
        run_cycle_lifecycle_with_hourly_activation(&activation, ACTIVATION_HEIGHT).unwrap();

        let cycle: Cycle<'_> = activation.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&METADOSIS_HOURLY_ID).unwrap(),
            activation_timestamp,
            "the new trigger anchors on its first active block"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&METADOSIS_HOURLY_ID)
                .unwrap(),
            0,
            "anchoring must not execute the hourly handler"
        );
    });
}

#[test]
fn hourly_settlement_integrity_failure_does_not_commit_the_due_slot() {
    const ACTIVATION_HEIGHT: u64 = 2;
    let previous_hour = GENESIS_TS + SECONDS_PER_DAY;
    let due_timestamp = previous_hour + 3_600 + 5;
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, due_timestamp), handle.clone());
        anchor_genesis(&ctx);
        account_parent(&ctx, 2);
        outbe_rewards::api::mark_day_settled(&ctx, 20_240_101).unwrap();

        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
        for spec in ACTIVE_TRIGGERS {
            cycle
                .last_executed_at
                .write(
                    &spec.id,
                    if spec.id == METADOSIS_HOURLY_ID {
                        previous_hour
                    } else {
                        due_timestamp
                    },
                )
                .unwrap();
        }

        assert!(matches!(
            run_cycle_lifecycle_with_hourly_activation(&ctx, ACTIVATION_HEIGHT),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));

        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&METADOSIS_HOURLY_ID).unwrap(),
            previous_hour,
            "a failed hourly handler must leave its due slot pending"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&METADOSIS_HOURLY_ID)
                .unwrap(),
            0
        );
    });
}

#[test]
fn activation_retires_legacy_domain_work_and_first_hour_runs_the_unified_flow() {
    const ACTIVATION_HEIGHT: u64 = 2;
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&anchor);
        run_cycle_lifecycle_with_hourly_activation(&anchor, ACTIVATION_HEIGHT).unwrap();

        let activation_timestamp = GENESIS_TS + SECONDS_PER_DAY + 60;
        let activation = BlockRuntimeContext::new(
            block_ctx(ACTIVATION_HEIGHT, activation_timestamp),
            handle.clone(),
        );
        account_parent(&activation, ACTIVATION_HEIGHT);
        run_cycle_lifecycle_with_hourly_activation(&activation, ACTIVATION_HEIGHT).unwrap();

        let rewards = activation
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            !rewards.daily_settled.read(&20_240_101).unwrap(),
            "the legacy midnight handler is a domain no-op after activation"
        );
        let cycle: Cycle<'_> = activation.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "the retired legacy trigger keeps its historical audit checkpoint"
        );
        assert_eq!(
            cycle.last_executed_at.read(&METADOSIS_HOURLY_ID).unwrap(),
            activation_timestamp,
            "the new trigger only anchors on the activation block"
        );

        let first_hour_timestamp = GENESIS_TS + SECONDS_PER_DAY + 3_600 + 5;
        let first_hour = BlockRuntimeContext::new(
            block_ctx(ACTIVATION_HEIGHT + 1, first_hour_timestamp),
            handle,
        );
        account_parent(&first_hour, ACTIVATION_HEIGHT + 1);
        run_cycle_lifecycle_with_hourly_activation(&first_hour, ACTIVATION_HEIGHT).unwrap();

        let rewards = first_hour
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "the first active hourly slot settles the previous UTC day"
        );
        assert!(matches!(
            outbe_metadosis::api::day_limit_formation_receipt(
                first_hour.storage.clone(),
                outbe_common::WorldwideDay::new(20_240_101),
            )
            .unwrap(),
            Some(outbe_metadosis::DayLimitFormationReceipt::Formed(_))
        ));
        let cycle: Cycle<'_> = first_hour.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&METADOSIS_HOURLY_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY + 3_600
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&METADOSIS_HOURLY_ID)
                .unwrap(),
            ACTIVATION_HEIGHT + 1
        );
    });
}

#[test]
fn block_1_begin_block_creates_genesis_worldwide_day() {
    // Production regression: at block 1 the daily Cycle trigger only anchors
    // (it never invokes `start_metadosis`), so `CycleLifecycle::begin_block`
    // must itself create the genesis metadosis worldwide day. Before the fix
    // the active-WWD set was empty until the first block past the next UTC
    // midnight.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        // Sanity: no worldwide day exists before begin_block.
        assert!(outbe_metadosis::api::has_active_ocomp_profile(ctx.storage.clone()).unwrap());
        assert!(
            outbe_metadosis::api::worldwide_days(ctx.storage.clone())
                .unwrap()
                .is_empty(),
            "no worldwide day should exist before block-1 begin_block"
        );

        run_cycle_lifecycle(&ctx).unwrap();

        assert!(
            !outbe_metadosis::api::worldwide_days(ctx.storage.clone())
                .unwrap()
                .is_empty(),
            "block-1 begin_block must create the genesis worldwide day"
        );

        // The daily trigger must only have anchored — no settlement fired.
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts,
            "daily trigger still only anchors on block 1"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            0,
            "daily settlement must not fire on block 1"
        );
    });
}

#[test]
fn block_1_begin_block_rejects_missing_genesis_ocomp_profile_without_partial_state() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        handle
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(3))
            .unwrap();
        handle
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        anchor_genesis(&ctx);
    });
    let storage_before = storage.storage.clone();
    let events_before = storage.events.clone();

    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        assert!(run_cycle_lifecycle(&ctx).is_err());
    });

    assert_eq!(storage.storage, storage_before);
    assert_eq!(storage.events, events_before);
}

#[test]
fn frozen_final_profile_initializes_metadosis_at_its_existing_activation_height() {
    let mut storage = cycle_storage();

    storage.enter(|handle| {
        let block_1 = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&block_1);
        run_cycle_lifecycle_at_activation(&block_1, 32).unwrap();

        assert!(
            outbe_metadosis::api::worldwide_days(block_1.storage.clone())
                .unwrap()
                .is_empty(),
            "the frozen Final/32 evidence profile must not initialize Metadosis at block 1"
        );

        let activation =
            BlockRuntimeContext::new(block_ctx(32, GENESIS_TS + 31 * 60), handle.clone());
        run_cycle_lifecycle_at_activation(&activation, 32).unwrap();

        assert!(
            !outbe_metadosis::api::worldwide_days(activation.storage.clone())
                .unwrap()
                .is_empty(),
            "the existing Final/32 evidence profile must initialize Metadosis after its fork install"
        );
    });
}

#[test]
fn does_not_fire_before_next_slot_after_anchor() {
    // Anchor at GENESIS_TS + 60. Next slot at 86_400 (UTC midnight
    // 1970-01-02 — already passed) → next > anchor → next slot is the
    // first multiple of 86_400 strictly greater than (GENESIS_TS + 60).
    // GENESIS_TS = 1_704_067_200 = 19723 * 86_400; +60 puts us in the
    // current slot, so next slot = 19724 * 86_400 = GENESIS_TS + 86_400.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at GENESIS_TS + 86_399 — still BEFORE next slot.
        let ctx_before = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + SECONDS_PER_DAY - 1),
            handle.clone(),
        );
        dispatch_triggers(&ctx_before).unwrap();

        let cycle: Cycle<'_> = ctx_before.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            anchor_ts,
            "trigger must not fire before the next slot"
        );
    });
}

#[test]
fn fires_at_first_block_past_next_slot() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Step 1: anchor.
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Step 2: block past next slot. Next slot after anchor =
        // ceil(anchor_ts / 86_400) * 86_400 = GENESIS_TS + 86_400.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "last_executed_at must be the slot, not block.timestamp"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&EMISSION_LIMIT_1_ID)
                .unwrap(),
            2
        );
    });
}

#[test]
fn does_not_refire_within_same_slot() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle.clone());
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();
        let after_first_fire = ctx_fire
            .storage
            .contract::<Cycle<'_>>()
            .last_executed_at
            .read(&EMISSION_LIMIT_1_ID)
            .unwrap();

        // Second block within the same slot.
        let ctx_again = BlockRuntimeContext::new(block_ctx(3, fire_ts + 30), handle);
        account_parent(&ctx_again, 3);
        dispatch_triggers(&ctx_again).unwrap();
        let cycle: Cycle<'_> = ctx_again.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            after_first_fire,
            "trigger must not refire within the same slot"
        );
    });
}

#[test]
fn multi_slot_gap_fires_only_for_latest_slot_after_anchor() {
    // Anchor, then jump 3 slots ahead in one block.
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Block at GENESIS_TS + 4 days — 3 slots crossed. next_fire_at
        // from anchor is the FIRST slot > anchor = GENESIS_TS + 86_400,
        // even though the block is 3 days later.
        let ctx_fire =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + 4 * SECONDS_PER_DAY), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY,
            "multi-slot gap fires once for the first slot strictly after anchor"
        );
    });
}

#[test]
fn cycle_lifecycle_begin_block_runs_dispatcher() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let block_ts = GENESIS_TS + 60;
        let ctx = BlockRuntimeContext::new(block_ctx(1, block_ts), handle);
        anchor_genesis(&ctx);

        run_cycle_lifecycle(&ctx).unwrap();

        // Same as `first_encounter_anchors_without_firing`: begin_block
        // delegates to dispatch_triggers.
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            block_ts
        );
    });
}

// ---------------------------------------------------------------------------
// auction_advance trigger
// ---------------------------------------------------------------------------

#[test]
fn auction_advance_runs_after_emission_limit_1() {
    use crate::triggers::ACTIVE_TRIGGERS;
    let position = |id: u32| {
        ACTIVE_TRIGGERS
            .iter()
            .position(|spec| spec.id == id)
            .expect("trigger registered")
    };
    assert!(
        position(TriggerId::AuctionAdvance.as_u32())
            > position(TriggerId::EmissionLimit1.as_u32()),
        "auction_advance must dispatch after emission_limit_1 so the same-slot brief starts the auction"
    );
}

#[test]
fn dispatcher_fires_auction_advance_at_its_slot() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 5;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        let auction_advance_id = TriggerId::AuctionAdvance.as_u32();
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&auction_advance_id).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY / 2,
            "fires the first 12h slot strictly after the anchor"
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&auction_advance_id)
                .unwrap(),
            2
        );
    });
}

// ---------------------------------------------------------------------------
// End-to-end: handler effects on Rewards, AgentReward, Metadosis
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_emission_dispatch_marks_day_settled_and_credits_metadosis() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Step 1: anchor at chain start.
        let anchor_ts = GENESIS_TS + 60;
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, anchor_ts), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        // Step 2: block past first slot. prev_day = genesis_utc_day
        // (20240101); day_number_since_genesis = 0; cap = INITIAL_DAY_EMISSION.
        let fire_ts = GENESIS_TS + SECONDS_PER_DAY + 60;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        dispatch_triggers(&ctx_fire).unwrap();

        // Rewards.daily_settled[20240101] = true (sealed against late
        // finalized metadata for the previous UTC day).
        let rewards = ctx_fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "Cycle handler must seal prev_day"
        );

        // Cycle's last_executed_at advanced to the slot
        // (GENESIS_TS + 86_400), not the block timestamp.
        let cycle: Cycle<'_> = ctx_fire.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle.last_executed_at.read(&EMISSION_LIMIT_1_ID).unwrap(),
            GENESIS_TS + SECONDS_PER_DAY
        );

        // No tributes for any AgentReward pool, so all three
        // WAA/SRA/CCA amounts are accounted for.
        // burn parity: WAA + SRA pools are pre-funded then burned in
        // their no-tribute branch; CCA lands on its own
        // accumulator address. AGENT_REWARD balance is therefore
        // zero (no claimable was credited).
        let agent_reward_balance = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::AGENT_REWARD_ADDRESS)
            .unwrap();
        assert_eq!(agent_reward_balance, U256::ZERO);

        // The CCA accumulator received its 4 %. The exact amount comes
        // from `day_emission_limit(0) * 4 / 100` which is fully covered
        // by emissionlimit pinned tests; here we only assert it is
        // non-zero.
        let cca = ctx_fire
            .storage
            .balance(outbe_primitives::addresses::CCA_ADDRESS)
            .unwrap();
        assert!(!cca.is_zero(), "CCA accumulator received its 4 %");
    });
}

/// a second `run_emission_limit_daily` invocation for an already-settled
/// `prev_day` is a no-op — the CCA agent pool (and terminal Metadosis)
/// are NOT minted twice. Guards the per-day idempotency added on top of the
/// C-01 timestamp drift band.
#[test]
fn emission_dispatch_is_idempotent_per_prev_day() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&ctx_anchor);
        dispatch_triggers(&ctx_anchor).unwrap();

        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&ctx, 2);

        // First settlement of prev_day = 20240101: mints the pools + seals.
        run_emission_limit_daily(&ctx).unwrap();
        let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
        assert!(
            rewards.daily_settled.read(&20_240_101).unwrap(),
            "first fire must seal prev_day"
        );
        let cca_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::CCA_ADDRESS)
            .unwrap();
        let metadosis_after_first = ctx
            .storage
            .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
            .unwrap();
        assert!(!cca_after_first.is_zero(), "first fire credited CCA");

        // Second invocation for the SAME prev_day: the idempotency guard sees
        // `daily_settled[20240101] == true` and returns early — no double-mint.
        run_emission_limit_daily(&ctx).unwrap();
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::CCA_ADDRESS)
                .unwrap(),
            cca_after_first,
            "CCA pool must not be minted twice for the same prev_day"
        );
        assert_eq!(
            ctx.storage
                .balance(outbe_primitives::addresses::METADOSIS_ADDRESS)
                .unwrap(),
            metadosis_after_first,
            "terminal Metadosis must not be re-dispatched for the same prev_day"
        );
    });
}

#[test]
fn hourly_flow_skips_settled_emission_but_still_runs_wwd_lifecycle() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let previous_day_ctx =
            BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        outbe_metadosis::commands::apply_cycle_day_limit(&previous_day_ctx, U256::from(17_u8))
            .unwrap();
        outbe_rewards::api::mark_day_settled(&previous_day_ctx, 20_240_101).unwrap();

        let days_before = outbe_metadosis::api::worldwide_days(handle.clone()).unwrap();
        assert_eq!(days_before.len(), 1, "only the settled previous WWD exists");
        let cca_before = handle
            .balance(outbe_primitives::addresses::CCA_ADDRESS)
            .unwrap();

        let hourly_ctx = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 3_600 + 5),
            handle,
        );
        run_emission_limit_hourly(&hourly_ctx).unwrap();

        let days_after = outbe_metadosis::api::worldwide_days(hourly_ctx.storage.clone()).unwrap();
        assert!(
            days_after.len() > days_before.len(),
            "an already-settled day must skip only allocation and still create the current WWD"
        );
        assert_eq!(
            hourly_ctx
                .storage
                .balance(outbe_primitives::addresses::CCA_ADDRESS)
                .unwrap(),
            cca_before,
            "the settled previous day must not mint its allocation twice"
        );
    });
}

#[test]
fn hourly_flow_processes_one_ready_wwd_with_its_persisted_day_limit() {
    let ready_day = outbe_common::WorldwideDay::new(20_240_101);
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(
            handle.clone(),
            &[ready_day],
        )
        .unwrap();
        let previous_day_ctx =
            BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        outbe_metadosis::commands::apply_cycle_day_limit(&previous_day_ctx, U256::from(17_u8))
            .unwrap();
        outbe_rewards::api::mark_day_settled(&previous_day_ctx, ready_day.value()).unwrap();

        let before = outbe_metadosis::api::worldwide_days(handle.clone())
            .unwrap()
            .into_iter()
            .find(|day| day.worldwide_day == ready_day)
            .expect("READY WWD fixture");
        assert_eq!(before.status, outbe_metadosis::WwdStatus::Ready);
        assert_eq!(before.metadosis_limit_amount, U256::from(17_u8));

        let hourly_ctx = BlockRuntimeContext::new(
            block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 3_600 + 5),
            handle,
        );
        run_emission_limit_hourly(&hourly_ctx).unwrap();

        let after = outbe_metadosis::api::worldwide_days(hourly_ctx.storage.clone())
            .unwrap()
            .into_iter()
            .find(|day| day.worldwide_day == ready_day)
            .expect("processed WWD remains in the bounded terminal set");
        assert_eq!(after.status, outbe_metadosis::WwdStatus::Completed);
        assert_eq!(after.metadosis_limit_amount, U256::from(17_u8));
    });
}

#[test]
fn repeated_settled_cycle_slot_replays_without_any_storage_or_event_write() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let anchor = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle.clone());
        anchor_genesis(&anchor);
        dispatch_triggers(&anchor).unwrap();

        let fire =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        account_parent(&fire, 2);
        run_emission_limit_daily(&fire).unwrap();
        assert!(matches!(
            outbe_metadosis::api::day_limit_formation_receipt(
                fire.storage.clone(),
                outbe_common::WorldwideDay::new(20_240_101),
            )
            .unwrap(),
            Some(outbe_metadosis::DayLimitFormationReceipt::Formed(_))
        ));
    });
    let storage_after_first = storage.storage.clone();
    let events_after_first = storage.events.clone();

    storage.enter(|handle| {
        let replay =
            BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        run_emission_limit_daily(&replay).unwrap();
    });
    assert_eq!(storage.storage, storage_after_first);
    assert_eq!(storage.events, events_after_first);
}

#[test]
fn settled_cycle_marker_without_metadosis_semantic_receipt_is_fatal() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        outbe_rewards::api::mark_day_settled(&ctx, 20_240_101).unwrap();
        assert!(matches!(
            run_emission_limit_daily(&ctx),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });
}

#[test]
fn metadosis_semantic_receipt_without_settled_cycle_marker_is_fatal_before_effects() {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
        outbe_metadosis::commands::apply_cycle_day_limit(&ctx, U256::from(17_u8)).unwrap();
    });
    let storage_before = storage.storage.clone();
    let events_before = storage.events.clone();

    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(2, GENESIS_TS + SECONDS_PER_DAY + 60), handle);
        let parent_storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
        let parent = TributeRepositoryReader::new(parent_storage);
        let scope = ExecutionScope::new();
        assert!(matches!(
            crate::handler::run_emission_limit_daily(&ctx, &scope, &parent),
            Err(outbe_primitives::error::PrecompileError::Fatal(_))
        ));
    });

    assert_eq!(storage.storage, storage_before);
    assert_eq!(storage.events, events_before);
}

#[test]
fn noon_dispatcher_commits_the_same_typed_missed_offering_outcome() {
    let wwd = outbe_common::WorldwideDay::new(20_240_105);
    let day_limit = U256::from(109);
    let mut storage = cycle_storage();
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    let offering_end = storage.enter(|handle| {
        let formation_ctx = BlockRuntimeContext::new(
            block_ctx(10, wwd.start_timestamp() + 2 * 3_600),
            handle.clone(),
        );
        outbe_metadosis::commands::apply_cycle_day_limit(&formation_ctx, day_limit).unwrap();
        outbe_metadosis::api::worldwide_day(handle, wwd)
            .unwrap()
            .unwrap()
            .offering_end
    });
    let noon_offset = 43_200;
    let fire_at = if offering_end <= noon_offset {
        noon_offset
    } else {
        noon_offset + (offering_end - noon_offset).div_ceil(SECONDS_PER_DAY) * SECONDS_PER_DAY
    };
    let previous_noon = fire_at - SECONDS_PER_DAY;

    storage.enter(|handle| {
        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
        for spec in ACTIVE_TRIGGERS {
            let last = if spec.id == TriggerId::WwdAdvanceNoon.as_u32() {
                previous_noon
            } else {
                fire_at
            };
            cycle.last_executed_at.write(&spec.id, last).unwrap();
        }
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(20, fire_at), handle.clone());
        dispatch_triggers(&ctx).unwrap();

        let projection = outbe_metadosis::api::worldwide_day(ctx.storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
        assert_eq!(
            projection.membership,
            outbe_metadosis::WwdMembership::Closed
        );
        let receipt = outbe_metadosis::api::missed_offering_receipt(ctx.storage.clone(), wwd)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.value_routed, day_limit);
        assert_eq!(receipt.carry_over_before, U256::ZERO);
        assert_eq!(receipt.carry_over_after, day_limit);
        assert_eq!(
            receipt.retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );
        assert_eq!(receipt.block_number, 20);

        let desis = ctx.storage.contract::<outbe_desis::schema::DesisContract>();
        assert_eq!(
            desis.auction_stage.read(&wwd).unwrap(),
            outbe_desis::schema::AuctionStage::None as u8
        );
        let cycle: Cycle<'_> = ctx.storage.contract::<Cycle<'_>>();
        assert_eq!(
            cycle
                .last_executed_at
                .read(&TriggerId::WwdAdvanceNoon.as_u32())
                .unwrap(),
            fire_at
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&TriggerId::WwdAdvanceNoon.as_u32())
                .unwrap(),
            20
        );
    });
}

#[test]
fn noon_dispatcher_applies_exact_capacity_forfeiture_to_the_new_due_candidate() {
    use outbe_common::WorldwideDay;
    use outbe_metadosis::constants::MAX_RETAINED_WWDS;

    let victim = WorldwideDay::new(20_260_910);
    let retained = retained_days_before(victim, MAX_RETAINED_WWDS);
    let day_limit = U256::from(100);
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        outbe_tribute::TributeContract::new(handle)
            .initialize_fresh_ocomp_profile()
            .unwrap();
    });

    storage.enter(|handle| {
        outbe_metadosis::test_support::seed_ready_worldwide_days_for_capacity(handle, &retained)
            .unwrap();
    });

    let mut next_block = 2_u64;
    storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    let victim_projection = storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(
            block_ctx(next_block, victim.start_timestamp() + 2 * 3_600),
            handle.clone(),
        );
        outbe_metadosis::commands::apply_cycle_day_limit(&ctx, day_limit).unwrap();
        outbe_metadosis::api::worldwide_day(handle, victim)
            .unwrap()
            .unwrap()
    });
    next_block += 1;
    for boundary in [
        victim_projection.forming_end,
        victim_projection.lookback_end,
        victim_projection.offering_end,
    ] {
        advance_metadosis_only(&mut storage, next_block, boundary).unwrap();
        next_block += 1;
    }

    let noon_offset = 43_200;
    let fire_at = noon_offset
        + victim_projection
            .scheduled_process_time
            .saturating_sub(noon_offset)
            .div_ceil(SECONDS_PER_DAY)
            * SECONDS_PER_DAY;
    let previous_noon = fire_at - SECONDS_PER_DAY;
    storage.enter(|handle| {
        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
        for spec in ACTIVE_TRIGGERS {
            cycle
                .last_executed_at
                .write(
                    &spec.id,
                    if spec.id == TriggerId::WwdAdvanceNoon.as_u32() {
                        previous_noon
                    } else {
                        fire_at
                    },
                )
                .unwrap();
        }
    });

    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block_ctx(next_block, fire_at), handle.clone());
        account_parent(&ctx, next_block);
        dispatch_triggers(&ctx).unwrap();

        let projection = outbe_metadosis::api::worldwide_day(handle.clone(), victim)
            .unwrap()
            .unwrap();
        assert_eq!(projection.status, outbe_metadosis::WwdStatus::Failed);
        assert_eq!(
            projection.membership,
            outbe_metadosis::WwdMembership::Closed
        );

        let receipt = outbe_metadosis::api::capacity_forfeiture_receipt(handle.clone(), victim)
            .unwrap()
            .unwrap();
        assert_eq!(receipt.max_retained_wwds, MAX_RETAINED_WWDS as u32);
        assert_eq!(receipt.retained_count_before, MAX_RETAINED_WWDS as u32);
        assert_eq!(receipt.value_routed, day_limit);
        assert_eq!(receipt.carry_over_before, U256::ZERO);
        assert_eq!(receipt.carry_over_after, day_limit);
        assert_eq!(receipt.forfeited_count, 0);
        assert_eq!(receipt.forfeited_nominal, U256::ZERO);
        assert_eq!(
            receipt.retirement,
            outbe_compressed_entities::RetirementOutcome::NotPresent
        );

        let cycle: Cycle<'_> = handle.contract::<Cycle<'_>>();
        assert_eq!(
            cycle
                .last_executed_at
                .read(&TriggerId::WwdAdvanceNoon.as_u32())
                .unwrap(),
            fire_at
        );
        assert_eq!(
            cycle
                .last_executed_block_number
                .read(&TriggerId::WwdAdvanceNoon.as_u32())
                .unwrap(),
            next_block
        );
    });
}

#[test]
fn genesis_midday_first_cycle_at_next_midnight_settles_genesis_day() {
    // Genesis at 10:00 UTC on day D. First CycleTick fires at 00:00:01 UTC
    // on day D+1. prev_day = D = genesis_utc_day → day_number = 0 → Ok.
    // This is the production scenario that was broken when genesis_utc_day
    // was derived from block 0 timestamp at 10:00 instead of genesisTime.
    const DAY_D_MIDNIGHT: u64 = GENESIS_TS; // 2024-01-01 00:00:00
    const DAY_D_10AM: u64 = DAY_D_MIDNIGHT + 10 * 3600; // 10:00 UTC

    let mut storage = cycle_storage();
    storage.enter(|handle| {
        // Block 1 at 10:00 — genesis anchor records genesis_utc_day = day D.
        let ctx_anchor = BlockRuntimeContext::new(block_ctx(1, DAY_D_10AM), handle.clone());
        anchor_genesis(&ctx_anchor);
        run_cycle_lifecycle(&ctx_anchor).unwrap();
        let canonical_wwd = outbe_common::WorldwideDay::new(20_240_102);
        assert_eq!(
            outbe_metadosis::api::worldwide_days(ctx_anchor.storage.clone())
                .unwrap()
                .into_iter()
                .map(|day| day.worldwide_day)
                .collect::<Vec<_>>(),
            vec![canonical_wwd],
            "block 1 at the UTC+14 boundary must create only the canonical WWD"
        );

        // Block at 00:00:01 UTC day D+1 — CycleTick fires.
        // prev_day = D = genesis_utc_day → day_number_since_genesis = 0.
        let fire_ts = DAY_D_MIDNIGHT + SECONDS_PER_DAY + 1;
        let ctx_fire = BlockRuntimeContext::new(block_ctx(2, fire_ts), handle);
        account_parent(&ctx_fire, 2);
        run_cycle_lifecycle(&ctx_fire).unwrap();

        assert_eq!(
            outbe_metadosis::api::worldwide_days(ctx_fire.storage.clone())
                .unwrap()
                .into_iter()
                .map(|day| day.worldwide_day)
                .collect::<Vec<_>>(),
            vec![
                outbe_common::WorldwideDay::new(20_240_101),
                canonical_wwd,
            ],
            "the first UTC midnight must retain the canonical genesis WWD and add only the explicitly settled previous UTC day"
        );

        let rewards = ctx_fire
            .storage
            .contract::<outbe_rewards::schema::Rewards<'_>>();
        let genesis_day = rewards.genesis_utc_day.read().unwrap();
        assert_eq!(genesis_day, 20_240_101, "genesis_utc_day = day D");
        assert!(
            rewards.daily_settled.read(&genesis_day).unwrap(),
            "CycleTick must settle genesis day (day_number=0)"
        );
    });
}
