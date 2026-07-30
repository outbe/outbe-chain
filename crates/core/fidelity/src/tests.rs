//! Fidelity crate tests over the confidential (enclave-backed) path.
//!
//! The deep RCFI/cohort math is pinned in the enclave engine
//! (`outbe-tee-enclave`'s `fidelity` module: LIFO split, golden decay.py
//! replay, blob padding). Here we test the on-chain orchestration: cohort ops
//! persist encrypted blobs, the global anchor is set once, leagues come back
//! from the snapshot, and the signed-auth query path decrypts only for the
//! owner.

use alloy_primitives::{address, Address, U256};
use k256::ecdsa::signature::hazmat::PrehashSigner;
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
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

#[test]
fn cohort_in_encrypts_and_sets_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No state yet: empty blob, unset anchor.
        assert!(c.cohorts_ct_of(ALICE).unwrap().is_empty());
        assert_eq!(c.first_qualified_start().unwrap(), 0);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();

        // Blob is now non-empty ciphertext, and the global anchor is set to the
        // first acquisition time.
        let blob = c.cohorts_ct_of(ALICE).unwrap();
        assert!(!blob.is_empty());
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
        assert!(c.cohorts_ct_of(ALICE).unwrap().is_empty());
        assert_eq!(c.first_qualified_start().unwrap(), 0);
    });
}

#[test]
fn league_reflects_holding_and_sale() {
    with_env(|storage| {
        // Sole holder, no sales → top league at a later time.
        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        let league = api::league_at(storage.clone(), ALICE, T0 + 100 * DAY).unwrap();
        assert_eq!(league, MAX_LEAGUE);

        // Selling most of the position drops efficiency → league falls.
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
        // Bob acquires then sells everything → low efficiency.
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

/// secp256k1 signer + its EVM address.
fn evm_signer(seed: u8) -> (k256::ecdsa::SigningKey, Address) {
    let sk = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
    let point = sk.verifying_key().to_encoded_point(false);
    let addr = Address::from_slice(&alloy_primitives::keccak256(&point.as_bytes()[1..])[12..]);
    (sk, addr)
}

/// EIP-191 owner authorization over the query-auth message for the test chain.
fn query_auth(sk: &k256::ecdsa::SigningKey, account: Address, expiry: u64) -> Vec<u8> {
    let msg = outbe_tee::protocol::fidelity_query_auth_message(
        test_enclave::dev_chain(),
        account,
        expiry,
    );
    let prehash = outbe_tee::protocol::eip191_hash(&msg);
    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        sk.sign_prehash(prehash.as_slice()).unwrap();
    let mut sig65 = [0u8; 65];
    sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
    sig65[64] = recid.to_byte();
    sig65.to_vec()
}

#[test]
fn signed_query_returns_index_for_owner_and_rejects_others() {
    with_env(|storage| {
        let (sk, account) = evm_signer(0x33);
        api::cohort_in(storage.clone(), account, U256::from(1_000u64), T0).unwrap();

        let c = FidelityContract::new(storage.clone());
        let expiry = T0 + 400 * DAY;

        // Owner-signed authorization → the enclave returns a positive RCFI.
        let sig = query_auth(&sk, account, expiry);
        let result = c
            .query_index_at(account, T0 + 100 * DAY, expiry, sig)
            .unwrap();
        assert!(result.rcfi > U256::ZERO);
        assert_eq!(result.league, MAX_LEAGUE);

        // A different key signing for `account` is rejected (wrong signer).
        let (other, _) = evm_signer(0x34);
        let forged = query_auth(&other, account, expiry);
        assert!(c
            .query_index_at(account, T0 + 100 * DAY, expiry, forged)
            .is_err());
    });
}

#[test]
fn max_rcfi_at_uses_plaintext_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No anchor yet → zero ceiling.
        assert_eq!(c.max_rcfi_at(T0).unwrap(), U256::ZERO);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        // After qualification, the ceiling grows with elapsed time (pure on-chain
        // t_dec of the plaintext anchor — no enclave).
        let early = c.max_rcfi_at(T0 + 10 * DAY).unwrap();
        let later = c.max_rcfi_at(T0 + 100 * DAY).unwrap();
        assert!(later > early);
    });
}
