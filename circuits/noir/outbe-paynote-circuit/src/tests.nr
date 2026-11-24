use super::main;
use bignum::{BigNum, U256};
use outbe_circuit_core::{hash, merkle_tree::{DEPTH, merkle_root_from_indices}};
use outbe_circuit_core::types::EthAddress;
use paynote_lib::{
    change_key, empty_leaf, merkle_root, note_commitment, note_nullifier, note_sn, PAYNOTE_DOMAIN,
};

global CHAIN_ID: u64 = 31337;
global KEY: Field = 17;
global AMOUNT: Field = 100;

// `EthAddress::from_field` range-checks, so these are functions rather than
// globals -- a global initialiser has to be comptime-evaluable.
fn asset() -> EthAddress {
    EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48)
}

fn other_asset() -> EthAddress {
    EthAddress::from_field(0x2260fac5e5542a773aa44fbcfedf7c193bc2c599)
}

global CONTEXT: Field = 1;

fn single_leaf_path(chain_id: u64) -> [Field; DEPTH] {
    let mut path = [0; DEPTH];
    path[0] = empty_leaf(chain_id);
    for level in 1..DEPTH {
        path[level] = hash::hash_3([PAYNOTE_DOMAIN, path[level - 1], path[level - 1]]);
    }
    path
}

/// The canonical fixture leaf: `(KEY, asset(), AMOUNT)` as the sole leaf of an
/// otherwise empty depth-32 tree. Returns `(root, nullifier, path)`.
fn fixture() -> (Field, Field, [Field; DEPTH]) {
    let serial = note_sn(KEY);
    let commitment = note_commitment(CHAIN_ID, serial, asset(), U256::from(AMOUNT));
    let path = single_leaf_path(CHAIN_ID);
    let root = merkle_root(commitment, 0, path);
    (root, note_nullifier(commitment, KEY), path)
}

/// The change commitment a correct partial spend of `spend` must publish.
fn expected_change(n: Field, change_asset: EthAddress, spend: Field) -> Field {
    let next_key = change_key(KEY, n);
    note_commitment(
        CHAIN_ID,
        note_sn(next_key),
        change_asset,
        U256::from(AMOUNT - spend),
    )
}

// ---------------------------------------------------------------- happy paths

#[test]
fn full_spend_uses_zero_change_sentinel() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test]
fn partial_spend_rotates_change_key_and_inherits_asset() {
    let (root, n, path) = fixture();
    let change = expected_change(n, asset(), 40);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(40),
        change,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

// --------------------------------------------------- new-behaviour negatives

#[test(should_fail)]
fn wrong_asset_fails_membership() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        other_asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn change_note_cannot_switch_asset() {
    let (root, n, path) = fixture();
    let change = expected_change(n, other_asset(), 40);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(40),
        change,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn zero_asset_fails() {
    let (root, n, path) = fixture();
    let zero = EthAddress::from_field(0);
    main(
        CHAIN_ID,
        root,
        n,
        zero,
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn zero_context_fails() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        0,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

/// The published nullifier and change commitment are functions of the note,
/// not of `context`, so two settlements of one note share one nullifier.
#[test]
fn context_does_not_change_nullifier_or_change_commitment() {
    let (root, n, path) = fixture();
    let change = expected_change(n, asset(), 40);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        1,
        U256::from(40),
        change,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        2,
        U256::from(40),
        change,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn zero_spend_amount_fails() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(0),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn overspend_fails() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT + 1),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

/// The tree tag must actually reach `merkle_root_from_indices`. A root built under a
/// different domain must not verify; delete `PAYNOTE_DOMAIN` from the wrapper
/// and this goes green.
#[test(should_fail)]
fn wrong_tree_domain_fails_membership() {
    let (_, n, path) = fixture();
    let serial = note_sn(KEY);
    let commitment = note_commitment(CHAIN_ID, serial, asset(), U256::from(AMOUNT));
    let root = merkle_root_from_indices(PAYNOTE_DOMAIN + 1, commitment, path, [true; DEPTH]);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

// ------------------------------------------------------ inherited negatives

#[test(should_fail)]
fn zero_key_fails() {
    let path = single_leaf_path(CHAIN_ID);
    main(
        CHAIN_ID,
        1,
        1,
        asset(),
        CONTEXT,
        U256::from(1),
        0,
        U256::from(1),
        0,
        0,
        path,
    );
}

#[test(should_fail)]
fn wrong_private_key_fails_membership() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        18,
        0,
        path,
    );
}

#[test(should_fail)]
fn wrong_amount_fails_membership() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(99),
        0,
        U256::from(99),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn wrong_path_fails_membership() {
    let (root, n, path) = fixture();
    let mut wrong_path = path;
    wrong_path[0] = 0;
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        wrong_path,
    );
}

#[test(should_fail)]
fn wrong_leaf_index_fails_membership() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        1,
        path,
    );
}

