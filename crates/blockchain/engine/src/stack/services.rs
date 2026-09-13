use super::*;

/// Type alias for the engine handle.
pub(super) type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;

/// Node-owned services consumed by one consensus stack runtime.
///
/// Keeping these lifecycle-coupled services together prevents the stack entry
/// point from growing one positional argument for every execution-side
/// subsystem.
pub struct ConsensusStackServices {
    pub(super) application_drain: crate::application_shutdown::ApplicationDrain,
    pub(super) follower_shutdown: Option<crate::follower_shutdown::FollowerDrain>,
    pub(super) projection_readiness: ProjectionReadinessHandle,
    pub(super) ocomp_readiness: Option<ProjectionReadinessHandle>,
    pub(super) retained_tribute_writer: Arc<RetainedTributeWriter>,
    pub(super) projection_retention_fence: Arc<ProjectionRetentionFence>,
    pub(super) retention_selector: Arc<SharedOcompRetentionSelector>,
    pub(super) finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
    pub(super) ce_startup_recovery: Arc<dyn CeStartupRecovery>,
    pub(super) radicle_status: outbe_radicle::integration::RadicleStatusHandle,
    pub(super) radicle_endpoint: Option<(
        outbe_radicle::integration::EndpointNetworkService,
        outbe_radicle::integration::LocalEndpointIdentityHandle,
        outbe_radicle::integration::EndpointTaskOwner,
    )>,
}

impl ConsensusStackServices {
    pub fn new(
        projection_readiness: ProjectionReadinessHandle,
        retained_tribute_writer: Arc<RetainedTributeWriter>,
        projection_retention_fence: Arc<ProjectionRetentionFence>,
        retention_selector: Arc<SharedOcompRetentionSelector>,
        finalized_ce_committer: Arc<dyn FinalizedCeCommitter>,
        ce_startup_recovery: Arc<dyn CeStartupRecovery>,
    ) -> Self {
        Self {
            application_drain: crate::application_shutdown::ApplicationDrain::default(),
            projection_readiness,
            follower_shutdown: None,
            ocomp_readiness: None,
            retained_tribute_writer,
            projection_retention_fence,
            retention_selector,
            finalized_ce_committer,
            ce_startup_recovery,
            radicle_status: outbe_radicle::integration::RadicleStatusChannel::disabled(),
            radicle_endpoint: None,
        }
    }

    /// Install the application pre-stop barrier shared with the runtime owner.
    #[must_use]
    pub fn with_application_drain(
        mut self,
        drain: crate::application_shutdown::ApplicationDrain,
    ) -> Self {
        self.application_drain = drain;
        self
    }

    /// Install the NodeHost-owned follower pre-stop handshake.
    #[must_use]
    pub fn with_follower_shutdown(
        mut self,
        shutdown: crate::follower_shutdown::FollowerDrain,
    ) -> Self {
        self.follower_shutdown = Some(shutdown);
        self
    }

    /// Installs the FullNode-only OCOMP execution barrier. Validator callers
    /// deliberately omit it.
    #[must_use]
    pub fn with_ocomp_readiness(mut self, readiness: ProjectionReadinessHandle) -> Self {
        self.ocomp_readiness = Some(readiness);
        self
    }

    #[must_use]
    pub fn with_radicle(
        mut self,
        status: outbe_radicle::integration::RadicleStatusHandle,
        endpoint: outbe_radicle::integration::EndpointNetworkService,
        local: outbe_radicle::integration::LocalEndpointIdentityHandle,
        owner: outbe_radicle::integration::EndpointTaskOwner,
    ) -> Self {
        self.radicle_status = status;
        self.radicle_endpoint = Some((endpoint, local, owner));
        self
    }
}
