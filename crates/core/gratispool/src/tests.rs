//! Unit tests for the shielded gratis pool.
//!
//! These tests exercise the schema / state / runtime control flow using the
//! `verifier::with_verifier_outcome` override (see `verifier.rs`). Real
//! UltraHonk proofs are not generated here — that lives in the e2e test
//! alongside the Noir circuit once it is compiled. The override
//! deliberately decouples runtime tests from the prover so the runtime
//! gates (root window, nullifier set, receiver binding, denomination range)
//! are tested independently of circuit churn.
//!
//! The pool is purely cryptographic — Gratis-balance assertions live in the
//! gratisfactory tests (which orchestrate the Gratis-side bookkeeping).

use alloy_primitives::{address, Address, U256};

use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api;
use crate::constants::{denomination, ACTION_REQUEST_CREDIS, ACTION_UNPLEDGE, TAG_MERKLE_GRATIS};
use crate::runtime::SpendArgs;
use crate::schema::GratisPoolContract;
use crate::state::{commitment_hash, merkle_node, nullifier_hash, receiver_binding};
use crate::verifier::{build_combined, with_verifier_outcome, NUM_PUBLIC_INPUTS};

const CHAIN_ID: u64 = 1;

fn bob() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}

fn carol() -> Address {
    address!("0x3333333333333333333333333333333333333333")
}

fn make_spend_args(
    storage: StorageHandle<'_>,
    nullifier_secret: U256,
    denom_id: u8,
    action_tag: u64,
    target: Address,
    nonce: U256,
) -> SpendArgs {
    let pool = GratisPoolContract::new(storage);
    let root = pool.current_root(denom_id).unwrap();
    SpendArgs {
        merkle_root: root,
        nullifier_hash: nullifier_hash(nullifier_secret).unwrap(),
        denom_id,
        receiver_binding: receiver_binding(action_tag, target, CHAIN_ID, nonce).unwrap(),
        proof: vec![0x00; 32], // dummy — verifier outcome forced via with_verifier_outcome
    }
}

// ---------------------------------------------------------------------------
// Deposit
// ---------------------------------------------------------------------------

#[test]
fn add_commitment_appends_leaf_and_returns_amount() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let expected_amount = denomination(denom_id).unwrap();

        let secret = U256::from(0xCAFE_u64);
        let null_s = U256::from(0xBEEF_u64);
        let commitment = commitment_hash(secret, null_s, denom_id).unwrap();

        let (root, idx, amount) =
            api::add_commitment(storage.clone(), denom_id, commitment).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(amount, expected_amount);
        assert_ne!(root, U256::ZERO);

        let pool = GratisPoolContract::new(storage);
        assert_eq!(pool.leaf_count(denom_id).unwrap(), 1);
        assert_eq!(pool.current_root(denom_id).unwrap(), root);
    });
}

#[test]
fn add_commitment_unknown_denom_reverts() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let err = api::add_commitment(storage, 99, U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("denomination id out of range"));
    });
}

#[test]
fn add_commitment_duplicate_reverts() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let c = commitment_hash(U256::from(1u64), U256::from(2u64), 1).unwrap();
        api::add_commitment(storage.clone(), 1, c).unwrap();
        let err = api::add_commitment(storage, 1, c).unwrap_err();
        assert!(err.to_string().contains("commitment already exists"));
    });
}

// ---------------------------------------------------------------------------
// Spend
// ---------------------------------------------------------------------------

#[test]
fn verify_and_spend_for_credis_consumes_nullifier_returns_amount() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let expected_amount = denomination(denom_id).unwrap();

        let secret = U256::from(0x11_u64);
        let null_s = U256::from(0x22_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        // Per-test nonce stands in for the reclaim_commitment that
        // credisfactory passes through in the real flow.
        let nonce = U256::from(0xDEAD_BEEF_u64);
        let args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_REQUEST_CREDIS,
            bob(),
            nonce,
        );
        let amt = with_verifier_outcome(true, || {
            api::verify_and_spend_for_credis(storage.clone(), bob(), nonce, &args).unwrap()
        });
        assert_eq!(amt, expected_amount);

        let pool = GratisPoolContract::new(storage);
        assert!(pool.nullifier_spent.contains(&args.nullifier_hash).unwrap());
    });
}

#[test]
fn verify_and_spend_for_unpledge_consumes_nullifier_returns_amount() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let expected_amount = denomination(denom_id).unwrap();

        let secret = U256::from(0x33_u64);
        let null_s = U256::from(0x44_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        let args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_UNPLEDGE,
            carol(),
            U256::ZERO,
        );
        let amt = with_verifier_outcome(true, || {
            api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap()
        });
        assert_eq!(amt, expected_amount);

        let pool = GratisPoolContract::new(storage);
        assert!(pool.nullifier_spent.contains(&args.nullifier_hash).unwrap());
    });
}

#[test]
fn double_spend_rejected() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0x55_u64);
        let null_s = U256::from(0x66_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        let args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_UNPLEDGE,
            carol(),
            U256::ZERO,
        );
        with_verifier_outcome(true, || {
            api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap();
            let err =
                api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap_err();
            assert!(err.to_string().contains("nullifier"));
        });
    });
}

#[test]
fn receiver_binding_mismatch_rejected() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0x77_u64);
        let null_s = U256::from(0x88_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        // Args built for unpledge → Carol, but submitted as credis → Bob.
        let args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_UNPLEDGE,
            carol(),
            U256::ZERO,
        );
        with_verifier_outcome(true, || {
            let err = api::verify_and_spend_for_credis(storage.clone(), bob(), U256::ZERO, &args)
                .unwrap_err();
            assert!(err.to_string().contains("receiver binding"));
        });
    });
}

