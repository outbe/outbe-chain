//! PayNote transfer proof profile: one ordinary note in, one locked transfer
//! note and an optional change note out.
//!
//! Amounts and the lock are private. The pool checks the public deadline.
#[cfg(feature = "alloy")]
pub use crate::noir::paynote_transfer::alloy;
pub use crate::noir::paynote_transfer::{
    decode_public_inputs, encode_combined_proof, PaynoteTransfer, PublicInputs, Witness,
};
