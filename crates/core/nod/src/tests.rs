use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_common::WorldwideDay;
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::erc::{ERC721_ENUMERABLE_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID};
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::dsl::StorageRecord;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::INod;
use crate::schema::NodItemState;
use crate::{NodContract, NodIssueParams};

/// Reference timestamp used as the baseline "issue time" in test fixtures.
const T_NOW: u64 = 1_700_000_000;

fn with_nod<R>(f: impl FnOnce(&mut NodContract) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |storage| {
        let mut nod = NodContract::new(storage.clone());
        f(&mut nod)
    })
}

fn sample_params() -> NodIssueParams {
    NodIssueParams {
        owner: address!("0x1111111111111111111111111111111111111111"),
        gratis_load_minor: U256::from(1_000_000_000_000_000_000u128),
        worldwide_day: WorldwideDay::new(20241220),
        league_id: 1,
        floor_price_minor: U256::from(540_000_000_000_000_000u128),
        entry_price_minor: U256::from(500_000_000_000_000_000u128),
        // cost_of_gratis * gratis_load / SCALE_1E18
        cost_amount_minor: U256::from(500_000_000_000_000_000u128),
        issuance_currency: 840,
        reference_currency: 840,
    }
}

/// Seed a Nod into the entity store at issue-time `now`.
///
/// Mirrors the bookkeeping the production [`outbe_nodfactory::api::issue_nod`]
/// performs: derives the deterministic nod id, computes the bucket key,
/// builds the `NodItemState`, and delegates the slot-write
/// to `state::add_nod`. No PoW / authorization / event emission — those
/// concerns belong to NodFactory and are exercised by NodFactory's tests.
fn seed_nod(nod: &mut NodContract, params: &NodIssueParams, now: u64) -> U256 {
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
    let item = NodItemState {
        nod_id,
        owner: params.owner,
        gratis_load_minor: params.gratis_load_minor,
        worldwide_day: params.worldwide_day,
        league_id: params.league_id,
        floor_price_minor: params.floor_price_minor,
        bucket_key,
        cost_amount_minor: params.cost_amount_minor,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at: now,
    };
    nod.add_nod(&item, params.entry_price_minor).unwrap();
    nod_id
}

fn qualify_params(nod: &mut NodContract, params: &NodIssueParams) {
    let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
    nod.set_qualified(bk, true).unwrap();
}

#[test]
fn test_add_nod_emits_complete_item_then_bucket_projection() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let params = sample_params();
    let nod_id = StorageHandle::enter(&mut provider, |storage| {
        let mut nod = NodContract::new(storage);
        seed_nod(&mut nod, &params, T_NOW)
    });

    let events = provider.get_events(NOD_ADDRESS);
    assert_eq!(events.len(), 2);
    let item = INod::NodBodyStored::decode_log_data(&events[0]).unwrap();
    assert_eq!(item.nodId, nod_id);
    assert_eq!(item.owner, params.owner);
    assert_eq!(
        item.bucketKey,
        NodContract::bucket_key(params.worldwide_day, params.floor_price_minor)
    );
    assert_eq!(item.issuanceCurrency, params.issuance_currency);
    assert_eq!(item.referenceCurrency, params.reference_currency);
    assert_eq!(item.issuedAt, T_NOW);
    let bucket = INod::NodBucketBodyStored::decode_log_data(&events[1]).unwrap();
    assert_eq!(bucket.bucketKey, item.bucketKey);
    assert_eq!(bucket.totalNods, 1);
    assert!(!bucket.isQualified);

    StorageHandle::enter(&mut provider, |storage| {
        let nod = NodContract::new(storage);
        let persisted_item = nod.get_item(nod_id).unwrap().unwrap();
        assert_eq!(item.nodId, persisted_item.nod_id);
        assert_eq!(item.owner, persisted_item.owner);
        assert_eq!(item.gratisLoadMinor, persisted_item.gratis_load_minor);
        assert_eq!(item.worldwideDay, u32::from(persisted_item.worldwide_day));
        assert_eq!(item.leagueId, persisted_item.league_id);
        assert_eq!(item.floorPriceMinor, persisted_item.floor_price_minor);
        assert_eq!(item.bucketKey, persisted_item.bucket_key);
        assert_eq!(item.costAmountMinor, persisted_item.cost_amount_minor);
        assert_eq!(item.issuanceCurrency, persisted_item.issuance_currency);
        assert_eq!(item.referenceCurrency, persisted_item.reference_currency);
        assert_eq!(item.issuedAt, persisted_item.issued_at);

        let persisted_bucket = nod.get_bucket(item.bucketKey).unwrap().unwrap();
        assert_eq!(bucket.bucketKey, persisted_bucket.bucket_key);
        assert_eq!(
            bucket.worldwideDay,
            u32::from(persisted_bucket.worldwide_day)
        );
        assert_eq!(bucket.floorPriceMinor, persisted_bucket.floor_price_minor);
        assert_eq!(bucket.isQualified, persisted_bucket.is_qualified);
        assert_eq!(bucket.totalNods, persisted_bucket.total_nods);
        assert_eq!(bucket.entryPriceMinor, persisted_bucket.entry_price_minor);
    });
}

