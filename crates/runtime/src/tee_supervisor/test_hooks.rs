//! Explicit test-hooks seam into the same local authority owner used by runtime.
use super::*;

pub struct LifecycleAuthorityProbe {
    owner: Arc<LiveTeeRuntimeV1>,
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
            owner: Arc::new(
                LiveTeeRuntimeV1::new(
                    crate::admission_authority::live::LiveTeeSnapshotV1 {
                        context,
                        identity,
                        gate: crate::admission::TeeLeaseGuardGateV1::Pending(
                            LocalTeeAdmissionAnchorV1 {
                                finalized_height: anchor.0,
                                finalized_hash: anchor.1,
                            },
                        ),
                    },
                    authority,
                )
                .map_err(|source| LifecycleAuthorityProbeError::Construction(Box::new(source)))?,
            ),
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
    pub fn identity(
        &self,
    ) -> Result<outbe_consensus::validators::LocalTeeRuntimeIdentityV1, LifecycleAuthorityProbeError>
    {
        Ok(self
            .owner
            .lock()
            .map_err(|source| LifecycleAuthorityProbeError::Refresh(eyre::Report::new(source)))?
            .identity)
    }

    pub fn bind_sync_refresh<P>(
        &self,
        provider: P,
        protocol: Arc<crate::OutbeRuntime>,
        chain: outbe_execution::tee_remote_session::RegistryChainIdentity,
    ) where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory
            + Clone
            + Send
            + Sync
            + 'static,
    {
        self.owner.bind_sync_refresh(provider, protocol, chain);
    }

    pub fn domain_rpc<P>(&self, provider: Arc<P>, chain: (u64, B256)) -> jsonrpsee::Methods
    where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory
            + Send
            + Sync
            + 'static,
    {
        use crate::domain_rpc::OutbeDomainApiServer as _;
        crate::domain_rpc::OutbeDomainRpcHandler::new(provider, chain)
            .with_authority_watch(self.owner.authority.subscribe())
            .into_rpc()
            .into()
    }

    /// Exercise the HTTP-independent launch slot, including the production worker.
    pub fn spawn_founding<P>(
        &self,
        modules: (jsonrpsee::RpcModule<()>, jsonrpsee::RpcModule<()>),
        (args, rpc): (
            &outbe_consensus::args::ConsensusArgs,
            &reth_node_core::args::RpcServerArgs,
        ),
        signers: (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        resident: (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
        local: (P, CancellationToken),
    ) -> eyre::Result<tokio::task::JoinHandle<()>>
    where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory
            + Send
            + Sync
            + 'static,
    {
        use crate::tee::{
            prepare_direct_founding, start_lifecycle_worker, FoundingResident, FoundingRun,
            FoundingSlotRequest,
        };

        let slot = Arc::new(std::sync::Mutex::new(None));
        let evm_signer = Some(&signers.0);
        crate::launch::founding_lifecycle_stager(args, evm_signer.is_some(), Arc::clone(&slot))(
            modules.0, &modules.1,
        )?;
        let request = FoundingSlotRequest::from_launch(rpc, args, evm_signer);
        let founding = prepare_direct_founding(&slot, request, None, || {
            Ok(FoundingResident {
                node_signing: signers.1,
                live: resident.0,
                node_data_dir: resident.1,
                descriptor: resident.2,
            })
        })?;
        start_lifecycle_worker(
            &slot,
            request,
            (founding, None),
            FoundingRun {
                owner: Arc::clone(&self.owner),
                provider: local.0,
                protocol: Arc::new(crate::OutbeRuntime::new()),
                shutdown: local.1,
                finalized: Arc::new(tokio::sync::Notify::new()),
            },
            &outbe_execution::shutdown::NodeShutdown::default(),
        )
    }
}

/// Observe the role and identity selected by the real launch policy.
pub fn tee_launch_profile_for_test(
    args: &crate::ConsensusArgs,
    signer: Option<&crate::OutbeEvmSigner>,
    resident: outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
) -> (
    outbe_primitives::consensus::ConsensusOperatingMode,
    bool,
    outbe_consensus::validators::LocalTeeRuntimeIdentityV1,
) {
    (
        crate::launch::consensus_operating_mode(args.is_validator),
        crate::tee::keyless_follower_launch(args),
        crate::tee::local_tee_launch_identity(args, signer, resident),
    )
}

/// Invoke a single observation of the production lifecycle supervisor.
pub struct LifecycleSupervisorProbe(TeeLifecycleSupervisor);

impl LifecycleSupervisorProbe {
    pub fn new(
        methods: jsonrpsee::Methods,
        signers: (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey),
        resident: (
            Arc<InProcessTeeContext>,
            PathBuf,
            TrustedNetworkDescriptorV1,
        ),
    ) -> eyre::Result<Self> {
        Ok(Self(TeeLifecycleSupervisor::new_in_process(
            methods, signers, resident,
        )?))
    }

    pub async fn observe_enrollment(&mut self) -> eyre::Result<Option<EnrollmentOutcome>> {
        Ok(match self.0.observe_tick().await? {
            engine::Step::Enrollment(outcome) => Some(outcome),
            _ => None,
        })
    }

    pub async fn observe_renewal(
        &mut self,
    ) -> eyre::Result<Option<outbe_tee::lifecycle::renewal::RenewalStep>> {
        Ok(match self.0.observe_tick().await? {
            engine::Step::Renewal(outcome)
            | engine::Step::Resident {
                renewal: Some(outcome),
                ..
            } => Some(outcome),
            _ => None,
        })
    }

    pub async fn upgrade_key(
        &self,
        context: &outbe_tee::dcap_protocol::DcapOnboardingContextV1,
    ) -> eyre::Result<LifecycleOnboardingProofV1> {
        let proved = self.0.port.finalized_checkpoint().await?;
        let proof = self.0.port.upgrade_proof(proved, context).await?;
        let head = self.0.port.finalized_checkpoint().await?;
        Ok(self.0.port.upgrade_key(head, context, &proof).await?)
    }
}
