//! Off-chain membership witnesses for the PayNote commitment tree.
//!
//! Feed ordered `NewNote` commitments into the shared [`Imt`]. Callers validate
//! event indexes and compare its root with the pool before using a witness.

use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::OutbeV1;

use crate::{
    errors::PayNoteError,
    hash::{empty_leaf, paynote_domain, Field},
    schema::PAYNOTE_TREE_DEPTH,
};

/// Start a PayNote tree with the chain-specific empty leaf.
pub fn new_tree(chain_id: u64) -> Result<Imt<OutbeV1>, PayNoteError> {
    Imt::new(paynote_domain(), empty_leaf(chain_id)?, PAYNOTE_TREE_DEPTH)
        .map_err(|error| PayNoteError::InvalidInput(error.to_string()))
}

/// Find a deposited commitment and return its circuit index and siblings.
pub fn witness(
    tree: &Imt<OutbeV1>,
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
