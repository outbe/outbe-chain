//! Promisfactory tests driven by the in-process Promis enclave stand-in. Mint/burn
//! are enclave-routed and modify-key authorized. The tests therefore check balances
//! by decrypting the ciphertext with the account's view key.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolInterface};

use outbe_common::erc::ERC165_INTERFACE_ID;
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};
use outbe_primitives::addresses::{GRATIS_ADDRESS, PROMIS_ADDRESS, PROMIS_FACTORY_ADDRESS};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::units::{ONE_COEN, SCALE_1E6_U64};
use outbe_promis::api::{self as promis_api, ModifyAuth};
use outbe_promis::enclave_client::test_enclave;
use outbe_tee::protocol::{GratisOp, PromisOp};
use outbe_tee_enclave::promis::{decrypt_balance, derive_view_key};

use crate::precompile::{dispatch, IPromisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}
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

/// Run `f` with the Promis, Gratis and Fidelity in-process enclaves installed and
/// the block time set. The gratis mint records a Fidelity acquisition cohort at
/// `now`, so a zero timestamp would not exercise it.
fn with_gratis_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    outbe_gratis::enclave_client::test_enclave::install();
    outbe_fidelity::enclave_client::test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    outbe_fidelity::enclave_client::test_enclave::uninstall();
    outbe_gratis::enclave_client::test_enclave::uninstall();
    test_enclave::uninstall();
    out
}

/// The Gratis modify authorization for `account` (the confidential gratis ledger is
/// keyed independently from promis).
fn gratis_auth(op: GratisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = outbe_gratis::enclave_client::test_enclave::state_key();
    let mk = outbe_tee_enclave::gratis::derive_modify_key(&sk, account).unwrap();
    ModifyAuth {
        mac: outbe_tee_enclave::gratis::modify_mac(
            &mk,
            &outbe_tee_enclave::gratis::ModifyOperation {
                account,
                op,
                amount,
                op_nonce: nonce,
                chain_id: chain_b256(),
            },
        ),
        op_nonce: nonce,
    }
}

/// Decrypt `account`'s confidential gratis balance with its view key.
fn gratis_view_balance(storage: &StorageHandle<'_>, account: Address) -> U256 {
    let sk = outbe_gratis::enclave_client::test_enclave::state_key();
    let vk = outbe_tee_enclave::gratis::derive_view_key(&sk, account).unwrap();
    let blob = outbe_gratis::api::balance_ct(storage.clone(), account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    outbe_tee_enclave::gratis::decrypt_balance(&vk, account, &blob).unwrap()
}

fn eve() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}

fn mine_gratis_with_ciphertext(
    encrypted: Vec<u8>,
    promis: &ModifyAuth,
    gratis: &ModifyAuth,
) -> Bytes {
    Bytes::from(
        IPromisFactory::IPromisFactoryCalls::mineGratis(IPromisFactory::mineGratisCall {
            encryptedPromisMinor: encrypted.into(),
            promisMac: alloy_primitives::FixedBytes(promis.mac),
            promisOpNonce: promis.op_nonce,
            gratisMac: alloy_primitives::FixedBytes(gratis.mac),
            gratisOpNonce: gratis.op_nonce,
        })
        .abi_encode(),
    )
}

fn mine_gratis_call(amount: U256, promis: &ModifyAuth, gratis: &ModifyAuth) -> Bytes {
    let encrypted = test_enclave::encrypt_mine_gratis_amount(alice(), amount);
    mine_gratis_with_ciphertext(encrypted, promis, gratis)
}

#[test]
fn mine_gratis_burns_promis_mints_gratis_creating_fidelity_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    with_gratis_env(|storage| {
        let amount = U256::from(1_000u64);

        // Seed only (confidential) promis to convert. No Fidelity cohort exists yet.
        // Promis is fidelity-neutral, so the aged RCFI a year out sits at the floor
        // up front. The post-conversion check then proves that the conversion
        // recorded a fresh gratis cohort, and that the cohort did not pre-exist.
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let later = CREATED_AT + ONE_YEAR_SECS;
        let league_before =
            outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_before, MIN_LEAGUE);

        // Both ledgers are enclave-confidential and independently keyed, so the call
        // carries two modify authorizations at each ledger's current op-nonce. The
        // seed mint already advanced promis to 1. Gratis is fresh (0).
        let pa = auth(PromisOp::Burn, alice(), amount, 1);
        let ga = gratis_auth(GratisOp::Mint, alice(), amount, 0);
        let call = mine_gratis_call(amount, &pa, &ga);
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted_ct = IPromisFactory::mineGratisCall::abi_decode_returns(&out).unwrap();
        let gratis_vk = outbe_tee_enclave::gratis::derive_view_key(
            &outbe_gratis::enclave_client::test_enclave::state_key(),
            alice(),
        )
        .unwrap();
        assert_eq!(
            outbe_tee_enclave::gratis::decrypt_balance(&gratis_vk, alice(), &minted_ct).unwrap(),
            amount
        );

        // Promis fully burned; gratis minted 1:1 to the account (decrypt both
        // confidential balances to check).
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(gratis_view_balance(&storage, alice()), amount);
        assert_eq!(
            gratis_view_balance(&storage, alice()) + view_balance(storage.clone(), alice()),
            amount
        );

        // A fresh gratis acquisition cohort was recorded at conversion time
        // (CREATED_AT): sole holder, no sales -> top league a year later. If the
        // conversion stopped recording the cohort, this would stay at the floor.
        let league_after = outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_after, MAX_LEAGUE);
    });
}

