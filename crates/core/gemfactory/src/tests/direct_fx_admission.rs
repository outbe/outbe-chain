//! The direct settlement rail converts through the trailing window and refuses
//! a payment whose target leg that window never priced, before any write.

use super::*;

/// The euro trades live and the dollar leg is priced in the window, but the
/// window holds no euro price. The direct settlement of a euro-referenced Gem
/// is refused as oracle unavailable, and the provider records no write or event.
#[test]
fn a_direct_settlement_through_a_leg_the_window_never_priced_is_refused_without_writes() {
    let rate = U256::from(2u64) * six_decimal_unit();
    let mut provider = test_storage(Some(rate));
    let (gem_id, required) = StorageHandle::enter(&mut provider, |storage| {
        let eur_pair = outbe_oracle::api::AddressPair::new_coen_to(978);
        outbe_oracle::api::register_pair(storage.clone(), eur_pair).unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            eur_pair,
            six_decimal_unit(),
            1,
            T_NOW,
        )
        .unwrap();
        OracleContract::new(storage.clone())
            .reference_currencies
            .push(978)
            .unwrap();
        seed_day_vwap(&storage, 840, rate);
        let gem_id = issue_at_live_rate(
            &storage,
            ALICE,
            GemTypes::Wallet,
            U256::from(10u64) * six_decimal_unit(),
            840,
            978,
        )
        .unwrap();
        seed_qualifying_day(&storage, gem_id);
        let required = outbe_oracle::api::current_vwap_snapshot(storage.clone())
            .unwrap()
            .to_u256();
        (gem_id, required)
    });
    let slots = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    provider.clear_mutation_failure();

    let result = StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_gem(&storage, ALICE, gem_id, STABLE, required)
    });
    let error = err_msg(result);
    assert!(error.contains("oracle nominal unavailable"), "{error}");

    assert_eq!(
        provider.clear_mutation_failure(),
        0,
        "the refusal reaches the provider with no write or event"
    );
    assert_eq!(provider.storage, slots);
    assert_eq!(provider.get_ordered_events(), events);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            gem_api::get_gem(&storage, gem_id).unwrap().unwrap().state,
            GemState::Issued as u8
        );
    });
}
