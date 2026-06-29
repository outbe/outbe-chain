use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_nod::constants::UNLOCK_PERIOD_SECONDS;
use outbe_nod::{NodContract, NodIssueParams};
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api as factory_api;
use crate::precompile::{dispatch, INodFactory};
use crate::runtime;

/// Reference timestamp used as the baseline "issue time" in test fixtures.
const T_NOW: u64 = 1_700_000_000;

/// Timestamp safely past `T_NOW + UNLOCK_PERIOD_SECONDS`, used as the
/// "mining time" for tests that exercise `mine_gratis`.
const T_AFTER_UNLOCK: u64 = T_NOW + UNLOCK_PERIOD_SECONDS + 1;

fn sample_params() -> NodIssueParams {
    NodIssueParams {
        owner: address!("0x1111111111111111111111111111111111111111"),
        gratis_load_minor: U256::from(1_000_000_000_000_000_000u128),
        worldwide_day: WorldwideDay::new(20241220),
        league_id: 1,
        floor_price_minor: U256::from(540_000_000_000_000_000u128),
        entry_price_minor: U256::from(500_000_000_000_000_000u128),
        // Tests that don't exercise the payment branch set cost to zero so
        // they can pass `Address::ZERO` for asset into
        // `mine_gratis` without hitting the `InvalidAsset` rejection. The
        // dedicated payment tests below override this with non-zero costs
        // after enabling `sub_call_stub` on the provider.
        cost_amount_minor: U256::ZERO,
        issuance_currency: 840,
        reference_currency: 840,
    }
}

/// Dummy asset address for payment-path tests. The provider has
/// `enable_sub_call_stub()` flipped on, so the sub-calls return
/// `default_success()` without touching real ERC20 or vault state.
const PAY_ASSET: Address = address!("0x000000000000000000000000000000000000A11C");

/// Dummy ERC-4626 vault registered for `PAY_ASSET`. The in-process
/// `deposit_liquidity` call routes its inner vault deposit here; the test pins
/// this address' `uint256` share return via `stub_sub_call_at` so the runtime's
/// decode succeeds.
const PAY_VAULT: Address = address!("0x0000000000000000000000000000000000000777");

/// Seeds the vaultprovider gate + a vault for `PAY_ASSET` so nodfactory's
/// in-process `deposit_liquidity` (source-gated) passes its gate and vault
/// lookup. `NOD_FACTORY_ADDRESS` is the registered source; in production
/// genesis seeds the same registration. The exact `LiquiditySource`
/// discriminant is irrelevant — the gate only rejects `UNKNOWN`.
fn seed_reserve_vault(storage: &StorageHandle<'_>) {
    let vp = outbe_vaultprovider::VaultProviderContract::new(storage.clone());
    vp.assets.insert(PAY_ASSET).unwrap();
    vp.asset_vault_set(PAY_ASSET).insert(PAY_VAULT).unwrap();
    vp.liquidity_source_types
        .write(&outbe_primitives::addresses::NOD_FACTORY_ADDRESS, 1u8)
        .unwrap();
}

fn qualify_params(storage: &StorageHandle<'_>, params: &NodIssueParams) {
    let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
    let mut nod = NodContract::new(storage.clone());
    nod.set_qualified(bk, true).unwrap();
}

fn find_valid_nonce(nod_id: U256) -> U256 {
    for n in 0u64..100_000 {
        let nonce = U256::from(n);
        if runtime::validate_pow(nod_id, nonce).is_ok() {
            return nonce;
        }
    }
    panic!("couldn't find valid nonce in 100k attempts");
}

fn with_storage<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, f)
}

#[test]
fn test_issue_nod() {
    with_storage(|storage| {
        let params = sample_params();
        let nod_id = factory_api::issue_nod(&storage, &params).unwrap();
        let expected_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
        assert_eq!(nod_id, expected_id);

        let nod = NodContract::new(storage.clone());
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(nod.owner_of(nod_id).unwrap(), params.owner);

        let stored = nod.get_item(nod_id).unwrap().unwrap();
        assert_eq!(stored.owner, params.owner);
        assert_eq!(stored.worldwide_day, params.worldwide_day);
        assert_eq!(stored.league_id, params.league_id);
        assert_eq!(stored.floor_price_minor, params.floor_price_minor);
        assert_eq!(stored.gratis_load_minor, params.gratis_load_minor);
    });
}

#[test]
fn test_issue_duplicate_fails() {
    with_storage(|storage| {
        let params = sample_params();
        factory_api::issue_nod(&storage, &params).unwrap();
        assert!(factory_api::issue_nod(&storage, &params).is_err());
    });
}

