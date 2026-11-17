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
pub struct PreparedOnboardingArtifactV2 {
    pub request_hash: B256,
    pub onboarding_artifact: Vec<u8>,
    pub attestation_tag: [u8; 64],
}
pub struct DcapRegistrationSealV1<'a> {
    pub evidence: &'a [u8],
    pub policy: &'a [u8],
    pub block_timestamp: u64,
    pub node_signature: &'a [u8; 65],
    pub enclave_signature: &'a [u8; 64],
    pub context: DcapOnboardingContextV1,
}
impl InProcessTeeContext {
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
    /// Native DCAP verifies quotes/collateral; a build without it fails closed.
    pub fn verify_dcap_registration_and_seal_v1(
        &self,
        request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingArtifactV1> {
        self.proof_source
            .ensure_available(AttestationMode::DcapRequired)?;
        self.verify_dcap_and_seal(request)
    }
    #[cfg(not(feature = "native-dcap"))]
    fn verify_dcap_and_seal(
        &self,
        _request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingArtifactV1> {
        Err(InProcessError::HardwareProofUnavailable)
    }
    #[cfg(feature = "native-dcap")]
    fn verify_dcap_and_seal(
        &self,
        request: DcapRegistrationSealV1<'_>,
    ) -> Result<DcapOnboardingArtifactV1> {
        use super::super::ProofSourceV1;
        use crate::dcap_protocol::DcapVerificationOutcomeV1;
        if self.manifest.attestation_mode != AttestationMode::DcapRequired {
            return Err(InProcessError::IdentityMismatch);
        }
        let evidence = outbe_dcap::verifier::validate_onboarding_registration(
            request.evidence,
            request.node_signature,
            request.enclave_signature,
        )
        .map_err(InProcessError::Dcap)?;
        let verdict = match outbe_dcap::verifier::verify_complete_request(
            request.evidence,
            request.policy,
            request.block_timestamp,
        ) {
            DcapVerificationOutcomeV1::Accepted(verdict) => verdict,
            DcapVerificationOutcomeV1::Rejected(code) => return Err(InProcessError::Dcap(code)),
        };
        let ProofSourceV1::Hardware(source) = &self.proof_source else {
            return Err(InProcessError::HardwareProofUnavailable);
        };
        if source.code_identity()?
            != (
                verdict.mrenclave,
                verdict.mrsigner,
                verdict.isv_prod_id,
                verdict.isv_svn,
            )
            || !intent_matches_context(&evidence.intent, &request.context)?
        {
            return Err(InProcessError::IdentityMismatch);
        }
        self.validate_source(&request.context)?;
        Ok(encrypt_onboarding_artifact_v1(
            self.material()?.as_ref(),
            request.context,
        )?)
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
