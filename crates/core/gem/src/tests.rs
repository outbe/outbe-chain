use alloy_primitives::{address, Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::math::tree_math;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::precompile::{dispatch, IGem};
use crate::schema::{GemAddParams, GemContract, GemState};

const T_NOW: u64 = 1_700_000_000;
const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");

fn with_storage<R>(f: impl FnOnce(&StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(T_NOW));
    StorageHandle::enter(&mut storage, |handle| f(&handle))
}

fn sample_params(owner: Address) -> GemAddParams {
    GemAddParams {
        owner,
        gem_type: 2, // WALLET
        gem_load: U256::from(1_000_000_000_000_000_000u128),
        entry_price: U256::from(500_000_000_000_000_000u128),
        cost_amount: U256::from(500_000_000_000_000_000u128),
        floor_price: U256::from(540_000_000_000_000_000u128),
        issuance_currency: 840,
        reference_currency: 840,
        initial_state: GemState::Issued,
        issued_at: T_NOW,
    }
}

#[test]
fn initial_state_empty() {
    with_storage(|storage| {
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 0);
    });
}

#[test]
fn add_gem_inserts_and_bumps_counters() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 1);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 1);
        assert_eq!(gem.owner_of(gem_id).unwrap(), ALICE);
        assert_eq!(gem.token_of_owner_by_index(ALICE, 0).unwrap(), gem_id);
        let stored = api::get_gem(storage, gem_id).unwrap().unwrap();
        assert_eq!(stored.state, GemState::Issued as u8);
    });
}

#[test]
fn add_gem_rejects_zero_owner() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.owner = Address::ZERO;
        assert!(api::add_gem(storage, p).is_err());
    });
}

#[test]
fn enumerable_returns_only_owned_gems() {
    with_storage(|storage| {
        let g1 = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut p2 = sample_params(ALICE);
        p2.gem_load = U256::from(2u64);
        let g2 = api::add_gem(storage, p2).unwrap();
        let p3 = sample_params(BOB);
        let _g3 = api::add_gem(storage, p3).unwrap();

        let gem = GemContract::new(storage.clone());
        let alice_count = gem.balance_of(ALICE).unwrap();
        let alice_gems: Vec<U256> = (0..alice_count)
            .map(|i| gem.token_of_owner_by_index(ALICE, i).unwrap())
            .collect();
        assert_eq!(alice_gems.len(), 2);
        assert!(alice_gems.contains(&g1));
        assert!(alice_gems.contains(&g2));
        assert_eq!(gem.balance_of(ALICE).unwrap(), 2);
        assert_eq!(gem.balance_of(BOB).unwrap(), 1);
        assert_eq!(gem.total_supply().unwrap(), 3);
    });
}

#[test]
fn burn_requires_settled_state() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        assert!(api::burn(storage, gem_id).is_err());

        api::set_state(storage, gem_id, GemState::Qualified).unwrap();
        assert!(api::burn(storage, gem_id).is_err());

        api::set_state(storage, gem_id, GemState::Settled).unwrap();
        api::burn(storage, gem_id).unwrap();

        let gem = GemContract::new(storage.clone());
        assert_eq!(gem.total_supply().unwrap(), 0);
        assert_eq!(gem.balance_of(ALICE).unwrap(), 0);
        assert!(gem.get_gem(gem_id).unwrap().is_none());
    });
}

#[test]
fn burn_compacts_owner_index() {
    with_storage(|storage| {
        let g1 = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut p2 = sample_params(ALICE);
        p2.gem_load = U256::from(2u64);
        let g2 = api::add_gem(storage, p2).unwrap();
        let mut p3 = sample_params(ALICE);
        p3.gem_load = U256::from(3u64);
        let g3 = api::add_gem(storage, p3).unwrap();

        api::set_state(storage, g1, GemState::Settled).unwrap();
        api::burn(storage, g1).unwrap();

        let gem = GemContract::new(storage.clone());
        let count = gem.balance_of(ALICE).unwrap();
        let remaining: Vec<U256> = (0..count)
            .map(|i| gem.token_of_owner_by_index(ALICE, i).unwrap())
            .collect();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&g2));
        assert!(remaining.contains(&g3));
        assert_eq!(gem.balance_of(ALICE).unwrap(), 2);
    });
}