#[test]
fn test_issue_preserves_explicit_cost_of_gratis_without_reinferring_from_floor() {
    with_storage(|storage| {
        let mut params = sample_params();
        params.entry_price_minor = U256::from(101u64);
        params.floor_price_minor =
            params.entry_price_minor * U256::from(108u64) / U256::from(100u64);
        factory_api::issue_nod(&storage, &params).unwrap();

        let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        let nod = NodContract::new(storage);
        let bucket = nod.get_bucket(bk).unwrap().unwrap();
        assert_eq!(bucket.entry_price_minor, U256::from(101u64));
    });
}

#[test]
fn test_bucket_creation_stores_cost_of_gratis() {
    with_storage(|storage| {
        let params = sample_params();
        factory_api::issue_nod(&storage, &params).unwrap();

        let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);
        let nod = NodContract::new(storage);
        let bucket = nod.get_bucket(bk).unwrap().unwrap();
        assert_eq!(bucket.total_nods, 1);
        assert_eq!(bucket.floor_price_minor, params.floor_price_minor);
        assert!(!bucket.is_qualified);
        assert_eq!(bucket.entry_price_minor, params.entry_price_minor);
    });
}

#[test]
fn test_issue_invalid_owner_fails() {
    with_storage(|storage| {
        let mut params = sample_params();
        params.owner = Address::ZERO;
        assert!(factory_api::issue_nod(&storage, &params).is_err());
    });
}

#[test]
fn test_mine_gratis_requires_qualification() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (nod_id, nonce, params) = StorageHandle::enter(&mut provider, |storage| {
        let params = sample_params();
        let nod_id = factory_api::issue_nod(&storage, &params).unwrap();
        let nonce = find_valid_nonce(nod_id);
        (nod_id, nonce, params)
    });

    provider.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut provider, |storage| {
        let err = factory_api::mine_gratis(&storage, params.owner, nod_id, nonce, Address::ZERO)
            .unwrap_err();
        assert!(err.to_string().contains("qualified"));
    });
}

#[test]
fn test_mine_gratis_with_valid_pow() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    let (nod_id, nonce, params) = StorageHandle::enter(&mut provider, |storage| {
        let params = sample_params();
        let nod_id = factory_api::issue_nod(&storage, &params).unwrap();
        qualify_params(&storage, &params);
        let nonce = find_valid_nonce(nod_id);
        (nod_id, nonce, params)
    });

    provider.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut provider, |storage| {
        let gratis =
            factory_api::mine_gratis(&storage, params.owner, nod_id, nonce, Address::ZERO).unwrap();
        assert_eq!(gratis, params.gratis_load_minor);
        let nod = NodContract::new(storage);
        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(nod.get_item(nod_id).unwrap().is_none());
    });
}

#[test]
fn test_mine_gratis_wrong_owner_fails() {
    // Wrong-owner check fires before the unlock check, so the locked period
    // does not affect this test.
    with_storage(|storage| {
        let params = sample_params();
        let nod_id = factory_api::issue_nod(&storage, &params).unwrap();

        let wrong_owner = address!("0x9999999999999999999999999999999999999999");
        assert!(
            factory_api::mine_gratis(&storage, wrong_owner, nod_id, U256::ZERO, Address::ZERO)
                .is_err()
        );
    });
}

#[test]
fn test_mine_gratis_invalid_pow_fails() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let bad_nonce = U256::from(12345678u64);
    let hash = runtime::compute_pow_hash(nod_id, bad_nonce).unwrap();
    if hash[0] == 0 {
        return; // skip silently if the random "bad" nonce happens to satisfy PoW
    }

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut provider, |storage| {
        factory_api::issue_nod(&storage, &params).unwrap();
        qualify_params(&storage, &params);
    });

    provider.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut provider, |storage| {
        assert!(
            factory_api::mine_gratis(&storage, params.owner, nod_id, bad_nonce, Address::ZERO)
                .is_err()
        );
    });
}

#[test]
fn test_mine_gratis_rejects_when_locked() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    // Boundary check: at exactly `unlocks_at - 1` mining must fail with
    // `NodLocked`; at `unlocks_at` it must succeed (assuming bucket
    // qualification and valid PoW).
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
        let nod = NodContract::new(s);
        let stored = nod.get_item(nod_id).unwrap().unwrap();
        assert_eq!(stored.unlocks_at, T_NOW + UNLOCK_PERIOD_SECONDS);
    });

    // One second before unlock — locked.
    storage.set_timestamp(U256::from(T_NOW + UNLOCK_PERIOD_SECONDS - 1));
    StorageHandle::enter(&mut storage, |s| {
        let err =
            factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap_err();
        assert!(err.to_string().contains("locked"), "expected locked: {err}");
    });

    // At unlock — mining succeeds.
    storage.set_timestamp(U256::from(T_NOW + UNLOCK_PERIOD_SECONDS));
    StorageHandle::enter(&mut storage, |s| {
        let gratis =
            factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap();
        assert_eq!(gratis, params.gratis_load_minor);
    });
}

