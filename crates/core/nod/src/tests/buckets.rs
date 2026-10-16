//! Bucket keys, bins, prices and qualification, and the issuance guards that keep a bucket reachable.

use super::*;

#[test]
fn a_nod_floor_is_its_entry_marked_up_and_rounded_down() {
    assert_eq!(
        crate::pricing::floor_price_minor(U256::from(2_000_000u64)),
        Some(U256::from(2_160_000u64))
    );
    assert_eq!(
        crate::pricing::floor_price_minor(U256::from(1_000_001u64)),
        Some(U256::from(1_080_001u64))
    );
    assert_eq!(
        crate::pricing::floor_price_minor(U256::from(999u64)),
        Some(U256::from(1_078u64))
    );
    assert_eq!(crate::pricing::floor_price_minor(U256::MAX), None);
}

#[test]
fn an_issuable_entry_keeps_the_call_price_at_any_rate_in_range() {
    let bound = U256::MAX / U256::from(100 + u32::from(u16::MAX));
    assert!(crate::pricing::is_issuable_entry(bound));
    assert!(!crate::pricing::is_issuable_entry(bound + U256::from(1)));
    assert!(crate::pricing::floor_price_minor(bound + U256::from(1)).is_some());
}

#[test]
fn bucket_key_binds_the_reference_currency() {
    let day = WorldwideDay::new(20_260_715);
    let entry = U256::from(13);
    assert_ne!(
        crate::identity::bucket_key(day, entry, USD),
        crate::identity::bucket_key(day, entry, EUR),
        "same day and entry in two currencies must not share a bucket"
    );

    // The ISO occupies the trailing two bytes of a 38-byte preimage.
    let mut expected = [0u8; 38];
    expected[0..4].copy_from_slice(&20_260_715u32.to_be_bytes());
    expected[4..36].copy_from_slice(&entry.to_be_bytes::<32>());
    expected[36..38].copy_from_slice(&USD.to_be_bytes());
    assert_eq!(
        crate::identity::bucket_key(day, entry, USD),
        alloy_primitives::keccak256(expected)
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
/// `entry_price_minor`, but are denominated differently. Such Nods are two buckets
/// in two independent bin tries. A day price only qualifies its own currency.
#[test]
fn same_day_and_entry_in_two_currencies_are_two_buckets_in_two_bins() {
    let entry = U256::from(5);
    let usd = item(Address::repeat_byte(0x11), entry, USD);
    let eur = item(Address::repeat_byte(0x22), entry, EUR);
    assert_ne!(usd.bucket_key, eur.bucket_key);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        api::add_nod(
            &storage,
            &usd,
            entry,
            crate::test_support::sealed_terms(&storage, &usd, entry),
        )
        .unwrap();
        api::add_nod(
            &storage,
            &eur,
            entry,
            crate::test_support::sealed_terms(&storage, &eur, entry),
        )
        .unwrap();

        let nod = NodContract::new(storage.clone());
        let call_price = nod.bucket_call_price_minor.read(&usd.bucket_key).unwrap();
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

        // A COEN/USD day above the shared floor qualifies the USD bucket only.
        qualify(&storage, &usd, entry);
        let usd_bucket = bucket_of(&storage, &usd);
        let eur_bucket = bucket_of(&storage, &eur);
        assert!(api::is_qualified(&storage, &usd_bucket).unwrap());
        assert!(
            !api::is_qualified(&storage, &eur_bucket).unwrap(),
            "a COEN/USD price must not qualify a EUR-denominated floor"
        );
    });
}

#[test]
fn same_day_and_currency_at_two_entries_are_two_buckets_with_their_own_terms() {
    let low_entry = U256::from(2_000_000u64);
    let high_entry = U256::from(3_000_000u64);
    let low = item(Address::repeat_byte(0x11), low_entry, USD);
    let high = item(Address::repeat_byte(0x22), high_entry, USD);
    assert_eq!(low.worldwide_day, high.worldwide_day);
    assert_ne!(low.bucket_key, high.bucket_key);

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        api::add_nod(
            &storage,
            &low,
            low_entry,
            crate::test_support::sealed_terms(&storage, &low, low_entry),
        )
        .unwrap();
        api::add_nod(
            &storage,
            &high,
            high_entry,
            crate::test_support::sealed_terms(&storage, &high, high_entry),
        )
        .unwrap();

        let nod = NodContract::new(storage.clone());
        for (body, entry, floor, call_price) in [
            (&low, low_entry, 2_160_000u64, 7_120_000u64),
            (&high, high_entry, 3_240_000, 10_680_000),
        ] {
            let bucket = bucket_of(&storage, body);
            assert_eq!(bucket.entry_price_minor, entry);
            assert_eq!(bucket.floor_price_minor().unwrap(), U256::from(floor));
            assert_eq!(nod.bucket_nod_count.read(&body.bucket_key).unwrap(), 1);
            assert_eq!(
                nod.bucket_call_price_minor.read(&body.bucket_key).unwrap(),
                U256::from(call_price)
            );
        }
    });
}

/// Q027: qualification uses the same `first_full_day(issued_at)` cutoff as the
/// call scan. A VWAP on the partial issuance UTC day, or any earlier day,
/// cannot qualify the bucket even when it stands strictly above the floor.
#[test]
fn qualification_skips_days_before_first_full_day() {
    let entry = U256::from(5);
    let floor = crate::pricing::floor_price_minor(entry).unwrap();
    let body = item(Address::repeat_byte(0x11), entry, USD);
    let issuance_day = timestamp_to_date_key(body.issued_at);
    let full_day = first_full_day(body.issued_at);
    assert_ne!(
        issuance_day, full_day,
        "the fixture must be a partial issuance day so the cutoff is observable"
    );

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        api::add_nod(
            &storage,
            &body,
            entry,
            crate::test_support::sealed_terms(&storage, &body, entry),
        )
        .unwrap();
        let qualified = || api::is_qualified(&storage, &bucket_of(&storage, &body)).unwrap();

        close_day_above(&storage, USD, floor, issuance_day);
        assert!(!qualified());
        close_day_above(&storage, USD, floor, full_day);
        assert!(qualified());
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
        let error = api::add_nod(
            &storage,
            &body,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("reference currency"),
            "unexpected error: {error}"
        );

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(nod.call_bin_tree_root.read(&0).unwrap().is_zero());
    });
}

/// The bucket key is derived, not supplied: a caller whose key disagrees with
/// `(day, entry, currency)` is rejected, so the on-chain and Lysis derivations
/// cannot drift apart silently.
#[test]
fn a_bucket_key_that_does_not_match_its_inputs_is_rejected() {
    let mut body = item(Address::repeat_byte(0x66), U256::from(5), EUR);
    body.bucket_key = crate::identity::bucket_key(body.worldwide_day, U256::from(5), USD);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        seed_production_nod_genesis(&storage);
        let error = api::add_nod(
            &storage,
            &body,
            U256::from(5),
            crate::test_support::sealed_terms(&storage, &body, U256::from(5)),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("bucket identity mismatch"),
            "unexpected error: {error}"
        );
        assert_eq!(NodContract::new(storage).total_supply().unwrap(), 0);
    });
}