#[test]
fn test_remove_nod_emits_item_then_final_bucket_projection() {
    let mut provider = HashMapStorageProvider::new(1);
    let params = sample_params();
    let (first, second) = StorageHandle::enter(&mut provider, |storage| {
        let mut nod = NodContract::new(storage);
        let first_id = seed_nod(&mut nod, &params, T_NOW);
        let first = nod.get_item(first_id).unwrap().unwrap();
        let second = NodItemState {
            nod_id: U256::from(2),
            issued_at: T_NOW + 1,
            ..first
        };
        nod.add_nod(&second, params.entry_price_minor).unwrap();
        (first, second)
    });

    let issue_events = provider.get_events(NOD_ADDRESS);
    assert_eq!(issue_events.len(), 4);
    assert_eq!(
        INod::NodBucketBodyStored::decode_log_data(&issue_events[3])
            .unwrap()
            .totalNods,
        2
    );

    provider.clear_events(NOD_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage).remove_nod(&first).unwrap();
    });
    let events = provider.get_events(NOD_ADDRESS);
    assert_eq!(events.len(), 2);
    assert_eq!(
        INod::NodBodyDeleted::decode_log_data(&events[0])
            .unwrap()
            .nodId,
        first.nod_id
    );
    assert_eq!(
        INod::NodBucketBodyStored::decode_log_data(&events[1])
            .unwrap()
            .totalNods,
        1
    );

    provider.clear_events(NOD_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage).remove_nod(&second).unwrap();
    });
    let events = provider.get_events(NOD_ADDRESS);
    assert_eq!(events.len(), 2);
    assert_eq!(
        INod::NodBodyDeleted::decode_log_data(&events[0])
            .unwrap()
            .nodId,
        second.nod_id
    );
    assert_eq!(
        INod::NodBucketBodyDeleted::decode_log_data(&events[1])
            .unwrap()
            .bucketKey,
        second.bucket_key
    );
}

#[test]
fn test_qualification_emits_complete_bucket_before_product_event_and_noop_is_silent() {
    let mut provider = HashMapStorageProvider::new(1);
    let params = sample_params();
    StorageHandle::enter(&mut provider, |storage| {
        let mut nod = NodContract::new(storage);
        seed_nod(&mut nod, &params, T_NOW);
    });
    provider.clear_events(NOD_ADDRESS);

    StorageHandle::enter(&mut provider, |storage| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(1, 1, 1),
            storage,
        );
        crate::hooks::qualify_buckets_with_rate(&ctx, params.floor_price_minor).unwrap();
    });
    assert!(provider.get_events(NOD_ADDRESS).is_empty());

    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage)
            .qualify_bucket(params.worldwide_day, params.floor_price_minor)
            .unwrap();
    });
    let events = provider.get_events(NOD_ADDRESS);
    assert_eq!(events.len(), 2);
    let stored = INod::NodBucketBodyStored::decode_log_data(&events[0]).unwrap();
    assert!(stored.isQualified);
    assert_eq!(stored.totalNods, 1);
    assert_eq!(
        events[1].topics()[0],
        INod::NodBucketQualified::SIGNATURE_HASH
    );

    provider.clear_events(NOD_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        NodContract::new(storage)
            .qualify_bucket(params.worldwide_day, params.floor_price_minor)
            .unwrap();
    });
    assert!(provider.get_events(NOD_ADDRESS).is_empty());
}

#[test]
fn test_initial_state() {
    with_nod(|nod| {
        assert_eq!(nod.total_supply().unwrap(), 0);
    });
}

#[test]
fn test_set_qualified() {
    with_nod(|nod| {
        let params = sample_params();
        seed_nod(nod, &params, T_NOW);

        let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        assert!(!nod
            .get_bucket(bk)
            .unwrap()
            .map(|b| b.is_qualified)
            .unwrap_or(false));

        nod.set_qualified(bk, true).unwrap();
        assert!(nod
            .get_bucket(bk)
            .unwrap()
            .map(|b| b.is_qualified)
            .unwrap_or(false));
    });
}

