//! Promisfactory tests driven by the in-process Promis enclave stand-in. Mint/burn
//! are enclave-routed and modify-key authorized. The tests therefore check balances
//! by decrypting the ciphertext with the account's view key.

use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolInterface};

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::api::{self as promis_api, ModifyAuth};
use outbe_promis::enclave_client::test_enclave;
use outbe_promis::{decrypt_balance, derive_view_key, PromisOp};
use outbe_system_common::addresses::{PROMIS_ADDRESS, PROMIS_FACTORY_ADDRESS};
use outbe_system_common::units::{ONE_COEN, SCALE_1E6_U64};

use crate::precompile::{dispatch, IPromisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

fn auth(op: PromisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    test_enclave::modify_auth(op, account, amount, nonce)
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
            promisMinor: amount,
            mac: alloy_primitives::FixedBytes(a.mac),
            opNonce: a.op_nonce,
        })
        .abi_encode(),
    )
}

#[test]
fn mine_rejects_zero_amount() {
    with_env(|storage| {
        let err = runtime::mint_from_gem(
            storage,
            U256::ONE,
            alice(),
            U256::ZERO,
            auth(PromisOp::Mint, alice(), U256::ZERO, 0),
        )
        .unwrap_err();
        let outbe_primitives::error::PrecompileError::RevertBytes(data) = err else {
            panic!("expected a typed Promis rejection, got {err:?}");
        };
        let rejection = outbe_promis::precompile::IPromis::Rejected::abi_decode(&data).unwrap();
        assert_eq!(rejection.value, "amount must be positive");
    });
}

#[test]
fn mine_coen_success_burns_and_mints_native() {
    with_env(|storage| {
        let one_promis = U256::from(SCALE_1E6_U64);
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            one_promis,
            auth(PromisOp::Mint, alice(), one_promis, 0),
        )
        .unwrap();

        let call = mine_coen_call(one_promis, &auth(PromisOp::Burn, alice(), one_promis, 1));
        let output = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IPromisFactory::mineCoenCall::abi_decode_returns(&output).unwrap();

        // One six-decimal PROMIS becomes one 18-decimal native COEN.
        assert_eq!(minted, ONE_COEN);
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(storage.balance(alice()).unwrap(), ONE_COEN);
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    with_env(|storage| {
        // Alice holds 100 promis but tries to convert 200.
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_promis::errors::PromisError::Rejected("insufficient balance".into())
            )
            .to_string()
        );

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

fn decoded<T>(
    logs: &[alloy_primitives::LogData],
    decode: impl Fn(&alloy_primitives::LogData) -> Result<T, alloy_sol_types::Error>,
) -> Vec<T> {
    logs.iter().filter_map(|log| decode(log).ok()).collect()
}

fn assert_v2_envelope(blob: &[u8], marker: &[u8; 4]) {
    assert_eq!(blob.len(), 92);
    assert_eq!(&blob[8..12], marker);
}

/// Promis burn and native COEN mint are one sale: both logs are present together.
#[test]
fn mine_coen_emits_promis_burned_and_coen_mined() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let one_promis = U256::from(SCALE_1E6_U64);
    StorageHandle::enter(&mut provider, |storage| {
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            one_promis,
            auth(PromisOp::Mint, alice(), one_promis, 0),
        )
        .unwrap();
        let call = mine_coen_call(one_promis, &auth(PromisOp::Burn, alice(), one_promis, 1));
        dispatch(storage, &call, alice(), U256::ZERO).unwrap();
    });
    let burned = decoded(provider.get_events(PROMIS_ADDRESS), |log| {
        outbe_promis::precompile::IPromis::PromisBurned::decode_log_data(log)
    });
    let mined = decoded(provider.get_events(PROMIS_FACTORY_ADDRESS), |log| {
        IPromisFactory::CoenMinedFromPromis::decode_log_data(log)
    });
    test_enclave::uninstall();

    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].account, alice());
    assert_v2_envelope(&burned[0].encryptedAmountMinor, b"PRO2");
    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].account, alice());
    assert_eq!(mined[0].amountMinor, ONE_COEN);
}
