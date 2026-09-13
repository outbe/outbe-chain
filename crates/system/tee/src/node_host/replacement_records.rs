use super::codec_error;
use super::read_owned_bounded_file;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;
use crate::remote_session::FinalizedRegistryViewV1;

use crate::TransportError;
use alloy_primitives::keccak256;

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::MAX_ATTESTATION_EVIDENCE_BYTES;

use std::path::Path;

const REPLACEMENT_CANDIDATE_VERSION_V1: u8 = 1;

const MAX_REPLACEMENT_CANDIDATE_BYTES: u64 = 1 + 32 + 2 + MAX_INITIALIZATION_MANIFEST_BYTES;

const REPLACEMENT_SUBMISSION_VERSION_V1: u8 = 1;

pub(super) const MAX_REPLACEMENT_SUBMISSION_BYTES: u64 =
    1 + 4 + MAX_ATTESTATION_EVIDENCE_BYTES as u64 + 65 + 64;

const REPLACEMENT_RELAY_VERSION_V1: u8 = 1;

const MAX_REPLACEMENT_RELAY_BYTES: u64 = MAX_ATTESTATION_EVIDENCE_BYTES as u64 + 4_096;

const REPLACEMENT_PROMOTION_VERSION_V1: u8 = 1;

const REPLACEMENT_PROMOTION_BYTES: u64 = 1 + 32 + 32;

/// Exact durable transaction material returned for relay retries. The evidence
/// already contains the canonical replacement intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementCandidateSubmissionV1 {
    pub(super) evidence: Vec<u8>,
    pub(super) node_signature: [u8; 65],
    pub(super) enclave_signature: [u8; 64],
}

/// Exact signed registration transaction persisted before relay. It is bound
/// to the durable candidate submission so a restart cannot attach another
/// transaction to already-quoted evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacementCandidateRelayV1 {
    pub(super) submission_hash: B256,
    pub(super) calldata_hash: B256,
    pub(super) transaction_hash: B256,
    pub(super) raw_transaction: Vec<u8>,
}

/// Opaque authority issued only after I6 verifies an exact finalized registry
/// binding. I5 defines and consumes the capability but has no production
/// constructor for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedReplacementAuthorizationV1 {
    pub(super) intent_hash: B256,
    pub(super) candidate_manifest_hash: B256,
}

/// Exact replacement binding authenticated at one consensus-finalized Registry
/// state. Constructing the opaque promotion capability additionally proves
/// that these fields match the durable candidate and evidence byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedReplacementBindingV1 {
    pub view: FinalizedRegistryViewV1,
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub intent_hash: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub valid_until: u64,
    pub recipient_x25519: [u8; 32],
    pub attestation_ed25519: [u8; 32],
    pub noise_responder_x25519: [u8; 32],
    pub node_host_authorization_hash: B256,
}

impl ReplacementCandidateSubmissionV1 {
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
        let evidence_len = u32::try_from(self.evidence.len()).map_err(|_| {
            TransportError::Codec("replacement submission evidence length overflow".into())
        })?;
        let capacity = 134_usize.checked_add(self.evidence.len()).ok_or_else(|| {
            TransportError::Codec("replacement submission allocation length overflow".into())
        })?;
        let mut out = Vec::with_capacity(capacity);
        out.push(REPLACEMENT_SUBMISSION_VERSION_V1);
        out.extend_from_slice(&evidence_len.to_be_bytes());
        out.extend_from_slice(&self.evidence);
        out.extend_from_slice(&self.node_signature);
        out.extend_from_slice(&self.enclave_signature);
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 134
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_SUBMISSION_BYTES
            || input[0] != REPLACEMENT_SUBMISSION_VERSION_V1
        {
            return Err(TransportError::Codec(
                "replacement submission framing is invalid".into(),
            ));
        }
        let evidence_len =
            usize::try_from(u32::from_be_bytes([input[1], input[2], input[3], input[4]])).map_err(
                |_| TransportError::Codec("replacement evidence length overflow".into()),
            )?;
        let expected_len = 134_usize.checked_add(evidence_len).ok_or_else(|| {
            TransportError::Codec("replacement submission length overflow".into())
        })?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES || input.len() != expected_len {
            return Err(TransportError::Codec(
                "replacement submission evidence length is non-canonical".into(),
            ));
        }
        let evidence_end = 5 + evidence_len;
        let evidence = input[5..evidence_end].to_vec();
        AttestationEvidenceV1::decode_canonical(&evidence).map_err(codec_error)?;
        let node_signature = input[evidence_end..evidence_end + 65]
            .try_into()
            .map_err(|_| TransportError::Codec("replacement node signature length".into()))?;
        let enclave_signature = input[evidence_end + 65..]
            .try_into()
            .map_err(|_| TransportError::Codec("replacement enclave signature length".into()))?;
        Ok(Self {
            evidence,
            node_signature,
            enclave_signature,
        })
    }
}