#[test]
fn stale_root_rejected() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0x99_u64);
        let null_s = U256::from(0xAA_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        let mut args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_UNPLEDGE,
            carol(),
            U256::ZERO,
        );
        args.merkle_root = U256::from(0xDEADBEEF_u64);
        with_verifier_outcome(true, || {
            let err =
                api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap_err();
            assert!(err.to_string().contains("recent-roots"));
        });
    });
}

#[test]
fn receiver_binding_changes_when_nonce_changes() {
    // The nonce slot is what binds the proof to the application's
    // context-binding payload (e.g. `reclaim_commitment` for
    // requestCredis). If the same `(action_tag, target, chain_id)` tuple
    // produced the same binding for any nonce, a mempool front-runner
    // could swap the reclaim commitment without disturbing the
    // receiver_binding check.
    let target = bob();
    let zero = receiver_binding(ACTION_REQUEST_CREDIS, target, CHAIN_ID, U256::ZERO).unwrap();
    let seven =
        receiver_binding(ACTION_REQUEST_CREDIS, target, CHAIN_ID, U256::from(7u64)).unwrap();
    assert_ne!(
        zero, seven,
        "receiver_binding must vary with the nonce slot; \
         otherwise the reclaim-swap attack stays open"
    );

    // And nonce changes must propagate independently of action / target.
    let big = U256::from_be_bytes([0xAB; 32]);
    let big_binding = receiver_binding(ACTION_REQUEST_CREDIS, target, CHAIN_ID, big).unwrap();
    assert_ne!(zero, big_binding);
    assert_ne!(seven, big_binding);
}

#[test]
fn proof_invalid_rejected() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0xBB_u64);
        let null_s = U256::from(0xCC_u64);
        let c = commitment_hash(secret, null_s, denom_id).unwrap();
        api::add_commitment(storage.clone(), denom_id, c).unwrap();

        let args = make_spend_args(
            storage.clone(),
            null_s,
            denom_id,
            ACTION_UNPLEDGE,
            carol(),
            U256::ZERO,
        );
        with_verifier_outcome(false, || {
            let err =
                api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap_err();
            assert!(err.to_string().contains("zk proof"));
        });
    });
}

// ---------------------------------------------------------------------------
// Circuit / runtime parity
// ---------------------------------------------------------------------------
//
// These tests lock in the byte-level recipes the on-chain hashes follow so
// any change here that diverges from the Noir circuit fails fast at unit-
// test time, rather than only being caught when a real proof is produced and
// rejected by the Barretenberg verifier (which is expensive and only run
// behind `#[ignore]`).

#[test]
fn merkle_node_uses_arity_2_with_tag_added_into_left_input() {
    // Circuit recipe (upstream `outbe-commitment-nullifier-circuit::merkle_node`):
    //     hash_2([TAG_MERKLE_GRATIS + left, right])
    // The on-chain `merkle_node` must produce the same bytes. Verify by
    // re-constructing the expected output through a direct call to
    // `outbe_poseidon::Poseidon::<Fr>::new_circom(2)` and comparing.
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};
    use outbe_poseidon::{Poseidon, PoseidonHasher};

    let left = U256::from(0x1234_5678_u64);
    let right = U256::from(0xDEAD_BEEF_u64);

    let tag = Fr::from(TAG_MERKLE_GRATIS);
    let left_fr = Fr::from_be_bytes_mod_order(&left.to_be_bytes::<32>());
    let right_fr = Fr::from_be_bytes_mod_order(&right.to_be_bytes::<32>());

    let mut hasher = Poseidon::<Fr>::new_circom(2).unwrap();
    let expected_fr = hasher.hash(&[tag + left_fr, right_fr]).unwrap();
    let be = expected_fr.into_bigint().to_bytes_be();
    let mut buf = [0u8; 32];
    let off = 32 - be.len().min(32);
    buf[off..].copy_from_slice(&be[be.len().saturating_sub(32)..]);
    let expected = U256::from_be_bytes(buf);

    let actual = merkle_node(left, right).unwrap();
    assert_eq!(
        actual, expected,
        "merkle_node must follow the circuit recipe \
         poseidon_2(TAG_MERKLE_GRATIS + left, right); \
         any other arity or tag placement breaks proof / runtime parity"
    );
}

#[test]
fn build_combined_lays_out_count_inputs_then_body() {
    // The combined proof shape `verify_ultra_honk_keccak` expects is
    // `[u32-BE num_public_inputs | pub_in_0:32B | … | pub_in_{N-1}:32B |
    // proof_body]`. Mirror the parse from `outbe_zk_circuit_noir::split_proof`
    // to assert the build round-trips byte-for-byte.
    let public_inputs: [U256; NUM_PUBLIC_INPUTS] = [
        U256::from(0xAA_u64),
        U256::from(0xBB_u64),
        U256::from(0x02_u64), // denom_id-shaped small int
        U256::from(0xCC_u64),
    ];
    let proof_body: Vec<u8> = (0u8..=255).collect();

    let combined = build_combined(&public_inputs, &proof_body);

    // Count prefix.
    assert!(combined.len() >= 4);
    let n = u32::from_be_bytes(combined[0..4].try_into().unwrap()) as usize;
    assert_eq!(n, NUM_PUBLIC_INPUTS);

    // Public inputs, in declaration order.
    let header_end = 4 + n * 32;
    assert!(combined.len() >= header_end);
    for (i, expected) in public_inputs.iter().enumerate() {
        let slot = &combined[4 + i * 32..4 + (i + 1) * 32];
        let actual: [u8; 32] = slot.try_into().unwrap();
        assert_eq!(
            actual,
            expected.to_be_bytes::<32>(),
            "public input {i} mismatch in combined-proof layout"
        );
    }

    // Body.
    assert_eq!(&combined[header_end..], proof_body.as_slice());
}
