use super::codec_error;
use super::read_owned_bounded_file;
use super::validate_finalized_join_admission_anchor;
use super::MAX_REPLACEMENT_SUBMISSION_BYTES;

use crate::TransportError;
use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;

use outbe_primitives::tee_attestation_v1::MAX_ATTESTATION_EVIDENCE_BYTES;

use std::path::Path;

const COMMITTED_JOIN_SUBMISSION_VERSION_V1: u8 = 1;

const MAX_COMMITTED_JOIN_SUBMISSION_BYTES: u64 = MAX_REPLACEMENT_SUBMISSION_BYTES + 20;

const COMMITTED_JOIN_RELAY_VERSION_V1: u8 = 1;

const MAX_COMMITTED_JOIN_RELAY_BYTES: u64 = MAX_ATTESTATION_EVIDENCE_BYTES as u64 + 4_104;

const FINALIZED_JOIN_ADMISSION_ANCHOR_VERSION_V1: u8 = 1;

const FINALIZED_JOIN_ADMISSION_ANCHOR_BYTES: u64 = 1 + (7 * 32) + 8 + 8;

/// Exact finalized checkpoint that allows a restarted validator to catch up
/// without trusting its stale local Registry state. This is owner-only local
/// recovery evidence; it grants no consensus membership or voting authority.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedJoinSubmissionV1 {
    pub(super) registration_caller: Address,
    pub(super) evidence: Vec<u8>,
    pub(super) node_signature: [u8; 65],
    pub(super) enclave_signature: [u8; 64],
}

/// Exact signed committed-join transaction persisted before its first relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedJoinRelayV1 {
    pub(super) submission_hash: B256,
    pub(super) calldata_hash: B256,
    pub(super) transaction_hash: B256,
    pub(super) from_block: u64,
    pub(super) raw_transaction: Vec<u8>,
}

impl CommittedJoinSubmissionV1 {
    #[must_use]
    pub const fn registration_caller(&self) -> Address {
        self.registration_caller
    }

    #[must_use]
    pub fn evidence(&self) -> &[u8] {
        &self.evidence
    }

    #[must_use]
    pub const fn node_signature(&self) -> &[u8; 65] {
        &self.node_signature
    }

    #[must_use]
    pub const fn enclave_signature(&self) -> &[u8; 64] {
        &self.enclave_signature
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
        let evidence_len = u32::try_from(self.evidence.len())
            .map_err(|_| TransportError::Codec("committed join evidence length overflow".into()))?;
        let capacity = 154_usize.checked_add(self.evidence.len()).ok_or_else(|| {
            TransportError::Codec("committed join submission allocation length overflow".into())
        })?;
        let mut out = Vec::with_capacity(capacity);
        out.push(COMMITTED_JOIN_SUBMISSION_VERSION_V1);
        out.extend_from_slice(self.registration_caller.as_slice());
        out.extend_from_slice(&evidence_len.to_be_bytes());
        out.extend_from_slice(&self.evidence);
        out.extend_from_slice(&self.node_signature);
        out.extend_from_slice(&self.enclave_signature);
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > MAX_COMMITTED_JOIN_SUBMISSION_BYTES {
            return Err(TransportError::Codec(
                "committed join submission exceeds its fixed cap".into(),
            ));
        }
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 154
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_COMMITTED_JOIN_SUBMISSION_BYTES
            || input[0] != COMMITTED_JOIN_SUBMISSION_VERSION_V1
        {
            return Err(TransportError::Codec(
                "committed join submission framing is invalid".into(),
            ));
        }
        let evidence_len =
            usize::try_from(u32::from_be_bytes(input[21..25].try_into().map_err(
                |_| TransportError::Codec("committed join evidence length".into()),
            )?))
            .map_err(|_| TransportError::Codec("committed join evidence length overflow".into()))?;
        let expected_len = 154_usize.checked_add(evidence_len).ok_or_else(|| {
            TransportError::Codec("committed join submission length overflow".into())
        })?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES || input.len() != expected_len {
            return Err(TransportError::Codec(
                "committed join evidence length is non-canonical".into(),
            ));
        }
        let registration_caller = Address::from_slice(&input[1..21]);
        if registration_caller.is_zero() {
            return Err(TransportError::Codec(
                "committed join registration caller is zero".into(),
            ));
        }
        let evidence_end = 25 + evidence_len;
        let evidence = input[25..evidence_end].to_vec();
        AttestationEvidenceV1::decode_canonical(&evidence).map_err(codec_error)?;
        let node_signature = input[evidence_end..evidence_end + 65]
            .try_into()
            .map_err(|_| TransportError::Codec("committed join node signature length".into()))?;
        let enclave_signature = input[evidence_end + 65..]
            .try_into()
            .map_err(|_| TransportError::Codec("committed join enclave signature length".into()))?;
        Ok(Self {
            registration_caller,
            evidence,
            node_signature,
            enclave_signature,
        })
    }
}

impl CommittedJoinRelayV1 {
    #[must_use]
    pub const fn calldata_hash(&self) -> B256 {
        self.calldata_hash
    }

    #[must_use]
    pub const fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    #[must_use]
    pub const fn from_block(&self) -> u64 {
        self.from_block
    }

    #[must_use]
    pub fn raw_transaction(&self) -> &[u8] {
        &self.raw_transaction
    }

    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        let raw_len = u32::try_from(self.raw_transaction.len())
            .map_err(|_| TransportError::Codec("committed join relay length overflow".into()))?;
        let mut out = Vec::with_capacity(109 + self.raw_transaction.len());
        out.push(COMMITTED_JOIN_RELAY_VERSION_V1);
        out.extend_from_slice(self.submission_hash.as_slice());
        out.extend_from_slice(self.calldata_hash.as_slice());
        out.extend_from_slice(self.transaction_hash.as_slice());
        out.extend_from_slice(&self.from_block.to_be_bytes());
        out.extend_from_slice(&raw_len.to_be_bytes());
        out.extend_from_slice(&self.raw_transaction);
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > MAX_COMMITTED_JOIN_RELAY_BYTES {
            return Err(TransportError::Codec(
                "committed join relay exceeds its fixed cap".into(),
            ));
        }
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 109
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_COMMITTED_JOIN_RELAY_BYTES
            || input[0] != COMMITTED_JOIN_RELAY_VERSION_V1
        {
            return Err(TransportError::Codec(
                "committed join relay framing is invalid".into(),
            ));
        }
        let raw_len = u32::from_be_bytes(
            input[105..109]
                .try_into()
                .map_err(|_| TransportError::Codec("committed join relay length".into()))?,
        ) as usize;
        if input.len() != 109 + raw_len || raw_len == 0 {
            return Err(TransportError::Codec(
                "committed join relay raw transaction length is invalid".into(),
            ));
        }
        let relay = Self {
            submission_hash: B256::from_slice(&input[1..33]),
            calldata_hash: B256::from_slice(&input[33..65]),
            transaction_hash: B256::from_slice(&input[65..97]),
            from_block: u64::from_be_bytes(
                input[97..105]
                    .try_into()
                    .map_err(|_| TransportError::Codec("committed join from_block".into()))?,
            ),
            raw_transaction: input[109..].to_vec(),
        };
        if relay.submission_hash.is_zero()
            || relay.calldata_hash.is_zero()
            || relay.transaction_hash != keccak256(&relay.raw_transaction)
        {
            return Err(TransportError::Codec(
                "committed join relay commitments are invalid".into(),
            ));
        }
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