#[test]
fn test_qualify_bucket_sets_flag_for_dimensions() {
    with_nod(|nod| {
        let params = sample_params();
        seed_nod(nod, &params, T_NOW);

        let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        assert!(!nod
            .get_bucket(bk)
            .unwrap()
            .map(|b| b.is_qualified)
            .unwrap_or(false));

        nod.qualify_bucket(params.worldwide_day, params.floor_price_minor)
            .unwrap();
        assert!(nod
            .get_bucket(bk)
            .unwrap()
            .map(|b| b.is_qualified)
            .unwrap_or(false));

        // Idempotent: second call stays qualified.
        nod.qualify_bucket(params.worldwide_day, params.floor_price_minor)
            .unwrap();
        assert!(nod
            .get_bucket(bk)
            .unwrap()
            .map(|b| b.is_qualified)
            .unwrap_or(false));
    });
}

#[test]
fn test_hook_qualifies_bucket_when_oracle_rate_exceeds_floor_price() {
    use outbe_oracle::contract::OracleContract;
    use outbe_oracle::logic::{init_from_genesis, OracleGenesisConfig};
    use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};

    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        // Oracle setup — register COEN/0xUSD pair; no initial rate.
        {
            let mut oracle = OracleContract::new(storage.clone());
            let cfg = OracleGenesisConfig {
                settlement_currencies: vec![(840, "0xUSD".into(), "COEN".into(), "0xUSD".into())],
                ..OracleGenesisConfig::default_config()
            };
            init_from_genesis(&mut oracle, &cfg).unwrap();
        }

        // Seed a NOD — bucket registered in the bin-tree but unqualified.
        let params = sample_params();
        let bucket_key = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        {
            let mut nod = NodContract::new(storage.clone());
            seed_nod(&mut nod, &params, T_NOW);
            assert!(!nod
                .get_bucket(bucket_key)
                .unwrap()
                .map(|b| b.is_qualified)
                .unwrap_or(false));
            // The bucket is parked in the LB-style bin index, not a heap.
            // Look it up by its floor_price_minor's bin id.
            let bin_id = NodContract::price_to_bin(params.floor_price_minor).unwrap();
            assert_eq!(nod.unqualified_bin_count.read(&bin_id).unwrap(), 1);
            assert!(tree_math::contains(&nod, bin_id).unwrap());
        }

        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage.clone());

        // Hook with NO rate set: stays unqualified.
        <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
        {
            let nod = NodContract::new(storage.clone());
            assert!(!nod
                .get_bucket(bucket_key)
                .unwrap()
                .map(|b| b.is_qualified)
                .unwrap_or(false));
        }

        // Rate below floor: still not qualified.
        {
            let mut oracle = OracleContract::new(storage.clone());
            oracle
                .set_exchange_rate(
                    Address::ZERO,
                    "COEN",
                    "0xUSD",
                    params.floor_price_minor - U256::from(1u64),
                    0,
                    0,
                )
                .unwrap();
        }
        <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
        {
            let nod = NodContract::new(storage.clone());
            assert!(!nod
                .get_bucket(bucket_key)
                .unwrap()
                .map(|b| b.is_qualified)
                .unwrap_or(false));
        }

        // Rate == floor: strict comparison keeps it unqualified.
        {
            let mut oracle = OracleContract::new(storage.clone());
            oracle
                .set_exchange_rate(
                    Address::ZERO,
                    "COEN",
                    "0xUSD",
                    params.floor_price_minor,
                    0,
                    0,
                )
                .unwrap();
        }
        <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
        {
            let nod = NodContract::new(storage.clone());
            assert!(!nod
                .get_bucket(bucket_key)
                .unwrap()
                .map(|b| b.is_qualified)
                .unwrap_or(false));
        }

        // Rate strictly above floor: qualifies on next tick.
        {
            let mut oracle = OracleContract::new(storage.clone());
            oracle
                .set_exchange_rate(
                    Address::ZERO,
                    "COEN",
                    "0xUSD",
                    params.floor_price_minor + U256::from(1u64),
                    0,
                    0,
                )
                .unwrap();
        }
        <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
        {
            let nod = NodContract::new(storage.clone());
            assert!(nod
                .get_bucket(bucket_key)
                .unwrap()
                .map(|b| b.is_qualified)
                .unwrap_or(false));
        }

        // Running the hook again on an already-qualified bucket is a no-op.
        <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
    });
}

