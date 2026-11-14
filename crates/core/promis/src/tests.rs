//! Confidential Promis tests driven by the in-process enclave stand-in
//! (`enclave_client::test_enclave`), which runs the real enclave engine against a
//! fixed dev state key. The tests assert balances by decrypting the ciphertext with
//! the account's view key exactly as a client would.

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_enclave::promis::{decrypt_balance, derive_view_key};
use outbe_enclave::protocol::{ModifyAuth, PromisOp};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::enclave_client::test_enclave;
use crate::precompile::{dispatch, IPromis};
use outbe_system_common::units::SCALE_1E6_U64;

const CHAIN_ID: u64 = 1;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
fn bob() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}

fn auth(op: PromisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    test_enclave::modify_auth(op, account, amount, nonce)
}

fn view_balance(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::balance_ct(storage, account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, account, &blob).unwrap()
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
        let amount = U256::from(SCALE_1E6_U64);
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);

        // Second mine advances the op nonce and accumulates the (hidden) balance.
        let more = U256::from(SCALE_1E6_U64 / 2);
        api::mint_from_intex(
            storage.clone(),
            U256::from(2u64),
            alice(),
            more,
            auth(PromisOp::Mint, alice(), more, 1),
        )
        .unwrap();
        assert_eq!(
            view_balance(storage.clone(), alice()),
            U256::from(SCALE_1E6_U64 + SCALE_1E6_U64 / 2)
        );
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(PromisOp::Mint, alice(), amount, 0);
        api::mint_from_gem(storage.clone(), U256::ONE, alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail. The nonce advanced to 1.
        assert!(api::mint_from_gem(storage.clone(), U256::ONE, alice(), amount, a).is_err());
    });
}

#[test]
fn mine_rejects_forged_auth() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let mut a = auth(PromisOp::Mint, alice(), amount, 0);
        a.mac[0] ^= 0xff;
        assert!(api::mint_from_gem(storage.clone(), U256::ONE, alice(), amount, a).is_err());
    });
}

