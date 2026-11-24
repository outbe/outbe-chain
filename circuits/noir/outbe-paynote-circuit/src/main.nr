// Paynote private ERC20 payment-note circuit.
//
// Proves the right to spend `spend_amount` of a public `asset` out of a private
// note committed under the public `root`, publishing a nullifier and -- for a
// partial spend -- the commitment of the change note. Amounts are 256-bit
// (`bignum::U256`, three 120-bit little-endian limbs). The note is a bearer
// instrument: spend authority is knowledge of `note_spend_key`.
//
// `context` is an opaque public commitment. The circuit authenticates it and
// rejects zero. It does not interpret the preimage, and the nullifier does
// not depend on it.

mod tests;

use bignum::{BigNum, U256};
use outbe_circuit_core::merkle_tree::DEPTH;
use outbe_circuit_core::types::EthAddress;
use paynote_lib::{change_key, merkle_root, note_commitment, note_nullifier, note_sn};

fn main(
    chain_id: pub u64,
    root: pub Field,
    nullifier: pub Field,
    asset: pub EthAddress,
    context: pub Field,
    spend_amount: pub U256,
    change_commitment: pub Field,
    note_amount: U256,
    note_spend_key: Field,
    leaf_index: u32,
    auth_path: [Field; DEPTH],
) {
    // ABI-supplied limbs are unconstrained. `validate_in_field` bounds each
    // amount to <= 2^256; modular nonzero then rejects both zero encodings
    // (`[0, 0, 0]` and the modulus `[0, 0, 2^16]`), leaving exactly 1..2^256-1.
    spend_amount.validate_in_field();
    note_amount.validate_in_field();
    spend_amount.assert_is_not_zero();
    note_amount.assert_is_not_zero();
    assert(spend_amount <= note_amount);
    assert(nullifier != 0);
    assert(note_spend_key != 0);
    asset.validate();
    assert(context != 0);

    let serial = note_sn(note_spend_key);
    let commitment = note_commitment(chain_id, serial, asset, note_amount);
    let computed_root = merkle_root(commitment, leaf_index, auth_path);
    let computed_nullifier = note_nullifier(commitment, note_spend_key);

    assert(serial != 0);
    // Load-bearing: blocks spending a zero-padded empty tree slot.
    assert(commitment != 0);
    assert_eq(computed_root, root);
    assert_eq(computed_nullifier, nullifier);

    // `<=` above rules out the mod-2^256 subtraction wrap.
    let remaining = note_amount - spend_amount;
    let creates_change = !remaining.is_zero_integer();
    assert((change_commitment != 0) == creates_change);
    if creates_change {
        let next_key = change_key(note_spend_key, computed_nullifier);
        let next_serial = note_sn(next_key);
        // The change note inherits the SAME asset, so it stays spendable in the
        // same token.
        let next_commitment = note_commitment(chain_id, next_serial, asset, remaining);
        let next_nullifier = note_nullifier(next_commitment, next_key);
        assert(next_key != 0);
        assert(next_serial != 0);
        assert(next_commitment != 0);
        assert_eq(change_commitment, next_commitment);
        assert(next_nullifier != 0);
    }
}