#[test]
fn test_get_nods_by_owner() {
    with_nod(|nod| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        // Two NODs for alice require distinct (owner, wwd) tuples under the
        // current formula → use distinct worldwide_days.
        let mut p1 = sample_params();
        p1.owner = alice;
        let mut p2 = sample_params();
        p2.owner = alice;
        p2.worldwide_day = WorldwideDay::new(u32::from(p1.worldwide_day) + 1);
        let mut p3 = sample_params();
        p3.owner = bob;

        let id1 = seed_nod(nod, &p1, T_NOW);
        let id2 = seed_nod(nod, &p2, T_NOW);
        let id3 = seed_nod(nod, &p3, T_NOW);

        let alice_nods = nod.get_nods_by_owner(alice).unwrap();
        assert_eq!(alice_nods.len(), 2);
        assert_eq!(alice_nods[0], id1);
        assert_eq!(alice_nods[1], id2);

        let bob_nods = nod.get_nods_by_owner(bob).unwrap();
        assert_eq!(bob_nods.len(), 1);
        assert_eq!(bob_nods[0], id3);
    });
}

#[test]
fn test_get_nods_by_owner_empty() {
    with_nod(|nod| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let nods = nod.get_nods_by_owner(alice).unwrap();
        assert!(nods.is_empty());
    });
}

#[test]
fn test_format_and_parse_nod_id_roundtrip() {
    let p = sample_params();
    let nod_id = NodContract::generate_nod_id(p.owner, p.worldwide_day);
    let encoded = NodContract::format_nod_id(nod_id);
    assert_eq!(NodContract::parse_nod_id(&encoded).unwrap(), nod_id);
    assert_eq!(
        NodContract::parse_nod_id(&format!("0x{encoded}")).unwrap(),
        nod_id
    );
}

#[test]
fn test_token_uri_contains_legacy_metadata_fields() {
    use base64::Engine;
    with_nod(|nod| {
        let params = sample_params();
        let nod_id = seed_nod(nod, &params, T_NOW);
        qualify_params(nod, &params);

        let token_uri = nod.token_uri(nod_id).unwrap();
        assert!(token_uri.starts_with("data:application/json;base64,"));
        let encoded = token_uri.trim_start_matches("data:application/json;base64,");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let json = String::from_utf8(decoded).unwrap();
        assert!(json.contains("cost_of_gratis_minor"));
        assert!(json.contains("cost_amount_minor"));
        assert!(json.contains("is_qualified"));
        assert!(json.contains(&NodContract::format_nod_id(nod_id)));
    });
}

#[test]
fn test_precompile_supports_metadata_interface() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let calldata = INod::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes(ERC721_METADATA_INTERFACE_ID),
        }
        .abi_encode();
        let result = crate::precompile::dispatch(
            storage.clone(),
            calldata.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let supported = INod::supportsInterfaceCall::abi_decode_returns(&result).unwrap();
        assert!(supported);
    });
}

#[test]
fn test_precompile_owner_of_token_uri_and_tokens_use_uint256_ids() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let params = sample_params();
        let nod_id = {
            let mut nod = NodContract::new(storage.clone());
            let nod_id = seed_nod(&mut nod, &params, T_NOW);
            qualify_params(&mut nod, &params);
            nod_id
        };

        let owner_call = INod::ownerOfCall { nodId: nod_id }.abi_encode();
        let owner_raw = crate::precompile::dispatch(
            storage.clone(),
            owner_call.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let owner = INod::ownerOfCall::abi_decode_returns(&owner_raw).unwrap();
        assert_eq!(owner, params.owner);

        let token_uri_call = INod::tokenURICall { nodId: nod_id }.abi_encode();
        let token_uri_raw = crate::precompile::dispatch(
            storage.clone(),
            token_uri_call.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let token_uri = INod::tokenURICall::abi_decode_returns(&token_uri_raw).unwrap();
        assert!(token_uri.starts_with("data:application/json;base64,"));

        let tokens_call = INod::tokensCall {
            owner: params.owner,
        }
        .abi_encode();
        let tokens_raw = crate::precompile::dispatch(
            storage.clone(),
            tokens_call.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let tokens = INod::tokensCall::abi_decode_returns(&tokens_raw).unwrap();
        assert_eq!(tokens, vec![nod_id]);
    });
}

