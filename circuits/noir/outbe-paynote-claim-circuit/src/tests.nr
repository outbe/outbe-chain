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
global DEADLINE: u64 = 1_700_000_000;
global BOB_OUTPUT_KEY: Field = 23;

fn asset() -> EthAddress {
    EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48)
}

fn locked() -> Field {
    let lock = transfer_lock(note_sn(BOB_KEY), note_sn(ALICE_KEY), RHO);
    transfer_commitment(CHAIN_ID, lock, DEADLINE, asset(), U256::from(AMOUNT))
}

fn output(serial: Field, amount: Field) -> Field {
    note_commitment(CHAIN_ID, serial, asset(), U256::from(amount))
}

/// Resolves the sole-leaf locked note with `auth_key` at `not_before`, into
/// `output_commitment` under `output_serial`.
fn run(auth_key: Field, not_before: u64, output_serial: Field, output_commitment: Field) {
    let mut path = [empty_leaf(CHAIN_ID); DEPTH];
    for level in 1..DEPTH {
        path[level] = hash_3([PAYNOTE_DOMAIN, path[level - 1], path[level - 1]]);
    }
    main(
        CHAIN_ID,
        merkle_root(locked(), 0, path),
        note_nullifier(locked(), RHO),
        asset(),
        not_before,
        output_commitment,
        U256::from(AMOUNT),
        DEADLINE,
        note_sn(BOB_KEY),
        note_sn(ALICE_KEY),
        RHO,
        auth_key,
        output_serial,
        0,
        path,
    );
}

#[test]
fn recipient_claims_before_the_deadline() {
    let serial = note_sn(BOB_OUTPUT_KEY);
    run(BOB_KEY, 0, serial, output(serial, AMOUNT));
}

#[test]
fn recipient_declines_to_the_sender() {
    let serial = note_sn(ALICE_KEY);
    run(BOB_KEY, 0, serial, output(serial, AMOUNT));
}

#[test]
fn recipient_claims_after_the_deadline() {
    let serial = note_sn(BOB_OUTPUT_KEY);
    run(BOB_KEY, DEADLINE + 1, serial, output(serial, AMOUNT));
}

#[test]
fn sender_reclaims_at_the_deadline() {
    let serial = note_sn(ALICE_KEY);
    run(ALICE_KEY, DEADLINE, serial, output(serial, AMOUNT));
}

#[test(should_fail)]
fn sender_reclaims_before_the_deadline() {
    let serial = note_sn(ALICE_KEY);
    run(ALICE_KEY, DEADLINE - 1, serial, output(serial, AMOUNT));
}

#[test(should_fail)]
fn stranger_key() {
    let serial = note_sn(99);
    run(99, DEADLINE, serial, output(serial, AMOUNT));
}

#[test(should_fail)]
fn output_for_more_than_the_locked_amount() {
    let serial = note_sn(BOB_OUTPUT_KEY);
    run(BOB_KEY, 0, serial, output(serial, AMOUNT + 1));
}

#[test(should_fail)]
fn output_in_another_asset() {
    let serial = note_sn(BOB_OUTPUT_KEY);
    let other = note_commitment(CHAIN_ID, serial, EthAddress::from_field(1), U256::from(AMOUNT));
    run(BOB_KEY, 0, serial, other);
}

/// A locked note is not an ordinary note under any of its parts.
#[test]
fn locked_and_ordinary_commitments_differ() {
    assert(locked() != output(note_sn(BOB_KEY), AMOUNT));
    assert(locked() != output(transfer_lock(note_sn(BOB_KEY), note_sn(ALICE_KEY), RHO), AMOUNT));
}
