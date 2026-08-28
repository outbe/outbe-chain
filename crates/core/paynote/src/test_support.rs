//! Proving and pool-seeding fixtures for Paynote, shared by this crate's own
//! tests and by downstream modules that consume notes (`nodfactory`, …).
//!
//! Enabled by the `test-utils` feature. Witness construction and proving stay
//! out of production builds entirely: nothing here is reachable from
//! [`crate::runtime`].

use alloy_primitives::{Address, B256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_protocol::protocol::zk::{Circuit, ProofGenerator};
use outbe_protocol::OutbeV1;
use outbe_zk_backend::barretenberg::Barretenberg;
use outbe_zk_canonical::noir::paynote::{Paynote, PublicInputs, Witness};

use ark_ff::{BigInteger as _, PrimeField};

use crate::hash::{
    address_field, change_key, empty_subtrees, field_to_be_bytes, merkle_node, note_commitment,
    note_nullifier, note_sn, Field,
};
use crate::runtime;
use crate::schema::{PaynoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_DEPTH};

// ---- reference tree -------------------------------------------------------

/// Naive recompute-from-leaves tree, used both to derive witnesses and to
/// cross-check the runtime's stored incremental tree. Deliberately a different
/// algorithm from `runtime::append` so agreement means something.
pub struct ReferenceTree {
    pub leaves: Vec<Field>,
    zeros: Vec<Field>,
}

impl ReferenceTree {
    pub fn new(chain_id: u64) -> Self {
        Self {
            leaves: Vec::new(),
            zeros: empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).unwrap(),
        }
    }

    pub fn append(&mut self, leaf: Field) -> u32 {
        let index = self.leaves.len() as u32;
        self.leaves.push(leaf);
        index
    }

    pub fn root(&self) -> Field {
        let mut nodes = self.leaves.clone();
        for level in 0..PAYNOTE_TREE_DEPTH {
            if nodes.len() % 2 == 1 {
                nodes.push(self.zeros[level]);
            }
            nodes = nodes
                .chunks_exact(2)
                .map(|pair| merkle_node(pair[0], pair[1]).unwrap())
                .collect();
        }
        nodes[0]
    }

    pub fn path_at(&self, leaf_index: u32) -> [Field; PAYNOTE_TREE_DEPTH] {
        let mut index = leaf_index as usize;
        let mut path = [Field::from(0u64); PAYNOTE_TREE_DEPTH];
        let mut nodes = self.leaves.clone();
        for (level, sibling) in path.iter_mut().enumerate() {
            if nodes.len() % 2 == 1 {
                nodes.push(self.zeros[level]);
            }
            *sibling = nodes.get(index ^ 1).copied().unwrap_or(self.zeros[level]);
            nodes = nodes
                .chunks_exact(2)
                .map(|pair| merkle_node(pair[0], pair[1]).unwrap())
                .collect();
            index >>= 1;
        }
        path
    }
}

/// Everything the pool and the prover need about one note.
pub struct Note {
    pub key: Field,
    pub serial: Field,
    pub commitment: Field,
    pub nullifier: Field,
}

pub fn note(chain_id: u64, key: u64, asset: Address, amount: u128) -> Note {
    let key = Field::from(key);
    let serial = note_sn(key).unwrap();
    let commitment = note_commitment(chain_id, serial, asset.into(), amount).unwrap();
    let nullifier = note_nullifier(commitment, key).unwrap();
    Note {
        key,
        serial,
        commitment,
        nullifier,
    }
}

pub fn combined_from(public: &PublicInputs, proof_words: &[Vec<u8>]) -> Vec<u8> {
    let fields = <Paynote as Circuit<OutbeV1>>::public_inputs(public);
    let mut combined = Vec::with_capacity(4 + 32 * (fields.len() + proof_words.len()));
    combined.extend_from_slice(&(fields.len() as u32).to_be_bytes());
    for f in fields {
        let bytes = f.into_bigint().to_bytes_be();
        combined.resize(combined.len() + 32 - bytes.len(), 0);
        combined.extend_from_slice(&bytes);
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
        let paynote: PaynoteContract<'_> = storage.contract();
        let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).unwrap();
        let empty_root = B256::new(field_to_be_bytes(zeros[PAYNOTE_TREE_DEPTH]));
        paynote.current_root.write(empty_root).unwrap();
        paynote.recent_roots.setup(PAYNOTE_ROOT_WINDOW).unwrap();
        paynote.recent_roots.push(empty_root).unwrap();
        for leaf in leaves {
            runtime::append(&paynote, &zeros, *leaf).unwrap();
            paynote
                .commitments
                .write(&B256::new(field_to_be_bytes(*leaf)), true)
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
    pub tree: ReferenceTree,
}

/// Builds a note of `note_amount` in `asset` and proves a `spend_amount` spend
/// of it by `spender`, over a tree holding that note alone.
///
/// Proving is real Barretenberg work — roughly half a second per call — so
/// callers should build one fixture per assertion, not one per iteration.
pub fn note_and_spend_proof(
    chain_id: u64,
    asset: Address,
    spender: Address,
    note_amount: u128,
    spend_amount: u128,
) -> SpendFixture {
    let n = note(chain_id, 17, asset, note_amount);
    let mut tree = ReferenceTree::new(chain_id);
    let leaf_index = tree.append(n.commitment);

    let remaining = note_amount - spend_amount;
    let change_commitment = if remaining > 0 {
        let next_key = change_key(n.key, n.nullifier).unwrap();
        let next_serial = note_sn(next_key).unwrap();
        note_commitment(chain_id, next_serial, asset.into(), remaining).unwrap()
    } else {
        Field::from(0u64)
    };

    let public = PublicInputs {
        chain_id,
        root: tree.root(),
        nullifier: n.nullifier,
        asset: address_field(asset.into()),
        spender: address_field(spender.into()),
        spend_amount,
        change_commitment,
    };
    let witness = Witness {
        note_amount,
        note_spend_key: n.key,
        leaf_index,
        auth_path: tree.path_at(leaf_index),
    };
    let proof =
        ProofGenerator::<OutbeV1, Paynote>::generate(&Barretenberg::default(), &witness, &public)
            .expect("paynote proof generation");

    SpendFixture {
        commitment: n.commitment,
        proof: combined_from(&public, &proof.proof),
        public,
        tree,
    }
}
