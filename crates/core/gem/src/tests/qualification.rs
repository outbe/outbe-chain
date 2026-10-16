//! Qualification is per gem: its own floor, its own reference currency, its own
//! first full day. The bucket decides calls, never qualification.

use alloy_primitives::U256;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::time::{date_key_to_utc_timestamp, previous_date_key, timestamp_to_date_key};

use super::*;
use crate::schema::{GemContract, GemState};

fn eur_gem(storage: &StorageHandle) -> U256 {
    let mut p = sample_params(BOB);
    p.reference_currency = EUR;
    add(storage, p)
}

/// An Issued gem of `iso` issued at `issued_at` with its own `floor`; `nonce` keeps
/// the ids apart.
fn issued_gem_of(
    storage: &StorageHandle,
    iso: u16,
    nonce: u64,
    issued_at: u64,
    floor: U256,
) -> U256 {
    let mut p = sample_params(ALICE);
    p.promis_load_minor = U256::from(1_000_000 + u64::from(iso) * 1_000 + nonce);
    p.reference_currency = iso;
    p.issued_at = issued_at;
    p.floor_price_minor = floor;
    add(storage, p)
}

#[test]
fn a_gem_qualifies_on_a_closed_day_above_its_floor() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let floor = sample_params(ALICE).floor_price_minor;

        assert!(!is_qualified(storage, gem_id), "no finalized day yet");
        let index = seed_day_price(storage, USD, Some(floor));
        assert!(
            !is_qualified(storage, gem_id),
            "a day at the floor does not qualify"
        );
        close_day(storage, index, floor + U256::from(1u64));
        assert!(is_qualified(storage, gem_id));
        assert_eq!(
            item(storage, gem_id).unwrap().state,
            GemState::Issued as u8,
            "nothing is stored"
        );
    });
}

#[test]
fn is_qualified_dispatch() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let floor = sample_params(ALICE).floor_price_minor;
        seed_day_price(storage, USD, Some(floor + U256::from(1u64)));

        let out = call(
            storage,
            IGem::isQualifiedCall { gemId: gem_id }.abi_encode(),
        )
        .unwrap();
        assert!(IGem::isQualifiedCall::abi_decode_returns(&out).unwrap());
    });
}

/// A zero floor still needs one finalized day. Any positive price then clears it.
#[test]
fn a_gem_without_a_floor_qualifies_on_its_first_closed_day() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.gem_type = 0;
        p.floor_price_minor = U256::ZERO;
        let gem_id = add(storage, p);
        assert!(!is_qualified(storage, gem_id), "no finalized day yet");
        seed_day_price(storage, USD, Some(U256::from(1u64)));
        assert!(is_qualified(storage, gem_id));
        let gem = GemContract::new(storage.clone());
        let bin =
            outbe_primitives::call_bins::price_to_bin(view(storage, gem_id).terms.call_price_minor)
                .unwrap();
        assert!(tree_math::contains(&crate::state::CallBins(&gem, USD), bin).unwrap());
    });
}

/// Both gems carry the same floor: each must be qualified only by its own currency.
#[test]
fn each_currency_qualifies_against_its_own_day_price() {
    with_storage(|storage| {
        let usd_id = add(storage, sample_params(ALICE));
        let eur_id = eur_gem(storage);
        let floor = sample_params(ALICE).floor_price_minor;

        seed_day_price(storage, USD, Some(floor + U256::from(1u64)));
        seed_day_price(storage, EUR, Some(floor - U256::from(1u64)));

        assert!(is_qualified(storage, usd_id));
        assert!(!is_qualified(storage, eur_id));
    });
}

