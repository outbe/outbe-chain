//! A validator reward Gem is priced by the UTC day before its delivery, not the day it rewards.

use alloy_primitives::{Address, U256};
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::hashmap::HashMapStorageProvider,
};
use outbe_rewards::api::{
    deliver_oldest_reward_gem_batch, prepare_daily_validator_gem_batch, RewardGemDeliveryOutcome,
};

const CHAIN_ID: u64 = 1;
const GENESIS_TS: u64 = 1_704_067_200;
const REWARD_DAY: u32 = 20_240_101;
const DELIVERY_PREVIOUS_DAY: u32 = 20_240_103;
const VOTER: Address = Address::repeat_byte(0x5a);
const LOAD: u64 = 90;

fn one_coen840() -> U256 {
    U256::from(1_000_000u64)
}

/// Registers COEN/840 and publishes `live_quote` on it.
fn seed_oracle(ctx: &BlockRuntimeContext, live_quote: U256) {
    outbe_oracle::api::register_pair(ctx.storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        ctx.storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        live_quote,
        ctx.block.block_number,
        ctx.block.timestamp,
    )
    .unwrap();
    outbe_oracle::schema::OracleContract::new(ctx.storage.clone())
        .reference_currencies
        .push(840u16)
        .unwrap();
}

/// Publishes `vwap` as `day`'s finalized COEN/840 VWAP.
fn seed_day_vwap(ctx: &BlockRuntimeContext, day: u32, vwap: U256) {
    let index = outbe_oracle::api::coen_pair_index_opt(ctx.storage.clone(), 840)
        .unwrap()
        .expect("COEN/840 registered");
    outbe_oracle::schema::OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_value
        .get_nested(&day)
        .write(&index, vwap)
        .unwrap();
}

fn prepare(ctx: &BlockRuntimeContext) {
    prepare_daily_validator_gem_batch(ctx, REWARD_DAY, U256::from(LOAD), &[(VOTER, 1)]).unwrap();
}

/// The entry price the voter's delivered gem carries.
fn delivered_entry_price(ctx: &BlockRuntimeContext) -> U256 {
    let gem = outbe_gem::GemContract::new(ctx.storage.clone());
    let gem_id = gem.token_of_owner_by_index(VOTER, 0).unwrap();
    outbe_gem::api::get_gem(&ctx.storage, gem_id)
        .unwrap()
        .unwrap()
        .entry_price_minor
}

/// Anchors genesis on the reward day, then runs `f` in a block three days later.
fn with_ctx<R>(f: impl FnOnce(&BlockRuntimeContext) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        let genesis = BlockRuntimeContext::new(
            BlockContext::new(1, GENESIS_TS + 60, CHAIN_ID, Address::ZERO, Vec::new()),
            handle.clone(),
        );
        outbe_rewards::runtime::ensure_genesis_anchor(&genesis).unwrap();
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(
                2,
                GENESIS_TS + 3 * 86_400 + 60,
                CHAIN_ID,
                Address::ZERO,
                Vec::new(),
            ),
            handle,
        );
        f(&ctx)
    })
}

#[test]
fn a_batch_prices_off_the_day_before_its_delivery() {
    with_ctx(|ctx| {
        // The reward day, the day before delivery and the live quote all differ,
        // so only the day before delivery satisfies the assertion.
        seed_oracle(ctx, U256::from(9u64) * one_coen840());
        seed_day_vwap(ctx, REWARD_DAY, U256::from(2u64) * one_coen840());
        seed_day_vwap(ctx, DELIVERY_PREVIOUS_DAY, U256::from(5u64) * one_coen840());

        prepare(ctx);
        deliver_oldest_reward_gem_batch(ctx).unwrap();

        assert_eq!(delivered_entry_price(ctx), U256::from(5u64) * one_coen840());
    });
}

#[test]
fn a_batch_waits_while_the_day_before_its_delivery_has_no_vwap() {
    with_ctx(|ctx| {
        // The live quote is no fallback.
        seed_oracle(ctx, U256::from(9u64) * one_coen840());
        seed_day_vwap(ctx, REWARD_DAY, U256::from(2u64) * one_coen840());

        prepare(ctx);
        assert!(matches!(
            deliver_oldest_reward_gem_batch(ctx).unwrap(),
            RewardGemDeliveryOutcome::PendingRate {
                reward_utc_day: REWARD_DAY
            }
        ));
        assert_eq!(
            outbe_gem::GemContract::new(ctx.storage.clone())
                .balance_of(VOTER)
                .unwrap(),
            0
        );
    });
}
