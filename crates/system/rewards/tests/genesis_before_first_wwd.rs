//! Genesis reward Gems keep a zero floor. The issuance call records the privilege
//! when no Worldwide Day has ever been created; a later block re-read keeps it
//! after that day is created. Creation earlier on the same UTC day, issuance on
//! a later day, and issuance after the retained day is deleted all leave the Gem
//! waiting: a missing bit stays unqualified. Entry, cost and call stay on the
//! ordinary issuance formulas.

use alloy_primitives::{Address, U256};
use outbe_metadosis::genesis::{FreshDevnetGenesisBuilder, GenesisWorldwideDay};
use outbe_metadosis::{api as metadosis_api, WwdDayType, WwdStatus};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::hashmap::HashMapStorageProvider,
};
use outbe_rewards::api::{
    deliver_oldest_reward_gem_batch, prepare_daily_validator_gem_batch, RewardGemDeliveryOutcome,
};
use outbe_rewards::runtime::ensure_genesis_anchor;

const CHAIN_ID: u64 = 1;
const GENESIS_TS: u64 = 1_704_067_200;
const REWARD_DAY: u32 = 20_240_101;
const DELIVERY_TS: u64 = GENESIS_TS + 3 * 86_400 + 60;
/// Same UTC day as `DELIVERY_TS`, earlier in that day.
const SAME_DAY_EARLY: u64 = DELIVERY_TS - 50;
/// A later UTC day. Its previous completed day is `LATER_PREVIOUS_DAY`.
const LATER_TS: u64 = DELIVERY_TS + 2 * 86_400;
const DELIVERY_PREVIOUS_DAY: u32 = 20_240_103;
const LATER_PREVIOUS_DAY: u32 = 20_240_105;
const REWARD_DAY_VWAP_DAY: u32 = 20_240_101;
const VOTER: Address = Address::repeat_byte(0x5a);
const LOAD: u64 = 90;
const FROZEN_ENTRY: u64 = 5;
const REWARD_DAY_VWAP: u64 = 2;
const LIVE_QUOTE: u64 = 9;

fn one_coen840() -> U256 {
    U256::from(1_000_000u64)
}

fn seed_oracle(ctx: &BlockRuntimeContext) {
    outbe_oracle::api::register_pair(ctx.storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        ctx.storage.clone(),
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        U256::from(LIVE_QUOTE) * one_coen840(),
        ctx.block.block_number,
        ctx.block.timestamp,
    )
    .unwrap();
    outbe_oracle::schema::OracleContract::new(ctx.storage.clone())
        .reference_currencies
        .push(840u16)
        .unwrap();
}

fn seed_day_vwap(ctx: &BlockRuntimeContext, day: u32, whole_coen: u64) {
    let index = outbe_oracle::api::coen_pair_index_opt(ctx.storage.clone(), 840)
        .unwrap()
        .expect("COEN/840 registered");
    outbe_oracle::schema::OracleContract::new(ctx.storage.clone())
        .record_utc_day_vwap(day, index, U256::from(whole_coen) * one_coen840())
        .unwrap();
}

fn first_worldwide_day() -> GenesisWorldwideDay {
    let forming_start = GENESIS_TS;
    let forming_end = forming_start + 86_400;
    let lookback_end = forming_end + 86_400;
    GenesisWorldwideDay {
        worldwide_day: WorldwideDay::new(REWARD_DAY),
        status: WwdStatus::Offering,
        day_type: WwdDayType::Green,
        forming_start,
        forming_end,
        lookback_end,
        offering_end: lookback_end,
        scheduled_process_time: lookback_end,
        metadosis_limit_minor: U256::from(1u64),
        previous_vwap: U256::ZERO,
        current_vwap: U256::ZERO,
    }
}

fn install_first_worldwide_day(storage: &mut HashMapStorageProvider, timestamp: u64) {
    storage.set_block_number(1);
    storage.set_timestamp(U256::from(timestamp));
    storage.enter(|handle| {
        FreshDevnetGenesisBuilder::new()
            .seed_active_worldwide_day(first_worldwide_day())
            .apply(handle.clone())
            .unwrap();
        outbe_metadosis::test_support::seed_bootstrap_end_time(
            handle,
            first_worldwide_day().lookback_end,
        )
        .unwrap();
    });
}

fn deliver_at(
    storage: &mut HashMapStorageProvider,
    block_number: u64,
    timestamp: u64,
) -> outbe_gem::GemData {
    storage.set_block_number(block_number);
    storage.set_timestamp(U256::from(timestamp));
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new()),
            handle,
        );
        ensure_genesis_anchor(&ctx).unwrap();
        seed_oracle(&ctx);
        seed_day_vwap(&ctx, REWARD_DAY_VWAP_DAY, REWARD_DAY_VWAP);
        seed_day_vwap(&ctx, DELIVERY_PREVIOUS_DAY, FROZEN_ENTRY);
        seed_day_vwap(&ctx, LATER_PREVIOUS_DAY, FROZEN_ENTRY);
        prepare_daily_validator_gem_batch(&ctx, REWARD_DAY, U256::from(LOAD), &[(VOTER, 1)])
            .unwrap();
        assert!(matches!(
            deliver_oldest_reward_gem_batch(&ctx).unwrap(),
            RewardGemDeliveryOutcome::Delivered {
                reward_utc_day: REWARD_DAY,
                recipient_count: 1,
                ..
            }
        ));
        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        let gem_id = gem.token_of_owner_by_index(VOTER, 0).unwrap();
        outbe_gem::api::get_gem(&ctx.storage, gem_id)
            .unwrap()
            .unwrap()
    })
}

