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

use outbe_primitives::units::ONE_COEN;

use crate::api;
use crate::constants::{
    DenomAmount, ACTION_REQUEST_CREDIS, ACTION_UNPLEDGE, DENOMINATION_COUNT, TAG_MERKLE_GRATIS,
};
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
        let expected_amount = DenomAmount::from_id(denom_id).unwrap().amount();

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

#[test]
fn add_commitment_rejects_non_canonical_commitment() {
    // A commitment `>= p` (BN254 scalar field modulus) is non-canonical and
    // would panic merkle_node's `u256_to_fr(..).unwrap()` on every validator.
    // The deposit gate in `append_leaf` must reject it with a typed error
    // instead of panicking. Mirrors `non_canonical_nullifier_rejected` on the
    // spend path.
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;

        // p = BN254 scalar field modulus, as a U256. `p` itself is the
        // smallest non-canonical value.
        let p_be = <Fr as PrimeField>::MODULUS.to_bytes_be();
        let mut buf = [0u8; 32];
        buf[32 - p_be.len()..].copy_from_slice(&p_be);
        let p = U256::from_be_bytes(buf);

        let err = api::add_commitment(storage, denom_id, p).unwrap_err();
        assert!(
            err.to_string().contains("canonical"),
            "non-canonical commitment must be rejected, got: {err}"
        );
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
        let expected_amount = DenomAmount::from_id(denom_id).unwrap().amount();

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
        let expected_amount = DenomAmount::from_id(denom_id).unwrap().amount();

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
}

#[test]
fn proof_invalid_does_not_consume_nullifier() {
    // Checks-before-effects: a failed proof must not have marked the nullifier
    // spent, so the note remains spendable once a valid proof is produced.
    // (In production the precompile error reverts the frame too; this asserts
    // the runtime ordering is correct independent of revert semantics.)
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0xD1_u64);
        let null_s = U256::from(0xD2_u64);
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
            api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap_err();
        });

        let pool = GratisPoolContract::new(storage);
        assert!(
            !pool.nullifier_spent.contains(&args.nullifier_hash).unwrap(),
            "a rejected proof must not consume the nullifier"
        );
    });
}

#[test]
fn non_canonical_nullifier_rejected() {
    // A nullifier `N` and its non-canonical alias `N + p` reduce to the same
    // field element inside the verifier, but are distinct `U256` keys in the
    // nullifier set. Without the canonical-form gate a note could be spent
    // once per representative; assert the alias is rejected outright.
    use ark_bn254::Fr;
    use ark_ff::{BigInteger, PrimeField};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let secret = U256::from(0xE1_u64);
        let null_s = U256::from(0xE2_u64);
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

        // p = BN254 scalar field modulus, as a U256.
        let p_be = <Fr as PrimeField>::MODULUS.to_bytes_be();
        let mut buf = [0u8; 32];
        buf[32 - p_be.len()..].copy_from_slice(&p_be);
        let p = U256::from_be_bytes(buf);

        // N + p is a non-canonical representative of the same field element.
        args.nullifier_hash += p;

        with_verifier_outcome(true, || {
            let err =
                api::verify_and_spend_for_unpledge(storage.clone(), carol(), &args).unwrap_err();
            assert!(
                err.to_string().contains("canonical"),
                "non-canonical nullifier must be rejected, got: {err}"
            );
        });
    });
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
    // The combined proof shape `Barretenberg::verify_combined` expects is
    // `[u32-BE num_public_inputs | pub_in_0:32B | … | pub_in_{N-1}:32B |
    // proof_body]`. Mirror the parse `verify_combined` performs to assert the
    // build round-trips byte-for-byte.
    let public_inputs: [U256; NUM_PUBLIC_INPUTS] = [
        U256::from(0xAA_u64),
        U256::from(0xBB_u64),
        U256::from(0x02_u64), // denom_id-shaped small int
        U256::from(0xCC_u64),
        U256::from(0x11_u64), // tag_commit slot
        U256::from(0x22_u64), // tag_nullifier slot
        U256::from(0x33_u64), // tag_merkle slot
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

// ---------------------------------------------------------------------------
// Denomination ladder (DenomAmount)
// ---------------------------------------------------------------------------

#[test]
fn denom_amount_id_roundtrips_and_rejects_out_of_range() {
    for d in DenomAmount::ALL {
        assert_eq!(DenomAmount::from_id(d.id()), Some(d));
    }
    // Id 0 is intentionally invalid; ids past the ladder are unknown.
    assert_eq!(DenomAmount::from_id(0), None);
    assert_eq!(DenomAmount::from_id(DENOMINATION_COUNT + 1), None);
}

#[test]
fn denom_amount_values_follow_power_of_ten_ladder() {
    use crate::constants::DenomAmount::*;
    assert_eq!(Gratis1.amount(), U256::from(1u64) * ONE_COEN);
    assert_eq!(Gratis10.amount(), U256::from(10u64) * ONE_COEN);
    assert_eq!(Gratis100.amount(), U256::from(100u64) * ONE_COEN);
    assert_eq!(Gratis1k.amount(), U256::from(1_000u64) * ONE_COEN);
    assert_eq!(Gratis10k.amount(), U256::from(10_000u64) * ONE_COEN);
}

#[test]
fn anadosis_denomination_is_exact_one_tenth() {
    for d in DenomAmount::ALL {
        let anadosis = d.anadosis_denomination();
        // Exactly one tenth, with no truncation: ten installments reconstitute
        // the full deposit amount (credisfactory's NUMBER_OF_ANADOSIS = 10).
        assert_eq!(anadosis, d.amount() / U256::from(10u64));
        assert_eq!(anadosis * U256::from(10u64), d.amount());
    }
    // Smallest denomination: 1 GRATIS / 10 == 0.1 GRATIS == 10^17 base units.
    assert_eq!(
        DenomAmount::Gratis1.anadosis_denomination(),
        ONE_COEN / U256::from(10u64),
    );
}

#[test]
fn dispatch_supported_denoms_returns_full_ladder() {
    use alloy_sol_types::SolCall;

    use crate::precompile::{dispatch, IGratisPool};

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = IGratisPool::supportedDenomsCall {}.abi_encode();
        let out = dispatch(storage, &call, bob(), U256::ZERO).unwrap();
        let ret = IGratisPool::supportedDenomsCall::abi_decode_returns(&out).unwrap();

        let want_ids: Vec<u8> = DenomAmount::ALL.iter().map(|d| d.id()).collect();
        let want_amounts: Vec<U256> = DenomAmount::ALL.iter().map(|d| d.amount()).collect();
        assert_eq!(ret.ids, want_ids);
        assert_eq!(ret.amounts, want_amounts);
    });
}
