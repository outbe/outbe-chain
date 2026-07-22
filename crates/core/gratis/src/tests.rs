//! Confidential Gratis tests driven by the in-process enclave stand-in
//! (`enclave_client::test_enclave`), which runs the real enclave engine against a
//! fixed dev state key. Balances are asserted by decrypting the ciphertext with
//! the account's view key exactly as a client would.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
    pledge_secret, spend_auth_mac,
};

use crate::api;
use crate::enclave_client::test_enclave;
use crate::precompile::{dispatch, IGratis};

const CHAIN_ID: u64 = 1;

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}
fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
fn bundle() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}

/// Build the modify authorization a client would send for `op`.
fn auth(op: GratisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, account, op, amount, nonce, chain_b256()),
        op_nonce: nonce,
    }
}

fn view_balance(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::balance_ct(storage, account).unwrap();
    decrypt_balance(&vk, account, &blob).unwrap()
}

fn view_pledged(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::pledged_ct(storage, account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, account, &blob).unwrap()
}

/// Run `f` inside a fresh storage scope with the in-process enclave installed.
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

#[test]
fn mine_credits_encrypted_balance() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(api::total_supply(storage.clone()).unwrap(), amount);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);

        // Second mine advances the op nonce and accumulates the (hidden) balance.
        let more = U256::from(500u64);
        api::mint(
            storage.clone(),
            alice(),
            more,
            auth(GratisOp::Mint, alice(), more, 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(1500u64));
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(1500u64)
        );
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(GratisOp::Mint, alice(), amount, 0);
        api::mint(storage.clone(), alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail — nonce advanced to 1.
        assert!(api::mint(storage.clone(), alice(), amount, a).is_err());
    });
}

#[test]
fn mine_rejects_forged_auth() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let mut a = auth(GratisOp::Mint, alice(), amount, 0);
        a.mac[0] ^= 0xff;
        assert!(api::mint(storage.clone(), alice(), amount, a).is_err());
    });
}

#[test]
fn burn_reduces_balance_and_supply() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(1000u64),
            auth(GratisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();
        let remaining = api::burn(
            storage.clone(),
            alice(),
            U256::from(400u64),
            auth(GratisOp::Burn, alice(), U256::from(400u64), 1),
        )
        .unwrap();
        assert_eq!(remaining, U256::from(600u64));
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(600u64));
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(600u64)
        );
    });
}

#[test]
fn burn_insufficient_balance_reverts() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(GratisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();
        assert!(api::burn(
            storage.clone(),
            alice(),
            U256::from(200u64),
            auth(GratisOp::Burn, alice(), U256::from(200u64), 1)
        )
        .is_err());
    });
}

#[test]
fn pledge_consume_and_pay_anadosis_flow() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let sk = test_enclave::state_key();
        // Mine + pledge: balance drained, amount parked in the ticket (pledged_ct
        // still 0), pledged_total counts it.
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let handle = api::pledge(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), amount);

        // requestCredis from a distinct bundle account: alice derives the pledge
        // secret from her modify key + the public handle and binds it to `bundle`.
        // The collateral is credited into alice's OWN pledged ledger (no escrow) and
        // the ticket is deleted; pledged_total is unchanged.
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), bundle());
        let (credis_amount, eoa_ct) =
            api::consume_pledge(storage.clone(), handle, bundle(), spend).unwrap();
        assert_eq!(credis_amount, amount);
        assert_eq!(view_pledged(storage.clone(), alice()), amount);
        assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), amount);
        // The sealed EOA opens back to alice (the plaintext never left the enclave).
        assert!(!eoa_ct.is_empty());
        assert_eq!(
            api::reveal_owner(storage.clone(), &eoa_ct).unwrap(),
            alice()
        );

        // Re-consuming the now-deleted ticket is rejected.
        assert!(api::consume_pledge(storage.clone(), handle, bundle(), spend).is_err());

        // Pay 10 installments: each releases 1/10 from alice's pledged ledger back to
        // her balance.
        let per = amount / U256::from(10u64);
        for _ in 0..10 {
            api::release_to_eoa(storage.clone(), alice(), per).unwrap();
        }
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        // A further release rejected — pledged ledger is empty.
        assert!(api::release_to_eoa(storage.clone(), alice(), per).is_err());
    });
}

#[test]
fn burn_pledged_reduces_supply_and_pledged() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let sk = test_enclave::state_key();
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let handle = api::pledge(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), bundle());
        api::consume_pledge(storage.clone(), handle, bundle(), spend).unwrap();

        // Release 3 installments (300), leaving 700 outstanding, then burn it.
        let per = amount / U256::from(10u64);
        for _ in 0..3 {
            api::release_to_eoa(storage.clone(), alice(), per).unwrap();
        }
        let outstanding = U256::from(700u64);
        let burned = api::burn_pledged(storage.clone(), alice(), outstanding).unwrap();
        assert_eq!(burned, outstanding);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        // total_supply drops by the burned collateral; the 300 released stays liquid.
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(300u64)
        );
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn direct_unpledge_returns_collateral_and_blocks_credis() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let sk = test_enclave::state_key();
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let handle = api::pledge(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();

        // Credis rejected → direct unpledge returns the whole (pending) collateral.
        api::unpledge(
            storage.clone(),
            alice(),
            amount,
            handle,
            auth(GratisOp::Unpledge, alice(), amount, 2),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );

        // The deleted ticket can no longer be consumed for credis.
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), bundle());
        assert!(api::consume_pledge(storage.clone(), handle, bundle(), spend).is_err());
    });
}

// --- Precompile ABI surface (no enclave needed for the non-transferable stubs) ---

fn run_dispatch(call: Bytes, caller: Address) -> outbe_primitives::error::Result<Bytes> {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        dispatch(storage.clone(), &call, caller, U256::ZERO)
    })
}

#[test]
fn precompile_transfer_reverts() {
    let call = Bytes::from(
        IGratis::IGratisCalls::transfer(IGratis::transferCall {
            to: bundle(),
            amount: U256::from(1u64),
        })
        .abi_encode(),
    );
    let err = run_dispatch(call, alice()).unwrap_err();
    assert!(err.to_string().contains("transfers are not allowed"));
}

#[test]
fn precompile_balance_of_returns_ciphertext() {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| {
        let amount = U256::from(777u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let call = Bytes::from(
            IGratis::IGratisCalls::balanceOf(IGratis::balanceOfCall { account: alice() })
                .abi_encode(),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap()
    });
    // The returned bytes are the ciphertext blob; decrypt with the view key.
    let blob = IGratis::balanceOfCall::abi_decode_returns(&out).unwrap();
    let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    assert_eq!(
        decrypt_balance(&vk, alice(), &blob).unwrap(),
        U256::from(777u64)
    );
    test_enclave::uninstall();
}