#[test]
fn burn_reduces_balance() {
    with_env(|storage| {
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            U256::from(1000u64),
            auth(PromisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();

        api::burn(
            storage.clone(),
            alice(),
            U256::from(300u64),
            auth(PromisOp::Burn, alice(), U256::from(300u64), 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(700u64));
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 2);
    });
}

#[test]
fn burn_insufficient_reverts_atomically() {
    with_env(|storage| {
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            U256::from(100u64),
            auth(PromisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        let err = api::burn(
            storage.clone(),
            alice(),
            U256::from(200u64),
            auth(PromisOp::Burn, alice(), U256::from(200u64), 1),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            PrecompileError::from(crate::errors::PromisError::Rejected(
                "insufficient balance".into(),
            ))
            .to_string()
        );
        // No partial state change: balance intact.
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(100u64));
    });
}

#[test]
fn multiple_users_accumulate_balances() {
    with_env(|storage| {
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            U256::from(1000u64),
            auth(PromisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();
        api::mint_from_intex(
            storage.clone(),
            U256::from(2u64),
            bob(),
            U256::from(2000u64),
            auth(PromisOp::Mint, bob(), U256::from(2000u64), 0),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(1000u64));
        assert_eq!(view_balance(storage.clone(), bob()), U256::from(2000u64));
    });
}

#[test]
fn precompile_balance_of_returns_ciphertext() {
    with_env(|storage| {
        let amount = U256::from(777u64);
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        let call = IPromis::balanceOfCall { account: alice() };
        let out = dispatch(
            storage.clone(),
            &call.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let blob = IPromis::balanceOfCall::abi_decode_returns(&out).unwrap();
        // The ABI surface returns the raw ciphertext blob (fixed 92 bytes), not a
        // plaintext balance; decrypting it with the view key recovers the amount.
        assert_eq!(blob.len(), 92);
        assert_eq!(&blob[8..12], b"PRO2");
        let sk = test_enclave::state_key();
        let vk = derive_view_key(&sk, alice()).unwrap();
        assert_eq!(decrypt_balance(&vk, alice(), &blob).unwrap(), amount);
        assert_eq!(
            outbe_enclave::promis_decrypt::decrypt_promis_balance(&vk, alice(), &blob).unwrap(),
            amount
        );

        // opNonceOf reflects the advanced replay counter.
        let call = IPromis::opNonceOfCall { account: alice() };
        let out = dispatch(
            storage.clone(),
            &call.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(IPromis::opNonceOfCall::abi_decode_returns(&out).unwrap(), 1);
    });
}

#[test]
fn never_written_account_returns_empty_blob() {
    with_env(|storage| {
        let call = IPromis::balanceOfCall { account: bob() };
        let out = dispatch(
            storage.clone(),
            &call.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let blob = IPromis::balanceOfCall::abi_decode_returns(&out).unwrap();
        assert!(blob.is_empty());
        assert_eq!(api::op_nonce(storage.clone(), bob()).unwrap(), 0);
    });
}

#[test]
fn precompile_rejects_value() {
    with_env(|storage| {
        let call = IPromis::nameCall {};
        assert!(dispatch(
            storage.clone(),
            &call.abi_encode(),
            Address::ZERO,
            U256::from(1)
        )
        .is_err());
    });
}

#[test]
fn non_transferable_stubs_match_contract() {
    with_env(|storage| {
        let allowance_call = IPromis::allowanceCall {
            owner: alice(),
            spender: bob(),
        };
        let out = dispatch(
            storage.clone(),
            &allowance_call.abi_encode(),
            alice(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IPromis::allowanceCall::abi_decode_returns(&out).unwrap(),
            U256::ZERO
        );

        let transfer_call = IPromis::transferCall {
            to: bob(),
            amount: U256::ONE,
        };
        let err = dispatch(
            storage.clone(),
            &transfer_call.abi_encode(),
            alice(),
            U256::ZERO,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            PrecompileError::from(crate::errors::PromisError::TransferNotAllowed).to_string()
        );
    });
}

#[test]
fn metadata_and_layout() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let p = crate::Promis::new(storage.clone());
        assert_eq!(p.name(), "promis");
        assert_eq!(p.symbol(), "PROMIS");
        assert_eq!(p.decimals(), 6);
        assert_eq!(p.balance_ct.base_slot(), U256::ZERO);
        assert_eq!(p.op_nonce.base_slot(), U256::from(1u64));
    });
}

#[test]
fn an_owner_result_that_differs_from_the_request_is_refused() {
    use crate::runtime::ensure_owner_result;
    use outbe_enclave::protocol::{PromisOpResult, PromisOpStatus};
    let result = |event_amount: u64, next_op_nonce: u64| PromisOpResult {
        status: PromisOpStatus::Applied,
        new_balance: vec![1],
        event_amount: U256::from(event_amount),
        encrypted_event_amount: vec![1],
        next_op_nonce,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    };
    let amount = U256::from(5u64);
    assert!(ensure_owner_result(amount, 3, &result(5, 4)).is_ok());
    let err = ensure_owner_result(amount, 3, &result(4, 4)).unwrap_err();
    assert!(
        err.to_string().contains("owner movement result mismatch"),
        "{err}"
    );
    let err = ensure_owner_result(amount, 3, &result(5, 5)).unwrap_err();
    assert!(err.to_string().contains("wrong op nonce"), "{err}");
    assert!(ensure_owner_result(amount, u64::MAX, &result(5, u64::MAX)).is_err());
    let mut no_balance = result(5, 4);
    no_balance.new_balance.clear();
    let err = ensure_owner_result(amount, 3, &no_balance).unwrap_err();
    assert!(
        err.to_string().contains("owner movement result mismatch"),
        "{err}"
    );
    let mut no_event_amount = result(5, 4);
    no_event_amount.encrypted_event_amount.clear();
    let err = ensure_owner_result(amount, 3, &no_event_amount).unwrap_err();
    assert!(
        err.to_string().contains("owner movement result mismatch"),
        "{err}"
    );
}

/// The first mint seals the event amount apart from the stored balance.
/// A later mint's event opens to the operation delta, not the cumulative balance.
#[test]
fn event_amount_differs_from_balance_and_opens_to_the_delta() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let first_amount = U256::from(SCALE_1E6_U64);
    let second_amount = U256::from(SCALE_1E6_U64 / 2);
    let (first_balance, second_balance) = StorageHandle::enter(&mut provider, |storage| {
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            first_amount,
            auth(PromisOp::Mint, alice(), first_amount, 0),
        )
        .unwrap();
        let first_balance = api::balance_ct(storage.clone(), alice()).unwrap();
        api::mint_from_intex(
            storage.clone(),
            U256::from(2u64),
            alice(),
            second_amount,
            auth(PromisOp::Mint, alice(), second_amount, 1),
        )
        .unwrap();
        let second_balance = api::balance_ct(storage, alice()).unwrap();
        (first_balance, second_balance)
    });
    let first_mint: Vec<_> = provider
        .get_events(outbe_primitives::addresses::PROMIS_ADDRESS)
        .iter()
        .filter_map(|log| IPromis::PromisMinedFromGem::decode_log_data(log).ok())
        .collect();
    let second_mint: Vec<_> = provider
        .get_events(outbe_primitives::addresses::PROMIS_ADDRESS)
        .iter()
        .filter_map(|log| IPromis::PromisMinedFromIntex::decode_log_data(log).ok())
        .collect();
    let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    test_enclave::uninstall();

    assert_eq!(first_mint.len(), 1);
    assert_ne!(
        first_mint[0].encryptedAmountMinor.as_ref(),
        first_balance.as_slice()
    );
    assert_eq!(first_mint[0].encryptedAmountMinor.len(), 92);
    assert_eq!(&first_mint[0].encryptedAmountMinor[8..12], b"PRO2");
    assert_eq!(second_mint.len(), 1);
    let delta = outbe_enclave::promis_decrypt::decrypt_promis_balance(
        &vk,
        alice(),
        &second_mint[0].encryptedAmountMinor,
    )
    .unwrap();
    assert_eq!(delta, second_amount);
    assert_ne!(delta, first_amount + second_amount);
    assert_eq!(
        outbe_enclave::promis_decrypt::decrypt_promis_balance(&vk, alice(), &second_balance)
            .unwrap(),
        first_amount + second_amount
    );
}

fn expect_transfer_not_allowed(storage: StorageHandle<'_>, calldata: &[u8]) {
    let err = dispatch(storage, calldata, alice(), U256::ZERO).unwrap_err();
    let PrecompileError::RevertBytes(data) = err else {
        panic!("expected TransferNotAllowed bytes, got {err}");
    };
    IPromis::TransferNotAllowed::abi_decode(&data).unwrap();
}

/// Promis stays non-transferable: approve and transferFrom revert, ERC-20
/// transfer logs stay absent, and the removed totalSupply selector is rejected.
#[test]
fn non_transferable_surface_emits_no_erc20_logs_and_rejects_total_supply() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let amount = U256::from(100u64);
        api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        api::burn(
            storage.clone(),
            alice(),
            U256::from(40u64),
            auth(PromisOp::Burn, alice(), U256::from(40u64), 1),
        )
        .unwrap();

        expect_transfer_not_allowed(
            storage.clone(),
            &IPromis::transferCall {
                to: bob(),
                amount: U256::ONE,
            }
            .abi_encode(),
        );
        expect_transfer_not_allowed(
            storage.clone(),
            &IPromis::approveCall {
                spender: bob(),
                amount: U256::ONE,
            }
            .abi_encode(),
        );
        expect_transfer_not_allowed(
            storage.clone(),
            &IPromis::transferFromCall {
                from: alice(),
                to: bob(),
                amount: U256::ONE,
            }
            .abi_encode(),
        );

        let err = dispatch(storage, &[0x18, 0x16, 0x0d, 0xdd], alice(), U256::ZERO).unwrap_err();
        let PrecompileError::Revert(message) = err else {
            panic!("expected an unknown-selector revert, got {err}");
        };
        assert!(message.contains("unknown selector"), "{message}");
        assert!(
            message.to_ascii_lowercase().contains("18160ddd"),
            "{message}"
        );
    });
    let promis_logs = provider.get_events(outbe_primitives::addresses::PROMIS_ADDRESS);
    assert!(promis_logs
        .iter()
        .any(|log| { IPromis::PromisMinedFromGem::decode_log_data(log).is_ok() }));
    assert!(promis_logs
        .iter()
        .any(|log| IPromis::PromisBurned::decode_log_data(log).is_ok()));
    for log in provider.get_ordered_events() {
        assert!(IPromis::Transfer::decode_log(log).is_err());
        assert!(IPromis::Approval::decode_log(log).is_err());
    }
    test_enclave::uninstall();
}
