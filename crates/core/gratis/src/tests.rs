use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, IGratis};
use crate::Gratis;

const CHAIN_ID: u64 = 1;

fn with_gratis<R>(f: impl FnOnce(Gratis) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| f(Gratis::new(storage.clone())))
}

fn with_gratis_mut<R>(f: impl FnOnce(&mut Gratis) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut gratis = Gratis::new(storage.clone());
        f(&mut gratis)
    })
}

#[test]
fn test_metadata() {
    with_gratis(|g| {
        assert_eq!(g.name(), "gratis");
        assert_eq!(g.symbol(), "GRATIS");
        assert_eq!(g.decimals(), 18);
    });
}

#[test]
fn test_initial_state() {
    with_gratis(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        assert_eq!(g.total_supply().unwrap(), U256::ZERO);
        assert_eq!(g.balance_of(alice).unwrap(), U256::ZERO);
        assert_eq!(g.pledged_total_supply().unwrap(), U256::ZERO);
        assert_eq!(g.pledged_of(alice).unwrap(), U256::ZERO);
    });
}

#[test]
fn test_mine() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let amount = U256::from(1000);

        let supply = g.mine(alice, amount).unwrap();
        assert_eq!(supply, amount);
        assert_eq!(g.balance_of(alice).unwrap(), amount);
        assert_eq!(g.total_supply().unwrap(), amount);

        let supply = g.mine(alice, U256::from(500)).unwrap();
        assert_eq!(supply, U256::from(1500));
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(1500));
    });
}

#[test]
fn test_mine_zero_fails() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        assert!(g.mine(alice, U256::ZERO).is_err());
    });
}

#[test]
fn test_mine_zero_address_fails() {
    with_gratis_mut(|g| {
        assert!(g.mine(Address::ZERO, U256::from(100)).is_err());
    });
}

#[test]
fn test_burn() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();

        let remaining = g.burn(alice, U256::from(400)).unwrap();
        assert_eq!(remaining, U256::from(600));
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(600));
        assert_eq!(g.total_supply().unwrap(), U256::from(600));
    });
}

#[test]
fn test_burn_insufficient_fails() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(100)).unwrap();
        assert!(g.burn(alice, U256::from(200)).is_err());
    });
}

#[test]
fn test_transfer_gratis_internal() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");
        g.mine(alice, U256::from(1000)).unwrap();

        g.transfer_gratis(alice, bob, U256::from(300)).unwrap();

        assert_eq!(g.balance_of(alice).unwrap(), U256::from(700));
        assert_eq!(g.balance_of(bob).unwrap(), U256::from(300));
        // total_supply unchanged
        assert_eq!(g.total_supply().unwrap(), U256::from(1000));
    });
}

#[test]
fn test_transfer_gratis_insufficient_fails() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");
        g.mine(alice, U256::from(100)).unwrap();
        assert!(g.transfer_gratis(alice, bob, U256::from(200)).is_err());
    });
}

#[test]
fn test_multiple_users() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        g.mine(alice, U256::from(1000)).unwrap();
        g.mine(bob, U256::from(2000)).unwrap();

        assert_eq!(g.balance_of(alice).unwrap(), U256::from(1000));
        assert_eq!(g.balance_of(bob).unwrap(), U256::from(2000));
        assert_eq!(g.total_supply().unwrap(), U256::from(3000));

        g.burn(bob, U256::from(1000)).unwrap();

        assert_eq!(g.balance_of(alice).unwrap(), U256::from(1000));
        assert_eq!(g.balance_of(bob).unwrap(), U256::from(1000));
        assert_eq!(g.total_supply().unwrap(), U256::from(2000));
    });
}

