use super::*;

/// Type alias for the engine handle.
pub type EngineHandle = ConsensusEngineHandle<OutbePayloadTypes>;

/// Node-owned services consumed by one consensus stack runtime.
///
/// Keeping these lifecycle-coupled services together prevents the stack entry
/// point from growing one positional argument for every execution-side
/// subsystem.
pub struct ConsensusStackServices {
    pub application_drain: crate::application_shutdown::ApplicationDrain,
    pub follower_shutdown: Option<crate::follower_shutdown::FollowerDrain>,
    pub finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
    pub finalization_notify: Arc<tokio::sync::Notify>,
    pub tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
    pub radicle_status: outbe_radicle::integration::RadicleStatusHandle,
    pub radicle_endpoint: Option<(
        outbe_radicle::integration::EndpointNetworkService,
        outbe_radicle::integration::LocalEndpointIdentityHandle,
        outbe_radicle::integration::EndpointTaskOwner,
    )>,
    pub protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
}

/// Execution-side services shared with the node's consensus stack.
pub struct ConsensusExecutionServices {
    /// Proves native durable execution before acknowledging finality.
    pub finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
    /// Wakes lease admission on canonical finalized progress.
    pub finalization_notify: Arc<tokio::sync::Notify>,
    /// True only after local validator TEE admission is ready.
    pub tee_lease_armed_rx: tokio::sync::watch::Receiver<bool>,
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
        protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
    ) -> Self {
        let ConsensusExecutionServices {
            finalized_execution_barrier,
            finalization_notify,
            tee_lease_armed_rx,
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
            finalized_execution_barrier,
            finalization_notify,
            tee_lease_armed_rx,
            radicle_status,
            radicle_endpoint,
            protocol,
        }
    }
}

/// Execution-side capabilities consumed only by the certified follower.
pub struct FollowerStackServices {
    pub finalized_execution_barrier: Arc<dyn FinalizedExecutionBarrier>,
    pub follower_shutdown: crate::follower_shutdown::FollowerDrain,
    pub parent_cert_store: crate::finalization::parent_cert_store::FinalizedParentCertStore,
    pub bootstrap: Option<super::follower::ValidatorBootstrap>,
    pub finalization_notify: Arc<tokio::sync::Notify>,
    pub protocol: Arc<dyn crate::protocol_state::EngineProtocolState>,
}

/// The execution node and the transport selected for its follower stack.
pub struct FollowerConnection<N> {
    pub node: N,
    pub bridge: ConsensusExecutionBridge,
    pub upstream: String,
}
