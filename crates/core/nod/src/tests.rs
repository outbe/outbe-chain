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

use crate::{api, state::CallBins, NodBucketState, NodContract, NodItemState, NodRepositoryReader};

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

/// Close `day` just above `floor` on `COEN/<iso>`, registering the pair on first use.
pub(crate) fn close_day_above(storage: &StorageHandle<'_>, iso: u16, floor: U256, day: u32) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let mut index = oracle.pair_index_of(pair).unwrap();
    if index == 0 {
        index = outbe_oracle::api::register_pair(storage.clone(), pair).unwrap();
    }
    oracle
        .utc_day_vwap_value
        .get_nested(&day)
        .write(&index, floor + U256::from(1))
        .unwrap();
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < day {
        oracle.utc_day_vwap_last_finalized.write(day).unwrap();
    }
}

/// Qualify the Nod's bucket: close its first full day above its floor.
pub(crate) fn qualify(storage: &StorageHandle<'_>, item: &NodItemState) {
    close_day_above(
        storage,
        item.reference_currency,
        item.floor_price_minor,
        first_full_day(item.issued_at),
    );
}

fn bucket_of(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &NodRepositoryReader,
    item: &NodItemState,
) -> NodBucketState {
    api::get_bucket(
        storage,
        scope,
        parent,
        WwdEntityId::from_day_and_digest(item.worldwide_day, item.bucket_key),
    )
    .unwrap()
    .unwrap()
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
            nod.retired_bin_tree_root.base_slot(),
            nod.retired_bin_tree_mid.base_slot(),
            nod.retired_bin_tree_leaf.base_slot(),
            nod.retired_unqualified_bin_count.base_slot(),
            nod.retired_unqualified_bin_buckets.base_slot(),
            nod.retired_unqualified_bin_scan_cursor.base_slot(),
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
/// independent bin tries, and a day price only qualifies its own currency.
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
        let call_price = nod
            .callable_bucket_call_price
            .read(&usd.bucket_key)
            .unwrap();
        let bin = NodContract::price_to_bin(call_price).unwrap();

        // Identical call prices land in the same bin id under different namespaces.
        for iso in [USD, EUR] {
            assert_eq!(
                nod.call_bin_count
                    .read(&NodContract::scoped(iso, bin))
                    .unwrap(),
                1
            );
            assert!(tree_math::contains(&CallBins(&nod, iso), bin).unwrap());
        }
        // Negative control: nothing landed in the un-namespaced (ISO 0) key.
        assert_eq!(nod.call_bin_count.read(&u64::from(bin)).unwrap(), 0);

        // A COEN/USD day above the shared floor qualifies the USD bucket only.
        qualify(&storage, &usd);
        let usd_bucket = bucket_of(&storage, &scope, &parent, &usd);
        let eur_bucket = bucket_of(&storage, &scope, &parent, &eur);
        assert!(api::is_qualified(&storage, &usd_bucket).unwrap());
        assert!(
            !api::is_qualified(&storage, &eur_bucket).unwrap(),
            "a COEN/USD price must not qualify a EUR-denominated floor"
        );
        assert!(!usd_bucket.is_qualified, "nothing is stored");
    });
}

/// Q027: qualification uses the same `first_full_day(issued_at)` cutoff as the
/// call scan. A VWAP on the partial issuance UTC day, or any earlier day,
/// cannot qualify the bucket even when it stands strictly above the floor.
#[test]
fn qualification_skips_days_before_first_full_day() {
    let parent = NodRepositoryReader::new(Arc::new(MemoryStorage::new()));
    let body = item(
        Address::repeat_byte(0x11),
        U256::from(500_000_000_000_000_000u128),
        USD,
    );
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
        let qualified =
            || api::is_qualified(&storage, &bucket_of(&storage, &scope, &parent, &body)).unwrap();

        close_day_above(&storage, USD, body.floor_price_minor, issuance_day);
        assert!(!qualified());
        close_day_above(&storage, USD, body.floor_price_minor, full_day);
        assert!(qualified());
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
        qualify(&storage, &body);
        assert!(
            !api::is_qualified(&storage, &bucket_of(&storage, &scope, &parent, &body)).unwrap()
        );
    });
}

/// ISO 0 would be parked in a namespace the call scan never visits, so the
/// funnel rejects it before any write.
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
        assert!(nod.call_bin_tree_root.read(&0).unwrap().is_zero());
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
        qualify(&storage, &item);
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
            .contains("\"trait_type\":\"isSettled\",\"value\":true"));
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
                qualify(&storage, &item);
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
            for (name, value) in [
                ("effectiveState", state.to_string()),
                ("calledAt", called_at.to_string()),
                ("settlementDeadline", deadline.to_string()),
                ("callPriceMinor", "\"937\"".to_string()),
                ("callRate", "23".to_string()),
                ("callWindow", "432000".to_string()),
                ("callThreshold", "172800".to_string()),
                ("callNoticePeriod", notice.to_string()),
            ] {
                assert!(
                    json.contains(&format!("{{\"trait_type\":\"{name}\",\"value\":{value}}}")),
                    "{json}"
                );
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