/// The conversion with insufficient balance must fail with no partial state: no
/// promis burned, no gratis minted (atomic revert).
#[test]
fn mine_gratis_rejects_insufficient_balance() {
    with_gratis_env(|storage| {
        // Alice holds 100 (confidential) promis but tries to convert 200.
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            U256::from(100u64),
            auth(PromisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        // The promis burn fails before execution reaches the gratis mint. Nothing
        // checks the gratis auth (zero placeholder). The promis burn auth must be
        // valid to reach the balance check (op-nonce 1 after the seed mint).
        let pa = auth(PromisOp::Burn, alice(), U256::from(200u64), 1);
        let placeholder = ModifyAuth {
            mac: [0u8; 32],
            op_nonce: 0,
        };
        let call = mine_gratis_call(U256::from(200u64), &pa, &placeholder);
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_promis::errors::PromisError::Rejected("insufficient balance".into())
            )
            .to_string()
        );

        // No gratis minted (no ciphertext ever written), promis untouched.
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(100u64));
        assert!(outbe_gratis::api::balance_ct(storage.clone(), alice())
            .unwrap()
            .is_empty());
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

/// Promis burn and Gratis mint are one conversion: both logs are present together.
#[test]
fn mine_gratis_emits_promis_burned_and_gratis_mined() {
    test_enclave::install();
    outbe_gratis::enclave_client::test_enclave::install();
    outbe_fidelity::enclave_client::test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(CREATED_AT));
    let amount = U256::from(1_000u64);
    StorageHandle::enter(&mut provider, |storage| {
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let promis_auth = auth(PromisOp::Burn, alice(), amount, 1);
        let gratis_auth = gratis_auth(GratisOp::Mint, alice(), amount, 0);
        let call = mine_gratis_call(amount, &promis_auth, &gratis_auth);
        dispatch(storage, &call, alice(), U256::ZERO).unwrap();
    });
    let burned = decoded(provider.get_events(PROMIS_ADDRESS), |log| {
        outbe_promis::precompile::IPromis::PromisBurned::decode_log_data(log)
    });
    let minted = decoded(provider.get_events(GRATIS_ADDRESS), |log| {
        outbe_gratis::precompile::IGratis::GratisMinedFromPromis::decode_log_data(log)
    });
    outbe_fidelity::enclave_client::test_enclave::uninstall();
    outbe_gratis::enclave_client::test_enclave::uninstall();
    test_enclave::uninstall();

    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].account, alice());
    assert_v2_envelope(&burned[0].encryptedAmountMinor, b"PRO2");
    assert_eq!(minted.len(), 1);
    assert_eq!(minted[0].account, alice());
    assert_v2_envelope(&minted[0].encryptedAmountMinor, b"GRA2");
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

/// Eve's own valid MAC over Alice's ciphertext must fail AEAD, not the balance check.
#[test]
fn mine_gratis_rejects_a_replayed_ciphertext_for_another_account() {
    with_gratis_env(|storage| {
        let amount = U256::from(1_000u64);
        promis_api::mint_from_gem(
            storage.clone(),
            U256::ONE,
            alice(),
            amount,
            auth(PromisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let encrypted = test_enclave::encrypt_mine_gratis_amount(alice(), amount);
        let call = mine_gratis_with_ciphertext(
            encrypted,
            &auth(PromisOp::Burn, eve(), amount, 0),
            &gratis_auth(GratisOp::Mint, eve(), amount, 0),
        );
        let err = dispatch(storage.clone(), &call, eve(), U256::ZERO).unwrap_err();
        assert!(!matches!(
            err,
            outbe_primitives::error::PrecompileError::EnclaveUnavailable(_)
        ));
        let outbe_primitives::error::PrecompileError::RevertBytes(data) = err else {
            panic!("expected a typed Promis rejection, got {err:?}");
        };
        let rejection = outbe_promis::precompile::IPromis::Rejected::abi_decode(&data).unwrap();
        assert_eq!(
            rejection.value,
            "AEAD decryption failed (bad key, nonce, or tampered ciphertext)"
        );
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert!(outbe_gratis::api::balance_ct(storage, eve())
            .unwrap()
            .is_empty());
    });
}

#[test]
fn mine_gratis_rejects_wrong_ciphertext_lengths_without_the_enclave() {
    with_gratis_env(|storage| {
        let placeholder = ModifyAuth {
            mac: [0u8; 32],
            op_nonce: 0,
        };
        for len in [0usize, 91, 93, 70_000] {
            let call = mine_gratis_with_ciphertext(vec![0x11; len], &placeholder, &placeholder);
            let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
            assert!(
                matches!(err, outbe_primitives::error::PrecompileError::Revert(_)),
                "{err}"
            );
            assert!(
                err.to_string()
                    .contains("invalid bytes length: expected 92"),
                "{err}"
            );
        }
    });
}

#[test]
fn mine_gratis_runtime_rejects_a_short_ciphertext_before_decrypt() {
    with_gratis_env(|storage| {
        let amount = U256::from(1u64);
        let err = runtime::mine_gratis(
            storage,
            alice(),
            vec![0u8; 91],
            auth(PromisOp::Burn, alice(), amount, 0),
            gratis_auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap_err();
        assert!(!matches!(
            err,
            outbe_primitives::error::PrecompileError::EnclaveUnavailable(_)
        ));
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_promis::errors::PromisError::Rejected("decrypt failed".into())
            )
            .to_string()
        );
    });
}
