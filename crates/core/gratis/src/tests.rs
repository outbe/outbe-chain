//! The in-process enclave stand-in (`enclave_client::test_enclave`) drives these
//! confidential Gratis tests. It runs the real enclave engine against a fixed dev
//! state key. The tests check balances: they decrypt the ciphertext with the
//! account's view key exactly as a client would.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolInterface};

use crate::engine::{
    decrypt_balance, decrypt_pledged, derive_account_keys, derive_view_key, modify_mac,
    ModifyOperation,
};
use crate::protocol::{GratisOp, ModifyAuth};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

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

/// Build the modify authorization a client would send for `op`.
fn auth(op: GratisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = test_enclave::state_key();
    let mk = derive_account_keys(&sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(
            &mk,
            &ModifyOperation {
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

fn view_balance(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::balance_ct(storage, account).unwrap();
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
        let amount = U256::from(1000u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), amount);
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
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(GratisOp::Mint, alice(), amount, 0);
        api::mint(storage.clone(), alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail. The nonce advanced to 1.
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
fn burn_reduces_encrypted_balance() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(1000u64),
            auth(GratisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();
        api::burn(
            storage.clone(),
            alice(),
            U256::from(400u64),
            auth(GratisOp::Burn, alice(), U256::from(400u64), 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(600u64));
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

// --- Precompile ABI surface (no enclave needed for the non-transferable stubs) ---

fn run_dispatch(call: Bytes, caller: Address) -> outbe_primitives::error::Result<Bytes> {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        dispatch(storage.clone(), &call, caller, U256::ZERO)
    })
}

#[test]
fn metadata_uses_six_decimal_gratis_units() {
    let name = run_dispatch(IGratis::nameCall {}.abi_encode().into(), alice()).unwrap();
    let symbol = run_dispatch(IGratis::symbolCall {}.abi_encode().into(), alice()).unwrap();
    let decimals = run_dispatch(IGratis::decimalsCall {}.abi_encode().into(), alice()).unwrap();
    assert_eq!(
        IGratis::nameCall::abi_decode_returns(&name).unwrap(),
        "gratis"
    );
    assert_eq!(
        IGratis::symbolCall::abi_decode_returns(&symbol).unwrap(),
        "GRATIS"
    );
    assert_eq!(
        IGratis::decimalsCall::abi_decode_returns(&decimals).unwrap(),
        6
    );
}

#[test]
fn precompile_transfer_reverts() {
    let call = Bytes::from(
        IGratis::IGratisCalls::transfer(IGratis::transferCall {
            to: Address::repeat_byte(0x22),
            amount: U256::from(1u64),
        })
        .abi_encode(),
    );
    let err = run_dispatch(call, alice()).unwrap_err();
    assert_eq!(
        err.to_string(),
        outbe_primitives::error::PrecompileError::from(
            crate::errors::GratisError::TransferNotAllowed
        )
        .to_string()
    );
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
    // The returned bytes are the ciphertext blob. Decrypt them with the view key.
    let blob = IGratis::balanceOfCall::abi_decode_returns(&out).unwrap();
    let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    assert_eq!(
        decrypt_balance(&vk, alice(), &blob).unwrap(),
        U256::from(777u64)
    );
    test_enclave::uninstall();
}

fn view_pledged(storage: StorageHandle<'_>, account: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), account).unwrap();
    let blob = api::pledged_ct(storage, account).unwrap();
    decrypt_pledged(&vk, account, &blob).unwrap()
}

#[test]
fn pledge_release_and_burn_move_only_the_pledged_balance() {
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let pledged = U256::from(600u64);
        api::pledge_sealed(
            &storage,
            &test_enclave::sealed_pledge(alice(), pledged, U256::from(1u64), CHAIN_ID),
            U256::from(1u64),
            pledged,
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(400u64));
        assert_eq!(view_pledged(storage.clone(), alice()), pledged);
        assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), pledged);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);

        api::release_pledged(&storage, alice(), U256::from(100u64)).unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(500u64));
        assert_eq!(view_pledged(storage.clone(), alice()), U256::from(500u64));

        api::burn_pledged(&storage, alice(), U256::from(200u64)).unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(500u64));
        assert_eq!(view_pledged(storage.clone(), alice()), U256::from(300u64));
        assert_eq!(
            view_balance(storage.clone(), alice()) + view_pledged(storage.clone(), alice()),
            U256::from(800u64)
        );
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::from(300u64)
        );
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);

        assert!(api::release_pledged(&storage, alice(), U256::from(301u64)).is_err());
        assert!(api::burn_pledged(&storage, alice(), U256::from(301u64)).is_err());
        assert_eq!(view_pledged(storage.clone(), alice()), U256::from(300u64));
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::from(300u64)
        );
    });
}