impl ReplacementCandidateRelayV1 {
    #[must_use]
    pub const fn calldata_hash(&self) -> B256 {
        self.calldata_hash
    }

    #[must_use]
    pub const fn transaction_hash(&self) -> B256 {
        self.transaction_hash
    }

    #[must_use]
    pub fn raw_transaction(&self) -> &[u8] {
        &self.raw_transaction
    }

    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        let raw_len = u32::try_from(self.raw_transaction.len())
            .map_err(|_| TransportError::Codec("replacement relay length overflow".into()))?;
        let mut out = Vec::with_capacity(101 + self.raw_transaction.len());
        out.push(REPLACEMENT_RELAY_VERSION_V1);
        out.extend_from_slice(self.submission_hash.as_slice());
        out.extend_from_slice(self.calldata_hash.as_slice());
        out.extend_from_slice(self.transaction_hash.as_slice());
        out.extend_from_slice(&raw_len.to_be_bytes());
        out.extend_from_slice(&self.raw_transaction);
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_RELAY_BYTES {
            return Err(TransportError::Codec(
                "replacement relay exceeds its fixed cap".into(),
            ));
        }
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 101
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_RELAY_BYTES
            || input[0] != REPLACEMENT_RELAY_VERSION_V1
        {
            return Err(TransportError::Codec(
                "replacement relay framing is invalid".into(),
            ));
        }
        let raw_len = u32::from_be_bytes(
            input[97..101]
                .try_into()
                .map_err(|_| TransportError::Codec("replacement relay length".into()))?,
        ) as usize;
        if input.len() != 101 + raw_len || raw_len == 0 {
            return Err(TransportError::Codec(
                "replacement relay raw transaction length is invalid".into(),
            ));
        }
        let relay = Self {
            submission_hash: B256::from_slice(&input[1..33]),
            calldata_hash: B256::from_slice(&input[33..65]),
            transaction_hash: B256::from_slice(&input[65..97]),
            raw_transaction: input[101..].to_vec(),
        };
        if relay.submission_hash.is_zero()
            || relay.calldata_hash.is_zero()
            || relay.transaction_hash != keccak256(&relay.raw_transaction)
        {
            return Err(TransportError::Codec(
                "replacement relay commitments are invalid".into(),
            ));
        }
        Ok(relay)
    }
}

pub(super) struct ReplacementCandidateRecordV1 {
    pub(super) predecessor_manifest_hash: B256,
    pub(super) manifest: EnclaveInitializationManifestV1,
}