#[test]
fn test_precompile_supports_erc721_enumerable_interface() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let calldata = INod::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes(ERC721_ENUMERABLE_INTERFACE_ID),
        }
        .abi_encode();
        let result = crate::precompile::dispatch(
            storage.clone(),
            calldata.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let supported = INod::supportsInterfaceCall::abi_decode_returns(&result).unwrap();
        assert!(supported);
    });
}

#[test]
fn test_precompile_enumerable_methods_address_index() {
    // Two seeded NODs for the same owner exercise the enumerable indexes
    // without involving mining (the global-swap-on-mine path is covered by
    // NodFactory tests).
    let p1 = sample_params();
    let mut p2 = sample_params();
    p2.worldwide_day = WorldwideDay::new(u32::from(p1.worldwide_day) + 1);
    let id1 = NodContract::generate_nod_id(p1.owner, p1.worldwide_day);
    let id2 = NodContract::generate_nod_id(p2.owner, p2.worldwide_day);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |storage| {
        {
            let mut nod = NodContract::new(storage.clone());
            seed_nod(&mut nod, &p1, T_NOW);
            seed_nod(&mut nod, &p2, T_NOW);
        }

        // balanceOf(owner) == 2
        let bal_call = INod::balanceOfCall { owner: p1.owner }.abi_encode();
        let bal_raw = crate::precompile::dispatch(
            storage.clone(),
            bal_call.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            INod::balanceOfCall::abi_decode_returns(&bal_raw).unwrap(),
            U256::from(2u64)
        );

        // tokenByIndex(0) and tokenByIndex(1) are the seeded NODs in
        // insertion order; tokenByIndex(2) reverts.
        let by_idx = |i: u64| {
            let cd = INod::tokenByIndexCall {
                index: U256::from(i),
            }
            .abi_encode();
            crate::precompile::dispatch(storage.clone(), cd.as_ref(), Address::ZERO, U256::ZERO)
        };
        assert_eq!(
            INod::tokenByIndexCall::abi_decode_returns(&by_idx(0).unwrap()).unwrap(),
            id1
        );
        assert_eq!(
            INod::tokenByIndexCall::abi_decode_returns(&by_idx(1).unwrap()).unwrap(),
            id2
        );
        assert!(by_idx(2).is_err());

        // tokenOfOwnerByIndex(owner, 1) returns the second NOD; out-of-bounds reverts.
        let own_idx = |i: u64| {
            let cd = INod::tokenOfOwnerByIndexCall {
                owner: p1.owner,
                index: U256::from(i),
            }
            .abi_encode();
            crate::precompile::dispatch(storage.clone(), cd.as_ref(), Address::ZERO, U256::ZERO)
        };
        assert_eq!(
            INod::tokenOfOwnerByIndexCall::abi_decode_returns(&own_idx(1).unwrap()).unwrap(),
            id2
        );
        assert!(own_idx(2).is_err());
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_nod(|nod| {
        assert_eq!(nod.total_supply.slot(), alloy_primitives::U256::ZERO);
        assert_eq!(
            nod.nod_items.base_slot(),
            alloy_primitives::U256::from(1u64)
        );
        assert_eq!(<crate::schema::NodItemState as StorageRecord>::SLOTS, 10);
        assert_eq!(
            nod.nod_buckets.base_slot(),
            alloy_primitives::U256::from(11u64)
        );
        assert_eq!(<crate::schema::NodBucketState as StorageRecord>::SLOTS, 5);
        assert_eq!(
            nod.owner_nod_counts.base_slot(),
            alloy_primitives::U256::from(16u64)
        );
        assert_eq!(
            nod.owner_nod_ids.base_slot(),
            alloy_primitives::U256::from(17u64)
        );
        // LB-style bin index slots (replaced the legacy unqualified_heap).
        assert_eq!(
            nod.bin_tree_root.slot(),
            alloy_primitives::U256::from(18u64)
        );
        assert_eq!(
            nod.bin_tree_mid.base_slot(),
            alloy_primitives::U256::from(19u64)
        );
        assert_eq!(
            nod.bin_tree_leaf.base_slot(),
            alloy_primitives::U256::from(20u64)
        );
        assert_eq!(
            nod.unqualified_bin_count.base_slot(),
            alloy_primitives::U256::from(21u64)
        );
        assert_eq!(
            nod.unqualified_bin_buckets.base_slot(),
            alloy_primitives::U256::from(22u64)
        );
        // ERC-721 Enumerable global index. StorageVec hides its base
        // slot, so we only pin the reverse-map slot here; slot 23 is
        // owned by `global_nod_ids` (List<U256>).
        assert_eq!(
            nod.global_nod_index.base_slot(),
            alloy_primitives::U256::from(24u64)
        );
    });
}

// --- LB bin-tree e2e tests --------------------------------------------------

fn run_qualifier_with_rate(storage: StorageHandle<'_>, rate: U256) {
    use outbe_oracle::contract::OracleContract;
    use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
    {
        let mut oracle = OracleContract::new(storage.clone());
        oracle
            .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate, 0, 0)
            .unwrap();
    }
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage);
    <crate::hooks::NodLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
}

