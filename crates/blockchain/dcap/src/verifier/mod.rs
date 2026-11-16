//! Stateless verification; secret keys and upload sessions belong to callers.
mod onboarding;
#[cfg(feature = "native-dcap")]
use crate::dcap_protocol::{DcapRejectCodeV1, DcapVerificationOutcomeV1};
pub use onboarding::validate_onboarding_registration;

#[cfg(feature = "native-dcap")]
pub fn verify_complete_request(
    evidence: &[u8],
    policy: &[u8],
    block_timestamp: u64,
) -> DcapVerificationOutcomeV1 {
    use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, TeePolicyV1};

    let evidence = match AttestationEvidenceV1::decode_canonical(evidence) {
        Ok(AttestationEvidenceV1::Dcap(evidence)) => evidence,
        Ok(AttestationEvidenceV1::GramineDirectDev(_)) | Err(_) => {
            return DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::EvidenceNonCanonical);
        }
    };
    let policy = match TeePolicyV1::decode_canonical(policy) {
        Ok(policy) => policy,
        Err(_) => {
            return DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::PolicyNonCanonical);
        }
    };
    match crate::dcap_v1::verify_dcap_evidence(&evidence, &policy, block_timestamp) {
        Ok(verdict) => DcapVerificationOutcomeV1::Accepted(verdict),
        Err(code) => DcapVerificationOutcomeV1::Rejected(code),
    }
}
