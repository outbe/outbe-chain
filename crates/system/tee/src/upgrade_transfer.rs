//! Finalized upgrade authorization transport. Only ciphertext leaves the source.
use crate::{
    dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1},
    errors::TransportError,
    finalized_admission::*,
    protocol::{EnclaveRequest, EnclaveResponse},
};
use alloy_primitives::{Bytes, B256};
use serde::{Deserialize, Serialize};

pub const MAX_UPGRADE_PROOF_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_UPGRADE_COMMITTEES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeKeyProofV1 {
    pub anchor_outcome: Bytes,
    pub committee_transitions: Vec<Bytes>,
    pub admission: Bytes,
}
impl UpgradeKeyProofV1 {
    pub fn validate(&self) -> Result<(), TransportError> {
        ValidatedUpgradeProof::new(self).map(|_| ())
    }

    fn has_valid_dimensions(&self) -> bool {
        let anchor_valid = !self.anchor_outcome.is_empty()
            && self.anchor_outcome.len() <= MAX_COMMITTEE_OUTCOME_BYTES;
        let transition_count_valid = self.committee_transitions.len() <= MAX_UPGRADE_COMMITTEES;
        let admission_valid = !self.admission.is_empty()
            && self.admission.len() <= MAX_FINALIZED_ADMISSION_RECORD_BYTES;
        anchor_valid && transition_count_valid && admission_valid
    }
}

struct ValidatedUpgradeProof<'a> {
    anchor_outcome: &'a Bytes,
    committee_transitions: &'a [Bytes],
    admission: &'a Bytes,
}

impl<'a> ValidatedUpgradeProof<'a> {
    fn new(proof: &'a UpgradeKeyProofV1) -> Result<Self, TransportError> {
        if !proof.has_valid_dimensions() {
            return Err(invalid("upgrade proof dimensions exceed limits"));
        }
        let mut total = proof.anchor_outcome.len() + proof.admission.len();
        for item in &proof.committee_transitions {
            if item.is_empty() || item.len() > MAX_COMMITTEE_TRANSITION_RECORD_BYTES {
                return Err(invalid("invalid upgrade committee record size"));
            }
            total = total
                .checked_add(item.len())
                .ok_or_else(|| invalid("proof size overflow"))?;
        }
        if total > MAX_UPGRADE_PROOF_BYTES {
            return Err(invalid("upgrade proof exceeds aggregate limit"));
        }
        Ok(Self {
            anchor_outcome: &proof.anchor_outcome,
            committee_transitions: &proof.committee_transitions,
            admission: &proof.admission,
        })
    }
}
fn invalid(message: &str) -> TransportError {
    TransportError::EnclaveError(message.into())
}

/// A placeholder is never decrypted. Its context commits the export recipient.
pub fn export_placeholder(context: DcapOnboardingContextV1) -> Result<Vec<u8>, TransportError> {
    DcapOnboardingArtifactV1 {
        context,
        nonce: [0; 12],
        ciphertext: vec![0; 16],
    }
    .encode_canonical()
    .map_err(|_| invalid("invalid upgrade context"))
}

pub fn transfer(
    mut request: impl FnMut(&EnclaveRequest) -> Result<EnclaveResponse, TransportError>,
    proof: &UpgradeKeyProofV1,
    artifact: &[u8],
    export: bool,
) -> Result<EnclaveResponse, TransportError> {
    PreparedTransfer::new(proof, artifact, export)?
        .begin(&mut request)?
        .send_all(&mut request)?
        .finish(&mut request)
}

#[derive(Clone, Copy)]
enum TransferMode {
    Export,
    Import,
}

impl TransferMode {
    fn from_export(export: bool) -> Self {
        if export {
            Self::Export
        } else {
            Self::Import
        }
    }

    fn export(self) -> bool {
        matches!(self, Self::Export)
    }
}

struct PreparedTransfer<'a> {
    proof: ValidatedUpgradeProof<'a>,
    artifact: &'a [u8],
    hash: B256,
    mode: TransferMode,
}

impl<'a> PreparedTransfer<'a> {
    fn new(
        proof: &'a UpgradeKeyProofV1,
        artifact: &'a [u8],
        export: bool,
    ) -> Result<Self, TransportError> {
        let proof = ValidatedUpgradeProof::new(proof)?;
        let hash = upgrade_key_transfer_request_hash_v1(artifact, proof.anchor_outcome, export)
            .map_err(|e| invalid(&e.to_string()))?;
        Ok(Self {
            proof,
            artifact,
            hash,
            mode: TransferMode::from_export(export),
        })
    }

    fn begin(
        self,
        request: &mut impl FnMut(&EnclaveRequest) -> Result<EnclaveResponse, TransportError>,
    ) -> Result<StreamingTransfer<'a>, TransportError> {
        let response = request(&EnclaveRequest::BeginUpgradeKeyTransferV1 {
            request_hash: self.hash,
            artifact: self.artifact.to_vec(),
            anchor_outcome: self.proof.anchor_outcome.to_vec(),
            export: self.mode.export(),
        })?;
        if !matches!(response, EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 { request_hash } if request_hash == self.hash)
        {
            return Err(TransportError::UnexpectedResponse);
        }
        Ok(StreamingTransfer {
            proof: self.proof,
            hash: self.hash,
            mode: self.mode,
        })
    }
}

struct StreamingTransfer<'a> {
    proof: ValidatedUpgradeProof<'a>,
    hash: B256,
    mode: TransferMode,
}

