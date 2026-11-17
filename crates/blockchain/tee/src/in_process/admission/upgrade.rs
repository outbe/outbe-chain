//! Upgrade key transport requires genesis-authenticated registry authorization.
use super::super::{InProcessError, InProcessTeeContext, Result};
use super::{
    onboarding::{decode_artifact, intent_matches_context},
    FinalizedOnboardingV1, VerifiedAdmissionAnchorV1,
};
use crate::{dcap_protocol::DcapOnboardingArtifactV1, resident::encrypt_onboarding_artifact_v1};
use outbe_primitives::tee_attestation_v1::{
    AttestationOperationV1, RegistrationIntentV1, TransitionKeyReadyProofV1,
};

impl InProcessTeeContext {
    /// Seal resident material only after verifying finalized upgrade authorization from genesis.
    pub fn export_upgrade_key_v1(
        &self,
        request: FinalizedOnboardingV1<'_>,
    ) -> Result<DcapOnboardingArtifactV1> {
        self.proof_source
            .ensure_available(self.manifest.attestation_mode)?;
        let context = decode_artifact(request.artifact)?.context;
        let authorization = outbe_primitives::tee_attestation_v1::NodeHostAuthorizationWitnessV1 {
            chain_id: request.intent.chain_id,
            genesis_hash: request.intent.genesis_hash,
            attestation_mode: request.intent.attestation_mode,
            node_id: request.intent.node_id.clone(),
            node_host_noise_x25519: request.authorization.node_host_noise_x25519,
        };
        let authorized = request.authorization == &authorization
            && authorization.authorization_hash()? == request.intent.node_host_authorization_hash;
        let transition_matches = request.intent.operation
            == AttestationOperationV1::TransitionEnclaveMeasurement
            && request.intent.attestation_mode == self.manifest.attestation_mode;
        if !authorized || !transition_matches {
            return Err(InProcessError::IdentityMismatch);
        }
        if !intent_matches_context(request.intent, &context)? || !request.matches_material(&context)
        {
            return Err(InProcessError::IdentityMismatch);
        }
        self.validate_source(&context)?;
        self.verify_material_proof(&context, &request, true)?;
        Ok(encrypt_onboarding_artifact_v1(
            self.material()?.as_ref(),
            context,
        )?)
    }
    /// Verify a candidate transition and finalized upgrade proof before resident import.
    pub fn ingest_upgrade_key_v1(
        &self,
        request: FinalizedOnboardingV1<'_>,
    ) -> Result<VerifiedAdmissionAnchorV1> {
        if request.intent.operation != AttestationOperationV1::TransitionEnclaveMeasurement {
            return Err(InProcessError::IdentityMismatch);
        }
        self.ingest_proven_material(request, true)
    }
    /// Sign key readiness for this candidate manifest and exact measurement-transition intent.
    pub fn transition_key_ready_proof_v1(
        &self,
        intent: &RegistrationIntentV1,
    ) -> Result<TransitionKeyReadyProofV1> {
        self.proof_source
            .ensure_available(self.manifest.attestation_mode)?;
        self.manifest.validate_intent_binding(intent)?;
        if intent.operation != AttestationOperationV1::TransitionEnclaveMeasurement {
            return Err(InProcessError::IdentityMismatch);
        }
        let mut proof = TransitionKeyReadyProofV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            transition_intent_hash: intent.intent_hash()?,
            candidate_manifest_hash: self.manifest.authorization_hash()?,
            transition_nonce: intent.transition_nonce,
            resident_offer_public: self.network_encryption_public()?,
            candidate_attestation_signature: [0; 64],
        };
        proof.candidate_attestation_signature =
            self.sign_domain_attestation(proof.signing_hash()?.as_slice());
        Ok(proof)
    }
}
