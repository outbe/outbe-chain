//! Completed daily passes remain idle, including repeated entry at one height.

use alloy_primitives::{Address, U256};
use outbe_nod::{NodContract, NodItemState};
use outbe_nodfactory::{called, hooks};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

fn item(owner: Address, day: WorldwideDay) -> NodItemState {
    let nod_id = outbe_nod::identity::generate_nod_id(owner, day).unwrap();
    outbe_nod::test_support::item(
        outbe_nod::test_support::NodItemFixture {
            is_settled: false,
            nod_id,
            owner,
            gratis_load_minor: U256::from(11),
            worldwide_day: day,
            league_id: 4,
            group_key: outbe_nod::test_support::group_key(1_752_534_000, U256::from(5), 978),
            issuance_currency: 840,
            reference_currency: 978,
            issued_at: 1_752_534_000,
        },
        U256::from(5),
    )
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(1)
}

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
        oracle.reference_currencies.push(978).unwrap();
        oracle
            .record_utc_day_vwap(20260715, index, U256::from(13))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(20260715).unwrap();
        for (owner, entry) in [(0x51, 12u64), (0x52, 13)] {
            let entry = U256::from(entry);
            let mut body = item(Address::repeat_byte(owner), WorldwideDay::new(20260715));
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
        called::run_daily(&ctx).unwrap();
        called::run_call_slice(&ctx).unwrap();
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
        called::run_call_slice(&ctx).unwrap();
        hooks::sweep_cleanup(&ctx).unwrap();
        let nod = NodContract::new(storage);
        assert_eq!(nod.call_sweep_day.read().unwrap(), 0);
        assert_eq!(nod.call_latest_day.read().unwrap(), 20260715);
        let allowance = outbe_primitives::sweep_budget::BlockAllowance::unpack(
            nod.daily_budget.read().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(allowance.height, 2);
        assert_eq!(allowance.day, 0);
        assert_eq!(
            allowance.budget,
            outbe_primitives::sweep_budget::SweepBudget::per_block()
        );
    });
    let before = provider.storage.clone();
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, midnight, 1), storage);
        called::run_call_slice(&ctx).unwrap();
        hooks::sweep_cleanup(&ctx).unwrap();
    });
    assert_eq!(
        provider.storage, before,
        "same-height idle reentry preserves all state"
    );
}
