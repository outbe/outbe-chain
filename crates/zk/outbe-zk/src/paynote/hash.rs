//! Rust mirror of the PayNote hash and tree formulas.
//!
//! Mirrors the frozen circuit formulas in
//! `outbe-paynote-lib/src/lib.nr` and
//! `outbe-circuit-core/src/{hash,tags,merkle_tree}.nr`:
//!
//! - BN254 fields use canonical 32-byte big-endian encodings; a word that
//!   would require reduction is invalid input.
//! - `h2(a, b)` / `h3(a, b, c)` are noir's `hash_2` / `hash_3` — the
//!   `outbe-poseidon` sponge at `len = 2` / `len = 3`.
//! - A purpose tag is *folded with the owning domain*:
//!   `tag(base) = h2(PAYNOTE_DOMAIN, base)`, so no PayNote hash can collide
//!   with another domain's hash of the same purpose.
//! - `hash_multi(tag, values)` mirrors noir `hash_multi(tag, values)`: absorb the
//!   folded tag, the tuple arity, then the ordered values.
//! - Merkle inner nodes are `h3(PAYNOTE_DOMAIN, left, right)`, where
//!   `PAYNOTE_DOMAIN` is the big-endian ASCII `OUTBE_PAYNOTE`.
//!
//! The commitment binds the **asset** as well as the amount, and the serial
//! does not. That is what lets the pool derive a deposit leaf from the
//! transfer it actually performed: an asset carried in the serial would be
//! user-supplied and unverifiable, letting a depositor fund a note in a cheap
//! token and spend it as an expensive one.

use crate::paynote::{Field, Pool, Tree};
#[cfg(feature = "alloy")]
use crate::{codec, FieldElement};
use crate::{codec::field_from_be_bytes, error::Error, primitive::hash};
#[cfg(feature = "alloy")]
use alloy_primitives::{Address, U256};

/// Big-endian ASCII domain; 12 bytes fit in the proving field.
pub const PAYNOTE_DOMAIN: &str = "OUTBE_PAYNOTE";

/// The circuit's domain as a field element.
pub fn paynote_domain() -> Field {
    hash::ascii_field(PAYNOTE_DOMAIN)
}

fn tag(base: Field) -> Result<Field, Error> {
    Pool::tag(paynote_domain(), base)
}

/// `note_sn = P(NOTE_SN, [spend_key])` — a hiding commitment to the spend
/// key. Chain-, asset- and amount-independent, so the pool can accept one at
/// deposit time and build the leaf around it.
pub fn note_sn(note_spend_key: Field) -> Result<Field, Error> {
    Pool::hash_multi(tag(Pool::tag_note_sn())?, &[note_spend_key])
}

/// `C = P(COMMITMENT, [chain_id, note_sn, asset, amount_limb_0,
/// amount_limb_1, amount_limb_2])` — the Merkle leaf and the only commitment
/// form the runtime appends. Hashing all canonical radix-2^120 limbs binds the
/// full uint256 amount without field-modulus aliases.
#[cfg(feature = "alloy")]
pub fn note_commitment(
    chain_id: u64,
    note_sn: Field,
    asset: Address,
    note_amount: U256,
) -> Result<Field, Error> {
    let [lo, mid, hi] = codec::fields_from_u256(&note_amount)?;
    Pool::hash_multi(
        tag(Pool::tag_commitment())?,
        &[
            chain_id.to_field()?,
            note_sn,
            asset.to_field()?,
            lo,
            mid,
            hi,
        ],
    )
}

/// Commit to a note with a 20-byte asset address and three radix-`2^120` amount fields.
/// Limbs must be inside the `[120, 120, 16]`-bit bounds.
pub fn note_commitment_raw(
    chain_id: u64,
    note_sn: Field,
    asset: [u8; 20],
    note_amount: [Field; 3],
) -> Result<Field, Error> {
    let [lo, mid, hi] = note_amount;
    Pool::hash_multi(
        tag(Pool::tag_commitment())?,
        &[
            Field::from(chain_id),
            note_sn,
            field_from_be_bytes(&asset),
            lo,
            mid,
            hi,
        ],
    )
}

/// `nullifier = P(NULLIFIER, [commitment, spend_key])` — derived from the
/// commitment rather than the serial, so every leaf has exactly one
/// nullifier. Two leaves sharing a serial carry different amounts, hence
/// different commitments and different nullifiers, and both stay spendable.
pub fn note_nullifier(note_commitment: Field, note_spend_key: Field) -> Result<Field, Error> {
    Pool::hash_multi(
        tag(Pool::tag_nullifier())?,
        &[note_commitment, note_spend_key],
    )
}