impl ReplacementCandidateRecordV1 {
    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        let manifest = self
            .manifest
            .encode_canonical()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        let manifest_len = u16::try_from(manifest.len()).map_err(|_| {
            TransportError::Codec("replacement candidate manifest exceeds its wire field".into())
        })?;
        let mut out = Vec::with_capacity(35 + manifest.len());
        out.push(REPLACEMENT_CANDIDATE_VERSION_V1);
        out.extend_from_slice(self.predecessor_manifest_hash.as_slice());
        out.extend_from_slice(&manifest_len.to_be_bytes());
        out.extend_from_slice(&manifest);
        if u64::try_from(out.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_CANDIDATE_BYTES {
            return Err(TransportError::Codec(
                "replacement candidate record exceeds its fixed cap".into(),
            ));
        }
        Ok(out)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() < 35
            || u64::try_from(input.len()).unwrap_or(u64::MAX) > MAX_REPLACEMENT_CANDIDATE_BYTES
        {
            return Err(TransportError::Codec(
                "replacement candidate record length is invalid".into(),
            ));
        }
        if input[0] != REPLACEMENT_CANDIDATE_VERSION_V1 {
            return Err(TransportError::Codec(
                "replacement candidate record version is unsupported".into(),
            ));
        }
        let predecessor_manifest_hash = B256::from_slice(&input[1..33]);
        let manifest_len = usize::from(u16::from_be_bytes([input[33], input[34]]));
        if manifest_len > usize::try_from(MAX_INITIALIZATION_MANIFEST_BYTES).unwrap_or(usize::MAX)
            || input.len() != 35 + manifest_len
        {
            return Err(TransportError::Codec(
                "replacement candidate manifest length is non-canonical".into(),
            ));
        }
        let manifest =
            EnclaveInitializationManifestV1::decode_canonical(&input[35..]).map_err(codec_error)?;
        Ok(Self {
            predecessor_manifest_hash,
            manifest,
        })
    }
}

pub(super) fn read_replacement_candidate(
    path: &Path,
) -> Result<ReplacementCandidateRecordV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        MAX_REPLACEMENT_CANDIDATE_BYTES,
        "replacement candidate",
    )?;
    ReplacementCandidateRecordV1::decode_canonical(&bytes)
}

pub(super) fn read_replacement_submission(
    path: &Path,
) -> Result<ReplacementCandidateSubmissionV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        MAX_REPLACEMENT_SUBMISSION_BYTES,
        "replacement submission",
    )?;
    ReplacementCandidateSubmissionV1::decode_canonical(&bytes)
}

pub(super) fn read_replacement_relay(
    path: &Path,
) -> Result<ReplacementCandidateRelayV1, TransportError> {
    let bytes = read_owned_bounded_file(path, MAX_REPLACEMENT_RELAY_BYTES, "replacement relay")?;
    ReplacementCandidateRelayV1::decode_canonical(&bytes)
}

pub(super) fn read_replacement_promotion(
    path: &Path,
) -> Result<FinalizedReplacementAuthorizationV1, TransportError> {
    let bytes = read_owned_bounded_file(
        path,
        REPLACEMENT_PROMOTION_BYTES,
        "replacement promotion receipt",
    )?;
    FinalizedReplacementAuthorizationV1::decode_canonical(&bytes)
}

impl FinalizedReplacementAuthorizationV1 {
    pub(super) fn encode_canonical(self) -> [u8; REPLACEMENT_PROMOTION_BYTES as usize] {
        let mut out = [0_u8; REPLACEMENT_PROMOTION_BYTES as usize];
        out[0] = REPLACEMENT_PROMOTION_VERSION_V1;
        out[1..33].copy_from_slice(self.intent_hash.as_slice());
        out[33..].copy_from_slice(self.candidate_manifest_hash.as_slice());
        out
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        if input.len() != REPLACEMENT_PROMOTION_BYTES as usize
            || input[0] != REPLACEMENT_PROMOTION_VERSION_V1
        {
            return Err(TransportError::Codec(
                "replacement promotion receipt framing is invalid".into(),
            ));
        }
        Ok(Self {
            intent_hash: B256::from_slice(&input[1..33]),
            candidate_manifest_hash: B256::from_slice(&input[33..]),
        })
    }
}
