//! Promisfactory tests driven by the in-process Promis enclave stand-in. Mint/burn
//! are enclave-routed and modify-key authorized, so balances are asserted by
//! decrypting the ciphertext with the account's view key.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::api::{self as promis_api, ModifyAuth};
use outbe_promis::enclave_client::test_enclave;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use crate::precompile::{dispatch, IPromisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}
fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

/// Build the modify authorization a client would send for `op`.
fn auth(op: PromisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
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
    let blob = promis_api::balance_ct(storage, account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, account, &blob).unwrap()
}

fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

fn mine_coen_call(amount: U256, a: &ModifyAuth) -> Bytes {
    Bytes::from(
        IPromisFactory::IPromisFactoryCalls::mineCoen(IPromisFactory::mineCoenCall {
            amount,
            mac: alloy_primitives::FixedBytes(a.mac),
            opNonce: a.op_nonce,
        })
        .abi_encode(),
    )
}

#[test]
fn mine_rejects_zero_amount() {
    with_env(|storage| {
        let err = runtime::mint(
            storage.clone(),
            alice(),
            U256::ZERO,
            auth(PromisOp::Mint, alice(), U256::ZERO, 0),
        )
        .unwrap_err();
        assert!(err.to_string().contains("amount must be positive"));
    });
}

#[test]
fn mine_coen_success_burns_and_mints_native() {
    with_env(|storage| {
        promis_api::mint(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(PromisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        let call = mine_coen_call(
            U256::from(100u64),
            &auth(PromisOp::Burn, alice(), U256::from(100u64), 1),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();

        // Promis burned to zero; native COEN minted 1:1.
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(storage.balance(alice()).unwrap(), U256::from(100u64));
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    with_env(|storage| {
        // Alice holds 100 promis but tries to convert 200.
        promis_api::mint(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(PromisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        let call = mine_coen_call(
            U256::from(200u64),
            &auth(PromisOp::Burn, alice(), U256::from(200u64), 1),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("insufficient balance"));

        // No native COEN minted, promis untouched (atomic revert).
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(100u64));
    });
}

#[test]
fn supports_interface() {
    with_env(|storage| {
        let call = Bytes::from(
            IPromisFactory::IPromisFactoryCalls::supportsInterface(
                IPromisFactory::supportsInterfaceCall {
                    interfaceId: alloy_primitives::FixedBytes(ERC165_INTERFACE_ID),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IPromisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = Bytes::from(
            IPromisFactory::IPromisFactoryCalls::supportsInterface(
                IPromisFactory::supportsInterfaceCall {
                    interfaceId: alloy_primitives::FixedBytes([0xde, 0xad, 0xbe, 0xef]),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IPromisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    with_env(|storage| {
        // Value is rejected before ABI decode, so the auth fields are irrelevant.
        let call = mine_coen_call(
            U256::from(1u64),
            &auth(PromisOp::Burn, alice(), U256::from(1u64), 0),
        );
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"));
    });
}