impl StreamingTransfer<'_> {
    fn send_all(
        self,
        request: &mut impl FnMut(&EnclaveRequest) -> Result<EnclaveResponse, TransportError>,
    ) -> Result<CommittedTransfer, TransportError> {
        for (kind, record) in self
            .proof
            .committee_transitions
            .iter()
            .map(|r| (FinalizedAdmissionRecordKindV1::CommitteeTransition, r))
            .chain(std::iter::once((
                FinalizedAdmissionRecordKindV1::Admission,
                self.proof.admission,
            )))
        {
            for (i, bytes) in record.chunks(MAX_ONBOARDING_INGEST_CHUNK_BYTES).enumerate() {
                let offset = u32::try_from(i * MAX_ONBOARDING_INGEST_CHUNK_BYTES)
                    .map_err(|_| invalid("offset overflow"))?;
                let next =
                    offset + u32::try_from(bytes.len()).map_err(|_| invalid("chunk overflow"))?;
                let response = request(&EnclaveRequest::DcapOnboardingArtifactChunkV1 {
                    request_hash: self.hash,
                    kind,
                    offset,
                    bytes: bytes.to_vec(),
                })?;
                if !matches!(response, EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 { request_hash, next_offset } if request_hash == self.hash && next_offset == next)
                {
                    return Err(TransportError::UnexpectedResponse);
                }
            }
            let response = request(&EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 {
                request_hash: self.hash,
                kind,
            })?;
            if !matches!(response, EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 { request_hash, kind: actual } if request_hash == self.hash && actual == kind)
            {
                return Err(TransportError::UnexpectedResponse);
            }
        }
        Ok(CommittedTransfer {
            hash: self.hash,
            mode: self.mode,
        })
    }
}

struct CommittedTransfer {
    hash: B256,
    mode: TransferMode,
}

impl CommittedTransfer {
    fn finish(
        self,
        request: &mut impl FnMut(&EnclaveRequest) -> Result<EnclaveResponse, TransportError>,
    ) -> Result<EnclaveResponse, TransportError> {
        let response = request(&EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 {
            request_hash: self.hash,
        })?;
        let matches = match &response {
            EnclaveResponse::UpgradeKeyExportedV1 { request_hash, .. } => {
                self.mode.export() && *request_hash == self.hash
            }
            EnclaveResponse::FinalizedAdmissionIngestedV1 { request_hash, .. } => {
                !self.mode.export() && *request_hash == self.hash
            }
            _ => false,
        };
        if !matches {
            return Err(TransportError::UnexpectedResponse);
        }
        Ok(response)
    }
}

pub fn export_from_network_source(
    context: DcapOnboardingContextV1,
    proof: &UpgradeKeyProofV1,
    legacy_direct_dev: bool,
) -> Result<Vec<u8>, TransportError> {
    proof.validate()?;
    // Fork under the execution lock, release it before any network/proof work.
    let mut session = crate::try_with_enclave(|s| s.fork_connection())
        .ok_or_else(|| invalid("source enclave is not configured"))?;
    let artifact = if legacy_direct_dev {
        session.prepare_gramine_direct_dev_onboarding_artifact_v1(context)?
    } else {
        let placeholder = export_placeholder(context)?;
        let response = session.export_upgrade_key_v1(proof, &placeholder)?;
        let EnclaveResponse::UpgradeKeyExportedV1 { artifact, .. } = response else {
            return Err(TransportError::UnexpectedResponse);
        };
        DcapOnboardingArtifactV1::decode_canonical(&artifact)
            .map_err(|_| invalid("source artifact codec"))?
    };
    if artifact.context != context {
        return Err(invalid("source returned a different recipient context"));
    }
    artifact
        .encode_canonical()
        .map_err(|_| invalid("source artifact encoding"))
}

pub fn artifact_context_hash(artifact: &[u8]) -> Result<B256, TransportError> {
    Ok(DcapOnboardingArtifactV1::decode_canonical(artifact)
        .map_err(|_| invalid("artifact codec"))?
        .context
        .context_hash())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> DcapOnboardingContextV1 {
        crate::test_utils::onboarding_context_fixture([1, 2, 3, 4, 5, 6, 7, 8, 9], 1, 2)
    }
    fn proof() -> UpgradeKeyProofV1 {
        UpgradeKeyProofV1 {
            anchor_outcome: vec![1].into(),
            committee_transitions: vec![],
            admission: vec![2].into(),
        }
    }
    #[test]
    fn malformed_or_oversized_proof_never_opens_an_enclave_stream() {
        let mut invalid = proof();
        invalid.committee_transitions =
            vec![vec![0; MAX_COMMITTEE_TRANSITION_RECORD_BYTES].into(); 65];
        let mut called = false;
        assert!(transfer(
            |_| {
                called = true;
                Err(TransportError::UnexpectedResponse)
            },
            &invalid,
            &export_placeholder(context()).unwrap(),
            true
        )
        .is_err());
        assert!(!called);
    }
    #[test]
    fn request_commitment_separates_export_import_and_recipient() {
        let proof = proof();
        let artifact = export_placeholder(context()).unwrap();
        let export =
            upgrade_key_transfer_request_hash_v1(&artifact, &proof.anchor_outcome, true).unwrap();
        assert_ne!(
            export,
            upgrade_key_transfer_request_hash_v1(&artifact, &proof.anchor_outcome, false).unwrap()
        );
        let mut other = context();
        other.recipient_x25519[0] ^= 1;
        assert_ne!(
            export,
            upgrade_key_transfer_request_hash_v1(
                &export_placeholder(other).unwrap(),
                &proof.anchor_outcome,
                true
            )
            .unwrap()
        );
    }
    #[test]
    fn source_wrong_request_commitment_cannot_advance_to_key_export() {
        let mut calls = 0;
        let result = transfer(
            |_| {
                calls += 1;
                Ok(EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 {
                    request_hash: B256::ZERO,
                })
            },
            &proof(),
            &export_placeholder(context()).unwrap(),
            true,
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
