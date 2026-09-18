//! Currency-aware bucket qualification: storage layout, key derivation, bin
//! namespacing, and the issuance guards that keep a bucket reachable.

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_offchain_storage::MemoryStorage;
use outbe_primitives::time::{first_full_day, timestamp_to_date_key, WorldwideDay};
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};

use crate::{api, hooks, state::CurrencyBins, NodContract, NodItemState, NodRepositoryReader};

const USD: u16 = 840;
const EUR: u16 = 978;

fn seed_compressed_entities_genesis(storage: &StorageHandle<'_>) {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .unwrap();
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .unwrap()
                    .as_slice(),
            ),
        )
        .unwrap();
}

/// A Nod whose `bucket_key` is derived the way `record_nod_issued` requires.
fn item(owner: Address, floor: U256, reference_currency: u16) -> NodItemState {
    let worldwide_day = WorldwideDay::new(20_260_715);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11),
        worldwide_day,
        league_id: 4,
        floor_price_minor: floor,
        bucket_key: NodContract::bucket_key(worldwide_day, floor, reference_currency),
        issuance_currency: 840,
        reference_currency,
        issued_at: 1_752_534_000,
    }
}

/// Dense `order`-packing puts these fields at contiguous offsets 0..=14. The
/// per-currency bin re-keying changed field *types* but must not move a single
/// slot; nothing else in CI guards this layout.
#[test]
fn nod_contract_slot_layout_is_pinned() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        for (index, actual) in [
            nod.total_supply.slot(),
            nod.bin_tree_root.base_slot(),
            nod.bin_tree_mid.base_slot(),
            nod.bin_tree_leaf.base_slot(),
            nod.unqualified_bin_count.base_slot(),
            nod.unqualified_bin_buckets.base_slot(),
            nod.unqualified_bin_scan_cursor.base_slot(),
            nod.bucket_worldwide_day.base_slot(),
            nod.ocomp_target_generation.base_slot(),
            nod.ocomp_namespace_root.base_slot(),
            nod.ocomp_bucket_root.base_slot(),
            nod.ocomp_output_manifest_root.base_slot(),
            nod.ocomp_generation_metadata.base_slot(),
            nod.ocomp_nod_amount_total.base_slot(),
            nod.ocomp_lysis_allocation_minor.base_slot(),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                actual,
                U256::from(index),
                "NodContract field #{index} moved slot"
            );
        }
    });
}

#[test]
fn bucket_key_binds_the_reference_currency() {
    let day = WorldwideDay::new(20_260_715);
    let floor = U256::from(13);
    assert_ne!(
        NodContract::bucket_key(day, floor, USD),
        NodContract::bucket_key(day, floor, EUR),
        "same day and floor in two currencies must not share a bucket"
    );

    // The ISO occupies the trailing two bytes of a 38-byte preimage.
    let mut expected = [0u8; 38];
    expected[0..4].copy_from_slice(&20_260_715u32.to_be_bytes());
    expected[4..36].copy_from_slice(&floor.to_be_bytes::<32>());
    expected[36..38].copy_from_slice(&USD.to_be_bytes());
    assert_eq!(
        NodContract::bucket_key(day, floor, USD),
        alloy_primitives::keccak256(expected)
    );
}

/// Why the bin columns had to widen to `u64`: mapping keys are left-padded to
/// 32 bytes before hashing, so integer width alone namespaces nothing — the
/// ISO has to occupy real high bits, and those bits do not fit in a `u32`
/// alongside a 24-bit bin id.
#[test]
fn currency_scoped_bin_keys_do_not_alias() {
    assert!(u32::try_from(NodContract::scoped(u16::MAX, MAX_BIN_ID)).is_err());
    assert_ne!(NodContract::scoped(USD, 7), NodContract::scoped(EUR, 7));
    assert_ne!(
        NodContract::bin_index_key(USD, 7, 0),
        NodContract::bin_index_key(EUR, 7, 0)
    );

    // ISO 0 is the one value that aliases the un-namespaced key. Issuance
    // rejects it (`zero_reference_currency_is_rejected_at_issuance`) precisely
    // because this collision cannot be detected downstream.
    assert_eq!(NodContract::scoped(0, 7), 7u64);
}

