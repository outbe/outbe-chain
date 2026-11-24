//! Paynote private ERC20 payment-note formulas.
//!
//! Every Paynote preimage is tagged with `tags::tag(PAYNOTE_DOMAIN, TAG_*)` — a
//! shared base tag from `outbe_circuit_core::tags` folded with Paynote's own
//! domain — so no Paynote hash can collide with another domain's hash of the
//! same purpose. Only the base tags and the Merkle primitives come from the
//! core; the preimages themselves are Paynote's. Merkle inner nodes use
//! `PAYNOTE_DOMAIN` directly.
//!
//! The commitment binds a fourth field the shared note formulas do not: the
//! asset, and it lives in the **commitment**, not the serial. The pool contract
//! derives a deposit leaf from the asset and amount it actually pulled, so the
//! leaf -- and therefore Merkle membership -- attests both. An asset carried in
//! the serial would be user-supplied and unverifiable at deposit time, letting a
//! depositor fund a note in a cheap token and spend it as an expensive one.

use bignum::{BigNum, U256};
use outbe_circuit_core::hash::{ascii_field, hash_multi};
use outbe_circuit_core::merkle_tree;
use outbe_circuit_core::tags::{
    tag, TAG_CHANGE_KEY, TAG_COMMITMENT, TAG_EMPTY, TAG_NOTE_SN, TAG_NULLIFIER,
};
use outbe_circuit_core::types::EthAddress;

/// Big-endian ASCII tree domain: every Paynote tag and Merkle node hangs off it.
pub global PAYNOTE_DOMAIN: Field = ascii_field("OUTBE_PAYNOTE");

/// Paynote's instance of a shared base tag.
pub fn paynote_tag(base: Field) -> Field {
    tag(PAYNOTE_DOMAIN, base)
}

/// Note serial -- a hiding commitment to the spend key. Chain-, asset- and
/// amount-independent, so the pool contract can accept one at deposit time and
/// build the leaf around it.
pub fn note_sn(note_spend_key: Field) -> Field {
    hash_multi(paynote_tag(TAG_NOTE_SN), [note_spend_key])
}

/// Note commitment -- the Merkle leaf. Binds the asset and the amount, both of
/// which the pool contract fills in from the transfer it actually performed.
/// The three canonical amount limbs are hashed independently: a 256-bit value
/// cannot be folded into the 254-bit circuit field without aliases.
pub fn note_commitment(
    chain_id: u64,
    serial: Field,
    asset: EthAddress,
    note_amount: U256,
) -> Field {
    let limbs = note_amount.get_limbs();
    hash_multi(
        paynote_tag(TAG_COMMITMENT),
        [
            chain_id as Field,
            serial,
            asset.to_field(),
            limbs[0] as Field,
            limbs[1] as Field,
            limbs[2] as Field,
        ],
    )
}

/// Spend nullifier, derived from the **commitment** rather than the serial, so
/// every leaf has exactly one nullifier. That is what makes two leaves sharing
/// a serial harmless: their amounts differ, so their commitments differ, so
/// their nullifiers differ and both stay spendable. It also binds the asset and
/// the amount for free. `chain_id` is deliberately absent -- the commitment
/// already binds it.
pub fn note_nullifier(commitment: Field, note_spend_key: Field) -> Field {
    hash_multi(paynote_tag(TAG_NULLIFIER), [commitment, note_spend_key])
}

pub fn change_key(note_spend_key: Field, spent_nullifier: Field) -> Field {
    hash_multi(
        paynote_tag(TAG_CHANGE_KEY),
        [note_spend_key, spent_nullifier],
    )
}

/// Empty-leaf value for a Paynote tree on `chain_id`.
pub fn empty_leaf(chain_id: u64) -> Field {
    hash_multi(paynote_tag(TAG_EMPTY), [chain_id as Field])
}

/// Resolve a Paynote Merkle root from a compact little-endian leaf index.
pub fn merkle_root<let DEPTH: u32>(
    leaf: Field,
    leaf_index: u32,
    auth_path: [Field; DEPTH],
) -> Field {
    merkle_tree::merkle_root(PAYNOTE_DOMAIN, leaf, leaf_index as Field, auth_path)
}

#[test]
fn asset_separates_equal_value_notes() {
    let chain_id = 31337;
    let usdc = EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48);
    let wbtc = EthAddress::from_field(0x2260fac5e5542a773aa44fbcfedf7c193bc2c599);
    let serial = note_sn(17);

    assert(
        note_commitment(chain_id, serial, usdc, U256::from(100))
            != note_commitment(chain_id, serial, wbtc, U256::from(100)),
    );
}

#[test]
fn key_rotation_changes_equal_value_notes() {
    let chain_id = 31337;
    let asset = EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48);
    let key = 17;
    let serial = note_sn(key);
    let commitment = note_commitment(chain_id, serial, asset, U256::from(100));
    let spent_nullifier = note_nullifier(commitment, key);
    let next_key = change_key(key, spent_nullifier);
    let next_serial = note_sn(next_key);
    let next_commitment = note_commitment(chain_id, next_serial, asset, U256::from(100));
    let next_nullifier = note_nullifier(next_commitment, next_key);

    assert(serial != next_serial);
    assert(commitment != next_commitment);
    assert(spent_nullifier != next_nullifier);
}

/// Binding the nullifier to the commitment -- not the serial -- is what makes a
/// serial collision harmless. Two leaves under one serial carry different
/// amounts, so they hash to different commitments and yield distinct
/// nullifiers; both remain spendable. Bind the serial instead and they would
/// alias onto one nullifier, locking one of the two up forever.
#[test]
fn one_serial_two_amounts_have_distinct_nullifiers() {
    let chain_id = 31337;
    let asset = EthAddress::from_field(0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48);
    let key = 17;
    let serial = note_sn(key);
    let commitment_a = note_commitment(chain_id, serial, asset, U256::from(40));
    let commitment_b = note_commitment(chain_id, serial, asset, U256::from(60));

    assert(commitment_a != commitment_b);
    assert(note_nullifier(commitment_a, key) != note_nullifier(commitment_b, key));
    assert_eq(note_nullifier(commitment_a, key), note_nullifier(commitment_a, key));
}
