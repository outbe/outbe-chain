use super::main;
use bignum::U256;
use outbe_circuit_core::{hash::hash_3, merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{
    change_key, empty_leaf, merkle_root, note_commitment, note_nullifier, note_sn, PAYNOTE_DOMAIN,
    transfer_commitment,
};

global CHAIN_ID: u64 = 31337;
global KEY: Field = 17;
global AMOUNT: Field = 100;
global LOCK: Field = 555;
global DEADLINE: u64 = 1_700_000_000;

fn asset() -> EthAddress {
    EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48)
}

/// `(KEY, asset(), AMOUNT)` as the sole leaf. Returns `(root, nullifier, path)`.
fn fixture() -> (Field, Field, [Field; DEPTH]) {
    let commitment = note_commitment(CHAIN_ID, note_sn(KEY), asset(), U256::from(AMOUNT));
    let mut path = [empty_leaf(CHAIN_ID); DEPTH];
    for level in 1..DEPTH {
        path[level] = hash_3([PAYNOTE_DOMAIN, path[level - 1], path[level - 1]]);
    }
    (merkle_root(commitment, 0, path), note_nullifier(commitment, KEY), path)
}

fn locked(lock: Field, amount: Field) -> Field {
    transfer_commitment(CHAIN_ID, lock, DEADLINE, asset(), U256::from(amount))
}

fn change(nullifier: Field, sent: Field) -> Field {
    let next_key = change_key(KEY, nullifier);
    note_commitment(
        CHAIN_ID,
        note_sn(next_key),
        asset(),
        U256::from(AMOUNT - sent),
    )
}

/// Transfers `sent` under `lock`, publishing `locked_commitment` and `change_commitment`.
fn run(sent: Field, lock: Field, locked_commitment: Field, change_commitment: Field) {
    let (root, nullifier, path) = fixture();
    main(
        CHAIN_ID,
        root,
        nullifier,
        asset(),
        DEADLINE,
        locked_commitment,
        change_commitment,
        U256::from(AMOUNT),
        KEY,
        0,
        path,
        U256::from(sent),
        lock,
    );
}

#[test]
fn full_transfer_uses_zero_change_sentinel() {
    run(AMOUNT, LOCK, locked(LOCK, AMOUNT), 0);
}

#[test]
fn partial_transfer_keeps_the_change() {
    let (_, nullifier, _) = fixture();
    run(40, LOCK, locked(LOCK, 40), change(nullifier, 40));
}

#[test(should_fail)]
fn transfer_above_the_note_amount() {
    run(AMOUNT + 1, LOCK, locked(LOCK, AMOUNT + 1), 0);
}

#[test(should_fail)]
fn zero_transfer_amount() {
    let (_, nullifier, _) = fixture();
    run(0, LOCK, locked(LOCK, 0), change(nullifier, 0));
}

#[test(should_fail)]
fn zero_lock() {
    run(AMOUNT, 0, locked(0, AMOUNT), 0);
}

#[test(should_fail)]
fn locked_note_for_more_than_was_taken() {
    let (_, nullifier, _) = fixture();
    run(40, LOCK, locked(LOCK, 41), change(nullifier, 40));
}

#[test(should_fail)]
fn partial_transfer_without_change() {
    run(40, LOCK, locked(LOCK, 40), 0);
}

#[test(should_fail)]
fn locked_note_under_another_deadline() {
    let other = transfer_commitment(CHAIN_ID, LOCK, DEADLINE - 1, asset(), U256::from(AMOUNT));
    run(AMOUNT, LOCK, other, 0);
}

#[test(should_fail)]
fn wrong_spend_key() {
    let (root, nullifier, path) = fixture();
    main(
        CHAIN_ID,
        root,
        nullifier,
        asset(),
        DEADLINE,
        locked(LOCK, AMOUNT),
        0,
        U256::from(AMOUNT),
        KEY + 1,
        0,
        path,
        U256::from(AMOUNT),
        LOCK,
    );
}