/// The headline regression: two Nods sharing a worldwide day and an identical
/// `floor_price_minor` but denominated differently are two buckets in two
/// independent bin tries, and a rate only qualifies its own currency.
#[test]
fn same_day_and_floor_in_two_currencies_are_two_buckets_in_two_bins() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let floor = U256::from(500_000_000_000_000_000u128);
    let usd = item(Address::repeat_byte(0x11), floor, USD);
    let eur = item(Address::repeat_byte(0x22), floor, EUR);
    assert_ne!(usd.bucket_key, eur.bucket_key);

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &usd, U256::from(5)).unwrap();
        api::add_nod(&storage, &scope, &parent, &eur, U256::from(5)).unwrap();

        let nod = NodContract::new(storage.clone());
        let bin = NodContract::price_to_bin(floor).unwrap();

        // Identical floors land in the same bin id under different namespaces.
        assert_eq!(
            nod.unqualified_bin_count
                .read(&NodContract::scoped(USD, bin))
                .unwrap(),
            1
        );
        assert_eq!(
            nod.unqualified_bin_count
                .read(&NodContract::scoped(EUR, bin))
                .unwrap(),
            1
        );
        // Negative control: nothing landed in the un-namespaced (ISO 0) key.
        assert_eq!(nod.unqualified_bin_count.read(&u64::from(bin)).unwrap(), 0);
        assert!(tree_math::contains(&CurrencyBins(&nod, USD), bin).unwrap());
        assert!(tree_math::contains(&CurrencyBins(&nod, EUR), bin).unwrap());

        // Qualify USD only, at a rate strictly above the shared floor value.
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        let inspected = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            floor + U256::from(1),
            first_full_day(usd.issued_at),
            crate::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK,
        )
        .unwrap();
        assert_eq!(inspected, 1);

        let usd_bucket = api::get_bucket(
            &storage,
            &scope,
            &parent,
            WwdEntityId::from_day_and_digest(usd.worldwide_day, usd.bucket_key),
        )
        .unwrap()
        .unwrap();
        let eur_bucket = api::get_bucket(
            &storage,
            &scope,
            &parent,
            WwdEntityId::from_day_and_digest(eur.worldwide_day, eur.bucket_key),
        )
        .unwrap()
        .unwrap();
        assert!(usd_bucket.is_qualified);
        assert!(
            !eur_bucket.is_qualified,
            "a COEN/USD rate must not qualify a EUR-denominated floor"
        );
        assert_eq!(usd_bucket.reference_currency, USD);
        assert_eq!(eur_bucket.reference_currency, EUR);

        // USD's trie is drained; EUR's is untouched.
        assert_eq!(
            nod.unqualified_bin_count
                .read(&NodContract::scoped(USD, bin))
                .unwrap(),
            0
        );
        assert!(!tree_math::contains(&CurrencyBins(&nod, USD), bin).unwrap());
        assert_eq!(
            nod.unqualified_bin_count
                .read(&NodContract::scoped(EUR, bin))
                .unwrap(),
            1
        );
        assert!(tree_math::contains(&CurrencyBins(&nod, EUR), bin).unwrap());
    });
}

/// The scan stops at its budget and resumes mid-bin on the next call via the
/// per-bin cursor. `run_qualify_slice` shares one budget across currencies, so
/// an over-run here would be unbounded work in a CycleTick.
#[test]
fn the_scan_stops_at_its_budget_and_resumes_from_the_bin_cursor() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let base = U256::from(1_000_000_000_000_000_000u128);
    // Floors within one 0.25% bin, so all three share a bin and the run has to
    // resume through `unqualified_bin_scan_cursor` rather than the trie walk.
    let bodies: Vec<NodItemState> = (0..3u8)
        .map(|i| item(Address::repeat_byte(0x40 + i), base + U256::from(i), USD))
        .collect();
    let bin = NodContract::price_to_bin(base).unwrap();
    for body in &bodies {
        assert_eq!(
            NodContract::price_to_bin(body.floor_price_minor).unwrap(),
            bin
        );
    }

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        for body in &bodies {
            api::add_nod(&storage, &scope, &parent, body, U256::from(5)).unwrap();
        }
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        let rate = base + U256::from(10);
        let qualified = |storage: &StorageHandle<'_>| {
            bodies
                .iter()
                .filter(|body| {
                    api::get_bucket(
                        storage,
                        &scope,
                        &parent,
                        WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key),
                    )
                    .unwrap()
                    .unwrap()
                    .is_qualified
                })
                .count()
        };

        let first = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            rate,
            first_full_day(bodies[0].issued_at),
            2,
        )
        .unwrap();
        assert_eq!(first, 2, "must inspect exactly the budget");
        assert_eq!(qualified(&storage), 2);
        assert_eq!(
            NodContract::new(storage.clone())
                .unqualified_bin_count
                .read(&NodContract::scoped(USD, bin))
                .unwrap(),
            1
        );

        let second = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            rate,
            first_full_day(bodies[0].issued_at),
            2,
        )
        .unwrap();
        assert_eq!(second, 1, "only the remaining bucket is left to inspect");
        assert_eq!(qualified(&storage), 3);
        assert!(
            !tree_math::contains(&CurrencyBins(&NodContract::new(storage.clone()), USD), bin)
                .unwrap()
        );
    });
}

