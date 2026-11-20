//! Autonomous service transactions for one continuous resident VM lifetime.
#[cfg(test)]
#[path = "tee_supervisor/boundary_artifact.rs"]
mod boundary_artifact;
mod driver;
mod engine;
#[cfg(test)]
#[path = "tee_supervisor/founding_http.rs"]
mod founding_http;
mod retry;
#[cfg(test)]
#[path = "tee_supervisor/rollover_tests.rs"]
mod rollover_tests;
mod rpc;
mod signer;
mod successor;
#[cfg(feature = "test-hooks")]
pub(crate) mod test_hooks;

use crate::admission::LocalTeeAdmissionAnchorV1;
use crate::admission_authority::live::LiveTeeRuntimeV1;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1;
use outbe_tee::{
    in_process::InProcessTeeContext,
    lifecycle::{enrollment::EnrollmentOutcome, renewal::RenewalUpgradePhaseV1, *},
};
use rpc::RpcLifecyclePort;
use signer::RuntimeSigner;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

pub(crate) type PrepareSupervisorError = driver::PrepareError<RuntimeSupervisorError>;

pub(crate) struct TeeLifecycleSupervisor {
    port: RpcLifecyclePort,
    signer: RuntimeSigner,
    live: Arc<InProcessTeeContext>,
    journal: ServiceTxJournalV1,
    node_data_dir: PathBuf,
    descriptor: TrustedNetworkDescriptorV1,
    successor: Option<successor::ResidentSuccessor>,
    phase: RenewalUpgradePhaseV1,
}

