//! Bounded source openings passed as arguments to enclave computations.

use alloy_primitives::{B256, U256};
use outbe_core_common::{
    tribute_encryption::EncryptedTributeV2, wwd_entity_id::derive_poseidon_entity_id,
};
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    storage::{
        proof::{AccountStorageProof, StateProofLimits},
        readonly::StorageReader,
    },
    time::WorldwideDay,
};
use outbe_tee::TransportError;
use serde::{Deserialize, Serialize};

use super::{
    errors::{EnclaveClientError, TributeSourceError},
    response_auth,
};

/// Maximum MPT proof dimensions permitted for a single [`TributeStateSource`].
#[must_use]
pub fn proof_limits() -> StateProofLimits {
    StateProofLimits {
        max_slots: 32,
        max_nodes: 33 * outbe_tee::finalized_admission::MAX_MPT_PROOF_NODES,
        max_bytes: 2 * 1024 * 1024,
    }
}

/// Finalized block checkpoint anchoring a [`TributeStateSource`] proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeSourceCheckpoint {
    /// Finalized block height.
    pub block_number: u64,
    /// Finalized block hash.
    pub block_hash: B256,
    /// Execution state root of `block_hash`.
    pub state_root: B256,
}

/// Subject metadata binding a [`TributeStateSource`] to a finalized day snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeSourceSubject {
    /// Finalized checkpoint anchoring the MPT proof.
    pub checkpoint: TributeSourceCheckpoint,
    /// Expected chain identifier.
    pub chain_id: u64,
    /// Expected UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// Total Tribute count committed for `worldwide_day`.
    pub tribute_count: u32,
}

/// Expected words come from the node's module-owned DSL, before enclave acceptance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeStateSource {
    /// Encrypted Tribute record stored on chain.
    pub tribute: EncryptedTributeV2,
    /// Zero-based index of `tribute` within `worldwide_day`.
    pub day_index: u32,
    /// Sorted `(slot, word)` pairs encoding `tribute` in `TRIBUTE_ADDRESS` storage.
    pub expected_words: Vec<(B256, U256)>,
    /// MPT account and storage proof against `checkpoint.state_root`.
    pub proof: AccountStorageProof,
}

impl TributeStateSource {
    /// Compute the canonical binding hash over `(tribute, day_index, expected_words)`.
    pub fn binding(&self) -> Result<B256, TransportError> {
        source_binding(&self.tribute, self.day_index, &self.expected_words)
    }

    /// Verify this source opening returning a structured [`EnclaveClientError`].
    pub fn verify_typed(
        &self,
        subject: &TributeSourceSubject,
        expected_binding: B256,
        limits: StateProofLimits,
    ) -> Result<(), EnclaveClientError> {
        if !self.context_matches(subject) {
            return Err(TributeSourceError::ContextMismatch.into());
        }
        if !self.slots_match(limits.max_slots) {
            return Err(TributeSourceError::SlotSetMismatch.into());
        }
        if self.binding()? != expected_binding {
            return Err(TributeSourceError::AuthorityMismatch.into());
        }
        let verified = self
            .proof
            .verify(TRIBUTE_ADDRESS, subject.checkpoint.state_root, limits)
            .map_err(TributeSourceError::from)?;
        for (slot, expected) in &self.expected_words {
            if verified
                .read_storage(TRIBUTE_ADDRESS, *slot)
                .map_err(|_| TributeSourceError::SlotSetMismatch)?
                != *expected
            {
                return Err(TributeSourceError::WordMismatch.into());
            }
        }
        Ok(())
    }

    /// The node supplies the expected binding independently during acceptance.
    pub fn verify(
        &self,
        subject: &TributeSourceSubject,
        expected_binding: B256,
        limits: StateProofLimits,
    ) -> Result<(), TransportError> {
        self.verify_typed(subject, expected_binding, limits)
            .map_err(TransportError::from)
    }

    fn context_matches(&self, subject: &TributeSourceSubject) -> bool {
        let checkpoint_valid = subject.chain_id != 0
            && subject.checkpoint.block_number != 0
            && !subject.checkpoint.block_hash.is_zero()
            && !subject.checkpoint.state_root.is_zero();
        let context = &self.tribute.context;
        if !checkpoint_valid
            || self.day_index >= subject.tribute_count
            || !self.tribute.has_valid_encoding()
        {
            return false;
        }
        let identity_matches = context.chain_id == subject.chain_id
            && context.worldwide_day == subject.worldwide_day
            && !context.owner.is_zero();
        identity_matches
            && derive_poseidon_entity_id(context.owner, context.worldwide_day) == context.tribute_id
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

/// Compute the canonical authority binding hash for a Tribute storage opening.
pub fn source_binding(
    tribute: &EncryptedTributeV2,
    day_index: u32,
    expected_words: &[(B256, U256)],
) -> Result<B256, TransportError> {
    response_auth::hash(
        b"outbe/tribute/state-source-binding",
        &(tribute, day_index, expected_words),
    )
    .map_err(TransportError::codec)
}