/// Q027: qualification uses the same `first_full_day(issued_at)` cutoff as the
/// call scan. A VWAP on the partial issuance UTC day, or any earlier day,
/// cannot promote the bucket even when it stands strictly above the floor.
#[test]
fn qualification_skips_days_before_first_full_day() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let body = item(
        Address::repeat_byte(0x11),
        U256::from(500_000_000_000_000_000u128),
        USD,
    );
    let bin = NodContract::price_to_bin(body.floor_price_minor).unwrap();
    let issuance_day = timestamp_to_date_key(body.issued_at);
    let full_day = first_full_day(body.issued_at);
    assert_ne!(
        issuance_day, full_day,
        "the fixture must be a partial issuance day so the cutoff is observable"
    );

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, body.issued_at, 1),
            storage.clone(),
        );
        let rate = body.floor_price_minor + U256::from(1);
        let bucket_id = WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key);
        let qualified = |storage: &StorageHandle<'_>| {
            api::get_bucket(storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap()
                .is_qualified
        };

        let inspected = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            rate,
            issuance_day,
            crate::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK,
        )
        .unwrap();
        assert_eq!(inspected, 1);
        assert!(!qualified(&storage));
        assert_eq!(
            NodContract::new(storage.clone())
                .unqualified_bin_count
                .read(&NodContract::scoped(USD, bin))
                .unwrap(),
            1,
            "a too-early day must leave the bucket parked"
        );

        let inspected = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            rate,
            full_day,
            crate::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK,
        )
        .unwrap();
        assert_eq!(inspected, 1);
        assert!(qualified(&storage));
        assert_eq!(
            NodContract::new(storage.clone())
                .unqualified_bin_count
                .read(&NodContract::scoped(USD, bin))
                .unwrap(),
            0
        );
    });
}

/// A bucket issued before the stamp existed carries zero. Zero is "unsealed",
/// not epoch-midnight; it cannot qualify on any VWAP day.
#[test]
fn a_zero_issued_at_stamp_does_not_qualify() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let body = item(
        Address::repeat_byte(0x11),
        U256::from(500_000_000_000_000_000u128),
        USD,
    );
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        NodContract::new(storage.clone())
            .callable_bucket_issued_at
            .clear(&body.bucket_key)
            .unwrap();
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, body.issued_at, 1),
            storage.clone(),
        );
        let inspected = hooks::qualify_buckets_with_rate(
            &context,
            &scope,
            &parent,
            USD,
            body.floor_price_minor + U256::from(1),
            first_full_day(body.issued_at),
            crate::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK,
        )
        .unwrap();
        assert_eq!(inspected, 1);
        assert!(
            !api::get_bucket(
                &storage,
                &scope,
                &parent,
                WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key),
            )
            .unwrap()
            .unwrap()
            .is_qualified
        );
    });
}

/// ISO 0 would be parked in a namespace the qualifier loop never visits, so
/// the funnel rejects it before any write.
#[test]
fn zero_reference_currency_is_rejected_at_issuance() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let body = item(Address::repeat_byte(0x66), U256::from(13), 0);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let error = api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap_err();
        assert!(
            error.to_string().contains("reference currency"),
            "unexpected error: {error}"
        );

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 0);
        let bin = NodContract::price_to_bin(body.floor_price_minor).unwrap();
        assert_eq!(nod.unqualified_bin_count.read(&u64::from(bin)).unwrap(), 0);
    });
}