impl TeeLifecycleSupervisor {
    pub(crate) fn new(
        upstream: &str,
        signers: (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        resident: (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
    ) -> Result<Self, RuntimeSupervisorError> {
        let chain_id = chain_id_of(&resident.0)?;
        let port = RpcLifecyclePort::new(upstream, chain_id, resident.0.manifest().genesis_hash)?;
        Self::assemble(port, signers, resident)
    }

    pub(crate) fn new_in_process(
        methods: jsonrpsee::Methods,
        signers: (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        resident: (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
    ) -> Result<Self, RuntimeSupervisorError> {
        let chain_id = chain_id_of(&resident.0)?;
        let port =
            RpcLifecyclePort::in_process(methods, chain_id, resident.0.manifest().genesis_hash);
        Self::assemble(port, signers, resident)
    }

    fn assemble(
        mut port: RpcLifecyclePort,
        (operator, node): (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        (live, node_data_dir, descriptor): (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
    ) -> Result<Self, RuntimeSupervisorError> {
        port.bind_network(descriptor.clone());
        Ok(Self {
            port,
            signer: RuntimeSigner { operator, node },
            journal: ServiceTxJournalV1::open(&node_data_dir)?,
            live,
            node_data_dir,
            descriptor,
            successor: None,
            phase: RenewalUpgradePhaseV1::Idle,
        })
    }

    async fn observe_tick(&mut self) -> Result<engine::Step, RuntimeSupervisorError> {
        let first = self.observe_once().await;
        if first
            .as_ref()
            .is_err_and(RuntimeSupervisorError::is_proof_window_moved)
        {
            return self.observe_once().await;
        }
        first
    }

    async fn observe_once(&mut self) -> Result<engine::Step, RuntimeSupervisorError> {
        if self.successor.is_none() && successor::cold_recovery_marked(&self.node_data_dir) {
            let promoted = matches!(self.phase, RenewalUpgradePhaseV1::Promoted { .. });
            let owns_live_binding = if promoted {
                false
            } else {
                self.owns_live_binding_now().await.unwrap_or(false)
            };
            if promoted || owns_live_binding {
                let _ = successor::clear_cold_marker(&self.node_data_dir);
            }
        }
        // A locally finalized cold successor is the pre-sync commit. Another
        // chain round would retry a dead port and never reach prepared.
        if self
            .successor
            .as_ref()
            .is_some_and(|pending| pending.activation.is_some() && pending.cold)
        {
            return Ok(engine::Step::SuccessorRequired);
        }
        if self.successor.is_some() {
            if let Some(renewal) = engine::maintain_source(
                &self.port,
                &self.signer,
                engine::ResidentSession {
                    live: &self.live,
                    journal: &mut self.journal,
                    node_data_dir: &self.node_data_dir,
                    phase: self.phase,
                },
            )
            .await?
            {
                return Ok(engine::Step::Renewal(renewal));
            }
            self.advance_successor().await?;
            return Ok(engine::Step::SuccessorRequired);
        }
        let step = engine::step(
            &self.port,
            &self.signer,
            engine::ResidentSession {
                live: &self.live,
                journal: &mut self.journal,
                node_data_dir: &self.node_data_dir,
                phase: self.phase,
            },
        )
        .await?;
        if matches!(step, engine::Step::SuccessorRequired) {
            self.advance_successor().await?;
        }
        Ok(step)
    }

    /// Reth must not execute protected blocks until this process imported the key.
    pub(crate) async fn prepare(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<LocalTeeAdmissionAnchorV1>, PrepareSupervisorError> {
        driver::prepare(self, cancellation, &tokio::sync::Notify::new())
            .await
            .map(|anchor| {
                anchor.map(|anchor| LocalTeeAdmissionAnchorV1 {
                    finalized_height: anchor.height,
                    finalized_hash: anchor.hash,
                })
            })
    }

    pub(crate) async fn run<P>(
        self,
        owner: Arc<LiveTeeRuntimeV1>,
        local: (P, Arc<crate::OutbeRuntime>),
        cancellation: CancellationToken,
        finalized: Arc<tokio::sync::Notify>,
    ) where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory
            + Send
            + Sync
            + 'static,
    {
        driver::run(
            LiveTask {
                supervisor: self,
                owner,
                provider: local.0,
                protocol: local.1,
            },
            cancellation,
            finalized,
        )
        .await;
    }
}

impl driver::LifecycleTask for TeeLifecycleSupervisor {
    type Error = RuntimeSupervisorError;
    fn is_retryable(error: &Self::Error) -> bool {
        error.is_retryable()
    }
    async fn tick(&mut self) -> Result<engine::Step, Self::Error> {
        self.observe_tick().await
    }
    fn prepared(
        &mut self,
        step: &engine::Step,
    ) -> Result<Option<driver::PreparedAnchor>, Self::Error> {
        let anchor = match step {
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { anchor }) => {
                Some(LocalTeeAdmissionAnchorV1 {
                    finalized_height: anchor.finalized_height,
                    finalized_hash: anchor.finalized_hash,
                })
            }
            engine::Step::SuccessorRequired
                if self
                    .successor
                    .as_ref()
                    .is_some_and(|s| s.activation.is_some()) =>
            {
                Some(self.complete_cold_recovery(None)?)
            }
            _ => None,
        };
        Ok(anchor.map(|a| driver::PreparedAnchor {
            height: a.finalized_height,
            hash: a.finalized_hash,
        }))
    }
}

struct LiveTask<P> {
    supervisor: TeeLifecycleSupervisor,
    owner: Arc<LiveTeeRuntimeV1>,
    provider: P,
    protocol: Arc<crate::OutbeRuntime>,
}
impl<P> driver::LifecycleTask for LiveTask<P>
where
    P: crate::BlockIdReader
        + crate::HeaderProvider<Header = crate::OutbeHeader>
        + crate::StateProviderFactory
        + Send
        + Sync
        + 'static,
{
    type Error = RuntimeSupervisorError;
    fn is_retryable(error: &Self::Error) -> bool {
        error.is_retryable()
    }
    async fn tick(&mut self) -> Result<engine::Step, Self::Error> {
        let step = self.supervisor.observe_tick().await?;
        if self.supervisor.cold_finalized() {
            self.supervisor
                .complete_cold_recovery(Some(self.owner.as_ref()))?;
        } else {
            self.supervisor
                .publish_successor(&self.owner, &self.provider, &self.protocol)?;
        }
        Ok(step)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeSupervisorError {
    #[error(transparent)]
    Rpc(#[from] rpc::RpcLifecycleError),
    #[error(transparent)]
    Journal(#[from] ServiceTxJournalError),
    #[error(transparent)]
    Renewal(
        #[from]
        outbe_tee::lifecycle::renewal::RenewalError<rpc::RpcLifecycleError, signer::SigningError>,
    ),
    #[error(transparent)]
    Observation(Box<engine::SupervisorError<rpc::RpcLifecycleError, signer::SigningError>>),
    #[error(transparent)]
    Successor(#[from] successor::SuccessorRuntimeError),
    #[error("chain id exceeds supported width")]
    InvalidChainId,
}

impl From<engine::SupervisorError<rpc::RpcLifecycleError, signer::SigningError>>
    for RuntimeSupervisorError
{
    fn from(source: engine::SupervisorError<rpc::RpcLifecycleError, signer::SigningError>) -> Self {
        Self::Observation(Box::new(source))
    }
}

fn chain_id_of(live: &InProcessTeeContext) -> Result<u64, RuntimeSupervisorError> {
    alloy_primitives::U256::from_be_bytes(live.manifest().chain_id)
        .try_into()
        .map_err(|_| RuntimeSupervisorError::InvalidChainId)
}

fn rpc_proof_window_moved(error: &rpc::RpcLifecycleError) -> bool {
    matches!(error, rpc::RpcLifecycleError::ProofWindowMoved { .. })
}

fn port_proof_window_moved(
    error: &outbe_tee::lifecycle::LifecyclePortError<rpc::RpcLifecycleError>,
) -> bool {
    match error {
        outbe_tee::lifecycle::LifecyclePortError::Source(error)
        | outbe_tee::lifecycle::LifecyclePortError::NonceTooLow { source: error }
        | outbe_tee::lifecycle::LifecyclePortError::TransactionDisplaced {
            source: error, ..
        } => rpc_proof_window_moved(error),
    }
}

fn renewal_proof_window_moved(
    error: &outbe_tee::lifecycle::renewal::RenewalError<
        rpc::RpcLifecycleError,
        signer::SigningError,
    >,
) -> bool {
    match error {
        outbe_tee::lifecycle::renewal::RenewalError::Port(error) => port_proof_window_moved(error),
        outbe_tee::lifecycle::renewal::RenewalError::Reconcile(
            outbe_tee::lifecycle::ReconcileError::Port(error),
        ) => port_proof_window_moved(error),
        _ => false,
    }
}

fn observation_proof_window_moved(
    error: &engine::SupervisorError<rpc::RpcLifecycleError, signer::SigningError>,
) -> bool {
    match error {
        engine::SupervisorError::Port(error) => port_proof_window_moved(error),
        engine::SupervisorError::Enrollment(
            outbe_tee::lifecycle::enrollment::EnrollmentError::Port(error),
        ) => port_proof_window_moved(error),
        engine::SupervisorError::Enrollment(
            outbe_tee::lifecycle::enrollment::EnrollmentError::Reconcile(
                outbe_tee::lifecycle::ReconcileError::Port(error),
            ),
        ) => port_proof_window_moved(error),
        engine::SupervisorError::Renewal(error) => renewal_proof_window_moved(error),
        _ => false,
    }
}

fn successor_proof_window_moved(
    error: &outbe_tee::lifecycle::successor::SuccessorError<
        rpc::RpcLifecycleError,
        signer::SigningError,
    >,
) -> bool {
    match error {
        outbe_tee::lifecycle::successor::SuccessorError::Lifecycle(error) => {
            renewal_proof_window_moved(error)
        }
        _ => false,
    }
}

impl RuntimeSupervisorError {
    /// `#[error(transparent)]` forwards `.source()` past the leaf, so a dynamic
    /// downcast never observes `RpcLifecycleError::ProofWindowMoved`.
    pub(super) fn is_proof_window_moved(&self) -> bool {
        match self {
            Self::Rpc(error) => rpc_proof_window_moved(error),
            Self::Successor(successor::SuccessorRuntimeError::Rpc(error)) => {
                rpc_proof_window_moved(error)
            }
            Self::Successor(successor::SuccessorRuntimeError::Port(error)) => {
                port_proof_window_moved(error)
            }
            Self::Successor(successor::SuccessorRuntimeError::Successor(error)) => {
                successor_proof_window_moved(error)
            }
            Self::Observation(error) => observation_proof_window_moved(error),
            Self::Renewal(error) => renewal_proof_window_moved(error),
            Self::Successor(_) | Self::Journal(_) | Self::InvalidChainId => false,
        }
    }

    fn is_retryable(&self) -> bool {
        match self {
            Self::Rpc(_) => true,
            Self::Observation(error) => retry::observation(error),
            Self::Renewal(error) => retry::renewal(error),
            Self::Successor(error) => error.is_retryable(),
            Self::Journal(_) | Self::InvalidChainId => false,
        }
    }
}
