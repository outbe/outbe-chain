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

pub(super) const CHAIN_ID: u64 = 1;
/// Genesis at midnight UTC of 2024-01-01.
pub(super) const GENESIS_TS: u64 = 1_704_067_200;
pub(super) const SECONDS_PER_DAY: u64 = 86_400;
pub(super) const EMISSION_LIMIT_1_ID: u32 = TriggerId::ProtocolCycle.as_u32();

pub(super) fn retained_days_before(
    victim: outbe_primitives::time::WorldwideDay,
    count: usize,
) -> Vec<outbe_primitives::time::WorldwideDay> {
    (0..count)
        .map(|offset| {
            let days_before = count - offset;
            let seconds_before = u64::try_from(days_before)
                .unwrap()
                .checked_mul(SECONDS_PER_DAY)
                .unwrap();
            outbe_primitives::time::WorldwideDay::from_timestamp(
                victim
                    .start_timestamp()
                    .checked_sub(seconds_before)
                    .unwrap(),
            )
        })
        .collect()
}

pub(super) fn cycle_storage() -> HashMapStorageProvider {
    cycle_storage_for(CHAIN_ID)
}

pub(super) fn cycle_storage_for(chain_id: u64) -> HashMapStorageProvider {
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
        ctx.storage
            .contract::<Cycle<'_>>()
            .active_utc_day
            .write(20_240_101)
            .unwrap();
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

pub(super) fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

pub(super) fn anchor_genesis(ctx: &BlockRuntimeContext) {
    outbe_rewards::runtime::ensure_genesis_anchor(ctx).unwrap();
}

pub(super) fn seed_fresh_reward_oracle(ctx: &BlockRuntimeContext) {
    outbe_oracle::api::set_exchange_rate(
        ctx.storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        U256::from(2_000_000u64),
        ctx.block.block_number,
        ctx.block.timestamp,
    )
    .unwrap();
    let oracle = ctx
        .storage
        .contract::<outbe_oracle::schema::OracleContract<'_>>();
    oracle.reference_currencies.push(840).unwrap();
    oracle
        .utc_day_vwap_last_finalized
        .write(29_991_231)
        .unwrap();
    let (_, index) = outbe_oracle::api::require_coen_pair(ctx.storage.clone(), 840).unwrap();
    let day = outbe_primitives::time::previous_date_key(
        outbe_primitives::time::timestamp_to_date_key(ctx.block.timestamp),
    );
    oracle
        .record_utc_day_vwap(day, index, U256::from(2_000_000u64))
        .unwrap();
}

pub(super) fn seed_daily_voters(ctx: &BlockRuntimeContext, day: u32, voters: &[(Address, u64)]) {
    let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
    let voter_at = rewards.daily_voter_at.get_nested(&day);
    let participation = rewards.daily_participation.get_nested(&day);
    let mut total = 0u64;
    for (index, (voter, count)) in voters.iter().enumerate() {
        voter_at.write(&(index as u32), *voter).unwrap();
        participation.write(voter, *count).unwrap();
        total = total.checked_add(*count).unwrap();
    }
    rewards
        .daily_voter_count
        .write(&day, voters.len() as u32)
        .unwrap();
    rewards
        .daily_total_participation
        .write(&day, total)
        .unwrap();
}

/// seed V2 Phase 1 accounting progress so the dispatcher's
/// new gate (`last_accounted_block_number >= block_number - 1`) is
/// satisfied for tests that fire the trigger at `block_number >= 2`.
/// Mirrors what `apply_phase1_commit_in_preexec` records in production.
pub(super) fn account_parent(ctx: &BlockRuntimeContext, block_number: u64) {
    if block_number >= 2 {
        outbe_accounting::record_phase1_progress(ctx, block_number - 1).unwrap();
    }
}

pub(super) fn with_execution_scope(
    ctx: &BlockRuntimeContext,
    f: impl FnOnce(&ExecutionScope, &TributeRepositoryReader) -> outbe_primitives::error::Result<()>,
) -> outbe_primitives::error::Result<()> {
    ctx.storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))?;
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

pub(super) fn dispatch_triggers(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        crate::runtime::dispatch_triggers(ctx, scope, parent)
    })
}

pub(super) fn run_cycle_lifecycle(
    ctx: &BlockRuntimeContext,
) -> outbe_primitives::error::Result<()> {
    run_cycle_lifecycle_at_activation(ctx, 1)
}

pub(super) fn run_cycle_lifecycle_at_activation(
    ctx: &BlockRuntimeContext,
    metadosis_genesis_activation_height: u64,
) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        let lifecycle = CycleLifecycleContext::new(ctx.clone(), scope, parent)
            .with_metadosis_genesis_activation_height(metadosis_genesis_activation_height);
        <CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)
    })
}

pub(super) fn run_emission_limit_daily(
    ctx: &BlockRuntimeContext,
) -> outbe_primitives::error::Result<()> {
    with_execution_scope(ctx, |scope, parent| {
        crate::handler::run_emission_limit_daily(ctx, scope, parent)
    })
}

pub(super) fn advance_metadosis_only(
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

mod calendar;
mod dispatch;
mod genesis_nod;
mod hourly;
mod schedule;

pub(super) fn seed_reward_cca(storage: &outbe_primitives::storage::StorageHandle<'_>) {
    let cca = Address::repeat_byte(0xc1);
    storage
        .increase_balance(
            outbe_primitives::addresses::CCA_REGISTRY_ADDRESS,
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
        )
        .unwrap();
    outbe_ccaregistry::runtime::bond(
        storage.clone(),
        cca,
        outbe_ccaregistry::constants::BOND_REQUIREMENT,
        "Test CCA".into(),
    )
    .unwrap();
    outbe_ccaregistry::api::position_opened(storage, cca, 20240101, U256::ONE).unwrap();
}