#[test]
fn test_events_emitted() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let contract_addr = outbe_primitives::addresses::GRATIS_ADDRESS;

    StorageHandle::enter(&mut storage, |storage| {
        let mut g = Gratis::new(storage.clone());
        let alice = address!("0x1111111111111111111111111111111111111111");

        g.mine(alice, U256::from(100)).unwrap();
        g.pledge(alice, U256::from(20)).unwrap();
        g.unpledge(alice, U256::from(10)).unwrap();
        g.burn(alice, U256::from(10)).unwrap();
    });

    // mine + pledge + unpledge + burn = 4 events
    let events = storage.get_events(contract_addr);
    assert_eq!(events.len(), 4);
}

// ---------------------------------------------------------------------------
// Pledge / unpledge — escrow at CREDIS_ADDRESS
// ---------------------------------------------------------------------------

#[test]
fn test_pledge_moves_balance_to_credis_escrow() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::ZERO);

        let total_pledged = g.pledge(alice, U256::from(300)).unwrap();
        assert_eq!(total_pledged, U256::from(300));

        // Balance moved alice → credis; supply unchanged.
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(700));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(300));
        assert_eq!(g.total_supply().unwrap(), U256::from(1000));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(300));

        // Invariant: pledged_total_supply == balance at CREDIS_ADDRESS.
        assert_eq!(
            g.pledged_total_supply().unwrap(),
            g.balance_of(CREDIS_ADDRESS).unwrap()
        );
    });
}

#[test]
fn test_pledge_zero_amount_fails() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        assert!(g.pledge(alice, U256::ZERO).is_err());
    });
}

#[test]
fn test_pledge_insufficient_balance_fails() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(100)).unwrap();
        assert!(g.pledge(alice, U256::from(200)).is_err());

        // Failure must not move state.
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(100));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(g.pledged_total_supply().unwrap(), U256::ZERO);
    });
}

#[test]
fn test_unpledge_returns_balance_from_credis_escrow() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(300)).unwrap();

        let remaining = g.unpledge(alice, U256::from(100)).unwrap();
        assert_eq!(remaining, U256::from(200));

        assert_eq!(g.balance_of(alice).unwrap(), U256::from(800));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(200));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(200));
        assert_eq!(g.total_supply().unwrap(), U256::from(1000));
    });
}

#[test]
fn test_unpledge_zero_amount_fails() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(300)).unwrap();
        assert!(g.unpledge(alice, U256::ZERO).is_err());
    });
}

#[test]
fn test_unpledge_more_than_pledged_fails() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(100)).unwrap();
        assert!(g.unpledge(alice, U256::from(200)).is_err());

        // State invariant: nothing moved on failure.
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(900));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(100));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(100));
    });
}

#[test]
fn test_pledge_multiple_users_share_escrow() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        g.mine(alice, U256::from(1000)).unwrap();
        g.mine(bob, U256::from(2000)).unwrap();

        g.pledge(alice, U256::from(100)).unwrap();
        g.pledge(bob, U256::from(400)).unwrap();

        // Single escrow account aggregates both pledges.
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(500));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(500));

        g.unpledge(alice, U256::from(50)).unwrap();
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(950));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(450));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(450));

        // total_supply never moved.
        assert_eq!(g.total_supply().unwrap(), U256::from(3000));
    });
}

// ---------------------------------------------------------------------------
// Per-account pledge ledger (`pledged_balances`)
// ---------------------------------------------------------------------------

#[test]
fn test_pledged_of_increases_on_pledge_and_decreases_on_unpledge() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        g.mine(alice, U256::from(1000)).unwrap();
        assert_eq!(g.pledged_of(alice).unwrap(), U256::ZERO);

        g.pledge(alice, U256::from(300)).unwrap();
        assert_eq!(g.pledged_of(alice).unwrap(), U256::from(300));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(300));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(300));

        g.unpledge(alice, U256::from(120)).unwrap();
        assert_eq!(g.pledged_of(alice).unwrap(), U256::from(180));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(180));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(180));
    });
}

