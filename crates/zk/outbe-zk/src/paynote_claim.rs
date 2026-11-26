//! PayNote claim proof profile: one locked transfer note in, one ordinary note
//! out. Serves the recipient's claim and decline and the sender's reclaim.
#[cfg(feature = "alloy")]
pub use crate::noir::paynote_claim::alloy;
pub use crate::noir::paynote_claim::{
    decode_public_inputs, encode_combined_proof, PaynoteClaim, PublicInputs, Witness,
};
