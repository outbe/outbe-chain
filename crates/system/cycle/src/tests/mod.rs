//! Cycle dispatcher tests.
//!
//! The `schedule_math_*` tests assert `next_fire_at`. Integration tests
//! exercise the dispatcher loop against the `HashMapStorageProvider`
//! so they cover the storage round-trip (`Cycle.last_executed_at`)
//! and the genesis-anchor interaction with Rewards.
//!
//! The dispatcher uses a lazy first-encounter anchor: on the very
//! first block it sees a trigger, it writes
//! `last_executed_at = block_ts` instead of firing. This anchors the
//! schedule at the chain's deployment instant. The first real fire then
//! happens at the *next* slot strictly after that anchor. Without
//! this, every chain would fire its daily trigger on block 1 because
//! `block_ts >> 86_400` is always true on a real chain.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{MetadosisMutationPurposeTag, StorageHandle};
use outbe_validatorset::contract::ValidatorSet;

use crate::lifecycle::{CycleLifecycle, CycleLifecycleContext};
use crate::schema::Cycle;
use crate::triggers::{last_fire_at, next_fire_at, TriggerId, ACTIVE_TRIGGERS};

pub(super) const CHAIN_ID: u64 = 1;
/// Genesis at midnight UTC of 2024-01-01.
pub(super) const GENESIS_TS: u64 = 1_704_067_200;
pub(super) use outbe_primitives::time::SECONDS_PER_DAY;
pub(super) const EMISSION_LIMIT_1_ID: u32 = TriggerId::ProtocolCycle.as_u32();

pub(super) fn cycle_storage() -> HashMapStorageProvider {
    cycle_storage_for(CHAIN_ID)
}

pub(super) fn cycle_storage_for(chain_id: u64) -> HashMapStorageProvider {
    let genesis_hash = B256::repeat_byte(0x11);
    let mut storage = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    storage.set_block_number(1);
    storage.enter(|handle| {
        let ctx = runtime_context(
            BlockContext::empty_for_tests(1, GENESIS_TS, chain_id),
            handle,
        );
        initialize_daily_passes(&ctx);
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
        validators.confirm_validator_ready(founder).unwrap();
        validators
            .activate_validator_via_boundary_for_test(founder)
            .unwrap();
        outbe_oracle::api::register_pair(ctx.storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
    });
    storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    storage
}

pub(super) fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

/// Synchronizes provider time; callers set provider height before entering each block.
pub(super) fn runtime_context(
    block: BlockContext,
    storage: StorageHandle<'_>,
) -> BlockRuntimeContext<'_> {
    storage
        .set_block_timestamp(U256::from(block.timestamp))
        .unwrap();
    BlockRuntimeContext::new(block, storage)
}

pub(super) fn anchor_genesis(ctx: &BlockRuntimeContext) {
    outbe_rewards::runtime::ensure_genesis_anchor(ctx).unwrap();
}

pub(super) fn seed_fresh_reward_oracle(ctx: &BlockRuntimeContext) {
    outbe_oracle::api::set_exchange_rate(
        ctx.storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        outbe_oracle::api::RateObservation {
            rate: U256::from(2_000_000u64),
            block_number: ctx.block.block_number,
            timestamp: ctx.block.timestamp,
        },
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
    )
    .expect("date step");
    oracle
        .record_utc_day_vwap(day, index, U256::from(2_000_000u64))
        .unwrap();
}

pub(super) fn seed_daily_voters(ctx: &BlockRuntimeContext, day: u32, voters: &[(Address, u64)]) {
    let rewards = ctx.storage.contract::<outbe_rewards::schema::Rewards<'_>>();
    let voter_at = rewards.daily_voter_at.get_nested(&day);
    let participation = rewards.daily_participation.get_nested(&day);
    for (index, (voter, count)) in voters.iter().enumerate() {
        voter_at.write(&(index as u32), *voter).unwrap();
        participation.write(voter, *count).unwrap();
    }
    rewards
        .daily_voter_count
        .write(&day, voters.len() as u32)
        .unwrap();
}

/// Writes the parent height into the accounting slot for tests that fire a
/// trigger at `block_number >= 2`. This is fixture setup. It does not call
/// the progress recorder.
pub(super) fn account_parent(ctx: &BlockRuntimeContext, block_number: u64) {
    if block_number >= 2 {
        ctx.storage
            .contract::<crate::schema::Accounting<'_>>()
            .last_accounted_block_number
            .write(block_number - 1)
            .unwrap();
    }
}