#[test]
fn pledge_needs_the_liquid_amount_and_an_authorization_for_its_reservation() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let too_much = U256::from(101u64);
        assert!(api::pledge_sealed(
            &storage,
            &test_enclave::sealed_pledge(alice(), too_much, U256::from(1u64), CHAIN_ID),
            U256::from(1u64),
            too_much,
        )
        .is_err());
        let other_reservation =
            test_enclave::sealed_pledge(alice(), amount, U256::from(2u64), CHAIN_ID);
        assert!(api::pledge_sealed(&storage, &other_reservation, U256::ONE, amount).is_err());
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);
    });
}

#[test]
fn precompile_pledged_of_returns_ciphertext() {
    with_env(|storage| {
        let amount = U256::from(50u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        api::pledge_sealed(
            &storage,
            &test_enclave::sealed_pledge(alice(), amount, U256::from(1u64), CHAIN_ID),
            U256::from(1u64),
            amount,
        )
        .unwrap();
        let call = Bytes::from(
            IGratis::IGratisCalls::pledgedOf(IGratis::pledgedOfCall { account: alice() })
                .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let blob = IGratis::pledgedOfCall::abi_decode_returns(&out).unwrap();
        let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
        assert_eq!(decrypt_pledged(&vk, alice(), &blob).unwrap(), amount);
    });
}

#[test]
fn pledge_and_pledged_burn_emit_no_gratis_event() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let amount = U256::from(100u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        api::pledge_sealed(
            &storage,
            &test_enclave::sealed_pledge(alice(), amount, U256::ONE, CHAIN_ID),
            U256::ONE,
            amount,
        )
        .unwrap();
        api::burn_pledged(&storage, alice(), U256::from(40u64)).unwrap();
    });
    test_enclave::uninstall();
    assert!(provider
        .get_events(outbe_system_common::addresses::GRATIS_ADDRESS)
        .is_empty());
}

#[test]
fn an_owner_result_that_differs_from_the_request_is_refused() {
    use crate::protocol::{GratisOpResult, GratisOpStatus};
    use crate::runtime::ensure_owner_result;
    let result = |event_amount: u64, next_op_nonce: u64| GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: vec![1],
        new_pledged: Vec::new(),
        event_amount: U256::from(event_amount),
        encrypted_event_amount: vec![1],
        next_op_nonce,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
        pledge_account: None,
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
    let mut pledged = result(5, 4);
    pledged.new_pledged = vec![1];
    let err = ensure_owner_result(amount, 3, &pledged).unwrap_err();
    assert!(
        err.to_string().contains("owner movement result mismatch"),
        "{err}"
    );
}

