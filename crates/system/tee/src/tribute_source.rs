//! Bounded source openings passed as arguments to enclave computations.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    storage::{
        proof::{AccountStorageProof, StateProofLimits},
        readonly::StorageReader,
    },
    time::WorldwideDay,
    tribute_encryption::EncryptedTributeV2,
    wwd_entity_id::derive_poseidon_entity_id,
};
use serde::{Deserialize, Serialize};

use crate::TransportError;

pub fn proof_limits() -> StateProofLimits {
    StateProofLimits {
        max_slots: 32,
        max_nodes: 33 * crate::finalized_admission::MAX_MPT_PROOF_NODES,
        max_bytes: 2 * 1024 * 1024,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeSourceCheckpoint {
    pub block_number: u64,
    pub block_hash: B256,
    pub state_root: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeSourceSubject {
    pub checkpoint: TributeSourceCheckpoint,
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub tribute_count: u32,
}

/// Expected words come from the node's module-owned DSL, before enclave acceptance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeStateSource {
    pub tribute: EncryptedTributeV2,
    pub day_index: u32,
    pub expected_words: Vec<(B256, U256)>,
    pub proof: AccountStorageProof,
}

impl TributeStateSource {
    pub fn binding(&self) -> Result<B256, TransportError> {
        source_binding(&self.tribute, self.day_index, &self.expected_words)
    }

    /// The node supplies the expected binding independently during acceptance.
    pub fn verify(
        &self,
        subject: &TributeSourceSubject,
        expected_binding: B256,
        limits: StateProofLimits,
    ) -> Result<(), TransportError> {
        if !self.context_matches(subject)? {
            return Err(rejected("Tribute source context mismatch"));
        }
        if !self.slots_match(limits.max_slots) {
            return Err(rejected("Tribute source slot set mismatch"));
        }
        if self.binding()? != expected_binding {
            return Err(rejected("Tribute source does not match node authority"));
        }
        let verified = self
            .proof
            .verify(TRIBUTE_ADDRESS, subject.checkpoint.state_root, limits)
            .map_err(rejected)?;
        for (slot, expected) in &self.expected_words {
            if verified
                .read_storage(TRIBUTE_ADDRESS, *slot)
                .map_err(rejected)?
                != *expected
            {
                return Err(rejected("Tribute source word mismatch"));
            }
        }
        Ok(())
    }

    fn context_matches(&self, subject: &TributeSourceSubject) -> Result<bool, TransportError> {
        let checkpoint_valid = subject.chain_id != 0
            && subject.checkpoint.block_number != 0
            && !subject.checkpoint.block_hash.is_zero()
            && !subject.checkpoint.state_root.is_zero();
        let context = &self.tribute.context;
        if !checkpoint_valid
            || self.day_index >= subject.tribute_count
            || !self.tribute.has_valid_encoding()
        {
            return Ok(false);
        }
        let identity_matches = context.chain_id == subject.chain_id
            && context.worldwide_day == subject.worldwide_day
            && !context.owner.is_zero();
        Ok(identity_matches
            && derive_poseidon_entity_id(context.owner, context.worldwide_day).map_err(rejected)?
                == context.tribute_id)
    }

    fn slots_match(&self, max_slots: usize) -> bool {
        let count_matches = !self.expected_words.is_empty()
            && self.expected_words.len() <= max_slots
            && self.expected_words.len() == self.proof.slots.len();
        count_matches
            && !self
                .expected_words
                .windows(2)
                .any(|words| words[0].0 >= words[1].0)
            && !self
                .expected_words
                .iter()
                .zip(&self.proof.slots)
                .any(|((slot, _), opening)| *slot != opening.slot)
    }
}

pub fn source_binding(
    tribute: &EncryptedTributeV2,
    day_index: u32,
    expected_words: &[(B256, U256)],
) -> Result<B256, TransportError> {
    crate::response_auth::hash(
        b"outbe/tribute/state-source-binding",
        &(tribute, day_index, expected_words),
    )
    .map_err(TransportError::codec)
}

fn rejected(error: impl std::fmt::Display) -> TransportError {
    TransportError::TributeSourceRejected(error.to_string())
}
