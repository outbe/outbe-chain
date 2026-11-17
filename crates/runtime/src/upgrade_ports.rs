use super::*;
use alloy_primitives::B256;
use outbe_execution::tee_remote_session::RegistryChainIdentity;
use outbe_execution::tee_remote_session::{
    construct_local_finalized_replacement_authorization_with_view_v1,
    inspect_local_finalized_successor_status_v1, LocalFinalizedSuccessorStatusV1,
    LocalRegistryAdmissionError, ReplacementAuthorizationRequest,
};
use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, NodeIdV1};
use outbe_tee::{FinalizedRegistryViewV1, FinalizedReplacementAuthorizationV1};

pub(crate) struct PromotionAuthorization<A> {
    pub(crate) authorization: A,
    pub(crate) view: FinalizedRegistryViewV1,
}

/// External finalized authority and durable host operations used by the watcher.
pub(crate) trait UpgradePromotionIo {
    type Authorization;

    fn inspect_journal(&self) -> eyre::Result<Option<UpgradeJournalSnapshotV1>>;
    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1>;
    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError>;
    fn replacement_authorization(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError>;
    fn promote_candidate(&self, authorization: &Self::Authorization) -> eyre::Result<()>;
    /// Legacy durable adapter; the live adapter overrides this atomic boundary.
    fn promote_at(
        &self,
        authorization: &Self::Authorization,
        _view: FinalizedRegistryViewV1,
    ) -> eyre::Result<()> {
        self.promote_candidate(authorization)
    }
    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()>;
    fn record_promoted(&self) -> eyre::Result<()>;
    fn record_missed_cutoff(&self, height: u64, activation_height: u64) -> eyre::Result<()>;
}

pub(crate) struct NodeUpgradeIo<P> {
    pub(crate) provider: P,
    pub(crate) chain: RegistryChainIdentity,
    pub(crate) node_data_dir: PathBuf,
    pub(crate) session: Arc<dyn outbe_execution::tee_remote_session::TeeSessionStateReader>,
}

impl<P> UpgradePromotionIo for NodeUpgradeIo<P>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    type Authorization = FinalizedReplacementAuthorizationV1;

    fn inspect_journal(&self) -> eyre::Result<Option<UpgradeJournalSnapshotV1>> {
        Ok(inspect_upgrade_journal_v1(&self.node_data_dir)?)
    }

    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1> {
        Ok(outbe_tee::load_committed_enclave_manifest_v1(
            &self.node_data_dir,
        )?)
    }

    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError> {
        inspect_local_finalized_successor_status_v1(&self.provider, self.chain, &*self.session)
    }

    fn replacement_authorization(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError> {
        let authorized = construct_local_finalized_replacement_authorization_with_view_v1(
            &self.provider,
            self.chain,
            ReplacementAuthorizationRequest {
                node_data_dir: &self.node_data_dir,
                node_id,
            },
            &*self.session,
        )?;
        Ok(PromotionAuthorization {
            authorization: authorized.authorization,
            view: authorized.view,
        })
    }

    fn promote_candidate(&self, authorization: &Self::Authorization) -> eyre::Result<()> {
        outbe_tee::promote_replacement_candidate(&self.node_data_dir, authorization)?;
        Ok(())
    }

    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()> {
        record_upgrade_finalized_v1(&self.node_data_dir, height, hash)?;
        Ok(())
    }

    fn record_promoted(&self) -> eyre::Result<()> {
        record_upgrade_promoted_v1(&self.node_data_dir)?;
        Ok(())
    }

    fn record_missed_cutoff(&self, height: u64, activation_height: u64) -> eyre::Result<()> {
        record_upgrade_missed_cutoff_v1(&self.node_data_dir, height, activation_height)?;
        Ok(())
    }
}

/// The startup integrator supplies the same owner to the lease guard and this
/// adapter. Candidate construction and global dispatcher wiring stay outside it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "The startup integrator connects this adapter to the shared live owner in the next lifecycle stage."
    )
)]
pub(crate) struct LiveUpgradeIo<I, P> {
    pub(crate) inner: I,
    pub(crate) provider: P,
    pub(crate) protocol: Arc<dyn outbe_consensus::protocol_state::EngineProtocolState>,
    pub(crate) chain: RegistryChainIdentity,
    pub(crate) owner: Arc<crate::admission_authority::live::LiveTeeRuntimeV1>,
    pub(crate) candidate: Arc<outbe_tee::in_process::InProcessTeeContext>,
}