/// The bucket key is derived, not supplied: a caller whose key disagrees with
/// `(day, floor, currency)` is rejected, so the on-chain and Lysis derivations
/// cannot drift apart silently.
#[test]
fn a_bucket_key_that_does_not_match_its_inputs_is_rejected() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut body = item(Address::repeat_byte(0x66), U256::from(13), EUR);
    body.bucket_key = NodContract::bucket_key(body.worldwide_day, U256::from(13), USD);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let error = api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap_err();
        assert!(
            error.to_string().contains("bucket identity mismatch"),
            "unexpected error: {error}"
        );
        assert_eq!(NodContract::new(storage).total_supply().unwrap(), 0);
    });
}

#[test]
fn settled_state_is_exposed_in_nod_data_and_metadata() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        let item = item(Address::repeat_byte(0x86), U256::from(13), USD);
        api::add_nod(&storage, &scope, &parent, &item, U256::from(20)).unwrap();
        NodContract::new(storage.clone())
            .qualify_bucket(&scope, &parent, item.bucket_key)
            .unwrap();
        let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
        api::settle_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, item.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let data = dispatch(
            storage.clone(),
            &scope,
            &parent,
            &INod::nodDataCall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode(),
            item.owner,
            U256::ZERO,
        )
        .unwrap();
        assert!(
            INod::nodDataCall::abi_decode_returns(&data)
                .unwrap()
                .isSettled
        );
        let data = dispatch(
            storage,
            &scope,
            &parent,
            &INod::tokenURICall {
                nodId: item.nod_id.to_u256(),
            }
            .abi_encode(),
            item.owner,
            U256::ZERO,
        )
        .unwrap();
        let uri = INod::tokenURICall::abi_decode_returns(&data).unwrap();
        let json = base64::engine::general_purpose::STANDARD
            .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
            .unwrap();
        assert!(String::from_utf8(json)
            .unwrap()
            .contains("\"trait_type\":\"State\",\"value\":\"Settled\""));
    });
}

