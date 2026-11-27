//! Off-chain membership witnesses for the PayNote commitment tree.
//!
//! Feed ordered `NewNote` commitments into the shared [`PayNoteTree`]. Callers validate
//! event indexes and compare its root with the pool before using a witness.

use alloy_primitives::{Address, B256, U256};
use outbe_system_common::addresses::PAYNOTE_ADDRESS;
use outbe_zk::codec::{field_from_b256, field_to_b256};
use outbe_zk::paynote_merge::{
    alloy::{PublicInputs, Witness},
    MAX_MERGE_INPUTS,
};
use outbe_zk::primitive::curve::{coords, Affine};

use crate::Field;
use crate::{
    errors::PayNoteError,
    hash::{empty_leaf, paynote_domain},
    schema::PAYNOTE_TREE_DEPTH,
    PayNoteTree,
};

/// What a recipient publishes in place of an address, in the style of an
/// ERC-5564 stealth meta-address:
/// `st:outbe:0x<chain id, 8 bytes><recipient key><view key x><view key y>`.
///
/// It names no account, so a payer learns nothing about the addresses the
/// recipient uses. The payer locks a note to `recipient_pk` and encrypts the
/// note details to `view_key`. They send the details off-chain.
///
/// TODO: publish the note details on-chain, so that the two parties need no
/// channel of their own. The design follows the ERC-5564 announcement:
/// - The payer draws an ephemeral Grumpkin key `e`, publishes `E = e·G` and
///   computes the shared secret `s = x(e·view_key)`.
/// - The view tag is the first byte of a hash of `s`. A recipient skips every
///   announcement with another tag without decrypting it.
/// - The memo is ChaCha20-Poly1305 over `(amount, deadline, sender serial,
///   rho)`, about 150 bytes, under a key derived from `s` and the locked
///   commitment.
/// - A `transfer(proof, ephemeralPubKey, metadata)` overload makes the pool
///   emit the ERC-5564 `Announcement` event under a PayNote scheme id, with
///   the view tag as the first metadata byte.
/// - A new version of the transfer circuit binds the hash of the memo as a
///   public input. Otherwise anyone could resubmit the proof with another memo.
/// - The recipient opens the memo, recomputes the locked commitment and checks
///   `hasCommitment` before they trust it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverBundle {
    /// The chain the recipient accepts notes on.
    pub chain_id: u64,
    /// The `note_sn` of a key the recipient holds. A transfer lock names it.
    pub recipient_pk: Field,
    /// Grumpkin public key that the note details are encrypted to.
    pub view_key: Affine,
}

const BUNDLE_PREFIX: &str = "st:outbe:0x";

impl ReceiverBundle {
    /// Read a bundle that a recipient published for `chain_id`.
    pub fn parse(text: &str, chain_id: u64) -> Result<Self, PayNoteError> {
        let invalid = |message: &str| PayNoteError::InvalidInput(message.into());
        let hex = text
            .strip_prefix(BUNDLE_PREFIX)
            .ok_or_else(|| invalid("receiver bundle must start with st:outbe:0x"))?;
        let bytes: [u8; 104] = alloy_primitives::hex::decode(hex)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("receiver bundle must hold 104 hex-encoded bytes"))?;
        let (chain, words) = bytes.split_at(8);
        if chain != chain_id.to_be_bytes() {
            return Err(invalid("receiver bundle is for another chain"));
        }
        let word = |index: usize| {
            field_from_b256(&B256::from_slice(&words[index * 32..][..32]))
                .map_err(|_| invalid("receiver bundle holds a non-canonical field"))
        };
        let (recipient_pk, view_key) = (word(0)?, Affine::new_unchecked(word(1)?, word(2)?));
        if recipient_pk == Field::from(0) {
            return Err(invalid("receiver bundle has a zero recipient key"));
        }
        // Grumpkin has cofactor one, so a point on the curve is in the group.
        if !view_key.is_on_curve() {
            return Err(invalid("receiver bundle view key is not on the curve"));
        }
        Ok(Self {
            chain_id,
            recipient_pk,
            view_key,
        })
    }
}

impl core::fmt::Display for ReceiverBundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (x, y) = coords(&self.view_key).map_err(|_| core::fmt::Error)?;
        write!(f, "{BUNDLE_PREFIX}{:016x}", self.chain_id)?;
        for field in [self.recipient_pk, x, y] {
            let word = field_to_b256(&field).map_err(|_| core::fmt::Error)?;
            write!(f, "{}", alloy_primitives::hex::encode(word))?;
        }
        Ok(())
    }
}

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
    // The depth-32 circuit uses u32 positions. Do not truncate generic IMT indices.
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
/// stay in the witness. The public statement contains only their commitments'
/// canonical nullifiers. The caller must save the fresh output key durably.
pub fn merge_witness(
    tree: &PayNoteTree,
    chain_id: u64,
    asset: Address,
    inputs: &[(U256, B256)],
    output_spend_key: B256,
) -> Result<(PublicInputs, Witness), PayNoteError> {
    use crate::hash::{note_commitment, note_nullifier, note_sn};
    let invalid = |message: &str| PayNoteError::InvalidInput(message.into());
    let field = |word: &B256| {
        field_from_b256(word).map_err(|_| invalid("spend key is not a canonical field"))
    };
    let word = |value: &Field| field_to_b256(value).map_err(|_| PayNoteError::Hash);
    if !(2..=MAX_MERGE_INPUTS).contains(&inputs.len()) {
        return Err(invalid("merge requires 2..4 inputs"));
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