/// The issuance currency is a settlement label and must never reach a lifecycle
/// decision: a gem whose two currencies differ is judged by its reference alone.
#[test]
fn a_gem_is_qualified_by_its_reference_currency_not_its_issuance_one() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.issuance_currency = EUR;
        let gem_id = add(storage, p);
        let floor = sample_params(ALICE).floor_price_minor;

        seed_day_price(storage, USD, Some(floor - U256::from(1u64)));
        seed_day_price(storage, EUR, Some(floor + U256::from(1u64)));

        assert!(!is_qualified(storage, gem_id));
    });
}

/// A currency whose COEN pair is unregistered qualifies nothing, and says so
/// without an error.
#[test]
fn a_currency_without_a_priced_pair_qualifies_nothing() {
    with_storage(|storage| {
        let eur_id = eur_gem(storage);
        seed_day_price(storage, EUR, None);
        assert!(!is_qualified(storage, eur_id));
    });
}

#[test]
fn a_day_without_a_price_qualifies_nothing_and_the_next_one_still_can() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let floor = sample_params(ALICE).floor_price_minor;
        // Finalized, but no trade that day: the pair is known and the day is empty.
        let index = seed_day_price(storage, USD, Some(U256::ZERO));
        assert!(!is_qualified(storage, gem_id));

        let next_day = previous_date_key(timestamp_to_date_key(QUALIFY_TS + SECONDS_PER_DAY));
        let oracle = OracleContract::new(storage.clone());
        oracle
            .record_utc_day_vwap(next_day, index, floor + U256::from(1u64))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(next_day).unwrap();
        assert!(is_qualified(storage, gem_id));
    });
}

/// A spike inside the day that leaves the day's VWAP at the floor qualifies nothing,
/// however high and fresh the live rate stands.
#[test]
fn a_spike_the_day_price_does_not_share_qualifies_no_gem() {
    with_storage(|storage| {
        let gem_id = add(storage, sample_params(ALICE));
        let floor = sample_params(ALICE).floor_price_minor;
        let index = seed_currency(storage, USD, Some(floor * U256::from(10u64)));
        OracleContract::new(storage.clone())
            .exchange_rate_timestamp
            .write(&index, QUALIFY_TS)
            .unwrap();
        close_day(storage, index, floor);

        assert!(!is_qualified(storage, gem_id));
    });
}

/// The issuance day counts only when the gem was issued at midnight: one issued a
/// second later waits for the next day's price.
#[test]
fn the_issue_day_qualifies_only_a_gem_issued_at_midnight() {
    let day = previous_date_key(timestamp_to_date_key(QUALIFY_TS));
    let midnight = date_key_to_utc_timestamp(day);
    for (issued_at, expected) in [(midnight, true), (midnight + 1, false)] {
        with_storage(|storage| {
            let floor = sample_params(ALICE).floor_price_minor;
            let gem_id = issued_gem_of(storage, USD, 0, issued_at, floor);
            seed_day_price(storage, USD, Some(floor + U256::from(1u64)));
            assert_eq!(
                is_qualified(storage, gem_id),
                expected,
                "issued at {issued_at}"
            );
        });
    }
}

/// Qualification reads the finalized day and writes nothing.
#[test]
fn qualification_reads_the_finalized_day_and_writes_nothing() {
    let mut provider = provider_at(T_NOW);
    let before = StorageHandle::enter(&mut provider, |storage| {
        let gem_id = add(&storage, sample_params(ALICE));
        let floor = sample_params(ALICE).floor_price_minor;
        seed_day_price(&storage, USD, Some(floor + U256::from(1u64)));
        gem_id
    });
    let events_before = provider.get_events(GEM_ADDRESS).len();
    StorageHandle::enter(&mut provider, |storage| {
        assert!(is_qualified(&storage, before));
        assert_eq!(status(&storage, before).state, 1, "Qualified in the view");
        assert_eq!(
            item(&storage, before).unwrap().state,
            GemState::Issued as u8
        );
    });
    assert_eq!(provider.get_events(GEM_ADDRESS).len(), events_before);
}

use alloy_sol_types::SolCall;
use outbe_primitives::addresses::GEM_ADDRESS;
