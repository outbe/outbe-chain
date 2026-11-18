//! Witness schemas for Promis and the Promis-to-Gratis mine.
//!
//! Tag 2 is `PromisOpApplied`, including a signed business rejection.
//! Tag 22 is `GratisMinedFromPromisV1`. The mine body is a postcard-transparent
//! wrapper of the `outbe_gratis` result, so the frozen bytes stay one schema.

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::{WitnessResponseTag, WitnessSchema};

use crate::protocol::{MineGratisFromPromisResultV1, PromisOpResult};

impl WitnessSchema for PromisOpResult {
    const TAG: WitnessResponseTag = WitnessResponseTag::PromisOpApplied;

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

/// Tag 22 body. Postcard treats this newtype as the inner result.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct MineGratisFromPromisBody(pub MineGratisFromPromisResultV1);

impl WitnessSchema for MineGratisFromPromisBody {
    const TAG: WitnessResponseTag = WitnessResponseTag::GratisMinedFromPromisV1;

    fn inputs_canonical_hash(&self) -> B256 {
        self.0.inputs_canonical_hash
    }

    fn attestation_tag(&self) -> &[u8] {
        &self.0.attestation_tag
    }

    fn set_attestation_tag(&mut self, tag: Vec<u8>) {
        self.0.attestation_tag = tag;
    }
}
