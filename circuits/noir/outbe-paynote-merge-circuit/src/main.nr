// One proof consumes 2..4 ordinary PayNotes and creates one ordinary note.
// Amounts, keys and input commitments never leave the private witness.
mod tests;

use bignum::{BigNum, U256};
use outbe_circuit_core::{merkle_tree::DEPTH, types::EthAddress};
use paynote_lib::{merkle_root, note_commitment, note_nullifier, note_sn};

global MIN_MERGE_INPUTS: u32 = 2;
global MAX_MERGE_INPUTS: u32 = 4;
global PAYNOTE_POOL_ADDRESS: Field = 0x1019;

fn main(
    chain_id: pub u64,
    pool: pub EthAddress,
    root: pub Field,
    input_count: pub u32,
    asset: pub EthAddress,
    nullifiers: pub [Field; MAX_MERGE_INPUTS],
    output_commitment: pub Field,
    note_amounts: [U256; MAX_MERGE_INPUTS],
    note_spend_keys: [Field; MAX_MERGE_INPUTS],
    leaf_indices: [u32; MAX_MERGE_INPUTS],
    auth_paths: [[Field; DEPTH]; MAX_MERGE_INPUTS],
    output_spend_key: Field,
) {
    assert(input_count >= MIN_MERGE_INPUTS & input_count <= MAX_MERGE_INPUTS);
    pool.validate();
    assert_eq(pool.to_field(), PAYNOTE_POOL_ADDRESS);
    asset.validate();
    assert(asset.to_field() != 0);
    assert(output_spend_key != 0);
    assert(output_commitment != 0);

    let mut total = U256::zero();
    let mut commitments = [0; MAX_MERGE_INPUTS];
    for i in 0..MAX_MERGE_INPUTS {
        if i < input_count {
            let amount = note_amounts[i];
            amount.validate_in_field();
            amount.assert_is_not_zero();
            let key = note_spend_keys[i];
            assert(key != 0);
            assert(output_spend_key != key);
            let serial = note_sn(key);
            assert(serial != 0);
            let commitment = note_commitment(chain_id, serial, asset, amount);
            commitments[i] = commitment;
            assert(commitment != 0);
            assert(commitment != output_commitment);
            assert_eq(merkle_root(commitment, leaf_indices[i], auth_paths[i]), root);
            assert(nullifiers[i] != 0);
            assert_eq(note_nullifier(commitment, key), nullifiers[i]);
            for j in 0..MAX_MERGE_INPUTS {
                if j < i {
                    assert(commitment != commitments[j]);
                    assert(nullifiers[i] != nullifiers[j]);
                }
            }
            // Positive canonical U256 operands: a modular wrap cannot increase
            // the previous total. Also reject the alternate modulus-zero encoding.
            let next = total + amount;
            next.validate_in_field();
            next.assert_is_not_zero();
            assert(next > total);
            total = next;
        } else {
            assert_eq(nullifiers[i], 0);
            assert_eq(note_amounts[i].get_limbs(), [0, 0, 0]);
            assert_eq(note_spend_keys[i], 0);
            assert_eq(leaf_indices[i], 0);
            assert_eq(auth_paths[i], [0; DEPTH]);
        }
    }
    let serial = note_sn(output_spend_key);
    assert(serial != 0);
    assert_eq(note_commitment(chain_id, serial, asset, total), output_commitment);
    assert(note_nullifier(output_commitment, output_spend_key) != 0);
}
