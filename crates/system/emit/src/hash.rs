//! Rust mirror of the Emit hash and tree formulas.
//!
//! Copied from the frozen circuit formulas in
//! `outbe-emit-mint-circuit/src/emit.nr`:
//!
//! - BN254 fields use canonical 32-byte big-endian encodings; a word that
//!   would require reduction is invalid input.
//! - `h2(left, right) = Poseidon2([left, right, 0, 2^65])[0]` — exactly noir's
//!   two-input `std::hash::poseidon2` (the `outbe-poseidon` sponge with
//!   `len = 2`).
//! - `p(tag, values)` absorbs the big-endian ASCII domain `OUTBE_EMIT`, the
//!   exact purpose tag, the tuple arity, then the ordered values.

use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon2, PoseidonHasher};

/// The proving field — BN254 scalar field, matching the noir circuits.
pub type Field = Fr;

fn ascii_field(value: &str) -> Field {
    Field::from_be_bytes_mod_order(value.as_bytes())
}

/// `h2(left, right) = Poseidon2([left, right, 0, 2^65])[0]`.
fn h2(left: Field, right: Field) -> Field {
    Poseidon2::<Field>::new()
        .hash(&[left, right])
        .expect("Poseidon2 sponge is infallible")
}

/// Purpose-tagged chaining: `p(tag, values)` absorbs domain, tag, arity, then
/// the ordered values.
pub fn p(tag: Field, values: &[Field]) -> Field {
    let mut state = h2(emit_domain(), tag);
    state = h2(state, Field::from(values.len() as u64));
    for value in values {
        state = h2(state, *value);
    }
    state
}

pub fn emit_domain() -> Field {
    ascii_field("OUTBE_EMIT")
}

pub fn tag_note_sn() -> Field {
    ascii_field("EMIT_NOTE_SN")
}

pub fn tag_commitment() -> Field {
    ascii_field("EMIT_COMMITMENT")
}

pub fn tag_nullifier() -> Field {
    ascii_field("EMIT_NULLIFIER")
}

pub fn tag_change_key() -> Field {
    ascii_field("EMIT_CHANGE_KEY")
}

pub fn tag_empty() -> Field {
    ascii_field("EMIT_EMPTY")
}

/// An owner address absorbed as one big-endian integer, matching the circuit.
pub fn address_field(owner: [u8; 20]) -> Field {
    Field::from_be_bytes_mod_order(&owner)
}

/// `note_sn = P(EMIT_NOTE_SN, [owner, spend_key])`.
pub fn note_sn(note_owner: [u8; 20], note_spend_key: Field) -> Field {
    p(tag_note_sn(), &[address_field(note_owner), note_spend_key])
}

/// `C = P(EMIT_COMMITMENT, [chain_id, note_sn, note_amount])` — the only
/// commitment form the runtime ever appends; opaque caller-supplied
/// commitments are prohibited.
pub fn note_commitment(chain_id: u64, note_sn: Field, note_amount: u64) -> Field {
    p(
        tag_commitment(),
        &[Field::from(chain_id), note_sn, Field::from(note_amount)],
    )
}

/// `nullifier = P(EMIT_NULLIFIER, [chain_id, note_sn, spend_key])` —
/// amount-independent, so serial-sharing notes strand each other exactly as
/// in the circuit.
pub fn nullifier(chain_id: u64, note_sn: Field, note_spend_key: Field) -> Field {
    p(
        tag_nullifier(),
        &[Field::from(chain_id), note_sn, note_spend_key],
    )
}

/// `next_key = P(EMIT_CHANGE_KEY, [spend_key, nullifier])` — the
/// circuit-ratcheted successor key of a partial mint.
pub fn change_key(note_spend_key: Field, note_nullifier: Field) -> Field {
    p(tag_change_key(), &[note_spend_key, note_nullifier])
}

/// Chain-specific empty leaf: `P(EMIT_EMPTY, [chain_id])`.
pub fn empty_leaf(chain_id: u64) -> Field {
    p(tag_empty(), &[Field::from(chain_id)])
}

/// Shared untagged Merkle inner node: `H2(left, right)`.
pub fn merkle_node(left: Field, right: Field) -> Field {
    h2(left, right)
}

/// The complete chain-specific empty ladder `zeros[0..=depth]`:
/// `zeros[0] = empty_leaf(chain_id)`,
/// `zeros[i+1] = H2(zeros[i], zeros[i])`.
/// Derived in memory on every request; never persisted.
pub fn empty_subtrees(chain_id: u64, depth: usize) -> Vec<Field> {
    let mut zeros = vec![Field::from(0u64); depth + 1];
    zeros[0] = empty_leaf(chain_id);
    for level in 0..depth {
        zeros[level + 1] = merkle_node(zeros[level], zeros[level]);
    }
    zeros
}

/// Canonical 32-byte big-endian encoding of a field element.
pub fn field_to_be_bytes(value: Field) -> [u8; 32] {
    let bytes = value.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    out
}

/// Parse a canonical 32-byte big-endian field word; `None` when the word
/// would require reduction. Callers attach the ABI field name to the error.
pub fn field_from_be_bytes(bytes: &[u8; 32]) -> Option<Field> {
    let field = Field::from_be_bytes_mod_order(bytes);
    (field_to_be_bytes(field) == *bytes).then_some(field)
}