#[test(should_fail)]
fn wrong_nullifier_fails() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n + 1,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn wrong_change_amount_fails() {
    let (root, n, path) = fixture();
    // Change built for a 40-spend, submitted with a 41-spend.
    let change = expected_change(n, asset(), 40);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(41),
        change,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn full_spend_rejects_nonzero_change_sentinel() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        1,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn partial_spend_rejects_zero_change_commitment() {
    let (root, n, path) = fixture();
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(40),
        0,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
    );
}

// `leaf_index_above_tree_capacity_fails` lived here. At DEPTH = 32 the tree holds
// 2^32 leaves and `main` takes `leaf_index: u32`, so no over-capacity index is
// constructible -- the ABI type is the bound. The generic check that a too-wide
// index cannot resolve a root lives in `outbe_circuit_core::merkle_tree`.

#[test]
fn max_leaf_index_boundary_proves() {
    let (_, n, path) = fixture();
    let serial = note_sn(KEY);
    let commitment = note_commitment(CHAIN_ID, serial, asset(), U256::from(AMOUNT));
    let last = 0xffffffff;
    let root = merkle_root(commitment, last, path);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY,
        last,
        path,
    );
}

/// Amounts are `U256`; the top of the range must survive the limb-hashed
/// commitment and `note_amount - spend_amount` change arithmetic.
#[test]
fn u256_max_amounts_full_and_partial_spend() {
    let max = U256::from_limbs([(1 << 120) - 1, (1 << 120) - 1, (1 << 16) - 1]);
    let serial = note_sn(KEY);
    let commitment = note_commitment(CHAIN_ID, serial, asset(), max);
    let path = single_leaf_path(CHAIN_ID);
    let root = merkle_root(commitment, 0, path);
    let n = note_nullifier(commitment, KEY);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        max,
        0,
        max,
        KEY,
        0,
        path,
    );

    let next_key = change_key(KEY, n);
    let change = note_commitment(CHAIN_ID, note_sn(next_key), asset(), max - U256::from(1));
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        U256::from(1),
        change,
        max,
        KEY,
        0,
        path,
    );
}

#[test]
fn amounts_above_u128_full_and_partial_spend() {
    let note = U256::from_limbs([100, 1 << 80, 0]); // 2^200 + 100
    let spend = U256::from_limbs([0, 1 << 79, 0]); // 2^199
    let remaining = U256::from_limbs([100, 1 << 79, 0]); // 2^199 + 100
    let serial = note_sn(KEY);
    let commitment = note_commitment(CHAIN_ID, serial, asset(), note);
    let path = single_leaf_path(CHAIN_ID);
    let root = merkle_root(commitment, 0, path);
    let n = note_nullifier(commitment, KEY);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        note,
        0,
        note,
        KEY,
        0,
        path,
    );

    let next_key = change_key(KEY, n);
    let change = note_commitment(CHAIN_ID, note_sn(next_key), asset(), remaining);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        spend,
        change,
        note,
        KEY,
        0,
        path,
    );
}

/// Limb hashing must distinguish values that folding into one `Field` aliases.
#[test]
fn field_modulus_offset_amounts_have_distinct_commitments() {
    let modulus_offset = U256::from_limbs([
        269518400912893473352610297457672193,
        407329191355489184324853464957476136,
        12388,
    ]);
    assert(
        note_commitment(CHAIN_ID, KEY, asset(), U256::zero())
            != note_commitment(CHAIN_ID, KEY, asset(), modulus_offset),
    );
}

#[test(should_fail)]
fn non_canonical_amount_limb_fails() {
    let bad = U256::from_limbs([1 << 127, 0, 0]);
    let commitment = note_commitment(CHAIN_ID, note_sn(KEY), asset(), bad);
    let path = single_leaf_path(CHAIN_ID);
    let root = merkle_root(commitment, 0, path);
    let n = note_nullifier(commitment, KEY);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        bad,
        0,
        bad,
        KEY,
        0,
        path,
    );
}

#[test(should_fail)]
fn modulus_zero_encoding_fails() {
    let modulus = U256::from_limbs([0, 0, 1 << 16]);
    let commitment = note_commitment(CHAIN_ID, note_sn(KEY), asset(), modulus);
    let path = single_leaf_path(CHAIN_ID);
    let root = merkle_root(commitment, 0, path);
    let n = note_nullifier(commitment, KEY);
    main(
        CHAIN_ID,
        root,
        n,
        asset(),
        CONTEXT,
        modulus,
        0,
        modulus,
        KEY,
        0,
        path,
    );
}

/// `EthAddress::from_field` is what bounds a constructed address at 160 bits --
/// check it actually bites. The ABI-supplied path is different: `asset` arrives
/// as a struct, so no constructor runs on it and `validate()` in `main` is the
/// only guard. That one cannot be reached from Noir (`inner` is private), so it
/// is covered by `oversized_asset_is_rejected` in `outbe-zk-backend/tests/paynote.rs`.
#[test(should_fail)]
fn address_above_160_bits_fails() {
    let _ = EthAddress::from_field(0x010000000000000000000000000000000000000000);
}
