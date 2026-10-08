use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GEM_ADDRESS;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

use super::{
    api, begin_block_at, block_ctx_at, call_gem, callable_gem_of, gem_state, mature_gem,
    priced_window, seed_currency, unallocated, with_storage, T_NOW,
};
use crate::called::{run_call_slice, scan_and_call};
use crate::precompile::IGem;
use crate::schema::{GemContract, GemState};

#[test]
fn expiry_waits_until_the_deadline_hour_has_closed() {
    with_storage(|storage| {
        let gem_id = mature_gem(storage);
        call_gem(storage, gem_id, T_NOW);
        let hour_end = GemContract::hour_end(GemContract::deadline_hour(T_NOW + 7 * 86_400));

        begin_block_at(storage, hour_end - 1);
        assert_eq!(gem_state(storage, gem_id), GemState::Called as u8);
        assert_eq!(unallocated(storage), U256::ZERO);

        begin_block_at(storage, hour_end);
        assert!(api::get_gem(storage, gem_id).unwrap().is_none());
        assert_eq!(unallocated(storage), U256::from(1_000_000));
    });
}

#[test]
fn an_unpriced_currency_does_not_prevent_calling_the_next_currency() {
    with_storage(|storage| {
        let unpriced = callable_gem_of(storage, 978, 0, T_NOW - 100 * 86_400, U256::from(100_000));
        seed_currency(storage, 978, None);
        let priced = callable_gem_of(storage, 840, 1, T_NOW - 100 * 86_400, U256::from(100_000));
        let pair = seed_currency(storage, 840, Some(U256::from(600_000)));
        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        priced_window(storage, pair, day, U256::from(300_000));

        assert_eq!(scan_and_call(&block_ctx_at(storage, T_NOW)).unwrap(), 1);
        assert_eq!(gem_state(storage, unpriced), GemState::Issued as u8);
        assert_eq!(gem_state(storage, priced), GemState::Called as u8);
        assert_eq!(
            GemContract::new(storage.clone())
                .call_sweep_day
                .read()
                .unwrap(),
            0
        );
    });
}

#[test]
fn only_a_slice_that_calls_buckets_emits_a_batch_metadata_update() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        callable_gem_of(&storage, 840, 0, T_NOW - 100 * 86_400, U256::from(100_000));
        let pair = seed_currency(&storage, 840, Some(U256::from(600_000)));
        let day = previous_date_key(timestamp_to_date_key(T_NOW));
        priced_window(&storage, pair, day, U256::from(300_000));
        let ctx = block_ctx_at(&storage, T_NOW);

        assert_eq!(scan_and_call(&ctx).unwrap(), 1);
        assert_eq!(run_call_slice(&ctx).unwrap(), 0);
    });

    let updates: Vec<_> = provider
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|log| IGem::BatchMetadataUpdate::decode_log_data(log).ok())
        .collect();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]._fromTokenId, U256::ZERO);
    assert_eq!(updates[0]._toTokenId, U256::MAX);
}