#[test]
fn test_compute_pow_hash_matches_sha256_string_id_plus_u64_nonce() {
    let p = sample_params();
    let nod_id = NodContract::generate_nod_id(p.owner, p.worldwide_day);
    let nonce = U256::from(42u64);
    let got = runtime::compute_pow_hash(nod_id, nonce).unwrap();

    let mut data = NodContract::format_nod_id(nod_id).into_bytes();
    data.extend_from_slice(&42u64.to_be_bytes());
    let expected = ring::digest::digest(&ring::digest::SHA256, &data);
    assert_eq!(got.as_slice(), expected.as_ref());
}

#[test]
fn test_nods_by_owner_sparse_after_mine() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    let mut p1 = sample_params();
    p1.owner = alice;
    let mut p2 = sample_params();
    p2.owner = alice;
    p2.worldwide_day = WorldwideDay::new(u32::from(p1.worldwide_day) + 1);

    let id1 = NodContract::generate_nod_id(p1.owner, p1.worldwide_day);
    let id2 = NodContract::generate_nod_id(p2.owner, p2.worldwide_day);
    let nonce = find_valid_nonce(id1);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &p1).unwrap();
        factory_api::issue_nod(&s, &p2).unwrap();
        qualify_params(&s, &p1);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::mine_gratis(&s, alice, id1, nonce, Address::ZERO).unwrap();

        let nod = NodContract::new(s);
        let nods = nod.get_nods_by_owner(alice).unwrap();
        assert_eq!(nods.len(), 1);
        assert_eq!(nods[0], id2);
    });
}

#[test]
fn test_mine_gratis_atomic_burn_and_gratis_mint() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let gratis_load =
            factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap();
        assert!(!gratis_load.is_zero());

        let nod = NodContract::new(s.clone());
        let gratis = outbe_gratis::Gratis::new(s);

        assert_eq!(nod.total_supply().unwrap(), 0);
        assert!(nod.get_item(nod_id).unwrap().is_none());
        assert_eq!(gratis.balance_of(params.owner).unwrap(), gratis_load);
        assert_eq!(gratis.total_supply().unwrap(), gratis_load);
    });
}

#[test]
fn test_mine_gratis_failure_no_partial_loss() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        let params = sample_params();
        let nod_id = factory_api::issue_nod(&s, &params).unwrap();

        let supply_before = NodContract::new(s.clone()).total_supply().unwrap();
        let wrong_owner = address!("0x9999999999999999999999999999999999999999");
        let result = factory_api::mine_gratis(&s, wrong_owner, nod_id, U256::ZERO, Address::ZERO);
        assert!(result.is_err());

        let nod = NodContract::new(s);
        assert_eq!(nod.total_supply().unwrap(), supply_before);
        assert!(nod.get_item(nod_id).unwrap().is_some());
    });
}

#[test]
fn test_mine_gratis_supply_invariant() {
    // Distinct (owner, wwd) gives distinct ids.
    let p1 = sample_params();
    let mut p2 = sample_params();
    p2.worldwide_day = WorldwideDay::new(u32::from(p1.worldwide_day) + 1);
    p2.gratis_load_minor = U256::from(2_000_000_000_000_000_000u128);
    let id1 = NodContract::generate_nod_id(p1.owner, p1.worldwide_day);
    let nonce = find_valid_nonce(id1);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &p1).unwrap();
        factory_api::issue_nod(&s, &p2).unwrap();
        qualify_params(&s, &p1);
        let nod = NodContract::new(s);
        assert_eq!(nod.total_supply().unwrap(), 2);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let load1 = factory_api::mine_gratis(&s, p1.owner, id1, nonce, Address::ZERO).unwrap();
        let nod = NodContract::new(s.clone());
        let gratis = outbe_gratis::Gratis::new(s);
        assert_eq!(nod.total_supply().unwrap(), 1);
        assert_eq!(gratis.total_supply().unwrap(), load1);
        assert_eq!(gratis.balance_of(p1.owner).unwrap(), load1);
    });
}

