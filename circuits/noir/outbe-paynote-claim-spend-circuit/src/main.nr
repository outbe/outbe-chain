// Resolves one locked transfer note and spends it in the same statement.
//
// The proof opens a locked transfer note as the claim circuit does, and
// releases `spend_amount` of it to the public `context` as the spend circuit
// does. The rest becomes an ordinary change note under a serial the prover
// picks. Thus a recipient withdraws or settles straight from the locked note,
// with no ordinary note in between.
//
// The recipient may act at any time. The sender may act only when
// `deadline <= not_before`; the pool checks `not_before` against its clock.
// The locked amount, both parties and the deadline stay in the private witness.
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
    context: pub Field,
    spend_amount: pub U256,
    change_commitment: pub Field,
    not_before: pub u64,
    amount: U256,
    deadline: u64,
    recipient_pk: Field,
    sender_serial: Field,
    rho: Field,
    auth_key: Field,
    change_serial: Field,
    leaf_index: u32,
    auth_path: [Field; DEPTH],
) {
    // Same amount discipline as the spend circuit: canonical, 1..2^256-1.
    spend_amount.validate_in_field();
    amount.validate_in_field();
    spend_amount.assert_is_not_zero();
    amount.assert_is_not_zero();
    assert(spend_amount <= amount);
    asset.validate();
    assert(nullifier != 0);
    assert(context != 0);
    assert(rho != 0);
    assert(auth_key != 0);

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

    // `<=` above rules out the mod-2^256 subtraction wrap.
    let remaining = amount - spend_amount;
    let creates_change = !remaining.unsafe_is_zero_integer();
    assert((change_commitment != 0) == creates_change);
    if creates_change {
        // A locked note has no spend key to derive a change key from, so the
        // prover names the serial, as the claim circuit's output does.
        assert(change_serial != 0);
        let change = note_commitment(chain_id, change_serial, asset, remaining);
        assert(change != 0);
        assert_eq(change_commitment, change);
    }
}