pub(super) fn dispatch_triggers(ctx: &BlockRuntimeContext) -> outbe_primitives::error::Result<()> {
    crate::runtime::dispatch_triggers(ctx)
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
    let lifecycle = CycleLifecycleContext::new(ctx.clone())
        .with_metadosis_genesis_activation_height(metadosis_genesis_activation_height);
    <CycleLifecycle as BlockLifecycle>::begin_block(&lifecycle)
}

pub(super) fn run_emission_limit_daily(
    ctx: &BlockRuntimeContext,
) -> outbe_primitives::error::Result<()> {
    crate::handler::run_emission_limit_daily(ctx)
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
    outbe_ccaregistry::api::credis_issued(storage, cca, 20240101, U256::ONE).unwrap();
}

/// Runs the first dispatcher block (block 1 at `timestamp`) after the genesis
/// anchor. Every trigger anchors at `timestamp` without firing.
pub(super) fn anchor_at(handle: StorageHandle<'_>, timestamp: u64) -> BlockRuntimeContext<'_> {
    anchor_with(handle, timestamp, |_| {})
}

/// [`anchor_at`] with `seed` run on the anchor block after the genesis anchor
/// and before the dispatcher.
pub(super) fn anchor_with<'s>(
    handle: StorageHandle<'s>,
    timestamp: u64,
    seed: impl FnOnce(&BlockRuntimeContext<'s>),
) -> BlockRuntimeContext<'s> {
    let ctx = genesis_block(handle, timestamp);
    seed(&ctx);
    dispatch_triggers(&ctx).unwrap();
    ctx
}

/// Runs the dispatcher at `block_number` and `timestamp` after Phase 1
/// accounts the parent block.
pub(super) fn dispatch_at(
    handle: StorageHandle<'_>,
    block_number: u64,
    timestamp: u64,
) -> BlockRuntimeContext<'_> {
    let ctx = runtime_context(block_ctx(block_number, timestamp), handle);
    account_parent(&ctx, block_number);
    dispatch_triggers(&ctx).unwrap();
    ctx
}

/// Day-0 emission allocation of `sink`.
pub(super) fn day_zero_allocation(sink: outbe_emissionlimit::allocation::EmissionSinkId) -> U256 {
    outbe_emissionlimit::allocation::allocate_emission(
        outbe_emissionlimit::day_emission::day_emission_limit(0),
    )
    .unwrap()
    .iter()
    .find(|allocation| allocation.id == sink)
    .unwrap()
    .amount
}

/// Block time of the dispatcher anchor in [`with_anchored_cycle`].
pub(super) const ANCHOR_TS: u64 = GENESIS_TS + 60;

/// Runs `f` on fresh Cycle storage after the dispatcher anchored at
/// [`ANCHOR_TS`].
pub(super) fn with_anchored_cycle(f: impl FnOnce(StorageHandle<'_>)) {
    let mut storage = cycle_storage();
    storage.enter(|handle| {
        anchor_at(handle.clone(), ANCHOR_TS);
    });
    storage.set_block_number(2);
    storage.enter(f);
}

/// `last_executed_at` of the ProtocolCycle trigger.
pub(super) fn last_executed_at(ctx: &BlockRuntimeContext<'_>) -> u64 {
    ctx.storage
        .contract::<Cycle<'_>>()
        .last_executed_at
        .read(&EMISSION_LIMIT_1_ID)
        .unwrap()
}

/// Block 1 at `timestamp` after the Rewards genesis anchor.
pub(super) fn genesis_block(handle: StorageHandle<'_>, timestamp: u64) -> BlockRuntimeContext<'_> {
    let ctx = runtime_context(block_ctx(1, timestamp), handle);
    anchor_genesis(&ctx);
    initialize_daily_passes(&ctx);
    ctx
}

pub(super) fn initialize_daily_passes(ctx: &BlockRuntimeContext<'_>) {
    outbe_nodfactory::called::initialize_daily(ctx).unwrap();
    outbe_gemfactory::called::initialize_daily(ctx).unwrap();
    outbe_intexfactory::called::initialize_daily(ctx).unwrap();
    outbe_credisfactory::called::initialize_daily(ctx).unwrap();
}

/// Runs `run` on `storage` and asserts that it writes no storage slot and
/// emits no event.
pub(super) fn assert_storage_unchanged(
    storage: &mut HashMapStorageProvider,
    run: impl FnOnce(StorageHandle<'_>),
) {
    let storage_before = storage.storage.clone();
    let events_before = storage.events.clone();
    storage.enter(run);
    assert_eq!(storage.storage, storage_before);
    assert_eq!(storage.events, events_before);
}