fn init_oracle_pair(storage: StorageHandle<'_>) {
    use outbe_oracle::contract::OracleContract;
    use outbe_oracle::logic::{init_from_genesis, OracleGenesisConfig};
    let mut oracle = OracleContract::new(storage);
    let cfg = OracleGenesisConfig {
        settlement_currencies: vec![(840, "0xUSD".into(), "COEN".into(), "0xUSD".into())],
        ..OracleGenesisConfig::default_config()
    };
    init_from_genesis(&mut oracle, &cfg).unwrap();
}

#[test]
fn test_hook_drains_many_buckets_same_bin_one_block() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        init_oracle_pair(storage.clone());

        // 10 buckets all at the same floor_price → all land in the same bin.
        let floor = U256::from(540_000_000_000_000_000u128);
        let mut bucket_keys = Vec::new();
        {
            let mut nod = NodContract::new(storage.clone());
            for i in 0..10u32 {
                let mut params = sample_params();
                params.worldwide_day = WorldwideDay::new(20241200 + i); // distinct buckets → distinct nod_ids
                params.floor_price_minor = floor;
                seed_nod(&mut nod, &params, T_NOW);
                bucket_keys.push(NodContract::bucket_key(params.worldwide_day, floor));
            }
            let bin_id = NodContract::price_to_bin(floor).unwrap();
            assert_eq!(nod.unqualified_bin_count.read(&bin_id).unwrap(), 10);
            assert!(tree_math::contains(&nod, bin_id).unwrap());
        }

        // Rate strictly above the shared floor (same bin) qualifies all 10.
        run_qualifier_with_rate(storage.clone(), floor + U256::from(1u64));

        let nod = NodContract::new(storage.clone());
        for bk in &bucket_keys {
            let b = nod.get_bucket(*bk).unwrap().unwrap();
            assert!(b.is_qualified, "bucket {bk:?} not qualified");
        }
        let bin_id = NodContract::price_to_bin(floor).unwrap();
        assert_eq!(nod.unqualified_bin_count.read(&bin_id).unwrap(), 0);
        assert!(!tree_math::contains(&nod, bin_id).unwrap());
    });
}

#[test]
fn test_hook_drains_only_bins_at_or_below_rate_bin() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        init_oracle_pair(storage.clone());

        // Issue buckets across a ladder of distinct prices.
        let prices = [
            U256::from(100_000_000_000_000_000u128),   // 0.10
            U256::from(300_000_000_000_000_000u128),   // 0.30
            U256::from(540_000_000_000_000_000u128),   // 0.54  <-- rate sits here
            U256::from(800_000_000_000_000_000u128),   // 0.80
            U256::from(1_500_000_000_000_000_000u128), // 1.50
        ];
        {
            let mut nod = NodContract::new(storage.clone());
            for (i, p) in prices.iter().enumerate() {
                let mut params = sample_params();
                params.worldwide_day = WorldwideDay::new(20241200 + i as u32);
                params.floor_price_minor = *p;
                seed_nod(&mut nod, &params, T_NOW);
            }
        }

        // Set rate to bin of price[2] (0.54).
        run_qualifier_with_rate(storage.clone(), prices[2]);

        let nod = NodContract::new(storage.clone());
        for (i, p) in prices.iter().enumerate() {
            let bk = NodContract::bucket_key((20241200 + i as u32).into(), *p);
            let qualified = nod.get_bucket(bk).unwrap().unwrap().is_qualified;
            // Prices strictly below the rate qualify; prices >= rate stay
            // unqualified. Same-bin tail is exact-checked: prices[2] (== rate)
            // does NOT qualify under the strict comparison.
            let expected = *p < prices[2];
            assert_eq!(qualified, expected, "price {p}: expected={expected}");
        }
    });
}

