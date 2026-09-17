//! Fidelity crate tests over the confidential (enclave-backed) path.
//!
//! The deep RCFI/cohort math is pinned in the enclave engine
//! (`outbe-tee-enclave`'s `fidelity` module: LIFO split, golden decay.py
//! replay, blob padding). Here we test the on-chain orchestration: cohort ops
//! persist encrypted blobs, the global anchor is set once, leagues come back
//! from the snapshot, and the signed-auth query path decrypts only for the
//! owner.

use alloy_primitives::{address, Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::enclave_client::test_enclave;
use crate::schema::FidelityContract;
use crate::{api, MAX_LEAGUE, MIN_LEAGUE};

const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");
const DAY: u64 = 86_400;
const T0: u64 = 1_000_000;

/// Run `f` in a fresh storage scope on the test-enclave chain, with the
/// in-process fidelity enclave installed.
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(test_enclave::DEV_CHAIN_ID);
    storage.set_timestamp(U256::from(T0 + 1000 * DAY));
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

#[test]
fn cohort_in_encrypts_and_sets_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No state yet: empty blob, unset anchor.
        assert_eq!(
            outbe_tee::pledge_ledger::head(&storage).unwrap().sequence,
            0
        );
        assert_eq!(c.first_qualified_start().unwrap(), 0);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();

        // Blob is now non-empty ciphertext, and the global anchor is set to the
        // first acquisition time.
        assert_eq!(
            outbe_tee::pledge_ledger::head(&storage).unwrap().sequence,
            1
        );
        assert_eq!(c.first_qualified_start().unwrap(), T0);

        // A later acquisition by a different owner does NOT move the set-once
        // anchor.
        api::cohort_in(storage.clone(), BOB, U256::from(5u64), T0 + 100 * DAY).unwrap();
        assert_eq!(c.first_qualified_start().unwrap(), T0);
    });
}

#[test]
fn zero_amount_cohort_op_is_a_noop() {
    with_env(|storage| {
        api::cohort_in(storage.clone(), ALICE, U256::ZERO, T0).unwrap();
        let c = FidelityContract::new(storage.clone());
        assert_eq!(
            outbe_tee::pledge_ledger::head(&storage).unwrap().sequence,
            0
        );
        assert_eq!(c.first_qualified_start().unwrap(), 0);
    });
}

#[test]
fn league_reflects_holding_and_sale() {
    with_env(|storage| {
        // Sole holder, no sales -> top league at a later time.
        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        let league = api::league_at(storage.clone(), ALICE, T0 + 100 * DAY).unwrap();
        assert_eq!(league, MAX_LEAGUE);

        // Selling most of the position drops efficiency -> league falls.
        api::cohort_out(storage.clone(), ALICE, U256::from(900u64), T0 + 100 * DAY).unwrap();
        let after = api::league_at(storage.clone(), ALICE, T0 + 200 * DAY).unwrap();
        assert!(after < MAX_LEAGUE);

        // An owner with no cohorts is at the floor.
        assert_eq!(
            api::league_at(storage.clone(), BOB, T0 + 200 * DAY).unwrap(),
            MIN_LEAGUE
        );
    });
}

#[test]
fn snapshot_batches_owner_leagues_in_order() {
    with_env(|storage| {
        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        // Bob acquires then sells everything -> low efficiency.
        api::cohort_in(storage.clone(), BOB, U256::from(1_000u64), T0).unwrap();
        api::cohort_out(storage.clone(), BOB, U256::from(1_000u64), T0 + 10 * DAY).unwrap();

        let leagues =
            api::snapshot_leagues(storage.clone(), T0 + 100 * DAY, &[ALICE, BOB]).unwrap();
        assert_eq!(leagues.len(), 2);
        assert_eq!(leagues[0], (ALICE, MAX_LEAGUE));
        assert_eq!(leagues[1].0, BOB);
        assert!(leagues[1].1 <= MAX_LEAGUE);
    });
}

