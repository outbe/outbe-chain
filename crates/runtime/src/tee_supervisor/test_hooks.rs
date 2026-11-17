//! Explicit test-hooks seam into the same local authority owner used by runtime.
use super::*;

pub struct LifecycleAuthorityProbe {
    owner: LiveTeeRuntimeV1,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleAuthorityProbeError {
    #[error("authority probe construction failed")]
    Construction(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("local authority refresh failed")]
    Refresh(#[source] eyre::Report),
}
impl LifecycleAuthorityProbe {
    pub fn new(
        context: Arc<InProcessTeeContext>,
        identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
        anchor: (u64, B256),
        authority: outbe_tee::TeeAuthorityChannel,
    ) -> Result<Self, LifecycleAuthorityProbeError> {
        Ok(Self {
            owner: LiveTeeRuntimeV1::new(
                crate::admission_authority::live::LiveTeeSnapshotV1 {
                    context,
                    identity,
                    anchors: crate::admission::TeeLeaseGuardAnchorsV1 {
                        gate: crate::admission::TeeLeaseGuardGateV1::Pending(
                            LocalTeeAdmissionAnchorV1 {
                                finalized_height: anchor.0,
                                finalized_hash: anchor.1,
                            },
                        ),
                        durable: None,
                    },
                },
                authority,
            )
            .map_err(|source| LifecycleAuthorityProbeError::Construction(Box::new(source)))?,
        })
    }
    pub fn refresh<P>(
        &self,
        provider: &P,
        protocol: &crate::OutbeRuntime,
        chain: &outbe_execution::tee_remote_session::RegistryChainIdentity,
    ) -> Result<outbe_tee::TeeAuthorityState, LifecycleAuthorityProbeError>
    where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory,
    {
        self.owner
            .refresh(provider, protocol, chain)
            .map_err(LifecycleAuthorityProbeError::Refresh)
    }
}
