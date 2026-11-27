// Locks part of one ordinary PayNote to a recipient.
//
// The proof spends an ordinary note and creates a locked transfer note of
// `transfer_amount`, plus the change note of a partial transfer. Amounts and
// the lock never leave the private witness. The pool checks `deadline` against
// its clock.
//
// The circuit does not open `lock`. A sender who commits to a lock nobody can
// open loses only their own note.
mod tests;

use bignum::{BigNum, U256};
use outbe_circuit_core::{merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{
    change_key, merkle_root, note_commitment, note_nullifier, note_sn, transfer_commitment,
};

fn main(
    chain_id: pub u64,
    root: pub Field,
    nullifier: pub Field,
    asset: pub EthAddress,
    deadline: pub u64,
    locked_commitment: pub Field,
    change_commitment: pub Field,
    note_amount: U256,
    note_spend_key: Field,
    leaf_index: u32,
    auth_path: [Field; DEPTH],
    transfer_amount: U256,
    lock: Field,
) {
    // Same amount discipline as the spend circuit: canonical, 1..2^256-1.
    transfer_amount.validate_in_field();
    note_amount.validate_in_field();
    transfer_amount.assert_is_not_zero();
    note_amount.assert_is_not_zero();
    assert(transfer_amount <= note_amount);
    assert(nullifier != 0);
    assert(note_spend_key != 0);
    assert(lock != 0);
    asset.validate();

    let serial = note_sn(note_spend_key);
    let commitment = note_commitment(chain_id, serial, asset, note_amount);
    assert(serial != 0);
    // Load-bearing: blocks spending a zero-padded empty tree slot.
    assert(commitment != 0);
    assert_eq(merkle_root(commitment, leaf_index, auth_path), root);
    assert_eq(note_nullifier(commitment, note_spend_key), nullifier);

    let locked = transfer_commitment(chain_id, lock, deadline, asset, transfer_amount);
    assert(locked != 0);
    assert_eq(locked_commitment, locked);

    // `<=` above rules out the mod-2^256 subtraction wrap.
    let remaining = note_amount - transfer_amount;
    let creates_change = !remaining.unsafe_is_zero_integer();
    assert((change_commitment != 0) == creates_change);
    if creates_change {
        let next_key = change_key(note_spend_key, nullifier);
        let next_serial = note_sn(next_key);
        let next_commitment = note_commitment(chain_id, next_serial, asset, remaining);
        assert(next_key != 0);
        assert(next_serial != 0);
        assert(next_commitment != 0);
        assert_eq(change_commitment, next_commitment);
        assert(note_nullifier(next_commitment, next_key) != 0);
    }
}
