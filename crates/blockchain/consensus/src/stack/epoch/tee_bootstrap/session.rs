//! Resident owner used by the founding ceremony and offer-key verification.
use outbe_tee::{in_process::InProcessTeeContext, tee_dkg::EnclaveChannel};
use std::sync::Arc;

pub(super) struct StartupEnclave(Arc<InProcessTeeContext>);

impl StartupEnclave {
    pub(super) fn connect(context: Option<Arc<InProcessTeeContext>>) -> eyre::Result<Self> {
        context
            .map(Self)
            .ok_or_else(|| outbe_tee::InProcessDispatchError::ContextNotInstalled.into())
    }

    pub(super) fn manifest(
        &self,
    ) -> &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1 {
        self.0.manifest()
    }

    pub(super) fn build_submission(
        &self,
        request: crate::tee_bootstrap::LocalRegistrationRequest<'_>,
        signer: &outbe_primitives::signer::OutbeEvmSigner,
        sign_node: impl Fn(alloy_primitives::B256) -> Result<[u8; 65], crate::errors::TeeBootstrapError>,
    ) -> eyre::Result<outbe_primitives::tee_bootstrap_v2::TeeBootstrapParticipantSubmissionV2> {
        crate::tee_bootstrap::build_in_process_tee_bootstrap_submission_v2(
            &self.0, request, signer, sign_node,
        )
    }
}

impl EnclaveChannel for StartupEnclave {
    fn request(
        &mut self,
        request: &outbe_tee::protocol::EnclaveRequest,
    ) -> Result<outbe_tee::protocol::EnclaveResponse, outbe_tee::TransportError> {
        self.0.as_ref().request(request)
    }
}
