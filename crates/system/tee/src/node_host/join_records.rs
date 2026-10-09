use super::journal_records::{
    decode_relay_fields, decode_submission_payload, encode_relay_fields, encode_submission_fields,
    validate_submission_evidence_length, validate_submission_frame, JournalRelayPayload,
    JournalRelayValidation, JournalSubmissionErrors, JournalSubmissionPayload,
    JournalSubmissionValidation, RelayMaterial,
};
use super::read_owned_bounded_file;
use super::validate_finalized_join_admission_anchor;
use super::MAX_REPLACEMENT_SUBMISSION_BYTES;

use crate::TransportError;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::MAX_ATTESTATION_EVIDENCE_BYTES;

use std::fmt;
use std::path::Path;

const COMMITTED_JOIN_SUBMISSION_VERSION_V1: u8 = 1;

const MAX_COMMITTED_JOIN_SUBMISSION_BYTES: u64 = MAX_REPLACEMENT_SUBMISSION_BYTES + 20;

const COMMITTED_JOIN_RELAY_VERSION_V1: u8 = 1;

const MAX_COMMITTED_JOIN_RELAY_BYTES: u64 = MAX_ATTESTATION_EVIDENCE_BYTES as u64 + 4_104;

const FINALIZED_JOIN_ADMISSION_ANCHOR_VERSION_V1: u8 = 1;

const FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES: u64 = 1 + (7 * 32) + 8 + 8;

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

const COMMITTED_JOIN_RELAY_VALIDATION: JournalRelayValidation = JournalRelayValidation {
    header_len: 109,
    max_bytes: MAX_COMMITTED_JOIN_RELAY_BYTES,
    version: COMMITTED_JOIN_RELAY_VERSION_V1,
    encode_length_overflow_error: "committed join relay length overflow",
    encode_cap_error: "committed join relay exceeds its fixed cap",
    raw_len_offset: 105,
    raw_len_error: "committed join relay length",
    from_block_offset: Some(97),
    from_block_error: "committed join from_block",
    framing_error: "committed join relay framing is invalid",
    raw_length_error: "committed join relay raw transaction length is invalid",
    commitments_error: "committed join relay commitments are invalid",
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

impl FinalizedJoinAdmissionAnchorV1 {
    pub(super) fn encode_canonical(self) -> [u8; FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES as usize] {
        let mut out = [0_u8; FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES as usize];
        out[0] = FINALIZED_JOIN_ADMISSION_ANCHOR_VERSION_V1;
        out[1..33].copy_from_slice(&self.chain_id);
        out[33..65].copy_from_slice(self.genesis_hash.as_slice());
        out[65..97].copy_from_slice(self.node_id_hash.as_slice());
        out[97..129].copy_from_slice(self.enclave_id.as_slice());
        out[129..161].copy_from_slice(self.intent_hash.as_slice());
        out[161..169].copy_from_slice(&self.finalized_height.to_be_bytes());
        out[169..201].copy_from_slice(self.finalized_hash.as_slice());
        out[201..233].copy_from_slice(self.finalized_state_root.as_slice());
        out[233..241].copy_from_slice(&self.finalized_consensus_timestamp.to_be_bytes());
        out
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() != FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES as usize
            || input[0] != FINALIZED_JOIN_ADMISSION_ANCHOR_VERSION_V1
        {
            return Err(TransportError::Codec(
                "finalized join admission anchor framing is invalid".into(),
            ));
        }
        let anchor = Self {
            chain_id: input[1..33]
                .try_into()
                .map_err(|_| TransportError::Codec("finalized join chain id".into()))?,
            genesis_hash: B256::from_slice(&input[33..65]),
            node_id_hash: B256::from_slice(&input[65..97]),
            enclave_id: B256::from_slice(&input[97..129]),
            intent_hash: B256::from_slice(&input[129..161]),
            finalized_height: u64::from_be_bytes(
                input[161..169]
                    .try_into()
                    .map_err(|_| TransportError::Codec("finalized join height".into()))?,
            ),
            finalized_hash: B256::from_slice(&input[169..201]),
            finalized_state_root: B256::from_slice(&input[201..233]),
            finalized_consensus_timestamp: u64::from_be_bytes(
                input[233..241]
                    .try_into()
                    .map_err(|_| TransportError::Codec("finalized join timestamp".into()))?,
            ),
        };
        validate_finalized_join_admission_anchor(anchor)?;
        Ok(anchor)
    }
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

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        validate_submission_frame(input, &COMMITTED_JOIN_SUBMISSION_VALIDATION)?;
        let evidence_len =
            usize::try_from(u32::from_be_bytes(input[21..25].try_into().map_err(
                |_| TransportError::Codec("committed join evidence length".into()),
            )?))
            .map_err(|_| TransportError::Codec("committed join evidence length overflow".into()))?;
        validate_submission_evidence_length(
            input.len(),
            evidence_len,
            &COMMITTED_JOIN_SUBMISSION_VALIDATION,
        )?;
        let registration_caller = Address::from_slice(&input[1..21]);
        if registration_caller.is_zero() {
            return Err(TransportError::Codec(
                "committed join registration caller is zero".into(),
            ));
        }
        let payload = decode_submission_payload(
            input,
            25,
            evidence_len,
            JournalSubmissionErrors {
                node_signature: "committed join node signature length",
                enclave_signature: "committed join enclave signature length",
            },
        )?;
        Ok(Self {
            registration_caller,
            payload,
        })
    }
}

impl CommittedJoinRelayV1 {
    pub(super) fn new(submission_hash: B256, from_block: u64, material: RelayMaterial) -> Self {
        Self {
            payload: JournalRelayPayload::new(submission_hash, material),
            from_block,
        }
    }

    pub(super) fn submission_hash(&self) -> B256 {
        self.payload.submission_hash
    }

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

    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        encode_relay_fields(
            &self.payload,
            Some(self.from_block),
            &COMMITTED_JOIN_RELAY_VALIDATION,
        )
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        let fields = decode_relay_fields(input, &COMMITTED_JOIN_RELAY_VALIDATION)?;
        let relay = Self {
            payload: fields.payload,
            from_block: fields.from_block,
        };
        Ok(relay)
    }
}

pub(super) fn read_committed_join_submission(
    path: &Path,
) -> Result<CommittedJoinSubmissionV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        MAX_COMMITTED_JOIN_SUBMISSION_BYTES,
        "committed join submission",
    )?;
    CommittedJoinSubmissionV1::decode_canonical(&bytes)
}

pub(super) fn read_committed_join_relay(
    path: &Path,
) -> Result<CommittedJoinRelayV1, TransportError> {
    let bytes =
        read_owned_bounded_file(path, MAX_COMMITTED_JOIN_RELAY_BYTES, "committed join relay")?;
    CommittedJoinRelayV1::decode_canonical(&bytes)
}

pub(super) fn read_finalized_join_admission_anchor(
    path: &Path,
) -> Result<FinalizedJoinAdmissionAnchorV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES,
        "finalized join admission anchor",
    )?;
    FinalizedJoinAdmissionAnchorV1::decode_canonical(&bytes)
}
