//! Confidential gratisfactory tests driven by the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`). Balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key exactly as a
//! client would; writes carry a `ModifyAuth` bound to the account's op-nonce.

use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_gratis::enclave_client::test_enclave;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
};

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

/// Build the modify authorization a client holding `owner`'s modify key sends for
/// `op` on `amount` at `op_nonce`.
fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    let mk = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

/// Give `account` a positive Fidelity index so `pledge_gratis` clears the
/// eligibility gate.
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

/// Run `f` in a fresh storage scope with the in-process enclave installed and the
/// block time set (so Fidelity reads a non-zero `now`).
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    let out = StorageHandle::enter(&mut storage, |s| f(s.clone()));
    test_enclave::uninstall();
    out
}

fn pledge_call(a: ModifyAuth, amount: U256) -> Bytes {
    Bytes::from(
        IGratisFactory::IGratisFactoryCalls::pledgeGratis(IGratisFactory::pledgeGratisCall {
            amount,
            mac: FixedBytes(a.mac),
            opNonce: a.op_nonce,
        })
        .abi_encode(),
    )
}

#[test]
fn pledge_debits_balance_and_credits_pledged_ledger() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let seed = amount * U256::from(2u64);
        outbe_gratis::api::mine(
            storage.clone(),
            alice(),
            seed,
            auth(GratisOp::Mine, alice(), seed, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        // Pledge at op-nonce 1 (mine advanced it from 0).
        let out = dispatch(
            storage.clone(),
            &pledge_call(auth(GratisOp::Pledge, alice(), amount, 1), amount),
            alice(),
            U256::ZERO,
        )
        .unwrap();
        let handle = IGratisFactory::pledgeGratisCall::abi_decode_returns(&out).unwrap();
        assert_ne!(handle, B256::ZERO, "a pledge handle is returned");

        // Balance debited, per-account pledged ledger + aggregate credited.
        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(view_pledged(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            amount
        );
    });
}

#[test]
fn pledge_rejects_wrong_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        outbe_gratis::api::mine(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mine, alice(), amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        // op-nonce is 1 after the mine; a stale/forged 5 must be rejected.
        let err = dispatch(
            storage.clone(),
            &pledge_call(auth(GratisOp::Pledge, alice(), amount, 5), amount),
            alice(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("op nonce"), "got: {err}");
    });
}

#[test]
fn unpledge_returns_collateral_to_pledger() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        outbe_gratis::api::mine(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mine, alice(), amount, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        let handle = runtime::pledge_gratis(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
        )
        .unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);

        // Direct unpledge (credis rejected) at op-nonce 2.
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::unpledgeGratis(
                IGratisFactory::unpledgeGratisCall {
                    amount,
                    pledgeHandle: handle,
                    mac: FixedBytes(auth(GratisOp::Unpledge, alice(), amount, 2).mac),
                    opNonce: 2,
                },
            )
            .abi_encode(),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();

        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn mine_mints_gratis_and_records_fidelity_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        let later = CREATED_AT + ONE_YEAR_SECS;
        let rcfi_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .compute_fidelity_index(alice(), later)
            .unwrap();
        assert_eq!(rcfi_before, U256::ZERO);

        runtime::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mine, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            amount
        );

        // The acquisition cohort was recorded, so aged RCFI a year later is positive.
        let rcfi_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .compute_fidelity_index(alice(), later)
            .unwrap();
        assert!(rcfi_after > U256::ZERO);
    });
}

#[test]
fn mine_rejects_zero_amount() {
    with_env(|storage| {
        let err = runtime::mint(
            storage.clone(),
            alice(),
            U256::ZERO,
            auth(GratisOp::Mine, alice(), U256::ZERO, 0),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("amount must be positive"),
            "got: {err}"
        );
    });
}

#[test]
fn mine_coen_burns_gratis_mints_native_and_records_sale_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        outbe_gratis::api::mine(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mine, alice(), amount, 0),
        )
        .unwrap();
        outbe_fidelity::api::cohort_in(
            storage.clone(),
            alice(),
            amount,
            CREATED_AT - ONE_YEAR_SECS,
        )
        .unwrap();
        let rcfi_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert!(rcfi_before > U256::ZERO);

        // mineCoen burns gratis (op = Burn) at op-nonce 1.
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IGratisFactory::mineCoenCall::abi_decode_returns(&out).unwrap();
        assert_eq!(minted, amount);

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(storage.balance(alice()).unwrap(), amount);

        let rcfi_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert_eq!(rcfi_after, U256::ZERO);
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    with_env(|storage| {
        outbe_gratis::api::mine(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(GratisOp::Mine, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        let amount = U256::from(200u64);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("insufficient balance"),
            "got: {err}"
        );

        // Atomic revert: no COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), U256::from(100u64));
    });
}

#[test]
fn supports_interface() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes(ERC165_INTERFACE_ID),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes([0xde, 0xad, 0xbe, 0xef]),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::pledgeGratis(IGratisFactory::pledgeGratisCall {
                amount: U256::from(1u64),
                mac: FixedBytes([0u8; 32]),
                opNonce: 0,
            })
            .abi_encode(),
        );
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"), "got: {err}");
    });
}