fn assert_genesis_terms(item: &outbe_gem::GemData, issued_at: u64) {
    let entry = U256::from(FROZEN_ENTRY) * one_coen840();
    assert_eq!(item.gem_type, outbe_gemfactory::GemTypes::Genesis as u8);
    assert_eq!(item.entry_price_minor, entry);
    assert!(item.floor_price_minor.is_zero());
    assert_eq!(
        item.call_price_minor,
        entry * U256::from(100 + u64::from(item.call_rate)) / U256::from(100u64)
    );
    assert_eq!(
        item.entry_price_minor * item.promis_load_minor / one_coen840(),
        U256::from(450u64)
    );
    assert_eq!(item.promis_load_minor, U256::from(LOAD));
    assert_eq!(item.issued_at, issued_at);
}

#[test]
fn a_genesis_gem_issued_before_the_first_worldwide_day_is_qualified_immediately() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let item = deliver_at(&mut storage, 1, SAME_DAY_EARLY);
    storage.enter(|handle| {
        assert!(metadosis_api::bootstrap_end_time(handle.clone())
            .unwrap()
            .is_none());
        assert_eq!(
            metadosis_api::worldwide_days_created(handle.clone()).unwrap(),
            0
        );
        assert!(metadosis_api::worldwide_days(handle.clone())
            .unwrap()
            .is_empty());
        assert_genesis_terms(&item, SAME_DAY_EARLY);
        assert!(
            outbe_gem::api::is_qualified(&handle, &item).unwrap(),
            "issued before the first Worldwide Day"
        );
    });

    install_first_worldwide_day(&mut storage, DELIVERY_TS);
    storage.set_block_number(40);
    storage.set_timestamp(U256::from(DELIVERY_TS + 30 * 86_400));
    storage.enter(|handle| {
        assert_eq!(
            metadosis_api::bootstrap_end_time(handle.clone()).unwrap(),
            Some(first_worldwide_day().lookback_end)
        );
        assert_eq!(
            metadosis_api::worldwide_days(handle.clone()).unwrap().len(),
            1
        );
        let gem = outbe_gem::GemContract::new(handle.clone());
        let gem_id = gem.token_of_owner_by_index(VOTER, 0).unwrap();
        let item = outbe_gem::api::get_gem(&handle, gem_id).unwrap().unwrap();
        assert_genesis_terms(&item, SAME_DAY_EARLY);
        assert!(
            outbe_gem::api::is_qualified(&handle, &item).unwrap(),
            "qualification survives creation of the first Worldwide Day"
        );
    });
}

#[test]
fn a_genesis_gem_issued_after_creation_in_the_same_block_waits() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    install_first_worldwide_day(&mut storage, SAME_DAY_EARLY);
    let item = deliver_at(&mut storage, 1, DELIVERY_TS);
    storage.enter(|handle| {
        assert_eq!(handle.block_number().unwrap(), 1);
        assert_eq!(
            metadosis_api::worldwide_days(handle.clone()).unwrap().len(),
            1
        );
        assert_eq!(
            metadosis_api::worldwide_days_created(handle.clone()).unwrap(),
            1
        );
        assert_genesis_terms(&item, DELIVERY_TS);
        assert!(
            !outbe_gem::api::is_qualified(&handle, &item).unwrap(),
            "same UTC day, creation already stored: the missing bit stays unqualified"
        );
    });
}

#[test]
fn a_genesis_gem_issued_after_the_first_worldwide_day_waits_for_a_closed_day() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    install_first_worldwide_day(&mut storage, SAME_DAY_EARLY);
    let item = deliver_at(&mut storage, 8, LATER_TS);
    storage.enter(|handle| {
        assert_eq!(handle.block_number().unwrap(), 8);
        assert_eq!(
            metadosis_api::worldwide_days(handle.clone()).unwrap().len(),
            1
        );
        assert_genesis_terms(&item, LATER_TS);
        assert!(
            !outbe_gem::api::is_qualified(&handle, &item).unwrap(),
            "a later Genesis Gem still needs an eligible finalized day"
        );
    });
}

#[test]
fn a_genesis_gem_issued_after_the_retained_day_is_deleted_still_waits() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    install_first_worldwide_day(&mut storage, SAME_DAY_EARLY);
    storage.enter(|handle| {
        FreshDevnetGenesisBuilder::new()
            .clear_single_offering_day(WorldwideDay::new(REWARD_DAY))
            .apply(handle.clone())
            .unwrap();
        assert!(metadosis_api::worldwide_days(handle.clone())
            .unwrap()
            .is_empty());
        assert_eq!(metadosis_api::worldwide_days_created(handle).unwrap(), 1);
    });
    let item = deliver_at(&mut storage, 2, DELIVERY_TS);
    storage.enter(|handle| {
        assert!(metadosis_api::worldwide_days(handle.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            metadosis_api::worldwide_days_created(handle.clone()).unwrap(),
            1
        );
        assert_genesis_terms(&item, DELIVERY_TS);
        assert!(
            !outbe_gem::api::is_qualified(&handle, &item).unwrap(),
            "deleting the retained day does not reopen Genesis privilege"
        );
    });
}
