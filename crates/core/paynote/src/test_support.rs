//! Proving and pool-seeding fixtures for PayNote, shared by this crate's own
//! tests and by downstream modules that consume notes (`nodfactory`, …).
//!
//! Enabled by the `test-utils` feature. These reference fixtures are unreachable
//! from [`crate::runtime`]. Client applications use [`crate::client`] for
//! production membership witnesses.

use alloy_primitives::{Address, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_protocol::codec::u256_limbs_be;
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::Codec as _;
use outbe_protocol::FieldElement as _;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::noir::paynote::{Paynote as PayNote, PublicInputs, Witness};

use crate::hash::{change_key, empty_subtrees, note_commitment, note_nullifier, note_sn};
use crate::runtime;
use crate::schema::{PayNoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_DEPTH};
use crate::Field;
use crate::{PayNoteSuit, PayNoteTree};

/// Everything the pool and the prover need about one note.
pub struct Note {
    pub key: Field,
    pub serial: Field,
    pub commitment: Field,
    pub nullifier: Field,
    pub asset: Address,
    pub amount: U256,
}

pub fn note(chain_id: u64, key: u64, asset: Address, amount: U256) -> Note {
    note_under_key(chain_id, Field::from(key), asset, amount)
}

fn note_under_key(chain_id: u64, key: Field, asset: Address, amount: U256) -> Note {
    let serial = note_sn(key).unwrap();
    let commitment = note_commitment(chain_id, serial, asset, amount).unwrap();
    let nullifier = note_nullifier(commitment, key).unwrap();
    Note {
        key,
        serial,
        commitment,
        nullifier,
        asset,
        amount,
    }
}

/// The change note a `spend_amount` spend of `note` leaves behind: the leaf the
/// pool appends, and the only thing its owner can pay with next. `None` for a
/// full spend, which the circuit represents with the zero sentinel rather than
/// a note for nothing.
///
/// The change key is derived from the spent note's key and nullifier, so the
/// owner can rebuild the change note from what they already hold — nothing
/// about it is published beyond the commitment.
pub fn change_note(chain_id: u64, note: &Note, spend_amount: U256) -> Option<Note> {
    let remaining = note.amount.checked_sub(spend_amount)?;
    if remaining.is_zero() {
        return None;
    }
    let key = change_key(note.key, note.nullifier).unwrap();
    Some(note_under_key(chain_id, key, note.asset, remaining))
}

/// Proves `owner` spending `spend_amount` of the note sitting at `leaf_index`
/// in `tree`, returning combined public-inputs-plus-proof bytes.
///
/// The tree is a parameter because a note's auth path only exists relative to
/// the pool state it is spent against — including any change leaf an earlier
/// spend appended.
pub fn spend_proof(
    chain_id: u64,
    tree: &PayNoteTree,
    leaf_index: u32,
    note: &Note,
    owner: Address,
    spend_amount: U256,
) -> Vec<u8> {
    let (public, proof) = prove_spend(chain_id, tree, leaf_index, note, owner, spend_amount);
    combined_from(&public, &proof)
}

fn prove_spend(
    chain_id: u64,
    tree: &PayNoteTree,
    leaf_index: u32,
    n: &Note,
    owner: Address,
    spend_amount: U256,
) -> (PublicInputs, Vec<Vec<u8>>) {
    let public = PublicInputs {
        chain_id,
        root: tree.root(),
        nullifier: n.nullifier,
        asset: n.asset.to_field().unwrap(),
        owner: owner.to_field().unwrap(),
        spend_amount: u256_limbs_be(&spend_amount.to_be_bytes::<32>()),
        change_commitment: change_note(chain_id, n, spend_amount)
            .map_or(Field::from(0u64), |change| change.commitment),
    };
    let witness = Witness {
        note_amount: u256_limbs_be(&n.amount.to_be_bytes::<32>()),
        note_spend_key: n.key,
        leaf_index,
        auth_path: tree
            .inclusion_path(u64::from(leaf_index))
            .unwrap()
            .siblings
            .try_into()
            .unwrap(),
    };
    let proof = ProofGenerator::<PayNoteSuit, PayNote>::generate(
        &Barretenberg::default(),
        &witness,
        &public,
    )
    .expect("paynote proof generation");
    (public, proof.proof)
}

pub fn combined_from(public: &PublicInputs, proof_words: &[Vec<u8>]) -> Vec<u8> {
    let fields = <PayNote as Circuit<PayNoteSuit>>::public_inputs(public);
    let mut combined = Vec::with_capacity(4 + 32 * (fields.len() + proof_words.len()));
    combined.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for f in fields {
        combined.extend_from_slice(PayNoteSuit::field_to_b256(&f).unwrap().as_slice());
    }
    for word in proof_words {
        combined.extend_from_slice(word);
    }
    combined
}

/// Seed an initialized pool holding exactly `leaves`, mirroring what a
/// sequence of deposits would have produced. Bypasses `deposit` because its
/// ERC20/VaultRouter sub-calls cannot be served in-memory.
pub fn seed_pool(provider: &mut HashMapStorageProvider, chain_id: u64, leaves: &[Field]) {
    provider.enter(|storage| {
        let paynote: PayNoteContract<'_> = storage.contract();
        let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).unwrap();
        let empty_root = PayNoteSuit::field_to_b256(&zeros[PAYNOTE_TREE_DEPTH]).unwrap();
        paynote.current_root.write(empty_root).unwrap();
        paynote.recent_roots.setup(PAYNOTE_ROOT_WINDOW).unwrap();
        paynote.recent_roots.push(empty_root).unwrap();
        for leaf in leaves {
            runtime::append(&paynote, &zeros, *leaf).unwrap();
            paynote
                .commitments
                .write(&PayNoteSuit::field_to_b256(leaf).unwrap(), true)
                .unwrap();
        }
    });
}

/// One deposited note plus a spend proof over it: everything a consuming
/// module needs to exercise `api::consume` without knowing how notes are built.
pub struct SpendFixture {
    /// The leaf to seed into the pool via [`seed_pool`] before spending.
    pub commitment: Field,
    /// Combined public-inputs-plus-proof bytes for [`crate::api::consume`].
    pub proof: Vec<u8>,
    /// The statement the proof carries.
    pub public: PublicInputs,
    /// The tree the membership path was taken from.
    pub tree: PayNoteTree,
}

/// Builds a note of `note_amount` in `asset` and proves a `spend_amount` spend
/// of it by `owner`, over a tree holding that note alone.
///
/// Proving is real Barretenberg work — roughly half a second per call — so
/// callers should build one fixture per assertion, not one per iteration.
pub fn note_and_spend_proof(
    chain_id: u64,
    asset: Address,
    owner: Address,
    note_amount: U256,
    spend_amount: U256,
) -> SpendFixture {
    let n = note(chain_id, 17, asset, note_amount);
    let mut tree = crate::client::new_tree(chain_id).unwrap();
    let leaf_index = u32::try_from(tree.append(n.commitment).unwrap().0).unwrap();
    let (public, proof) = prove_spend(chain_id, &tree, leaf_index, &n, owner, spend_amount);

    SpendFixture {
        commitment: n.commitment,
        proof: combined_from(&public, &proof),
        public,
        tree,
    }
}