/// `next_key = P(CHANGE_KEY, [spend_key, nullifier])` — the circuit-ratcheted
/// successor key of a partial spend.
pub fn change_key(note_spend_key: Field, note_nullifier: Field) -> Result<Field, Error> {
    Pool::hash_multi(
        tag(Pool::tag_change_key())?,
        &[note_spend_key, note_nullifier],
    )
}

/// `lock = P(TRANSFER_LOCK, [recipient_pk, sender_serial, rho])` — who may
/// resolve a locked transfer note. Both parties are [`note_sn`] of a key they
/// hold. `rho` is the sender's random nonce.
pub fn transfer_lock(
    recipient_pk: Field,
    sender_serial: Field,
    rho: Field,
) -> Result<Field, Error> {
    Pool::hash_multi(
        tag(hash::ascii_field("TRANSFER_LOCK"))?,
        &[recipient_pk, sender_serial, rho],
    )
}

/// `P(TRANSFER, [chain_id, lock, deadline, asset, amount limbs])` — the leaf of
/// a locked transfer note. Its nullifier is [`note_nullifier`] under `rho`.
#[cfg(feature = "alloy")]
pub fn transfer_commitment(
    chain_id: u64,
    lock: Field,
    deadline: u64,
    asset: Address,
    amount: U256,
) -> Result<Field, Error> {
    let [lo, mid, hi] = codec::fields_from_u256(&amount)?;
    Pool::hash_multi(
        tag(hash::ascii_field("TRANSFER"))?,
        &[
            chain_id.to_field()?,
            lock,
            deadline.to_field()?,
            asset.to_field()?,
            lo,
            mid,
            hi,
        ],
    )
}

/// Chain-specific empty leaf: `P(EMPTY, [chain_id])`. Deliberately not zero —
/// the circuit's `commitment != 0` assert is what blocks spending a
/// zero-padded slot, and this keeps empty slots distinguishable per chain.
pub fn empty_leaf(chain_id: u64) -> Result<Field, Error> {
    Pool::hash_multi(tag(Pool::tag_empty())?, &[Field::from(chain_id)])
}

/// Tagged Merkle inner node: `H3(PAYNOTE_DOMAIN, left, right)`.
pub fn merkle_node(left: Field, right: Field) -> Result<Field, Error> {
    Tree::node_hash(paynote_domain(), left, right)
}

/// The complete chain-specific empty ladder `zeros[0..=depth]`:
/// `zeros[0] = empty_leaf(chain_id)`,
/// `zeros[i + 1] = H3(PAYNOTE_DOMAIN, zeros[i], zeros[i])`.
/// Derived in memory on every request; never persisted.
pub fn empty_subtrees(chain_id: u64, depth: usize) -> Result<Vec<Field>, Error> {
    Tree::empty_roots(paynote_domain(), empty_leaf(chain_id)?, depth)
}

#[cfg(all(test, feature = "alloy"))]
mod tests {
    use super::*;

    #[test]
    fn alloy_hashes_match_raw() {
        let amounts = [
            (U256::ZERO, [0, 0, 0]),
            (U256::from(1), [1, 0, 0]),
            (U256::from(1) << 120usize, [0, 1, 0]),
            (U256::from(1) << 240usize, [0, 0, 1]),
            (
                (U256::from(1) << 200usize) + U256::from(7),
                [7, 1u128 << 80, 0],
            ),
            (
                U256::MAX,
                [(1u128 << 120) - 1, (1u128 << 120) - 1, u16::MAX as u128],
            ),
        ];
        for (chain_id, key, address) in [
            (0, Field::from(0u64), [0; 20]),
            (
                31_337,
                Field::from(17u64),
                core::array::from_fn(|i| i as u8),
            ),
            (u64::MAX, -Field::from(1u64), [0xff; 20]),
        ] {
            let serial = note_sn(key).unwrap();
            for (amount, limbs) in amounts {
                assert_eq!(
                    note_commitment(chain_id, serial, Address::from(address), amount).unwrap(),
                    note_commitment_raw(chain_id, serial, address, limbs.map(Field::from)).unwrap(),
                    "chain_id={chain_id}, amount={amount}",
                );
            }
        }
    }
}
