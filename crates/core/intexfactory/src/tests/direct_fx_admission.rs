//! The direct settlement rail converts through the trailing window and refuses
//! a payment whose target leg that window never priced, before any write.

use super::issuance::{dual_currency_series, stub_token_that_never_credits, COEN_ISO_RATE_SCALE};
use super::*;

/// The euro trades live and the reference leg is priced in the window, but the
/// window holds no euro price: the direct settlement is refused as oracle
/// unavailable and the provider records no write or event.
#[test]
fn a_direct_settlement_in_a_currency_the_window_never_priced_is_refused_without_writes() {
    let mut storage = dual_currency_series(EUR_ISO as u64);
    stub_token_that_never_credits(&mut storage);
    let required = StorageHandle::enter(&mut storage, |s| {
        let oracle = OracleContract::new(s.clone());
        write_day_rate(
            &oracle,
            REFERENCE_ISO,
            PAIR_ID,
            U256::from(2u64) * COEN_ISO_RATE_SCALE,
        );
        write_rate(&oracle, EUR_ISO, EUR_PAIR_ID, COEN_ISO_RATE_SCALE);
        seed_qualifying_day(&s);
        outbe_oracle::api::current_vwap_snapshot(s.clone())
            .unwrap()
            .to_u256()
    });
    let slots = storage.storage.clone();
    let events = storage.get_ordered_events().to_vec();
    storage.clear_mutation_failure();

    let error = StorageHandle::enter(&mut storage, |s| {
        runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::ONE,
            payment_token(),
            required,
        )
        .unwrap_err()
    });
    assert!(
        error.to_string().contains("oracle nominal unavailable"),
        "{error}"
    );

    assert_eq!(
        storage.clear_mutation_failure(),
        0,
        "the refusal reaches the provider with no write or event"
    );
    assert_eq!(storage.storage, slots);
    assert_eq!(storage.get_ordered_events(), events);
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbe_intex::api::settled_units(&s, sid(7)).unwrap(), 0);
    });
}
