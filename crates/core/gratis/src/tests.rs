//! Host integration over the real in-process private ledger.
use crate::{
    api,
    enclave_client::test_enclave,
    precompile::{dispatch, IGratis},
};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    error::PrecompileError,
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider, StorageHandle},
};
use outbe_tee::{
    pledge_ledger,
    pledgenote::OwnerAction,
    protocol::{GratisOp, ModifyAuth},
};
const CHAIN_ID: u64 = 1;
fn alice() -> Address {
    Address::repeat_byte(0x11)
}
fn auth(op: GratisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let key =
        outbe_tee_enclave::gratis::derive_modify_key(&test_enclave::state_key(), account).unwrap();
    ModifyAuth {
        mac: outbe_tee_enclave::gratis::modify_mac(
            &key,
            account,
            op,
            amount,
            nonce,
            B256::from(U256::from(CHAIN_ID)),
        ),
        op_nonce: nonce,
    }
}
fn view_balance(storage: StorageHandle<'_>, account: Address) -> U256 {
    test_enclave::query(&storage, account).balance
}
fn with_env(f: impl FnOnce(StorageHandle<'_>)) {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, f);
    test_enclave::uninstall();
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
        assert_eq!(test_enclave::query(&storage, alice()).next_nonce, 1);

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
fn one_whole_gratis_round_trips_as_one_million_raw_units() {
    with_env(|storage| {
        let one_gratis = U256::from(1_000_000u64);
        api::mint(
            storage.clone(),
            alice(),
            one_gratis,
            auth(GratisOp::Mint, alice(), one_gratis, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), one_gratis);
        assert_eq!(api::total_supply(storage).unwrap(), one_gratis);
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(GratisOp::Mint, alice(), amount, 0);
        api::mint(storage.clone(), alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail - nonce advanced to 1.
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
fn reverted_outer_operation_and_cold_restart_recover_only_committed_commands() {
    with_env(|storage| {
        let amount = U256::from(100);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let before = pledge_ledger::head(&storage).unwrap();
        let result: Result<(), PrecompileError> = storage.with_checkpoint(|| {
            api::burn(
                storage.clone(),
                alice(),
                amount,
                auth(GratisOp::Burn, alice(), amount, 1),
            )?;
            Err(PrecompileError::Revert("later public effect failed".into()))
        });
        assert!(result.is_err());
        assert_eq!(pledge_ledger::head(&storage).unwrap(), before);
        assert_eq!(test_enclave::query(&storage, alice()).balance, amount);
        test_enclave::install();
        assert_eq!(test_enclave::query(&storage, alice()).balance, amount);
        assert_eq!(test_enclave::query(&storage, alice()).next_nonce, 1);
    });
}

#[test]
fn warm_and_cold_query_have_identical_gas_and_encrypted_abi_output() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_production_storage_gas_metering();
    StorageHandle::enter(&mut provider, |storage| {
        let amount = U256::from(777);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
    });
    let envelope = StorageHandle::enter(&mut provider, |storage| {
        test_enclave::owner_envelope(&storage, alice(), 0, OwnerAction::Query)
    });
    let call = IGratis::queryCall {
        encryptedRequest: envelope.into(),
    }
    .abi_encode();
    provider.set_gas_limit(10_000_000);
    let warm = StorageHandle::enter(&mut provider, |storage| {
        dispatch(storage, &call, Address::repeat_byte(0xEE), U256::ZERO).unwrap()
    });
    let warm_gas = provider.gas_used();
    test_enclave::install();
    provider.set_gas_limit(10_000_000);
    let cold = StorageHandle::enter(&mut provider, |storage| {
        dispatch(storage, &call, Address::repeat_byte(0xEE), U256::ZERO).unwrap()
    });
    assert_eq!(warm, cold);
    assert_eq!(provider.gas_used(), warm_gas);
    let encrypted = IGratis::queryCall::abi_decode_returns(&cold).unwrap();
    let view =
        outbe_tee_enclave::gratis::derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    assert_eq!(
        outbe_tee::pledgenote::decrypt_receipt(&view, &encrypted)
            .unwrap()
            .balance,
        U256::from(777)
    );
    test_enclave::uninstall();
}

#[test]
fn metadata_and_nontransferability_remain_intact() {
    with_env(|storage| {
        let gratis = crate::Gratis::new(storage.clone());
        assert_eq!(
            (gratis.name(), gratis.symbol(), gratis.decimals()),
            ("gratis", "GRATIS", 6)
        );
        let call = IGratis::transferCall {
            to: alice(),
            amount: U256::from(1),
        }
        .abi_encode();
        assert!(dispatch(storage, &call, alice(), U256::ZERO).is_err());
    });
}