fn encrypted_nod(
    amount: U256,
    nod_id_byte: u8,
) -> outbe_core_common::nod_encryption::EncryptedNodV1 {
    use outbe_core_common::nod_encryption::NodTermsV1;
    use outbe_system_common::time::WorldwideDay;
    outbe_nod::nod_encryption::encrypt_nod(
        &outbe_tee::resident::StateKeyHandle::for_test([0x5a; 32]),
        NodTermsV1 {
            chain_id: CHAIN_ID,
            nod_id: U256::from_be_bytes([nod_id_byte; 32]),
            owner: alice(),
            emission_day: WorldwideDay::new(20250115).value(),
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
        amount,
    )
    .unwrap()
}

/// The first NOD mint seals the event amount apart from the stored balance.
/// A later mint's event opens to the operation delta, not the cumulative balance.
#[test]
fn nod_mint_event_amount_differs_from_balance_and_opens_to_the_delta() {
    test_enclave::install();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let first_amount = U256::from(1_000u64);
    let second_amount = U256::from(250u64);
    let (first_balance, second_balance) = StorageHandle::enter(&mut provider, |storage| {
        api::mint_encrypted_nod(
            storage.clone(),
            &encrypted_nod(first_amount, 7),
            auth(GratisOp::Mint, alice(), first_amount, 0),
        )
        .unwrap();
        let first_balance = api::balance_ct(storage.clone(), alice()).unwrap();
        api::mint_encrypted_nod(
            storage.clone(),
            &encrypted_nod(second_amount, 8),
            auth(GratisOp::Mint, alice(), second_amount, 1),
        )
        .unwrap();
        let second_balance = api::balance_ct(storage, alice()).unwrap();
        (first_balance, second_balance)
    });
    let mined: Vec<_> = provider
        .get_events(outbe_system_common::addresses::GRATIS_ADDRESS)
        .iter()
        .filter_map(|log| IGratis::GratisMinedFromNod::decode_log_data(log).ok())
        .collect();
    let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    test_enclave::uninstall();

    assert_eq!(mined.len(), 2);
    assert_eq!(mined[0].account, alice());
    assert_ne!(
        mined[0].encryptedAmountMinor.as_ref(),
        first_balance.as_slice()
    );
    assert_eq!(
        decrypt_balance(&vk, alice(), &mined[0].encryptedAmountMinor).unwrap(),
        first_amount
    );
    assert_eq!(
        decrypt_balance(&vk, alice(), &first_balance).unwrap(),
        first_amount
    );

    let delta = decrypt_balance(&vk, alice(), &mined[1].encryptedAmountMinor).unwrap();
    assert_eq!(delta, second_amount);
    assert_ne!(delta, first_amount + second_amount);
    assert_eq!(
        decrypt_balance(&vk, alice(), &second_balance).unwrap(),
        first_amount + second_amount
    );
}

#[test]
fn pledge_authorization_binds_one_reservation() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let sealed = test_enclave::sealed_pledge(alice(), amount, U256::ONE, CHAIN_ID);
        let rejected = |bytes: &[u8], reservation: U256, quantity: U256| {
            api::pledge_sealed(&storage, bytes, reservation, quantity).unwrap_err()
        };
        let reason = |sentence: &str| {
            outbe_primitives::error::PrecompileError::from(crate::errors::GratisError::Rejected(
                sentence.into(),
            ))
            .to_string()
        };
        let invalid = reason("invalid pledge authorization");
        let malformed = reason("malformed sealed pledge");
        assert_eq!(
            rejected(&sealed, U256::from(2u64), amount).to_string(),
            invalid
        );
        assert_eq!(
            rejected(&sealed, U256::ONE, amount - U256::ONE).to_string(),
            invalid
        );
        // A cash account cannot name the main account with a MAC of another key.
        let forged = test_enclave::sealed_pledge_by(
            address!("0x00000000000000000000000000000000000000b0"),
            alice(),
            amount,
            U256::ONE,
            CHAIN_ID,
        );
        assert_eq!(rejected(&forged, U256::ONE, amount).to_string(), invalid);
        let other_chain = test_enclave::sealed_pledge(alice(), amount, U256::ONE, CHAIN_ID + 1);
        assert_eq!(
            rejected(&other_chain, U256::ONE, amount).to_string(),
            invalid
        );
        // A Nod offer sealed to the same key does not open as a pledge.
        let (cipher, nonce, ephemeral) = outbe_core_common::confidential_tee::encrypt_offer(
            &outbe_core_common::confidential_tee::offer_public_key(),
            &[0x11; crate::engine::PLEDGE_PLAINTEXT_LEN],
        )
        .unwrap();
        let offer = [ephemeral.as_slice(), nonce.as_slice(), cipher.as_slice()].concat();
        assert_eq!(rejected(&offer, U256::ONE, amount).to_string(), malformed);
        assert_eq!(
            rejected(&sealed[..sealed.len() - 1], U256::ONE, amount).to_string(),
            malformed
        );
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);

        // Another Gratis operation of the main account between the MAC and the issue.
        api::burn(
            storage.clone(),
            alice(),
            U256::ONE,
            auth(GratisOp::Burn, alice(), U256::ONE, 1),
        )
        .unwrap();
        let pledged = amount - U256::ONE;
        let sealed = test_enclave::sealed_pledge(alice(), pledged, U256::ONE, CHAIN_ID);
        assert_eq!(
            api::pledge_sealed(&storage, &sealed, U256::ONE, pledged).unwrap(),
            alice()
        );
        assert_eq!(view_pledged(storage.clone(), alice()), pledged);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 2);
    });
}
