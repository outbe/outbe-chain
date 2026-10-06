//! Proving and pool-seeding fixtures for PayNote, shared by this crate's own
//! tests and by downstream modules that consume notes (`nodfactory`, …).
//!
//! The `test-utils` feature enables this module. These reference fixtures are unreachable
//! from [`crate::runtime`]. Client applications use [`crate::client`] for
//! production membership witnesses.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_protocol::codec::{field_from_b256, field_to_b256, u256_limbs_be};
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::FieldElement as _;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::noir::paynote::{Paynote as PayNote, PublicInputs, Witness};

use crate::hash::{change_key, empty_subtrees, note_commitment, note_nullifier, note_sn};
use crate::runtime;
use crate::schema::{PayNoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_DEPTH};
use crate::Field;
use crate::PayNoteTree;

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
/// The change key is derived from the spent note's key and nullifier. Thus the
/// owner can rebuild the change note from what they already hold. Nothing
/// about it is published beyond the commitment.
pub fn change_note(chain_id: u64, note: &Note, spend_amount: U256) -> Option<Note> {
    let remaining = note.amount.checked_sub(spend_amount)?;
    if remaining.is_zero() {
        return None;
    }
    let key = change_key(note.key, note.nullifier).unwrap();
    Some(note_under_key(chain_id, key, note.asset, remaining))
}

/// Proves a `spend_amount` spend of the note sitting at `leaf_index` in `tree`,
/// bound to `context`, returning combined public-inputs-plus-proof bytes.
///
/// The tree is a parameter because a note's auth path only exists relative to
/// the pool state it is spent against. That state includes any change leaf an
/// earlier spend appended.
pub fn spend_proof(
    chain_id: u64,
    tree: &PayNoteTree,
    leaf_index: u32,
    note: &Note,
    context: B256,
    spend_amount: U256,
) -> Vec<u8> {
    let (public, proof) = prove_spend(chain_id, tree, leaf_index, note, context, spend_amount);
    combined_from(&public, &proof)
}

fn prove_spend(
    chain_id: u64,
    tree: &PayNoteTree,
    leaf_index: u32,
    n: &Note,
    context: B256,
    spend_amount: U256,
) -> (PublicInputs, Vec<Vec<u8>>) {
    let public = PublicInputs {
        chain_id,
        root: tree.root(),
        nullifier: n.nullifier,
        asset: n.asset.to_field().unwrap(),
        context: field_from_b256(&context).expect("canonical settlement context"),
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
    let proof = ProofGenerator::<PayNote>::generate(&Barretenberg::default(), &witness, &public)
        .expect("paynote proof generation");
    (public, proof.proof)
}

pub fn combined_from(public: &PublicInputs, proof_words: &[Vec<u8>]) -> Vec<u8> {
    let fields = <PayNote as Circuit>::public_inputs(public);
    let mut combined = Vec::with_capacity(4 + 32 * (fields.len() + proof_words.len()));
    combined.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for f in fields {
        combined.extend_from_slice(field_to_b256(&f).unwrap().as_slice());
    }
    for word in proof_words {
        combined.extend_from_slice(word);
    }
    combined
}

/// A real merge proof built through the same checked witness builder as clients.
pub fn merge_proof(chain_id: u64, tree: &PayNoteTree, inputs: &[&Note], output: &Note) -> Vec<u8> {
    use outbe_zk_canonical::paynote_merge::{encode_combined_proof, PaynoteMerge};
    let private = inputs
        .iter()
        .map(|note| (note.amount, field_to_b256(&note.key).unwrap()))
        .collect::<Vec<_>>();
    let (public, witness) = crate::client::merge_witness(
        tree,
        chain_id,
        output.asset,
        &private,
        field_to_b256(&output.key).unwrap(),
    )
    .unwrap();
    assert_eq!(
        public.output_commitment,
        field_to_b256(&output.commitment).unwrap()
    );
    let public = public.try_into().unwrap();
    let proof = ProofGenerator::<PaynoteMerge>::generate(
        &Barretenberg::default(),
        &witness.try_into().unwrap(),
        &public,
    )
    .unwrap();
    encode_combined_proof(public, proof.proof).unwrap()
}

/// Seed an initialized pool holding exactly `leaves`, mirroring what a
/// sequence of deposits would have produced. Bypasses `deposit` because its
/// ERC20/VaultRouter sub-calls cannot be served in-memory.
pub fn seed_pool(provider: &mut HashMapStorageProvider, chain_id: u64, leaves: &[Field]) {
    provider.enter(|storage| {
        let paynote: PayNoteContract<'_> = storage.contract();
        let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).unwrap();
        let empty_root = field_to_b256(&zeros[PAYNOTE_TREE_DEPTH]).unwrap();
        paynote.current_root.write(empty_root).unwrap();
        paynote.recent_roots.setup(PAYNOTE_ROOT_WINDOW).unwrap();
        paynote.recent_roots.push(empty_root).unwrap();
        for leaf in leaves {
            runtime::append(&paynote, &zeros, *leaf).unwrap();
            paynote
                .commitments
                .write(&field_to_b256(leaf).unwrap(), true)
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
/// of it bound to `context`, over a tree holding that note alone.
///
/// Proving is real Barretenberg work, roughly half a second per call. Thus
/// callers should build one fixture per assertion, not one per iteration.
pub fn note_and_spend_proof(
    chain_id: u64,
    asset: Address,
    context: B256,
    note_amount: U256,
    spend_amount: U256,
) -> SpendFixture {
    let n = note(chain_id, 17, asset, note_amount);
    let mut tree = crate::client::new_tree(chain_id).unwrap();
    let leaf_index = u32::try_from(tree.append(n.commitment).unwrap().0).unwrap();
    let (public, proof) = prove_spend(chain_id, &tree, leaf_index, &n, context, spend_amount);

    SpendFixture {
        commitment: n.commitment,
        proof: combined_from(&public, &proof),
        public,
        tree,
    }
}

/// Seeds two funded notes, merges them through the public dispatch and proves a
/// full ordinary spend of the output. Returns that spend proof and the output
/// nullifier. Consumer tests use this to cover the real merge -> settlement
/// boundary with no token transfer or mock verifier.
pub fn merged_note_spend_proof(
    provider: &mut HashMapStorageProvider,
    chain_id: u64,
    asset: Address,
    context: B256,
    amount: U256,
) -> (Vec<u8>, Field) {
    use alloy_sol_types::SolCall;
    assert!(amount > U256::ONE);
    let inputs = [
        note(chain_id, 17, asset, U256::ONE),
        note(chain_id, 18, asset, amount - U256::ONE),
    ];
    let output = note(chain_id, 99, asset, amount);
    let mut tree = crate::client::new_tree(chain_id).unwrap();
    for input in &inputs {
        tree.append(input.commitment).unwrap();
    }
    seed_pool(provider, chain_id, tree.leaves());
    let merge = merge_proof(chain_id, &tree, &inputs.iter().collect::<Vec<_>>(), &output);
    provider.enter(|storage| {
        crate::precompile::dispatch(
            storage,
            &crate::precompile::IPayNote::mergePayNotesCall {
                proof: merge.into(),
            }
            .abi_encode(),
            Address::repeat_byte(0x77),
            U256::ZERO,
        )
        .unwrap()
    });
    tree.append(output.commitment).unwrap();
    (
        spend_proof(chain_id, &tree, 2, &output, context, amount),
        output.nullifier,
    )
}
