// Resolves one locked transfer note into one ordinary PayNote.
//
// One proof shape serves three actions:
//   * claim: the recipient moves the value to a serial of their own.
//   * decline: the recipient moves it to the sender's refund serial.
//   * reclaim: the sender moves it anywhere once the deadline has passed.
//
// The recipient may act at any time. The sender may act only when
// `deadline <= not_before`; the pool checks `not_before` against its clock.
// The amount, both parties and the deadline stay in the private witness.
mod tests;

use bignum::{BigNum, U256};
use outbe_circuit_core::{merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{
    merkle_root, note_commitment, note_nullifier, note_sn, transfer_commitment, transfer_lock,
};

fn main(
    chain_id: pub u64,
    root: pub Field,
    nullifier: pub Field,
    asset: pub EthAddress,
    not_before: pub u64,
    output_commitment: pub Field,
    amount: U256,
    deadline: u64,
    recipient_pk: Field,
    sender_serial: Field,
    rho: Field,
    auth_key: Field,
    output_serial: Field,
    leaf_index: u32,
    auth_path: [Field; DEPTH],
) {
    amount.validate_in_field();
    amount.assert_is_not_zero();
    asset.validate();
    assert(nullifier != 0);
    assert(rho != 0);
    assert(auth_key != 0);
    assert(output_serial != 0);

    let lock = transfer_lock(recipient_pk, sender_serial, rho);
    let locked = transfer_commitment(chain_id, lock, deadline, asset, amount);
    // Load-bearing: blocks resolving a zero-padded empty tree slot.
    assert(locked != 0);
    assert_eq(merkle_root(locked, leaf_index, auth_path), root);
    assert_eq(note_nullifier(locked, rho), nullifier);

    let auth = note_sn(auth_key);
    assert(auth != 0);
    let is_recipient = auth == recipient_pk;
    let is_sender_after_deadline = (auth == sender_serial) & (deadline <= not_before);
    assert(is_recipient | is_sender_after_deadline);

    let output = note_commitment(chain_id, output_serial, asset, amount);
    assert(output != 0);
    assert_eq(output_commitment, output);
}
