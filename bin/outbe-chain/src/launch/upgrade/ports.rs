use super::*;
use alloy_primitives::B256;
use outbe_node::tee_remote_session::{
    construct_local_finalized_replacement_authorization_with_view_v1,
    inspect_local_finalized_successor_status_v1, LocalFinalizedSuccessorStatusV1,
    LocalRegistryAdmissionError, ReplacementAuthorizationRequest,
};
use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, NodeIdV1};
use outbe_tee::{FinalizedRegistryViewV1, FinalizedReplacementAuthorizationV1};

pub(super) struct PromotionAuthorization<A> {
    pub(super) authorization: A,
    pub(super) view: FinalizedRegistryViewV1,
}

/// External finalized authority and durable host operations used by the watcher.
pub(super) trait UpgradePromotionIo {
    type Authorization;

    fn inspect_journal(
        &self,
    ) -> eyre::Result<Option<outbe_operator::tee::UpgradeJournalSnapshotV1>>;
    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1>;
    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError>;
    fn replacement_authorization(
        &self,
        node_id: &NodeIdV1,
    ) -> Result<PromotionAuthorization<Self::Authorization>, LocalRegistryAdmissionError>;
    fn promote_candidate(&self, authorization: &Self::Authorization) -> eyre::Result<()>;
    fn record_finalized(&self, height: u64, hash: B256) -> eyre::Result<()>;
    fn record_promoted(&self) -> eyre::Result<()>;
    fn record_missed_cutoff(&self, height: u64, activation_height: u64) -> eyre::Result<()>;
}

pub(super) struct NodeUpgradeIo<P> {
    pub(super) provider: P,
    pub(super) chain: RegistryChainIdentity,
    pub(super) node_data_dir: PathBuf,
}

impl<P> UpgradePromotionIo for NodeUpgradeIo<P>
where
    P: HeaderProvider<Header = OutbeHeader> + StateProviderFactory,
{
    type Authorization = FinalizedReplacementAuthorizationV1;

    fn inspect_journal(
        &self,
    ) -> eyre::Result<Option<outbe_operator::tee::UpgradeJournalSnapshotV1>> {
        inspect_upgrade_journal_v1(&self.node_data_dir)
    }

    fn committed_manifest(&self) -> eyre::Result<EnclaveInitializationManifestV1> {
        Ok(outbe_tee::load_committed_enclave_manifest_v1(
            &self.node_data_dir,
        )?)
    }

    fn successor_status(
        &self,
    ) -> Result<LocalFinalizedSuccessorStatusV1, LocalRegistryAdmissionError> {
        inspect_local_finalized_successor_status_v1(&self.provider, self.chain)
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