#[test]
fn test_pledged_of_is_per_account_independent() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        g.mine(alice, U256::from(1000)).unwrap();
        g.mine(bob, U256::from(1000)).unwrap();

        g.pledge(alice, U256::from(100)).unwrap();
        g.pledge(bob, U256::from(70)).unwrap();

        assert_eq!(g.pledged_of(alice).unwrap(), U256::from(100));
        assert_eq!(g.pledged_of(bob).unwrap(), U256::from(70));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(170));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(170));

        g.unpledge(alice, U256::from(50)).unwrap();

        // Bob's ledger is untouched by Alice's unpledge.
        assert_eq!(g.pledged_of(alice).unwrap(), U256::from(50));
        assert_eq!(g.pledged_of(bob).unwrap(), U256::from(70));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(120));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(120));
    });
}

#[test]
fn test_unpledge_rejects_when_caller_underfunded_even_if_escrow_has_balance() {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        g.mine(alice, U256::from(1000)).unwrap();
        g.mine(bob, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(100)).unwrap();
        g.pledge(bob, U256::from(50)).unwrap();
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(150));

        // Bob only pledged 50; cannot unpledge 80 even though the escrow
        // holds 150 in aggregate.
        let err = g.unpledge(bob, U256::from(80)).unwrap_err();
        assert!(
            err.to_string().contains("insufficient pledged balance"),
            "expected insufficient-pledged-balance revert, got: {err}"
        );

        // No state changed.
        assert_eq!(g.pledged_of(alice).unwrap(), U256::from(100));
        assert_eq!(g.pledged_of(bob).unwrap(), U256::from(50));
        assert_eq!(g.pledged_total_supply().unwrap(), U256::from(150));
        assert_eq!(g.balance_of(CREDIS_ADDRESS).unwrap(), U256::from(150));
        assert_eq!(g.balance_of(alice).unwrap(), U256::from(900));
        assert_eq!(g.balance_of(bob).unwrap(), U256::from(950));
    });
}

// ---------------------------------------------------------------------------
// checked_add overflow rejection
// ---------------------------------------------------------------------------

/// Mining more than U256::MAX total_supply must be a Fatal, and state must
/// stay consistent (no partial write).
#[test]
fn test_mine_rejects_total_supply_overflow_across_accounts() {
    with_gratis_mut(|g| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        // Start supply close to MAX via alice.
        let near_max = U256::MAX - U256::from(10u64);
        g.mine(alice, near_max).unwrap();

        // Bob's balance would fit (starts at 0), but total_supply += 100 overflows.
        let err = g.mine(bob, U256::from(100u64)).unwrap_err();
        assert!(err.to_string().contains("overflow"));

        // State invariants: balance for bob not credited, supply unchanged.
        assert_eq!(g.balance_of(bob).unwrap(), U256::ZERO);
        assert_eq!(g.total_supply().unwrap(), near_max);
    });
}

// ---------------------------------------------------------------------------
// Storage layout (legacy-equivalent shape)
// ---------------------------------------------------------------------------

#[test]
fn test_storage_dsl_layout_matches_legacy_shape() {
    with_gratis(|g| {
        assert_eq!(g.total_supply.slot(), U256::ZERO);
        assert_eq!(g.balances.base_slot(), U256::from(1u64));
        assert_eq!(g.pledged_balances.base_slot(), U256::from(2u64));
    });
}

// ---------------------------------------------------------------------------
// Precompile ABI surface — non-transferable token
// ---------------------------------------------------------------------------

fn run_dispatch(call: Bytes, caller: Address) -> outbe_primitives::error::Result<Bytes> {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        dispatch(storage.clone(), &call, caller, U256::ZERO)
    })
}

fn dispatch_call_bytes(call: IGratis::IGratisCalls) -> Bytes {
    Bytes::from(call.abi_encode())
}

#[test]
fn test_precompile_allowance_returns_zero() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let call = dispatch_call_bytes(IGratis::IGratisCalls::allowance(IGratis::allowanceCall {
        owner: alice,
        spender: bob,
    }));
    let out = run_dispatch(call, alice).unwrap();
    let decoded = IGratis::allowanceCall::abi_decode_returns(&out).unwrap();
    assert_eq!(decoded, U256::ZERO);
}