#[test]
fn test_hook_partial_drain_keeps_high_floor_buckets_in_tail_bin() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        init_oracle_pair(storage.clone());

        // Two buckets that fall in the same bin but have different exact prices.
        // Pick prices near each other so price_to_bin maps both to the same id.
        let p_low = U256::from(540_000_000_000_000_000u128);
        let p_high = p_low + U256::from(1_000_000_000u128);
        // Sanity-check: both prices share a bin.
        let bin_low = NodContract::price_to_bin(p_low).unwrap();
        let bin_high = NodContract::price_to_bin(p_high).unwrap();
        if bin_low != bin_high {
            // If the chosen prices straddle a bin boundary on this ladder,
            // skip the test silently — the property still holds.
            return;
        }

        let mut a = sample_params();
        a.worldwide_day = WorldwideDay::new(20241201);
        a.floor_price_minor = p_low;
        let mut b = sample_params();
        b.worldwide_day = WorldwideDay::new(20241202);
        b.floor_price_minor = p_high;

        {
            let mut nod = NodContract::new(storage.clone());
            seed_nod(&mut nod, &a, T_NOW);
            seed_nod(&mut nod, &b, T_NOW);
            assert_eq!(nod.unqualified_bin_count.read(&bin_low).unwrap(), 2);
        }

        // Rate sits between them: a qualifies, b survives.
        let rate = p_low + U256::from(500_000_000u128);
        assert!(rate < p_high && rate >= p_low);
        run_qualifier_with_rate(storage.clone(), rate);

        let nod = NodContract::new(storage.clone());
        let bk_a = NodContract::bucket_key(a.worldwide_day, p_low);
        let bk_b = NodContract::bucket_key(b.worldwide_day, p_high);
        assert!(nod.get_bucket(bk_a).unwrap().unwrap().is_qualified);
        assert!(!nod.get_bucket(bk_b).unwrap().unwrap().is_qualified);
        // Bin still has the survivor, bit still set.
        assert_eq!(nod.unqualified_bin_count.read(&bin_low).unwrap(), 1);
        assert!(tree_math::contains(&nod, bin_low).unwrap());
    });
}

#[test]
fn test_precompile_nod_data_unqualified() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let params = sample_params();
        let nod_id = {
            let mut nod = NodContract::new(storage.clone());
            seed_nod(&mut nod, &params, T_NOW)
        };

        let calldata = INod::nodDataCall { nodId: nod_id }.abi_encode();
        let raw = crate::precompile::dispatch(
            storage.clone(),
            calldata.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let data = INod::nodDataCall::abi_decode_returns(&raw).unwrap();

        assert_eq!(data.nodId, nod_id);
        assert_eq!(data.owner, params.owner);
        assert_eq!(data.worldwideDay, u32::from(params.worldwide_day));
        assert_eq!(data.leagueId, params.league_id);
        assert_eq!(data.floorPriceMinor, params.floor_price_minor);
        assert_eq!(data.gratisLoadMinor, params.gratis_load_minor);
        assert_eq!(data.costOfGratisMinor, params.entry_price_minor);
        assert!(!data.isQualified);
    });
}

#[test]
fn test_precompile_nod_data_qualified() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let params = sample_params();
        let nod_id = {
            let mut nod = NodContract::new(storage.clone());
            let nod_id = seed_nod(&mut nod, &params, T_NOW);
            qualify_params(&mut nod, &params);
            nod_id
        };

        let calldata = INod::nodDataCall { nodId: nod_id }.abi_encode();
        let raw = crate::precompile::dispatch(
            storage.clone(),
            calldata.as_ref(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let data = INod::nodDataCall::abi_decode_returns(&raw).unwrap();

        assert!(data.isQualified);
        assert_eq!(data.nodId, nod_id);
        assert_eq!(data.costOfGratisMinor, params.entry_price_minor);
    });
}

#[test]
fn test_precompile_nod_data_missing_reverts() {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let calldata = INod::nodDataCall {
            nodId: U256::from(42u64),
        }
        .abi_encode();
        let result = crate::precompile::dispatch(
            storage.clone(),
            calldata.as_ref(),
            Address::ZERO,
            U256::ZERO,
        );
        assert!(result.is_err());
    });
}