#[test]
fn qualify_respects_state_and_floor() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000_000_000_000_000u128);

        // Rate equals floor (strict `>`) — must NOT qualify.
        assert!(!gem.qualify(gem_id, T_NOW, floor).unwrap());

        // Rate below floor.
        assert!(!gem
            .qualify(gem_id, T_NOW, floor - U256::from(1u64))
            .unwrap());

        // Rate strictly above floor — qualifies.
        assert!(gem
            .qualify(gem_id, T_NOW, floor + U256::from(1u64))
            .unwrap());
        let after = gem.get_gem(gem_id).unwrap().unwrap();
        assert_eq!(after.state, GemState::Qualified as u8);

        // Second qualify is a no-op (already qualified).
        assert!(!gem
            .qualify(gem_id, T_NOW, floor + U256::from(1u64))
            .unwrap());
    });
}

#[test]
fn add_gem_parks_issued_in_bin_tree() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000_000_000_000_000u128);
        let bin = GemContract::price_to_bin(floor).unwrap();
        assert_eq!(gem.unqualified_bin_count.read(&bin).unwrap(), 1);
        assert_eq!(
            gem.unqualified_bin_gems
                .read(&GemContract::bin_index_key(bin, 0))
                .unwrap(),
            gem_id
        );
        assert!(tree_math::contains(&gem, bin).unwrap());
    });
}

#[test]
fn qualify_removes_from_bin_tree() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();
        let mut gem = GemContract::new(storage.clone());
        let floor = U256::from(540_000_000_000_000_000u128);
        let bin = GemContract::price_to_bin(floor).unwrap();

        assert!(gem
            .qualify(gem_id, T_NOW, floor + U256::from(1u64))
            .unwrap());
        assert_eq!(gem.unqualified_bin_count.read(&bin).unwrap(), 0);
        assert!(!tree_math::contains(&gem, bin).unwrap());
    });
}

#[test]
fn add_gem_qualified_initial_state_skips_bin_tree() {
    with_storage(|storage| {
        let mut p = sample_params(ALICE);
        p.gem_type = 0;
        p.initial_state = GemState::Qualified;
        let _gem_id = api::add_gem(storage, p.clone()).unwrap();
        let gem = GemContract::new(storage.clone());
        let bin = GemContract::price_to_bin(p.floor_price).unwrap();
        assert_eq!(gem.unqualified_bin_count.read(&bin).unwrap(), 0);
        assert!(!tree_math::contains(&gem, bin).unwrap());
    });
}

#[test]
fn scan_skips_bins_above_rate() {
    with_storage(|storage| {
        let mut low = sample_params(ALICE);
        low.floor_price = U256::from(100_000_000_000_000_000u128);
        let low_id = api::add_gem(storage, low.clone()).unwrap();

        let mut high = sample_params(BOB);
        high.floor_price = U256::from(900_000_000_000_000_000u128);
        let _high_id = api::add_gem(storage, high.clone()).unwrap();

        let mut gem = GemContract::new(storage.clone());
        let rate = U256::from(500_000_000_000_000_000u128);

        // Direct qualify call on low gem: passes (floor 0.1 < rate 0.5).
        assert!(gem.qualify(low_id, T_NOW, rate).unwrap());

        // High gem stays Issued (rate 0.5 < floor 0.9). It must still be
        // in its bin and the bin must still be set in the trie.
        let high_bin = GemContract::price_to_bin(high.floor_price).unwrap();
        assert_eq!(gem.unqualified_bin_count.read(&high_bin).unwrap(), 1);
        assert!(tree_math::contains(&gem, high_bin).unwrap());
    });
}

#[test]
fn precompile_transfer_paths_revert() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();

        let calls: Vec<Vec<u8>> = vec![
            IGem::transferFromCall {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::safeTransferFromCall {
                from: ALICE,
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::approveCall {
                to: BOB,
                gemId: gem_id,
            }
            .abi_encode(),
            IGem::setApprovalForAllCall {
                operator: BOB,
                approved: true,
            }
            .abi_encode(),
        ];

        for data in calls {
            let err = dispatch(storage.clone(), &data, ALICE, U256::ZERO).unwrap_err();
            assert!(
                format!("{err:?}").contains("non-transferable"),
                "expected NonTransferable revert, got {err:?}",
            );
        }
    });
}

#[test]
fn precompile_balance_and_owner_views() {
    with_storage(|storage| {
        let gem_id = api::add_gem(storage, sample_params(ALICE)).unwrap();

        let data = IGem::balanceOfCall { owner: ALICE }.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let bal = IGem::balanceOfCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(bal, U256::from(1u64));

        let data = IGem::ownerOfCall { gemId: gem_id }.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let owner = IGem::ownerOfCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(owner, ALICE);

        let data = IGem::totalSupplyCall {}.abi_encode();
        let bytes = dispatch(storage.clone(), &data, Address::ZERO, U256::ZERO).unwrap();
        let total = IGem::totalSupplyCall::abi_decode_returns(&bytes).unwrap();
        assert_eq!(total, U256::from(1u64));
    });
}