#[test]
fn max_rcfi_at_uses_plaintext_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No anchor yet -> zero ceiling.
        assert_eq!(c.max_rcfi_at(T0).unwrap(), U256::ZERO);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        // After qualification, the ceiling grows with elapsed time (pure on-chain
        // t_dec of the plaintext anchor - no enclave).
        let early = c.max_rcfi_at(T0 + 10 * DAY).unwrap();
        let later = c.max_rcfi_at(T0 + 100 * DAY).unwrap();
        assert!(later > early);
    });
}

#[test]
fn cohort_ciphertext_is_deterministic_across_executions() {
    // The consensus invariant: two independent executions of the SAME sequence
    // of cohort ops produce BYTE-IDENTICAL ciphertext (and league), so every
    // validator converges on identical encrypted state (deterministic nonce, no
    // randomness).
    let run = || {
        with_env(|storage| {
            api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
            api::cohort_in(storage.clone(), ALICE, U256::from(500u64), T0 + 10 * DAY).unwrap();
            api::cohort_out(storage.clone(), ALICE, U256::from(300u64), T0 + 20 * DAY).unwrap();
            let blob = outbe_tee::pledge_ledger::head(&storage).unwrap().root;
            let league = api::league_at(storage.clone(), ALICE, T0 + 100 * DAY).unwrap();
            (blob, league)
        })
    };
    let a = run();
    let b = run();
    assert!(!a.0.is_zero());
    assert_eq!(
        a.0, b.0,
        "cohort ciphertext must be byte-identical across runs"
    );
    assert_eq!(a.1, b.1, "league must be identical across runs");
}

#[test]
fn encrypted_query_auth_and_metadata() {
    use crate::precompile::{dispatch, IFidelity};
    use alloy_primitives::B256;
    use alloy_sol_types::SolCall;
    use outbe_tee::pledgenote::*;
    with_env(|storage| {
        api::cohort_in(storage.clone(), ALICE, U256::from(1000), T0).unwrap();
        let chain_id = B256::from(U256::from(test_enclave::DEV_CHAIN_ID));
        let action = OwnerAction::QueryAt {
            timestamp: T0 + 100 * DAY,
        };
        let key = outbe_tee_enclave::gratis::derive_modify_key(&test_enclave::state_key(), ALICE)
            .unwrap();
        let mac = owner_mac(&key, chain_id, ALICE, 0, &action).unwrap();
        let request = PrivateRequest::Owner {
            chain_id,
            account: ALICE,
            nonce: 0,
            action,
            mac,
        };
        let public =
            outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET);
        let envelope = encrypt_request(public, &request).unwrap();
        let call = IFidelity::queryCall {
            encryptedRequest: envelope.clone().into(),
        }
        .abi_encode();
        let encoded = dispatch(storage.clone(), &call, BOB, U256::ZERO).unwrap();
        let encrypted = IFidelity::queryCall::abi_decode_returns(&encoded).unwrap();
        let view =
            outbe_tee_enclave::gratis::derive_view_key(&test_enclave::state_key(), ALICE).unwrap();
        let receipt = decrypt_receipt(&view, &encrypted).unwrap();
        assert!(receipt.rcfi > U256::ZERO);
        assert_eq!(receipt.league, MAX_LEAGUE);
        assert!(decrypt_receipt(&[0; 32], &encrypted).is_err());
        let mut tampered = envelope;
        tampered[50] ^= 1;
        let call = IFidelity::queryCall {
            encryptedRequest: tampered.into(),
        }
        .abi_encode();
        assert!(dispatch(storage.clone(), &call, BOB, U256::ZERO).is_err());
        let call = IFidelity::minLeagueCall {}.abi_encode();
        let output = dispatch(storage, &call, BOB, U256::ZERO).unwrap();
        assert_eq!(
            IFidelity::minLeagueCall::abi_decode_returns(&output).unwrap(),
            MIN_LEAGUE
        );
    });
}
