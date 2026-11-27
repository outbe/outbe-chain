use super::main;
use bignum::U256;
use outbe_circuit_core::{hash::hash_3, merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{
    empty_leaf, merkle_root, note_commitment, note_nullifier, note_sn, PAYNOTE_DOMAIN,
    transfer_commitment, transfer_lock,
};

global CHAIN_ID: u64 = 31337;
global BOB_KEY: Field = 21;
global ALICE_KEY: Field = 22;
global RHO: Field = 777;
global AMOUNT: Field = 100;
global SPENT: Field = 40;
global DEADLINE: u64 = 1_700_000_000;
global BOB_CHANGE_KEY: Field = 23;
global CONTEXT: Field = 5;

fn asset() -> EthAddress {
    EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48)
}

fn locked() -> Field {
    let lock = transfer_lock(note_sn(BOB_KEY), note_sn(ALICE_KEY), RHO);
    transfer_commitment(CHAIN_ID, lock, DEADLINE, asset(), U256::from(AMOUNT))
}

/// The change note of `amount` under Bob's change serial.
fn change(amount: Field) -> Field {
    note_commitment(CHAIN_ID, note_sn(BOB_CHANGE_KEY), asset(), U256::from(amount))
}

/// Spends `spend_amount` of the sole-leaf locked note with `auth_key` at
/// `not_before`, leaving `change_commitment`.
fn run(auth_key: Field, not_before: u64, spend_amount: Field, change_commitment: Field) {
    let mut path = [empty_leaf(CHAIN_ID); DEPTH];
    for level in 1..DEPTH {
        path[level] = hash_3([PAYNOTE_DOMAIN, path[level - 1], path[level - 1]]);
    }
    main(
        CHAIN_ID,
        merkle_root(locked(), 0, path),
        note_nullifier(locked(), RHO),
        asset(),
        CONTEXT,
        U256::from(spend_amount),
        change_commitment,
        not_before,
        U256::from(AMOUNT),
        DEADLINE,
        note_sn(BOB_KEY),
        note_sn(ALICE_KEY),
        RHO,
        auth_key,
        note_sn(BOB_CHANGE_KEY),
        0,
        path,
    );
}

#[test]
fn recipient_spends_the_whole_note() {
    run(BOB_KEY, 0, AMOUNT, 0);
}

#[test]
fn recipient_spends_a_part_and_keeps_the_change() {
    run(BOB_KEY, 0, SPENT, change(AMOUNT - SPENT));
}

#[test]
fn sender_spends_at_the_deadline() {
    run(ALICE_KEY, DEADLINE, SPENT, change(AMOUNT - SPENT));
}

#[test(should_fail)]
fn sender_spends_before_the_deadline() {
    run(ALICE_KEY, DEADLINE - 1, SPENT, change(AMOUNT - SPENT));
}

#[test(should_fail)]
fn stranger_key() {
    run(99, DEADLINE, SPENT, change(AMOUNT - SPENT));
}

#[test(should_fail)]
fn spend_of_more_than_the_locked_amount() {
    run(BOB_KEY, 0, AMOUNT + 1, 0);
}

#[test(should_fail)]
fn change_for_more_than_the_remainder() {
    run(BOB_KEY, 0, SPENT, change(AMOUNT - SPENT + 1));
}

#[test(should_fail)]
fn partial_spend_without_change() {
    run(BOB_KEY, 0, SPENT, 0);
}

#[test(should_fail)]
fn full_spend_with_change() {
    run(BOB_KEY, 0, AMOUNT, change(1));
}
