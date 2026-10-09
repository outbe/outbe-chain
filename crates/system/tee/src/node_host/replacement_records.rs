use super::codec_error;
use super::journal_records::{
    decode_relay_fields, decode_submission_payload, encode_relay_fields, encode_submission_fields,
    validate_submission_evidence_length, validate_submission_frame, JournalRelayPayload,
    JournalRelayValidation, JournalSubmissionErrors, JournalSubmissionPayload,
    JournalSubmissionValidation, RelayMaterial,
};
use super::read_owned_bounded_file;
use super::MAX_INITIALIZATION_MANIFEST_BYTES;
use crate::remote_session::FinalizedRegistryViewV1;

use crate::TransportError;
use alloy_primitives::keccak256;

use alloy_primitives::B256;

use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::MAX_ATTESTATION_EVIDENCE_BYTES;

use std::fmt;
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

const REPLACEMENT_SUBMISSION_VALIDATION: JournalSubmissionValidation =
    JournalSubmissionValidation {
        min_len: 134,
        max_bytes: MAX_REPLACEMENT_SUBMISSION_BYTES,
        version: REPLACEMENT_SUBMISSION_VERSION_V1,
        framing_error: "replacement submission framing is invalid",
        length_base: 134,
        length_overflow_error: "replacement submission length overflow",
        noncanonical_length_error: "replacement submission evidence length is non-canonical",
        encode_evidence_length_error: "replacement submission evidence length overflow",
        encode_allocation_error: "replacement submission allocation length overflow",
        encode_cap_error: None,
    };

const REPLACEMENT_RELAY_VALIDATION: JournalRelayValidation = JournalRelayValidation {
    header_len: 101,
    max_bytes: MAX_REPLACEMENT_RELAY_BYTES,
    version: REPLACEMENT_RELAY_VERSION_V1,
    encode_length_overflow_error: "replacement relay length overflow",
    encode_cap_error: "replacement relay exceeds its fixed cap",
    raw_len_offset: 97,
    raw_len_error: "replacement relay length",
    from_block_offset: None,
    from_block_error: "replacement relay length",
    framing_error: "replacement relay framing is invalid",
    raw_length_error: "replacement relay raw transaction length is invalid",
    commitments_error: "replacement relay commitments are invalid",
};

/// Exact durable transaction material returned for relay retries. The evidence
/// already contains the canonical replacement intent.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplacementCandidateSubmissionV1 {
    payload: JournalSubmissionPayload,
}

/// Exact signed registration transaction persisted before relay. It is bound
/// to the durable candidate submission so a restart cannot attach another
/// transaction to already-quoted evidence.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplacementCandidateRelayV1 {
    payload: JournalRelayPayload,
}

impl fmt::Debug for ReplacementCandidateSubmissionV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplacementCandidateSubmissionV1")
            .field("evidence", &self.payload.evidence)
            .field("node_signature", &self.payload.node_signature)
            .field("enclave_signature", &self.payload.enclave_signature)
            .finish()
    }
}

impl fmt::Debug for ReplacementCandidateRelayV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplacementCandidateRelayV1")
            .field("submission_hash", &self.payload.submission_hash)
            .field("calldata_hash", &self.payload.calldata_hash)
            .field("transaction_hash", &self.payload.transaction_hash)
            .field("raw_transaction", &self.payload.raw_transaction)
            .finish()
    }
}

/// Opaque authority for one exact finalized registry binding.
/// `construct_finalized_replacement_authorization_v1` builds it in production.
/// The node session and the CLI call that constructor.
/// The caller checks the finalized registry state before construction.
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
    pub(super) fn new(
        evidence: Vec<u8>,
        node_signature: [u8; 65],
        enclave_signature: [u8; 64],
    ) -> Self {
        Self {
            payload: JournalSubmissionPayload::new(evidence, node_signature, enclave_signature),
        }
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
        encode_submission_fields(&[], &self.payload, &REPLACEMENT_SUBMISSION_VALIDATION)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        validate_submission_frame(input, &REPLACEMENT_SUBMISSION_VALIDATION)?;
        let evidence_len =
            usize::try_from(u32::from_be_bytes([input[1], input[2], input[3], input[4]])).map_err(
                |_| TransportError::Codec("replacement evidence length overflow".into()),
            )?;
        validate_submission_evidence_length(
            input.len(),
            evidence_len,
            &REPLACEMENT_SUBMISSION_VALIDATION,
        )?;
        let payload = decode_submission_payload(
            input,
            5,
            evidence_len,
            JournalSubmissionErrors {
                node_signature: "replacement node signature length",
                enclave_signature: "replacement enclave signature length",
            },
        )?;
        Ok(Self { payload })
    }
}

impl ReplacementCandidateRelayV1 {
    pub(super) fn new(submission_hash: B256, material: RelayMaterial) -> Self {
        Self {
            payload: JournalRelayPayload::new(submission_hash, material),
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
    pub fn raw_transaction(&self) -> &[u8] {
        &self.payload.raw_transaction
    }

    pub(super) fn encode_canonical(&self) -> Result<Vec<u8>, TransportError> {
        encode_relay_fields(&self.payload, None, &REPLACEMENT_RELAY_VALIDATION)
    }

    fn decode_canonical(input: &[u8]) -> Result<Self, TransportError> {
        let fields = decode_relay_fields(input, &REPLACEMENT_RELAY_VALIDATION)?;
        let relay = Self {
            payload: fields.payload,
        };
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
