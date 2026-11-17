//! Autonomous service transactions for one continuous resident VM lifetime.
mod driver;
mod engine;
mod retry;
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
        (operator, node): (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        (live, node_data_dir, descriptor): (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
    ) -> Result<Self, RuntimeSupervisorError> {
        let chain_id = alloy_primitives::U256::from_be_bytes(live.manifest().chain_id)
            .try_into()
            .map_err(|_| RuntimeSupervisorError::InvalidChainId)?;
        Ok(Self {
            port: RpcLifecyclePort::new(upstream, chain_id, live.manifest().genesis_hash)?,
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
                if self.successor.as_ref().is_some_and(|s| s.finalized) =>
            {
                Some(self.complete_cold_recovery()?)
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
        self.supervisor
            .publish_successor(&self.owner, &self.provider, &self.protocol)?;
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

impl RuntimeSupervisorError {
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
