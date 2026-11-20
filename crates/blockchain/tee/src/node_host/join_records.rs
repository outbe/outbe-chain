use super::journal_records::{
    encode_submission_fields, JournalRelayPayload, JournalSubmissionPayload,
    JournalSubmissionValidation,
};
use super::MAX_REPLACEMENT_SUBMISSION_BYTES;

use crate::TransportError;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;

use std::fmt;

const COMMITTED_JOIN_SUBMISSION_VERSION_V1: u8 = 1;

const MAX_COMMITTED_JOIN_SUBMISSION_BYTES: u64 = MAX_REPLACEMENT_SUBMISSION_BYTES + 20;

const COMMITTED_JOIN_SUBMISSION_VALIDATION: JournalSubmissionValidation =
    JournalSubmissionValidation {
        min_len: 154,
        max_bytes: MAX_COMMITTED_JOIN_SUBMISSION_BYTES,
        version: COMMITTED_JOIN_SUBMISSION_VERSION_V1,
        framing_error: "committed join submission framing is invalid",
        length_base: 154,
        length_overflow_error: "committed join submission length overflow",
        noncanonical_length_error: "committed join evidence length is non-canonical",
        encode_evidence_length_error: "committed join evidence length overflow",
        encode_allocation_error: "committed join submission allocation length overflow",
        encode_cap_error: Some("committed join submission exceeds its fixed cap"),
    };

/// Exact finalized checkpoint that allows a restarted validator to catch up
/// without trusting its stale local Registry state. This is owner-only local
/// recovery evidence. It grants no consensus membership or voting authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedJoinAdmissionAnchorV1 {
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub intent_hash: B256,
    pub finalized_height: u64,
    pub finalized_hash: B256,
    pub finalized_state_root: B256,
    pub finalized_consensus_timestamp: u64,
}

/// Exact registration material durably bound to the already committed
/// NodeHost enclave before its first registration transaction is constructed.
#[derive(Clone, PartialEq, Eq)]
pub struct CommittedJoinSubmissionV1 {
    registration_caller: Address,
    payload: JournalSubmissionPayload,
}

/// Exact signed committed-join transaction persisted before its first relay.
#[derive(Clone, PartialEq, Eq)]
pub struct CommittedJoinRelayV1 {
    payload: JournalRelayPayload,
    from_block: u64,
}

impl fmt::Debug for CommittedJoinSubmissionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommittedJoinSubmissionV1")
            .field("registration_caller", &self.registration_caller)
            .field("evidence", &self.payload.evidence)
            .field("node_signature", &self.payload.node_signature)
            .field("enclave_signature", &self.payload.enclave_signature)
            .finish()
    }
}

impl fmt::Debug for CommittedJoinRelayV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommittedJoinRelayV1")
            .field("submission_hash", &self.payload.submission_hash)
            .field("calldata_hash", &self.payload.calldata_hash)
            .field("transaction_hash", &self.payload.transaction_hash)
            .field("from_block", &self.from_block)
            .field("raw_transaction", &self.payload.raw_transaction)
            .finish()
    }
}

impl CommittedJoinSubmissionV1 {
    #[cfg(test)]
    pub(super) fn new(
        registration_caller: Address,
        evidence: Vec<u8>,
        node_signature: [u8; 65],
        enclave_signature: [u8; 64],
    ) -> Self {
        Self {
            registration_caller,
            payload: JournalSubmissionPayload::new(evidence, node_signature, enclave_signature),
        }
    }

    #[must_use]
    pub const fn registration_caller(&self) -> Address {
        self.registration_caller
    }

    #[must_use]
    pub fn evidence(&self) -> &[u8] {
        &self.payload.evidence
    }

    #[must_use]
    pub const fn node_signature(&self) -> &[u8; 65] {
        &self.payload.node_signature
    }

    #[must_use]
    pub const fn enclave_signature(&self) -> &[u8; 64] {
        &self.payload.enclave_signature
    }

    pub fn submission_hash(&self) -> Result<B256, TransportError> {
        Ok(keccak256(self.encode_canonical()?))
    }

    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        if self.registration_caller.is_zero() {
            return Err(TransportError::Codec(
                "committed join registration caller is zero".into(),
            ));
        }
        encode_submission_fields(
            self.registration_caller.as_slice(),
            &self.payload,
            &COMMITTED_JOIN_SUBMISSION_VALIDATION,
        )
    }
}

impl CommittedJoinRelayV1 {
    #[must_use]
    pub const fn calldata_hash(&self) -> B256 {
        self.payload.calldata_hash
    }

    #[must_use]
    pub const fn transaction_hash(&self) -> B256 {
        self.payload.transaction_hash
    }

    #[must_use]
    pub const fn from_block(&self) -> u64 {
        self.from_block
    }

    #[must_use]
    pub fn raw_transaction(&self) -> &[u8] {
        &self.payload.raw_transaction
    }
}
