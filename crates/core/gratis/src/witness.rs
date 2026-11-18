//! Witness schema for a recorded Gratis operation.
//!
//! Tag 1 is `GratisOpApplied`, including a signed business rejection.
//! NOD mine uses tags 9 and 8 on the types in [`crate::nod_mine`].

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::{WitnessResponseTag, WitnessSchema};

use crate::protocol::GratisOpResult;

impl WitnessSchema for GratisOpResult {
    const TAG: WitnessResponseTag = WitnessResponseTag::GratisOpApplied;

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.attestation_tag = tag;
    }
}
