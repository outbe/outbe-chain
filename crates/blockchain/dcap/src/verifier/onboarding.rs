//! Registration signature and operation checks for accepted DCAP onboarding.
use crate::dcap_protocol::DcapRejectCodeV1;

pub fn validate_onboarding_registration(
    bytes: &[u8],
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> Result<outbe_primitives::tee_attestation_v1::DcapEvidenceV1, DcapRejectCodeV1> {
    use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, AttestationOperationV1};
    let evidence = match AttestationEvidenceV1::decode_canonical(bytes) {
        Ok(AttestationEvidenceV1::Dcap(evidence)) => evidence,
        _ => return Err(DcapRejectCodeV1::EvidenceNonCanonical),
    };
    let intent = &evidence.intent;
    if intent.operation != AttestationOperationV1::RegisterEnclave {
        return Err(DcapRejectCodeV1::OperationMismatch);
    }
    if !intent.verify_node_signature(node_signature) {
        return Err(DcapRejectCodeV1::NodeSignatureInvalid);
    }
    if !intent.verify_enclave_signature(enclave_signature) {
        return Err(DcapRejectCodeV1::EnclaveSignatureInvalid);
    }
    Ok(evidence)
}
