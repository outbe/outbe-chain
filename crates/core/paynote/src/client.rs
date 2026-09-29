//! Off-chain membership witnesses for the PayNote commitment tree.
//!
//! Feed ordered `NewNote` commitments into the shared [`PayNoteTree`]. Callers validate
//! event indexes and compare its root with the pool before using a witness.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::addresses::PAYNOTE_ADDRESS;
use outbe_zk_canonical::paynote_merge::{
    alloy::{PublicInputs, Witness},
    MAX_MERGE_INPUTS,
};

use crate::Field;
use crate::{
    errors::PayNoteError,
    hash::{empty_leaf, paynote_domain},
    schema::PAYNOTE_TREE_DEPTH,
    PayNoteTree,
};

/// Start a PayNote tree with the chain-specific empty leaf.
pub fn new_tree(chain_id: u64) -> Result<PayNoteTree, PayNoteError> {
    PayNoteTree::new(
        paynote_domain(),
        empty_leaf(chain_id).map_err(|_| PayNoteError::Hash)?,
        PAYNOTE_TREE_DEPTH,
    )
    .map_err(|error| PayNoteError::InvalidInput(error.to_string()))
}

/// Find a deposited commitment and return its circuit index and siblings.
pub fn witness(
    tree: &PayNoteTree,
    commitment: Field,
) -> Result<(u32, [Field; PAYNOTE_TREE_DEPTH]), PayNoteError> {
    if tree.depth() != PAYNOTE_TREE_DEPTH {
        return Err(PayNoteError::InvalidInput(
            "wrong PayNote tree depth".into(),
        ));
    }
    let position = tree
        .leaves()
        .iter()
        .position(|leaf| *leaf == commitment)
        .ok_or_else(|| {
            PayNoteError::InvalidInput(
                "note commitment is not on-chain; deposit or change is not yet confirmed".into(),
            )
        })?;
    // The depth-32 circuit uses u32 positions; do not truncate generic IMT indices.
    let index = u32::try_from(position).map_err(|_| PayNoteError::TreeFull)?;
    let path = tree
        .inclusion_path(u64::from(index))
        .map_err(|error| PayNoteError::InvalidInput(error.to_string()))?;
    if path.domain != paynote_domain()
        || path
            .root(commitment)
            .map_err(|error| PayNoteError::InvalidInput(error.to_string()))?
            != tree.root()
    {
        return Err(PayNoteError::InvalidInput(
            "Merkle path root mismatch".into(),
        ));
    }
    let siblings = path
        .siblings
        .try_into()
        .map_err(|_| PayNoteError::InvalidInput("wrong PayNote path depth".into()))?;
    Ok((index, siblings))
}

/// Build the fixed-capacity merge witness from private `(amount, spend_key)`
/// pairs. All notes must belong to this tree, chain and exact asset. Amounts
/// stay in the witness; the public statement contains only their commitments'
/// canonical nullifiers. The caller must save the fresh output key durably.
pub fn merge_witness(
    tree: &PayNoteTree,
    chain_id: u64,
    asset: Address,
    inputs: &[(U256, B256)],
    output_spend_key: B256,
) -> Result<(PublicInputs, Witness), PayNoteError> {
    use crate::hash::{note_commitment, note_nullifier, note_sn};
    use crate::PayNoteSuit;
    let invalid = |message: &str| PayNoteError::InvalidInput(message.into());
    let field = |word: &B256| {
        PayNoteSuit::field_from_b256(word)
            .map_err(|_| invalid("spend key is not a canonical field"))
    };
    let word = |value: &Field| PayNoteSuit::field_to_b256(value).map_err(|_| PayNoteError::Hash);
    if !(2..=MAX_MERGE_INPUTS).contains(&inputs.len()) || asset.is_zero() {
        return Err(invalid("merge requires 2..4 inputs and a nonzero asset"));
    }
    if output_spend_key.is_zero() || inputs.iter().any(|(_, key)| *key == output_spend_key) {
        return Err(invalid("merge output key must be fresh and nonzero"));
    }
    let output_key = field(&output_spend_key)?;
    let mut witness = Witness {
        note_amounts: [U256::ZERO; MAX_MERGE_INPUTS],
        note_spend_keys: [B256::ZERO; MAX_MERGE_INPUTS],
        leaf_indices: [0; MAX_MERGE_INPUTS],
        auth_paths: [[B256::ZERO; PAYNOTE_TREE_DEPTH]; MAX_MERGE_INPUTS],
        output_spend_key,
    };
    let mut public = PublicInputs {
        chain_id,
        pool: PAYNOTE_ADDRESS,
        root: word(&tree.root())?,
        // The input count is at most the fixed four-slot proof capacity.
        input_count: u32::try_from(inputs.len()).map_err(|_| invalid("too many merge inputs"))?,
        asset,
        nullifiers: [B256::ZERO; MAX_MERGE_INPUTS],
        output_commitment: B256::ZERO,
    };
    let mut total = U256::ZERO;
    let mut commitments = Vec::with_capacity(inputs.len());
    for (i, &(amount, key_word)) in inputs.iter().enumerate() {
        if amount.is_zero() || key_word.is_zero() {
            return Err(invalid("merge input amount and key must be nonzero"));
        }
        total = total
            .checked_add(amount)
            .ok_or_else(|| invalid("merge amount overflow"))?;
        let key = field(&key_word)?;
        let serial = note_sn(key).map_err(|_| PayNoteError::Hash)?;
        let commitment =
            note_commitment(chain_id, serial, asset, amount).map_err(|_| PayNoteError::Hash)?;
        let nullifier = note_nullifier(commitment, key).map_err(|_| PayNoteError::Hash)?;
        if serial == Field::from(0)
            || commitment == Field::from(0)
            || nullifier == Field::from(0)
            || commitments.contains(&commitment)
            || public.nullifiers[..i].contains(&word(&nullifier)?)
        {
            return Err(invalid("merge input is zero or duplicated"));
        }
        commitments.push(commitment);
        let (index, path) = self::witness(tree, commitment)?;
        witness.note_amounts[i] = amount;
        witness.note_spend_keys[i] = key_word;
        witness.leaf_indices[i] = index;
        for (target, sibling) in witness.auth_paths[i].iter_mut().zip(path) {
            *target = word(&sibling)?;
        }
        public.nullifiers[i] = word(&nullifier)?;
    }
    let serial = note_sn(output_key).map_err(|_| PayNoteError::Hash)?;
    let output = note_commitment(chain_id, serial, asset, total).map_err(|_| PayNoteError::Hash)?;
    let nullifier = note_nullifier(output, output_key).map_err(|_| PayNoteError::Hash)?;
    if serial == Field::from(0)
        || output == Field::from(0)
        || nullifier == Field::from(0)
        || tree.leaves().contains(&output)
    {
        return Err(invalid("merge output is zero or already in the tree"));
    }
    public.output_commitment = word(&output)?;
    Ok((public, witness))
}