impl<I: UpgradePromotionIo, P> UpgradePromotionIo for LiveUpgradeIo<I, P>
where
    P: BlockIdReader + HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    type Authorization = I::Authorization;
    fn inspect_journal(&self) -> eyre::Result<Option<UpgradeJournalSnapshotV1>> {
        self.inner.inspect_journal()
    }
    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1> {
        self.inner.committed_manifest()
    }
    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError> {
        self.inner.successor_status()
    }
    fn replacement_authorization(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError> {
        self.inner.replacement_authorization(node_id)
    }
    fn promote_candidate(&self, _: &Self::Authorization) -> eyre::Result<()> {
        Err(crate::admission_authority::live::LiveTeeError::CandidateNotAdmitted.into())
    }
    fn promote_at(
        &self,
        authorization: &Self::Authorization,
        view: FinalizedRegistryViewV1,
    ) -> eyre::Result<()> {
        let result = (|| {
            let header = self.provider.sealed_header(view.block_number)?;
            let network = (view.chain_id, view.genesis_hash);
            let local_network = (
                alloy_primitives::U256::from(self.chain.chain_id).to_be_bytes(),
                self.chain.genesis_hash,
            );
            let manifest = self.candidate.manifest();
            let header_matches = header.is_some_and(|header| {
                (
                    header.hash(),
                    header.header().inner.timestamp,
                    header.header().inner.state_root,
                ) == (view.block_hash, view.consensus_timestamp, view.state_root)
            });
            if network != local_network
                || network != (manifest.chain_id, manifest.genesis_hash)
                || !header_matches
            {
                return Err(
                    crate::admission_authority::live::LiveTeeError::CandidateNotAdmitted.into(),
                );
            }
            let mut identity = self.owner.lock()?.identity;
            identity.expected_enclave_id = Some(self.candidate.manifest().enclave_id()?);
            let admission = crate::admission::read_local_tee_admission_at_block(
                self.protocol.as_ref(),
                &self.provider,
                self.chain,
                identity,
                (
                    alloy_eips::BlockNumHash::new(view.block_number, view.block_hash),
                    view.consensus_timestamp,
                ),
            )?;
            let canonical = outbe_consensus::validators::read_network_encryption_public_at_block(
                self.protocol.as_ref(),
                &self.provider,
                view.block_hash,
            )?;
            if self.provider.finalized_block_num_hash()?
                != Some(alloy_eips::BlockNumHash::new(
                    view.block_number,
                    view.block_hash,
                ))
            {
                return Err(
                    crate::admission_authority::live::LiveTeeError::FinalizedViewMismatch.into(),
                );
            }
            self.owner.promote(
                self.candidate.clone(),
                crate::admission::LocalTeeAdmissionAnchorV1 {
                    finalized_height: view.block_number,
                    finalized_hash: view.block_hash,
                },
                admission,
                canonical,
                || self.inner.promote_candidate(authorization),
            )?;
            Ok(())
        })();
        if result.is_err() {
            crate::admission_authority::publish_state(
                &self.owner.authority,
                outbe_tee::TeeAuthorityState::SuspendedPolicyMismatch,
            );
        }
        result
    }

    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()> {
        self.inner.record_finalized(height, hash)
    }
    fn record_promoted(&self) -> eyre::Result<()> {
        self.inner.record_promoted()
    }
    fn record_missed_cutoff(&self, height: u64, activation_height: u64) -> eyre::Result<()> {
        self.inner.record_missed_cutoff(height, activation_height)
    }
}
