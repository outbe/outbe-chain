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
    pub(super) ocomp_readiness: Option<ProjectionReadinessHandle>,
    pub(super) retention_selector: Arc<SharedOcompRetentionSelector>,
    pub(super) finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
    pub(super) radicle_status: outbe_radicle::integration::RadicleStatusHandle,
    pub(super) radicle_endpoint: Option<(
        outbe_radicle::integration::EndpointNetworkService,
        outbe_radicle::integration::LocalEndpointIdentityHandle,
        outbe_radicle::integration::EndpointTaskOwner,
    )>,
}

/// Execution-side services shared with the node's consensus stack.
pub struct ConsensusExecutionServices {
    /// FullNode execution barrier; validator callers supply `None`.
    pub ocomp_readiness: Option<ProjectionReadinessHandle>,
    /// Selects the OCOMP bodies retained by the node.
    pub retention_selector: Arc<SharedOcompRetentionSelector>,
    /// Proves native durable execution before acknowledging finality.
    pub finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
}

/// Shutdown barriers shared with the node runtime owner.
pub struct ConsensusShutdownServices {
    /// Drains application dependencies before transport stops.
    pub application_drain: crate::application_shutdown::ApplicationDrain,
    /// Pre-stop handshake required when starting a follower.
    pub follower_shutdown: Option<crate::follower_shutdown::FollowerDrain>,
}

/// Radicle endpoint capabilities installed together for one consensus stack.
pub struct ConsensusRadicleServices {
    /// Status consumed by consensus voting gates.
    pub status: outbe_radicle::integration::RadicleStatusHandle,
    /// Network service used by endpoint discovery.
    pub endpoint: outbe_radicle::integration::EndpointNetworkService,
    /// Identity advertised by the local endpoint.
    pub local_identity: outbe_radicle::integration::LocalEndpointIdentityHandle,
    /// Keeps endpoint tasks owned until they drain.
    pub task_owner: outbe_radicle::integration::EndpointTaskOwner,
}

impl ConsensusStackServices {
    /// Creates a complete service bundle before the consensus stack starts.
    pub fn new(
        execution: ConsensusExecutionServices,
        shutdown: ConsensusShutdownServices,
        radicle: Option<ConsensusRadicleServices>,
    ) -> Self {
        let ConsensusExecutionServices {
            ocomp_readiness,
            retention_selector,
            finalized_execution_barrier,
        } = execution;
        let ConsensusShutdownServices {
            application_drain,
            follower_shutdown,
        } = shutdown;
        let (radicle_status, radicle_endpoint) = match radicle {
            Some(ConsensusRadicleServices {
                status,
                endpoint,
                local_identity,
                task_owner,
            }) => (status, Some((endpoint, local_identity, task_owner))),
            None => (
                outbe_radicle::integration::RadicleStatusChannel::disabled(),
                None,
            ),
        };
        Self {
            application_drain,
            follower_shutdown,
            ocomp_readiness,
            retention_selector,
            finalized_execution_barrier,
            radicle_status,
            radicle_endpoint,
        }
    }
}

/// Execution-side capabilities consumed only by the certified follower.
pub(in crate::stack) struct FollowerStackServices {
    pub(in crate::stack) ocomp_readiness: Option<ProjectionReadinessHandle>,
    pub(in crate::stack) retention_selector: Arc<SharedOcompRetentionSelector>,
    pub(in crate::stack) finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
    pub(in crate::stack) follower_shutdown: crate::follower_shutdown::FollowerDrain,
}

/// The execution node and the transport selected for its follower stack.
pub(in crate::stack) struct FollowerConnection {
    pub(in crate::stack) node: OutbeFullNode,
    pub(in crate::stack) bridge: ConsensusExecutionBridge,
    pub(in crate::stack) upstream: String,
}