#[test]
fn test_precompile_mine_gratis_burns_and_mints() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let call = INodFactory::mineGratisCall {
            nodId: nod_id,
            nonce,
            asset: Address::ZERO,
        };
        let calldata = call.abi_encode();
        let output = dispatch(s.clone(), &calldata, params.owner, U256::ZERO).unwrap();
        let mined = INodFactory::mineGratisCall::abi_decode_returns(&output).unwrap();
        assert_eq!(mined, params.gratis_load_minor);

        let nod = NodContract::new(s.clone());
        assert!(nod.get_item(nod_id).unwrap().is_none());
        let gratis = outbe_gratis::Gratis::new(s);
        assert_eq!(gratis.balance_of(params.owner).unwrap(), mined);
    });
}

#[test]
fn test_precompile_rejects_msg_value() {
    let params = sample_params();
    with_storage(|storage| {
        factory_api::issue_nod(&storage, &params).unwrap();

        let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
        let call = INodFactory::mineGratisCall {
            nodId: nod_id,
            nonce: U256::ZERO,
            asset: Address::ZERO,
        };
        let calldata = call.abi_encode();
        let result = dispatch(storage, &calldata, params.owner, U256::from(1u64));
        assert!(result.is_err());
    });
}

#[test]
fn test_set_clear_does_not_corrupt_root() {
    let params = sample_params();
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);
    let bin_id = NodContract::price_to_bin(params.floor_price_minor).unwrap();
    let bk = NodContract::bucket_key(params.worldwide_day, params.floor_price_minor);

    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        // Issue a bucket → bin tree gets one bit set.
        factory_api::issue_nod(&s, &params).unwrap();
        let nod = NodContract::new(s.clone());
        assert!(tree_math::contains(&nod, bin_id).unwrap());
        // Manually qualify to remove from the bin (mimics the hook).
        let mut nod_mut = NodContract::new(s);
        nod_mut.set_qualified(bk, true).unwrap();
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        // Mining the only NOD in the bucket deletes the bucket; the bin
        // stays marked because we don't touch the bin-tree on mine.
        // That's the documented invariant — the qualifier loop's
        // stale-bucket branch handles dangling bin entries.
        factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap();
    });
}

/// Issue a Nod whose `cost_amount_minor` is non-zero, then mine it with the
/// sub-call stub enabled and dummy asset/vault addresses. Asserts that the
/// new payment branch runs to completion (transferFrom → approve →
/// depositLiquidity stubbed as `default_success()`) and the burn + gratis
/// mint still happen atomically.
#[test]
fn test_mine_gratis_pays_cost_amount() {
    let mut params = sample_params();
    params.cost_amount_minor = U256::from(500_000_000_000_000_000u128);
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    let mut storage = HashMapStorageProvider::new(1);
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(PAY_VAULT, Bytes::from(vec![0u8; 32]));
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
        seed_reserve_vault(&s);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let gratis = factory_api::mine_gratis(&s, params.owner, nod_id, nonce, PAY_ASSET).unwrap();
        assert_eq!(gratis, params.gratis_load_minor);
        let nod = NodContract::new(s);
        assert!(nod.get_item(nod_id).unwrap().is_none());
    });
}

/// With a non-zero `cost_amount_minor`, mining MUST reject a zero `asset`.
#[test]
fn test_mine_gratis_rejects_zero_asset_when_cost_nonzero() {
    let mut params = sample_params();
    params.cost_amount_minor = U256::from(500_000_000_000_000_000u128);
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    let mut storage = HashMapStorageProvider::new(1);
    storage.enable_sub_call_stub();
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let err =
            factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("asset"),
            "expected asset error: {err}"
        );
    });
}

/// With `cost_amount_minor == 0`, mining MUST skip the three sub-calls
/// entirely — including the asset zero-address check. Proves the
/// skip branch by running without `enable_sub_call_stub()`: any real
/// sub-call would fail with `SubCallError::NotAvailable`.
#[test]
fn test_mine_gratis_skips_payment_when_cost_zero() {
    let params = sample_params(); // cost_amount_minor = 0
    assert!(params.cost_amount_minor.is_zero());
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day);
    let nonce = find_valid_nonce(nod_id);

    let mut storage = HashMapStorageProvider::new(1);
    // NOTE: deliberately not enabling sub-call stub.
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |s| {
        factory_api::issue_nod(&s, &params).unwrap();
        qualify_params(&s, &params);
    });

    storage.set_timestamp(U256::from(T_AFTER_UNLOCK));
    StorageHandle::enter(&mut storage, |s| {
        let gratis =
            factory_api::mine_gratis(&s, params.owner, nod_id, nonce, Address::ZERO).unwrap();
        assert_eq!(gratis, params.gratis_load_minor);
    });
}