#[test]
fn test_qualify_buckets_with_rate_qualifies_buckets_below_rate() {
    use crate::hooks::qualify_buckets_with_rate;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use std::collections::BTreeMap;

    let owners = [
        address!("0x1111111111111111111111111111111111111111"),
        address!("0x2222222222222222222222222222222222222222"),
        address!("0x3333333333333333333333333333333333333333"),
    ];
    // (20260101, 10) and (20260104, 10) share floor=10 → same price bin,
    // exercising the per-bin loop with multiple unqualified entries.
    let days_and_floors: [(u32, u64); 4] = [
        (20260101, 10),
        (20260102, 20),
        (20260103, 15),
        (20260104, 10),
    ];

    let assert_qualified = |nod: &NodContract, expected: &BTreeMap<u32, bool>| {
        let count = u32::try_from(nod.total_supply().unwrap()).unwrap();
        for idx in 0..count {
            let nod_id = nod
                .global_nod_ids
                .get(idx)
                .unwrap()
                .expect("nod_id present");
            let (item, bucket) = nod.get_nod_data(nod_id).unwrap();
            let want = *expected
                .get(&u32::from(item.worldwide_day))
                .unwrap_or_else(|| panic!("no expectation for day {}", item.worldwide_day));
            assert_eq!(
                bucket.is_qualified, want,
                "nod_id {nod_id} day {} qualified mismatch",
                item.worldwide_day,
            );
        }
    };

    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        // Seed a few NODs per (day, floor_price). Distinct owners avoid
        // nod_id collisions (generate_nod_id keys on (owner, worldwide_day)).
        {
            let mut nod = NodContract::new(storage.clone());
            for (day, floor) in days_and_floors {
                for owner in owners {
                    let params = NodIssueParams {
                        owner,
                        worldwide_day: day.into(),
                        league_id: 1,
                        floor_price_minor: U256::from(floor),
                        gratis_load_minor: U256::from(1_000_000_000_000_000_000u128),
                        entry_price_minor: U256::from(floor - 3),
                        cost_amount_minor: U256::from(500_000_000_000_000_000u128),
                        issuance_currency: 840,
                        reference_currency: 840,
                    };
                    seed_nod(&mut nod, &params, T_NOW);
                }
            }

            let initial: BTreeMap<u32, bool> = days_and_floors
                .iter()
                .map(|(day, _)| (*day, false))
                .collect();
            assert_qualified(&nod, &initial);
        }

        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage.clone());
        qualify_buckets_with_rate(&ctx, U256::from(17u64)).unwrap();

        {
            let nod = NodContract::new(storage.clone());
            // At rate=17: floor<17 qualifies (10, 15), floor=20 does not.
            // Both day=20260101 and day=20260104 share floor=10 → both must
            // qualify in the same bin-walk pass.
            let after_17: BTreeMap<u32, bool> = BTreeMap::from([
                (20260101u32, true),
                (20260102u32, false),
                (20260103u32, true),
                (20260104u32, true),
            ]);
            assert_qualified(&nod, &after_17);
        }

        // Rate rises to 22: the remaining day-20260102 bucket (floor=20) qualifies.
        qualify_buckets_with_rate(&ctx, U256::from(22u64)).unwrap();

        {
            let nod = NodContract::new(storage.clone());
            let after_22: BTreeMap<u32, bool> = days_and_floors
                .iter()
                .map(|(day, _)| (*day, true))
                .collect();
            assert_qualified(&nod, &after_22);
        }
    });
}

#[test]
fn test_qualify_buckets_with_rate_keeps_floor_equal_to_rate_unqualified() {
    use crate::hooks::qualify_buckets_with_rate;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};

    let owner = address!("0x1111111111111111111111111111111111111111");
    let day: u32 = 20260101;
    let floor: u64 = 17;

    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        {
            let mut nod = NodContract::new(storage.clone());
            let params = NodIssueParams {
                owner,
                worldwide_day: day.into(),
                league_id: 1,
                floor_price_minor: U256::from(floor),
                gratis_load_minor: U256::from(1_000_000_000_000_000_000u128),
                entry_price_minor: U256::from(floor - 3),
                cost_amount_minor: U256::from(500_000_000_000_000_000u128),
                issuance_currency: 840,
                reference_currency: 840,
            };
            seed_nod(&mut nod, &params, T_NOW);
        }

        let bucket_key = NodContract::bucket_key(day.into(), U256::from(floor));
        let is_qualified = || {
            NodContract::new(storage.clone())
                .nod_buckets
                .get(bucket_key)
                .unwrap()
                .expect("bucket present")
                .is_qualified
        };

        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, 1, 1), storage.clone());

        // rate == floor: strict comparison keeps the bucket unqualified.
        qualify_buckets_with_rate(&ctx, U256::from(floor)).unwrap();
        assert!(!is_qualified(), "floor == rate must stay unqualified");

        // rate one minor unit above floor: now it qualifies.
        qualify_buckets_with_rate(&ctx, U256::from(floor + 1)).unwrap();
        assert!(is_qualified(), "floor < rate must qualify");
    });
}
