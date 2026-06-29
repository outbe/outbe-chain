use alloy_primitives::{address, U256};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::Promis;

fn with_promis_mut<R>(f: impl FnOnce(&mut Promis) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut storage, |storage| {
        let mut promis = Promis::new(storage.clone());
        f(&mut promis)
    })
}

#[test]
fn test_metadata() {
    with_promis_mut(|p| {
        assert_eq!(p.name(), "promis");
        assert_eq!(p.symbol(), "PROMIS");
        assert_eq!(p.decimals(), 18);
    });
}

#[test]
fn test_initial_state() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        assert_eq!(p.total_supply().unwrap(), U256::ZERO);
        assert_eq!(p.balance_of(alice).unwrap(), U256::ZERO);
    });
}

#[test]
fn test_mine() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let supply = p.mine(alice, U256::from(500)).unwrap();
        assert_eq!(supply, U256::from(500));
        assert_eq!(p.balance_of(alice).unwrap(), U256::from(500));
        assert_eq!(p.total_supply().unwrap(), U256::from(500));
    });
}

#[test]
fn test_burn() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        p.mine(alice, U256::from(1000)).unwrap();

        let remaining = p.burn(alice, U256::from(300)).unwrap();
        assert_eq!(remaining, U256::from(700));
        assert_eq!(p.balance_of(alice).unwrap(), U256::from(700));
    });
}

#[test]
fn test_burn_insufficient_fails() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        p.mine(alice, U256::from(100)).unwrap();
        assert!(p.burn(alice, U256::from(200)).is_err());
    });
}

#[test]
fn test_business_failures_return_revert() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let err = p.mine(alice, U256::ZERO).unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message == "amount must be positive")
        );

        let err = p.burn(alice, U256::from(1)).unwrap_err();
        assert!(
            matches!(err, PrecompileError::Revert(message) if message == "insufficient balance")
        );
    });
}

#[test]
fn test_multiple_users() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        p.mine(alice, U256::from(1000)).unwrap();
        p.mine(bob, U256::from(2000)).unwrap();
        assert_eq!(p.total_supply().unwrap(), U256::from(3000));

        p.burn(alice, U256::from(500)).unwrap();
        assert_eq!(p.balance_of(alice).unwrap(), U256::from(500));
        assert_eq!(p.total_supply().unwrap(), U256::from(2500));
    });
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    with_promis_mut(|p| {
        assert_eq!(p.total_supply.slot(), alloy_primitives::U256::ZERO);
        assert_eq!(p.balances.base_slot(), alloy_primitives::U256::from(1u64));
    });
}

#[test]
fn test_iface_id_matches_selector_xor() {
    use alloy_sol_types::SolCall;

    let xor: [u8; 4] = [
        crate::precompile::IPromis::nameCall::SELECTOR,
        crate::precompile::IPromis::symbolCall::SELECTOR,
        crate::precompile::IPromis::decimalsCall::SELECTOR,
        crate::precompile::IPromis::totalSupplyCall::SELECTOR,
        crate::precompile::IPromis::balanceOfCall::SELECTOR,
    ]
    .into_iter()
    .fold([0u8; 4], |acc, sel| {
        [
            acc[0] ^ sel[0],
            acc[1] ^ sel[1],
            acc[2] ^ sel[2],
            acc[3] ^ sel[3],
        ]
    });

    assert_eq!(
        xor,
        crate::precompile::IPROMIS_INTERFACE_ID,
        "IPROMIS_INTERFACE_ID is stale; update it to match the new selector XOR"
    );
}

// ---------------------------------------------------------------------------
// checked_add overflow rejection
// ---------------------------------------------------------------------------

#[test]
fn test_mine_rejects_total_supply_overflow_across_accounts() {
    with_promis_mut(|p| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        let near_max = U256::MAX - U256::from(10u64);
        p.mine(alice, near_max).unwrap();

        let err = p.mine(bob, U256::from(100u64)).unwrap_err();
        assert!(err.to_string().contains("overflow"));
        assert_eq!(p.balance_of(bob).unwrap(), U256::ZERO);
        assert_eq!(p.total_supply().unwrap(), near_max);
    });
}
