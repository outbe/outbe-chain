//! Verify finality and exact recipient binding before any resident activation.
use super::super::{InProcessError, InProcessTeeContext, Result};
use super::{FinalizedAdmissionVerifierV1, VerifiedAdmissionAnchorV1};
use crate::{
    dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1},
    resident::{decrypt_onboarding_artifact_v1, encrypt_onboarding_artifact_v1},
};
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, NodeHostAuthorizationWitnessV1, RegistrationIntentV1,
};

/// Exact recipient intent, authorization and genesis-rooted proof inputs for resident import.
pub struct FinalizedOnboardingV1<'a> {
    pub artifact: &'a [u8],
    pub intent: &'a RegistrationIntentV1,
    pub authorization: &'a NodeHostAuthorizationWitnessV1,
    pub anchor_outcome: &'a [u8],
    pub committee_transitions: &'a [Vec<u8>],
    pub admission_witness: &'a [u8],
    pub expected_network_encryption_public: [u8; 32],
    pub expected_key_epoch: u64,
    pub expected_network_encryption_epoch: u64,
}
#[derive(Debug)]
/// Canonical development donor artifact and its domain-separated attestation.
pub struct PreparedOnboardingArtifactV2 {
    pub request_hash: B256,
    pub onboarding_artifact: Vec<u8>,
    pub attestation_tag: [u8; 64],
}
/// Purpose-bound native verifier inputs and exact expected onboarding context.
pub struct DcapRegistrationSealV1<'a> {
    pub evidence: &'a [u8],
    pub policy: &'a [u8],
    pub block_timestamp: u64,
    pub node_signature: &'a [u8; 65],
    pub enclave_signature: &'a [u8; 64],
    pub context: DcapOnboardingContextV1,
}
/// Canonical DCAP witness fields, including authenticated rejection outcomes.
#[derive(Debug)]
pub struct DcapOnboardingResultV1 {
    /// Commitment produced by dcap_onboarding_request_hash for the exact request.
    pub request_hash: B256,
    /// Canonically encoded DcapVerificationOutcomeV1, accepted or rejected.
    pub outcome: Vec<u8>,
    /// Canonical encrypted artifact on acceptance; empty on rejection.
    pub onboarding_artifact: Vec<u8>,
    /// Ed25519 signature over dcap_onboarding_attestation_preimage.
    pub attestation_tag: [u8; 64],
}
impl InProcessTeeContext {
    /// Verify recipient binding, BLS finality and Registry MPT openings before RAM activation.
    pub fn ingest_finalized_onboarding_v1(
        &self,
        request: FinalizedOnboardingV1<'_>,
    ) -> Result<VerifiedAdmissionAnchorV1> {
        self.ingest_proven_material(request, false)
    }
    pub(super) fn ingest_proven_material(
        &self,
        request: FinalizedOnboardingV1<'_>,
        upgrade: bool,
    ) -> Result<VerifiedAdmissionAnchorV1> {
        self.proof_source
            .ensure_available(self.manifest.attestation_mode)?;
        let artifact = decode_artifact(request.artifact)?;
        self.validate_recipient(&artifact.context, &request)?;
        let anchor = self.verify_material_proof(&artifact.context, &request, upgrade)?;
        let material = decrypt_onboarding_artifact_v1(&self.keys, &artifact)?;
        self.activate(material, Some(anchor))?;
        Ok(anchor)
    }
    pub(super) fn verify_material_proof(
        &self,
        context: &DcapOnboardingContextV1,
        request: &FinalizedOnboardingV1<'_>,
        upgrade: bool,
    ) -> Result<VerifiedAdmissionAnchorV1> {
        let mut verifier =
            FinalizedAdmissionVerifierV1::new(&self.descriptor, context, request.anchor_outcome)?;
        if upgrade {
            verifier = verifier.for_upgrade();
        }
        for record in request.committee_transitions {
            verifier.advance_committee(record)?;
        }
        verifier.verify_admission(request.admission_witness)
    }
    pub(super) fn validate_recipient(
        &self,
        context: &DcapOnboardingContextV1,
        request: &FinalizedOnboardingV1<'_>,
    ) -> Result<()> {
        self.manifest.validate_intent_binding(request.intent)?;
        if *request.authorization != self.authorization_witness()?
            || !intent_matches_context(request.intent, context)?
            || !request.matches_material(context)
        {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(())
    }
    pub(super) fn validate_source(&self, context: &DcapOnboardingContextV1) -> Result<()> {
        let material = self.material()?;
        let network_matches = (context.chain_id, context.genesis_hash)
            == (self.manifest.chain_id, self.manifest.genesis_hash);
        let key_matches = (
            material.public(),
            material.key_epoch(),
            material.tribute_offer_epoch(),
        ) == (
            context.network_encryption_public,
            context.key_epoch,
            context.network_encryption_epoch,
        );
        if !network_matches || !key_matches {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(())
    }
    /// Seal resident material for an exact development context and sign the frozen V2 domain.
    pub fn prepare_no_attestation_dev_onboarding_artifact_v2(
        &self,
        context: DcapOnboardingContextV1,
    ) -> Result<PreparedOnboardingArtifactV2> {
        if self.manifest.attestation_mode != AttestationMode::NoAttestationDev {
            return Err(InProcessError::IdentityMismatch);
        }
        self.validate_source(&context)?;
        let artifact = encrypt_onboarding_artifact_v1(self.material()?.as_ref(), context)?;
        let onboarding_artifact = artifact.encode_canonical().map_err(InProcessError::Dcap)?;
        let request_hash = context.context_hash();
        let preimage = crate::tee_call::canonical_attestation_preimage(
            b"outbe/tee/gramine-direct-dev-onboarding-result/v2",
            request_hash,
            &onboarding_artifact,
        )?;
        Ok(PreparedOnboardingArtifactV2 {
            request_hash,
            onboarding_artifact,
            attestation_tag: self.sign_domain_attestation(&preimage),
        })
    }
    /// Return the canonical signed DCAP outcome and optional encrypted onboarding artifact.
    /// Native quote verification is required; absence never falls back to development evidence.
    pub fn verify_dcap_registration_and_seal_v1(
        &self,
        request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingResultV1> {
        self.proof_source
            .ensure_available(AttestationMode::DcapRequired)?;
        self.verify_dcap_and_seal(request)
    }
    #[cfg(not(feature = "native-dcap"))]
    fn verify_dcap_and_seal(
        &self,
        _request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingResultV1> {
        Err(InProcessError::HardwareProofUnavailable)
    }
    #[cfg(feature = "native-dcap")]
    fn verify_dcap_and_seal(
        &self,
        request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingResultV1> {
        let outcome = outbe_dcap::verifier::verify_complete_request(
            request.evidence,
            request.policy,
            request.block_timestamp,
        );
        self.finish_dcap_onboarding(request, outcome)
    }
    #[cfg(any(test, feature = "native-dcap"))]
    fn finish_dcap_onboarding(
        &self,
        request: DcapRegistrationSealV1<'_>,
        mut outcome: crate::dcap_protocol::DcapVerificationOutcomeV1,
    ) -> Result<DcapOnboardingResultV1> {
        use crate::dcap_protocol::{
            dcap_onboarding_attestation_preimage, dcap_onboarding_request_hash,
            RegistrationVerificationRequest,
        };
        let request_hash = dcap_onboarding_request_hash(RegistrationVerificationRequest {
            evidence: request.evidence,
            policy: request.policy,
            block_timestamp: request.block_timestamp,
            node_signature: request.node_signature,
            enclave_signature: request.enclave_signature,
            expected_network_encryption_public: request.context.network_encryption_public,
            key_epoch: request.context.key_epoch,
            network_encryption_epoch: request.context.network_encryption_epoch,
        })
        .map_err(InProcessError::Dcap)?;
        let artifact = self.dcap_onboarding_artifact(&request, &mut outcome)?;
        let outcome = outcome.encode_canonical().map_err(InProcessError::Dcap)?;
        let onboarding_artifact = artifact
            .map(|artifact| artifact.encode_canonical())
            .transpose()
            .map_err(InProcessError::Dcap)?
            .unwrap_or_default();
        let preimage =
            dcap_onboarding_attestation_preimage(request_hash, &outcome, &onboarding_artifact)
                .map_err(InProcessError::Dcap)?;
        Ok(DcapOnboardingResultV1 {
            request_hash,
            outcome,
            onboarding_artifact,
            attestation_tag: self.sign_domain_attestation(&preimage),
        })
    }
    #[cfg(any(test, feature = "native-dcap"))]
    fn dcap_onboarding_artifact(
        &self,
        request: &DcapRegistrationSealV1<'_>,
        outcome: &mut crate::dcap_protocol::DcapVerificationOutcomeV1,
    ) -> Result<Option<DcapOnboardingArtifactV1>> {
        use crate::dcap_protocol::DcapVerificationOutcomeV1;
        let DcapVerificationOutcomeV1::Accepted(verdict) = outcome else {
            return Ok(None);
        };
        let evidence = match outbe_dcap::verifier::validate_onboarding_registration(
            request.evidence,
            request.node_signature,
            request.enclave_signature,
        ) {
            Ok(evidence) => evidence,
            Err(code) => {
                *outcome = DcapVerificationOutcomeV1::Rejected(code);
                return Ok(None);
            }
        };
        if let Some(code) = self.dcap_context_rejection(request, &evidence.intent, verdict)? {
            *outcome = DcapVerificationOutcomeV1::Rejected(code);
            return Ok(None);
        }
        Ok(Some(encrypt_onboarding_artifact_v1(
            self.material()?.as_ref(),
            request.context,
        )?))
    }
    #[cfg(any(test, feature = "native-dcap"))]
    fn dcap_context_rejection(
        &self,
        request: &DcapRegistrationSealV1<'_>,
        intent: &RegistrationIntentV1,
        verdict: &crate::dcap_protocol::DcapVerdictV1,
    ) -> Result<Option<crate::dcap_protocol::DcapRejectCodeV1>> {
        use super::super::ProofSourceV1;
        use crate::dcap_protocol::DcapRejectCodeV1;
        let ProofSourceV1::Hardware(source) = &self.proof_source else {
            return Err(InProcessError::HardwareProofUnavailable);
        };
        let measurement_matches = source.code_identity()?
            == (
                verdict.mrenclave,
                verdict.mrsigner,
                verdict.isv_prod_id,
                verdict.isv_svn,
            );
        if self.manifest.attestation_mode != AttestationMode::DcapRequired || !measurement_matches {
            return Ok(Some(DcapRejectCodeV1::MeasurementRejected));
        }
        let source_matches = match self.validate_source(&request.context) {
            Ok(()) => true,
            Err(InProcessError::IdentityMismatch) => false,
            Err(error) => return Err(error),
        };
        if !intent_matches_context(intent, &request.context)? || !source_matches {
            return Ok(Some(DcapRejectCodeV1::OnboardingContextMismatch));
        }
        Ok(None)
    }
}

pub(super) fn decode_artifact(bytes: &[u8]) -> Result<DcapOnboardingArtifactV1> {
    DcapOnboardingArtifactV1::decode_canonical(bytes).map_err(InProcessError::Dcap)
}
pub(super) fn intent_matches_context(
    intent: &RegistrationIntentV1,
    context: &DcapOnboardingContextV1,
) -> Result<bool> {
    let network_matches =
        (intent.chain_id, intent.genesis_hash) == (context.chain_id, context.genesis_hash);
    let recipient_matches = (
        intent.node_id.node_id_hash()?,
        intent.derived_enclave_id()?,
        intent.recipient_x25519,
    ) == (
        context.node_id_hash,
        context.enclave_id,
        context.recipient_x25519,
    );
    let registration_matches = (intent.intent_hash()?, intent.binding_id, intent.policy_hash)
        == (context.intent_hash, context.binding_id, context.policy_hash);
    Ok(network_matches && recipient_matches && registration_matches)
}

impl FinalizedOnboardingV1<'_> {
    pub(super) fn matches_material(&self, context: &DcapOnboardingContextV1) -> bool {
        (
            context.network_encryption_public,
            context.key_epoch,
            context.network_encryption_epoch,
        ) == (
            self.expected_network_encryption_public,
            self.expected_key_epoch,
            self.expected_network_encryption_epoch,
        )
    }
}

#[cfg(test)]
mod tests;
