//! Prefer the installed resident owner; retain the explicit sidecar startup path.
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;
use outbe_tee::tee_dkg::EnclaveChannel;
use outbe_tee::{in_process::InProcessTeeContext, RuntimeEnclaveClient};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) enum StartupEnclave {
    Resident(Arc<InProcessTeeContext>),
    Sidecar(Box<RuntimeEnclaveClient>),
}
impl StartupEnclave {
    pub(super) fn connect(
        context: Option<Arc<InProcessTeeContext>>,
        socket: Option<PathBuf>,
        mode: crate::args::ResolvedTeeSession,
        data: &Path,
    ) -> eyre::Result<Self> {
        if let Some(context) = context {
            return Ok(Self::Resident(context));
        }
        let socket =
            socket.ok_or(crate::errors::StackEpochError::MandatoryTeeRequiresEnclaveSocket)?;
        let endpoint = socket
            .to_str()
            .ok_or(crate::errors::StackEpochError::TeeEnclaveEndpointInvalidUtf8)?;
        Ok(Self::Sidecar(Box::new(match mode {
            crate::args::ResolvedTeeSession::ProductionNodeHost => {
                RuntimeEnclaveClient::Production(
                    outbe_tee::connect_committed_node_host_enclave(endpoint, data).map_err(
                        crate::errors::StackEpochError::production_node_host_reconnect_failed,
                    )?,
                )
            }
            crate::args::ResolvedTeeSession::Development => {
                RuntimeEnclaveClient::Development(Box::new(
                    outbe_tee::EnclaveClient::connect_endpoint(endpoint).map_err(
                        crate::errors::StackEpochError::no_attestation_dev_reconnect_failed,
                    )?,
                ))
            }
        })))
    }
    pub(super) fn manifest(
        &self,
        data: &Path,
    ) -> eyre::Result<Option<EnclaveInitializationManifestV1>> {
        match self {
            Self::Resident(context) => Ok(Some(context.manifest().clone())),
            Self::Sidecar(client) => match client.as_ref() {
                RuntimeEnclaveClient::Production(_) => Ok(Some(
                    outbe_tee::load_committed_enclave_manifest_v1(data).map_err(
                        crate::errors::StackEpochError::committed_node_host_manifest_load_failed,
                    )?,
                )),
                RuntimeEnclaveClient::Development(_) => Ok(None),
            },
        }
    }
    pub(super) fn build_submission(
        &mut self,
        request: crate::tee_bootstrap::LocalRegistrationRequest<'_>,
        signer: &outbe_primitives::signer::OutbeEvmSigner,
        sign_node: impl Fn(alloy_primitives::B256) -> Result<[u8; 65], crate::errors::TeeBootstrapError>,
    ) -> eyre::Result<outbe_primitives::tee_bootstrap_v2::TeeBootstrapParticipantSubmissionV2> {
        match self {
            Self::Resident(context) => {
                crate::tee_bootstrap::build_in_process_tee_bootstrap_submission_v2(
                    context, request, signer, sign_node,
                )
            }
            Self::Sidecar(client) => crate::tee_bootstrap::build_local_tee_bootstrap_submission_v2(
                client, request, signer, sign_node,
            ),
        }
    }
}
impl EnclaveChannel for StartupEnclave {
    fn request(
        &mut self,
        request: &outbe_tee::protocol::EnclaveRequest,
    ) -> Result<outbe_tee::protocol::EnclaveResponse, outbe_tee::TransportError> {
        match self {
            Self::Resident(context) => context.as_ref().request(request),
            Self::Sidecar(client) => client.request(request),
        }
    }
}
