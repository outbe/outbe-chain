//! Completed daily passes remain idle, including repeated entry at one height.

use alloy_primitives::{Address, U256};
use outbe_nod::NodContract;
use outbe_nodfactory::{called, hooks};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

const ALL_PHASES: outbe_oracle::call_pass::Selection = outbe_oracle::call_pass::Selection {
    process_call: true,
    process_forfeit: true,
    process_cleanup: true,
};

#[test]
fn idle_daily_scans_do_not_reopen_a_completed_day() {
    let mut provider = provider();
    let midnight = outbe_primitives::time::date_key_to_utc_timestamp(20260716);
    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage.clone())
            .config_profile
            .write(outbe_nod::config::PROFILE_PROD)
            .unwrap();
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        let pair = outbe_oracle::api::AddressPair::new_coen_to(978);
        let index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
        oracle
            .record_utc_day_vwap(20260715, index, U256::from(13))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(20260715).unwrap();
        for (owner, entry) in [(0x51, 12u64), (0x52, 13)] {
            let entry = U256::from(entry);
            let mut body = outbe_nod::test_support::sample_item(
                Address::repeat_byte(owner),
                WorldwideDay::new(20260715),
            );
            body.group_key = outbe_nod::test_support::group_key(body.issued_at, entry, 978);
            outbe_nod::test_support::set_entry_price(&mut body, entry);
            outbe_nod::test_support::add(&storage, &body).unwrap();
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, midnight, 1),
            storage.clone(),
        );
        called::initialize_daily(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(0, midnight - 86_400, 1),
            storage.clone(),
        ))
        .unwrap();
        called::schedule_call_pass(&ctx).unwrap();
        called::process_call_pass(&ctx, ALL_PHASES).unwrap();
        assert_eq!(
            NodContract::new(storage).call_sweep_day.read().unwrap(),
            0,
            "both groups wait below their call price, so the sweep closes at once"
        );
    });
    // A new height records its idle allowance but does not reopen the completed day.
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, midnight, 1),
            storage.clone(),
        );
        called::process_call_pass(&ctx, ALL_PHASES).unwrap();
        hooks::cleanup(&ctx).unwrap();
        let nod = NodContract::new(storage);
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
        assert_eq!(nod.call_latest_day.read().unwrap(), 20260715);
        let allowance =
            outbe_primitives::pass_budget::BlockAllowance::unpack(nod.daily_budget.read().unwrap())
                .unwrap()
                .unwrap();
        assert_eq!(allowance.height, 2);
        assert_eq!(allowance.day, 0);
        assert_eq!(
            allowance.budget,
            outbe_primitives::pass_budget::PassBudget::per_block()
        );
    });
    let before = provider.storage.clone();
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, midnight, 1), storage);
        called::process_call_pass(&ctx, ALL_PHASES).unwrap();
        hooks::cleanup(&ctx).unwrap();
    });
    assert_eq!(
        provider.storage, before,
        "same-height idle reentry preserves all state"
    );
}
