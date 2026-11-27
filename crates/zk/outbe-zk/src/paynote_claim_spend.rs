//! PayNote claim-spend proof profile: one locked transfer note in, a spend to
//! a public context and an optional ordinary change note out.
#[cfg(feature = "alloy")]
pub use crate::noir::paynote_claim_spend::alloy;
pub use crate::noir::paynote_claim_spend::{
    decode_public_inputs, encode_combined_proof, PaynoteClaimSpend, PublicInputs, Witness,
};
