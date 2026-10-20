//! Group keys, bins, prices, and the issuance guards that keep a group reachable.

use super::*;

#[test]
fn an_issuable_entry_keeps_the_call_price_at_any_rate_in_range() {
    let bound = U256::MAX / U256::from(100 + u32::from(u16::MAX));
    assert!(crate::pricing::is_issuable_entry(bound));
    assert!(!crate::pricing::is_issuable_entry(bound + U256::from(1)));
}

#[test]
fn group_key_binds_the_reference_currency() {
    let issued_at = 1_752_534_000;
    let call_price = U256::from(13);
    assert_ne!(
        crate::identity::group_key(issued_at, call_price, USD),
        crate::identity::group_key(issued_at, call_price, EUR),
        "same day and call price in two currencies must not share a group"
    );
    assert_eq!(
        crate::identity::group_key(issued_at, call_price, USD),
        outbe_primitives::call_breach::group_key(
            outbe_primitives::time::first_full_day(issued_at),
            call_price,
            USD
        )
    );
}

/// Why the bin columns had to widen to `u64`: mapping keys are left-padded to
/// 32 bytes before hashing, so integer width alone namespaces nothing. The
/// ISO has to occupy real high bits, and those bits do not fit in a `u32`
/// alongside a 24-bit bin id.
#[test]
fn currency_scoped_bin_keys_do_not_alias() {
    assert!(u32::try_from(outbe_primitives::call_bins::scoped(u16::MAX, MAX_BIN_ID)).is_err());
    assert_ne!(
        outbe_primitives::call_bins::scoped(USD, 7),
        outbe_primitives::call_bins::scoped(EUR, 7)
    );
    assert_ne!(
        outbe_primitives::call_bins::bin_index_key(USD, 7, 0),
        outbe_primitives::call_bins::bin_index_key(EUR, 7, 0)
    );

    // ISO 0 is the one value that aliases the un-namespaced key. Issuance
    // rejects it (`zero_reference_currency_is_rejected_at_issuance`) precisely
    // because this collision cannot be detected downstream.
    assert_eq!(outbe_primitives::call_bins::scoped(0, 7), 7u64);
}

/// The headline regression: two Nods share a worldwide day and an identical
/// `entry_price_minor`, but are denominated differently. Such Nods are two groups
/// in two independent bin tries.
#[test]
fn same_day_and_entry_in_two_currencies_are_two_groups_in_two_bins() {
    let entry = U256::from(5);
    let usd = item(Address::repeat_byte(0x11), entry, USD);
    let eur = item(Address::repeat_byte(0x22), entry, EUR);
    assert_ne!(usd.group_key, eur.group_key);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        crate::test_support::add(&storage, &usd).unwrap();
        crate::test_support::add(&storage, &eur).unwrap();

        let nod = NodContract::new(storage.clone());
        let call_price = nod.group_call_price_minor.read(&usd.group_key).unwrap();
        let bin = outbe_primitives::call_bins::price_to_bin(call_price).unwrap();

        // Identical call prices land in the same bin id under different namespaces.
        for iso in [USD, EUR] {
            assert_eq!(
                nod.call_bin_count
                    .read(&outbe_primitives::call_bins::scoped(iso, bin))
                    .unwrap(),
                1
            );
            assert!(tree_math::contains(&CallBins(&nod, iso), bin).unwrap());
        }
        // Negative control: nothing landed in the un-namespaced (ISO 0) key.
        assert_eq!(nod.call_bin_count.read(&u64::from(bin)).unwrap(), 0);
    });
}

#[test]
fn same_day_and_currency_at_two_entries_are_two_groups_with_their_own_terms() {
    let low_entry = U256::from(2_000_000u64);
    let high_entry = U256::from(3_000_000u64);
    let low = item(Address::repeat_byte(0x11), low_entry, USD);
    let high = item(Address::repeat_byte(0x22), high_entry, USD);
    assert_eq!(low.worldwide_day, high.worldwide_day);
    assert_ne!(low.group_key, high.group_key);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        crate::test_support::add(&storage, &low).unwrap();
        crate::test_support::add(&storage, &high).unwrap();

        let nod = NodContract::new(storage.clone());
        for (body, entry, call_price) in [
            (&low, low_entry, 7_120_000u64),
            (&high, high_entry, 10_680_000),
        ] {
            assert_eq!(body.entry_price_minor(), entry);
            assert_eq!(nod.group_nod_count.read(&body.group_key).unwrap(), 1);
            assert_eq!(
                nod.group_call_price_minor.read(&body.group_key).unwrap(),
                U256::from(call_price)
            );
        }
    });
}

/// ISO 0 would be parked in a namespace the call scan never visits, so the
/// funnel rejects it before any write.
#[test]
fn zero_reference_currency_is_rejected_at_issuance() {
    let body = item(Address::repeat_byte(0x66), U256::from(5), 0);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        let error = crate::test_support::add(&storage, &body).unwrap_err();
        assert!(
            error.to_string().contains("reference currency"),
            "unexpected error: {error}"
        );

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(nod.call_bin_tree_root.read(&0).unwrap().is_zero());
    });
}

/// The group key is derived, not supplied: a caller whose key disagrees with
/// `(full day after issue, call price, currency)` is rejected, so the on-chain and Lysis derivations
/// cannot drift apart silently.
#[test]
fn a_group_key_that_does_not_match_its_inputs_is_rejected() {
    let mut body = item(Address::repeat_byte(0x66), U256::from(5), EUR);
    body.group_key = crate::test_support::group_key(body.issued_at, U256::from(5), USD);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        let error = crate::test_support::add(&storage, &body).unwrap_err();
        assert!(
            error.to_string().contains("group identity mismatch"),
            "unexpected error: {error}"
        );
        assert_eq!(NodContract::new(storage).total_supply().unwrap(), 0);
    });
}
