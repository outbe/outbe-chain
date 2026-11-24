//! Atomic same-asset PayNote merge proof profile.
//!
//! Amounts are private; conservation is checked in the circuit. Nullifiers use
//! the ordinary PayNote formula and must share settlement's spent state.
#[cfg(feature = "alloy")]
pub use crate::noir::paynote_merge::alloy;
pub use crate::noir::paynote_merge::{
    decode_public_inputs, encode_combined_proof, PaynoteMerge, PublicInputs, Witness, COMBINED_LEN,
    PROOF_WORDS, PUBLIC_INPUT_COUNT,
};

/// Fixed proof capacity; inputs occupy a prefix and remaining slots are zero.
pub const MAX_MERGE_INPUTS: usize = 4;