#[test]
fn public_lifecycle_reads_use_sealed_terms_and_effective_expiry() {
    use crate::precompile::{dispatch, INod};
    use crate::schema::CallTerms;
    use alloy_sol_types::SolCall;
    use base64::Engine;

    // qualified, paid, called_at, notice, now, expected state, expected deadline
    for (qualified, paid, called_at, notice, now, state, deadline) in [
        (false, false, 0, 17, 118, 0, 0),
        (true, false, 0, 17, 118, 1, 0),
        (true, false, 100, 17, 116, 2, 117),
        (true, false, 100, 17, 117, 2, 117),
        (true, false, 100, 17, 118, 4, 117),
        (true, true, 100, 17, 118, 3, 117),
        (true, false, 100, 0, u64::MAX, 2, u64::MAX),
        (true, false, u64::MAX - 1, 17, u64::MAX, 2, u64::MAX),
    ] {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_timestamp(U256::from(now));
        let scope = ExecutionScope::new();
        let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
        StorageHandle::enter(&mut provider, |storage| {
            seed_compressed_entities_genesis(&storage);
            begin_block(storage.clone(), &scope).unwrap();
            let item = item(Address::repeat_byte(0x87), U256::from(13), USD);
            api::add_nod(&storage, &scope, &parent, &item, U256::from(20)).unwrap();
            let mut nod = NodContract::new(storage.clone());
            nod.seal_bucket_call_terms(
                item.bucket_key,
                CallTerms {
                    call_price: U256::from(937),
                    reference_currency: USD,
                    call_rate: 23,
                    call_window: 432_000,
                    call_threshold: 172_800,
                    call_notice_period: notice,
                },
            )
            .unwrap();
            if qualified {
                nod.qualify_bucket(&scope, &parent, item.bucket_key)
                    .unwrap();
            }
            nod.bucket_called_at
                .write(&item.bucket_key, called_at)
                .unwrap();
            let bucket_id = WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key);
            if paid {
                api::settle_nod(
                    &storage,
                    &scope,
                    api::load_item(&storage, &scope, &parent, item.nod_id)
                        .unwrap()
                        .unwrap(),
                    api::load_bucket(&storage, &scope, &parent, bucket_id)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
            }
            let call = INod::nodDataCall {
                nodId: item.nod_id.to_u256(),
            };
            let bytes = dispatch(
                storage.clone(),
                &scope,
                &parent,
                &call.abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap();
            let data = INod::nodDataCall::abi_decode_returns(&bytes).unwrap();
            assert_eq!(data.effectiveState, state);
            assert_eq!(data.isQualified, qualified);
            assert_eq!(data.isSettled, paid);
            assert_eq!(data.calledAt, called_at);
            assert_eq!(data.settlementDeadline, deadline);
            assert_eq!(data.callPriceMinor, U256::from(937));
            assert_eq!(
                (
                    data.callRate,
                    data.callWindow,
                    data.callThreshold,
                    data.callNoticePeriod
                ),
                (23, 432_000, 172_800, notice)
            );
            let bytes = dispatch(
                storage.clone(),
                &scope,
                &parent,
                &INod::tokenURICall {
                    nodId: item.nod_id.to_u256(),
                }
                .abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap();
            let uri = INod::tokenURICall::abi_decode_returns(&bytes).unwrap();
            let json = String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
                    .unwrap(),
            )
            .unwrap();
            let label = ["Issued", "Qualified", "Called", "Settled", "Called"][usize::from(state)];
            let mut expected = vec![
                format!(r#"{{"trait_type":"State","value":"{label}"}}"#),
                r#"{"trait_type":"Call Price","value":0.000937,"display_type":"number"}"#
                    .to_string(),
            ];
            if called_at != 0 && !paid {
                expected.push(format!(
                    r#"{{"trait_type":"Called At","value":{called_at},"display_type":"date"}}"#
                ));
                if deadline != u64::MAX {
                    expected.push(format!(
                        r#"{{"trait_type":"Settlement Deadline","value":{deadline},"display_type":"date"}}"#
                    ));
                }
            }
            for trait_json in expected {
                assert!(json.contains(&trait_json), "{json}");
            }
            // Cleanup removes the public entity instead of retaining a tombstone.
            api::remove_nod(
                &storage,
                &scope,
                api::load_item(&storage, &scope, &parent, item.nod_id)
                    .unwrap()
                    .unwrap(),
                api::load_bucket(&storage, &scope, &parent, bucket_id)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let error = dispatch(
                storage,
                &scope,
                &parent,
                &call.abi_encode(),
                item.owner,
                U256::ZERO,
            )
            .unwrap_err();
            assert!(
                matches!(error, outbe_primitives::error::PrecompileError::Revert(reason)
                if reason == crate::errors::NodError::NodNotFound.to_string())
            );
        });
    }
}

#[test]
fn transfer_surface_is_soulbound() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;

    let owner = Address::repeat_byte(0x41);
    let other = Address::repeat_byte(0x42);
    let token = U256::from(7);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    StorageHandle::enter(&mut provider, |storage| {
        let call =
            |data: Vec<u8>| dispatch(storage.clone(), &scope, &parent, &data, owner, U256::ZERO);
        for data in [
            INod::transferFromCall {
                from: owner,
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::safeTransferFrom_0Call {
                from: owner,
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::safeTransferFrom_1Call {
                from: owner,
                to: other,
                nodId: token,
                data: Default::default(),
            }
            .abi_encode(),
            INod::approveCall {
                to: other,
                nodId: token,
            }
            .abi_encode(),
            INod::setApprovalForAllCall {
                operator: other,
                approved: true,
            }
            .abi_encode(),
        ] {
            let err = call(data).unwrap_err();
            assert!(format!("{err:?}").contains("non-transferable"), "{err:?}");
        }
        let out = call(INod::getApprovedCall { nodId: token }.abi_encode()).unwrap();
        assert_eq!(
            INod::getApprovedCall::abi_decode_returns(&out).unwrap(),
            Address::ZERO
        );
        let out = call(
            INod::isApprovedForAllCall {
                owner,
                operator: other,
            }
            .abi_encode(),
        )
        .unwrap();
        assert!(!INod::isApprovedForAllCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn metadata_updates_follow_qualification_passes_and_settlement() {
    use crate::precompile::INod;
    use alloy_sol_types::SolEvent;

    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let first = item(Address::repeat_byte(0x51), U256::from(500_000), USD);
    let second = item(Address::repeat_byte(0x52), U256::from(600_000), USD);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        for body in [&first, &second] {
            api::add_nod(&storage, &scope, &parent, body, U256::from(5)).unwrap();
        }
        let context = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1_752_534_000, 1),
            storage.clone(),
        );
        let rate = U256::from(700_000);
        let day = outbe_primitives::time::first_full_day(first.issued_at);
        let budget = crate::constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK;
        let inspected =
            hooks::qualify_buckets_with_rate(&context, &scope, &parent, USD, rate, day, budget)
                .unwrap();
        assert_eq!(inspected, 2);
        hooks::qualify_buckets_with_rate(&context, &scope, &parent, USD, rate, day, budget)
            .unwrap();

        let bucket_id = WwdEntityId::from_day_and_digest(first.worldwide_day, first.bucket_key);
        api::settle_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, first.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    });

    let events = provider.get_events(outbe_primitives::addresses::NOD_ADDRESS);
    let batches: Vec<(U256, U256)> = events
        .iter()
        .filter_map(|log| INod::BatchMetadataUpdate::decode_log_data(log).ok())
        .map(|event| (event._fromTokenId, event._toTokenId))
        .collect();
    let updates: Vec<U256> = events
        .iter()
        .filter_map(|log| INod::MetadataUpdate::decode_log_data(log).ok())
        .map(|event| event._tokenId)
        .collect();
    assert_eq!(batches.len(), 1);
    let (from, to) = batches[0];
    let day_of = |id: U256| WwdEntityId::from(id).worldwide_day().value();
    let day = first.worldwide_day.value();
    assert_eq!((day_of(from), day_of(to)), (day, day));
    assert_eq!(
        (day_of(from - U256::from(1)), day_of(to + U256::from(1))),
        (day - 1, day + 1)
    );
    assert!(from <= first.nod_id.to_u256() && first.nod_id.to_u256() <= to);
    assert_eq!(updates, vec![first.nod_id.to_u256()]);
}

#[test]
fn transfer_logs_announce_issuance_and_removal() {
    use crate::precompile::INod;
    use alloy_sol_types::SolEvent;

    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let owner = Address::repeat_byte(0x61);
    let body = item(owner, U256::from(500_000), USD);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &body, U256::from(5)).unwrap();
        let bucket_id = WwdEntityId::from_day_and_digest(body.worldwide_day, body.bucket_key);
        api::remove_nod(
            &storage,
            &scope,
            api::load_item(&storage, &scope, &parent, body.nod_id)
                .unwrap()
                .unwrap(),
            api::load_bucket(&storage, &scope, &parent, bucket_id)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    });

    let nod_id = body.nod_id.to_u256();
    let transfers: Vec<(Address, Address, U256)> = provider
        .get_events(outbe_primitives::addresses::NOD_ADDRESS)
        .iter()
        .filter_map(|log| INod::Transfer::decode_log_data(log).ok())
        .map(|event| (event.from, event.to, event.tokenId))
        .collect();
    assert_eq!(
        transfers,
        vec![
            (Address::ZERO, owner, nod_id),
            (owner, Address::ZERO, nod_id)
        ]
    );
}

#[test]
fn supported_interfaces_match_the_implemented_selectors() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use outbe_primitives::erc::{
        ERC165_INTERFACE_ID, ERC20_INTERFACE_ID, ERC4906_INTERFACE_ID,
        ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
    };

    let interface_id = |selectors: &[[u8; 4]]| {
        selectors.iter().fold([0u8; 4], |acc, selector| {
            std::array::from_fn(|i| acc[i] ^ selector[i])
        })
    };
    assert_eq!(
        interface_id(&[
            INod::balanceOfCall::SELECTOR,
            INod::ownerOfCall::SELECTOR,
            INod::safeTransferFrom_0Call::SELECTOR,
            INod::safeTransferFrom_1Call::SELECTOR,
            INod::transferFromCall::SELECTOR,
            INod::approveCall::SELECTOR,
            INod::setApprovalForAllCall::SELECTOR,
            INod::getApprovedCall::SELECTOR,
            INod::isApprovedForAllCall::SELECTOR,
        ]),
        ERC721_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            INod::nameCall::SELECTOR,
            INod::symbolCall::SELECTOR,
            INod::tokenURICall::SELECTOR,
        ]),
        ERC721_METADATA_INTERFACE_ID
    );
    assert_eq!(
        interface_id(&[
            INod::totalSupplyCall::SELECTOR,
            INod::tokenByIndexCall::SELECTOR,
            INod::tokenOfOwnerByIndexCall::SELECTOR,
        ]),
        ERC721_ENUMERABLE_INTERFACE_ID
    );

    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    StorageHandle::enter(&mut provider, |storage| {
        let supports = |id: [u8; 4]| {
            let data = INod::supportsInterfaceCall {
                interfaceId: id.into(),
            }
            .abi_encode();
            let out = dispatch(
                storage.clone(),
                &scope,
                &parent,
                &data,
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap();
            INod::supportsInterfaceCall::abi_decode_returns(&out).unwrap()
        };
        for id in [
            ERC165_INTERFACE_ID,
            ERC721_INTERFACE_ID,
            ERC721_METADATA_INTERFACE_ID,
            ERC721_ENUMERABLE_INTERFACE_ID,
            ERC4906_INTERFACE_ID,
        ] {
            assert!(supports(id), "{id:02x?}");
        }
        assert!(!supports(ERC20_INTERFACE_ID));
        assert!(!supports([0xff; 4]));
    });
}