#[test]
fn test_precompile_approve_reverts() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let call = dispatch_call_bytes(IGratis::IGratisCalls::approve(IGratis::approveCall {
        spender: bob,
        amount: U256::from(1u64),
    }));
    let err = run_dispatch(call, alice).unwrap_err();
    assert!(
        err.to_string().contains("transfers are not allowed"),
        "expected transfer-not-allowed revert, got: {err}"
    );
}

#[test]
fn test_precompile_transfer_reverts() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let call = dispatch_call_bytes(IGratis::IGratisCalls::transfer(IGratis::transferCall {
        to: bob,
        amount: U256::from(1u64),
    }));
    let err = run_dispatch(call, alice).unwrap_err();
    assert!(err.to_string().contains("transfers are not allowed"));
}

#[test]
fn test_precompile_transfer_from_reverts() {
    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let call = dispatch_call_bytes(IGratis::IGratisCalls::transferFrom(
        IGratis::transferFromCall {
            from: alice,
            to: bob,
            amount: U256::from(1u64),
        },
    ));
    let err = run_dispatch(call, alice).unwrap_err();
    assert!(err.to_string().contains("transfers are not allowed"));
}

#[test]
fn test_precompile_pledged_of_reflects_state() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    let call = dispatch_call_bytes(IGratis::IGratisCalls::pledgedOf(IGratis::pledgedOfCall {
        account: alice,
    }));

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        // No pledge yet — zero.
        let out = dispatch(storage.clone(), &call, alice, U256::ZERO).unwrap();
        let decoded = IGratis::pledgedOfCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded, U256::ZERO);

        // Seed state and pledge.
        let mut g = Gratis::new(storage.clone());
        g.mine(alice, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(250)).unwrap();

        // Re-query through the precompile.
        let out = dispatch(storage.clone(), &call, alice, U256::ZERO).unwrap();
        let decoded = IGratis::pledgedOfCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded, U256::from(250));
    });
}

#[test]
fn test_precompile_pledged_total_supply_reflects_state() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    // Initial — zero
    let call = dispatch_call_bytes(IGratis::IGratisCalls::pledgedTotalSupply(
        IGratis::pledgedTotalSupplyCall {},
    ));
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let out = dispatch(storage.clone(), &call, alice, U256::ZERO).unwrap();
        let decoded = IGratis::pledgedTotalSupplyCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded, U256::ZERO);

        // Seed state and pledge.
        let mut g = Gratis::new(storage.clone());
        g.mine(alice, U256::from(1000)).unwrap();
        g.pledge(alice, U256::from(250)).unwrap();

        // Re-query through the precompile.
        let out = dispatch(storage.clone(), &call, alice, U256::ZERO).unwrap();
        let decoded = IGratis::pledgedTotalSupplyCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded, U256::from(250));
    });
}

#[test]
fn test_precompile_supports_interface() {
    let alice = address!("0x1111111111111111111111111111111111111111");

    // ERC-165
    let call = dispatch_call_bytes(IGratis::IGratisCalls::supportsInterface(
        IGratis::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes(ERC165_INTERFACE_ID),
        },
    ));
    let out = run_dispatch(call, alice).unwrap();
    assert!(IGratis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

    // ERC-20 (legacy interface ID)
    let call = dispatch_call_bytes(IGratis::IGratisCalls::supportsInterface(
        IGratis::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes([0x36, 0x37, 0x2b, 0x07]),
        },
    ));
    let out = run_dispatch(call, alice).unwrap();
    assert!(IGratis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

    // Unknown
    let call = dispatch_call_bytes(IGratis::IGratisCalls::supportsInterface(
        IGratis::supportsInterfaceCall {
            interfaceId: alloy_primitives::FixedBytes([0xde, 0xad, 0xbe, 0xef]),
        },
    ));
    let out = run_dispatch(call, alice).unwrap();
    assert!(!IGratis::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
}