#[test]
fn token_uri_renders_the_nod_image_and_metadata() {
    use crate::precompile::{dispatch, INod};
    use alloy_sol_types::SolCall;
    use base64::Engine;

    let engine = base64::engine::general_purpose::STANDARD;
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let mut body = item(Address::repeat_byte(0x71), U256::from(500_000), USD);
    body.gratis_load_minor = U256::from(1_250_123_456u64);
    let mut provider = HashMapStorageProvider::new(1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        seed_compressed_entities_genesis(&storage);
        begin_block(storage.clone(), &scope).unwrap();
        api::add_nod(&storage, &scope, &parent, &body, U256::from(400_000)).unwrap();
        let read = || {
            let data = INod::tokenURICall {
                nodId: body.nod_id.to_u256(),
            }
            .abi_encode();
            let out = dispatch(
                storage.clone(),
                &scope,
                &parent,
                &data,
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap();
            let uri = INod::tokenURICall::abi_decode_returns(&out).unwrap();
            let json = engine
                .decode(uri.strip_prefix("data:application/json;base64,").unwrap())
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
            let svg = engine
                .decode(
                    json["image"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("data:image/svg+xml;base64,")
                        .unwrap(),
                )
                .unwrap();
            (json, String::from_utf8(svg).unwrap())
        };
        let value = |json: &serde_json::Value, name: &str| {
            json["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["trait_type"] == name)
                .map(|entry| entry["value"].clone())
        };

        let (json, svg) = read();
        assert_eq!(value(&json, "State").unwrap(), "Issued");
        assert!(svg.contains(">ISSUED</text>"));
        let row = |label: &str| svg.find(&format!(">{label}</text>")).unwrap();
        assert!(row("Gratis Load") < row("Entry Price"));
        assert!(row("Entry Price") < row("Floor Price"));
        assert!(row("Floor Price") < row("Call Price"));

        NodContract::new(storage.clone())
            .qualify_bucket(&scope, &parent, body.bucket_key)
            .unwrap();
        let (json, svg) = read();
        let hex = format!("{:064x}", body.nod_id.to_u256());
        let id = format!("{}-{}", body.worldwide_day, &hex[8..16]);
        assert_eq!(json["name"], format!("Nod {id}"));
        assert_eq!(json["description"], crate::constants::TOKEN_DESCRIPTION);
        assert!(!json.to_string().contains("https://"));
        assert_eq!(value(&json, "State").unwrap(), "Qualified");
        assert_eq!(value(&json, "Worldwide Day").unwrap(), 20_260_715);
        assert_eq!(value(&json, "League").unwrap(), 4);
        assert_eq!(value(&json, "Entry Price").unwrap(), 0.4);
        assert_eq!(value(&json, "Floor Price").unwrap(), 0.5);
        assert_eq!(value(&json, "Call Price").unwrap(), 1.424);
        assert_eq!(value(&json, "Gratis Load").unwrap(), 1250.12);
        assert!(value(&json, "Settlement Deadline").is_none());

        assert!(svg.contains(">NOD</text>"));
        assert!(svg.contains(&format!(">{id}</text>")));
        assert!(svg.contains(">QUALIFIED</text>"));
        assert!(svg.contains(">1.424</text>"));
        assert!(svg.contains(">1,250.12</text>"));
        assert!(!svg.contains("Floor Price"));
    });
}
