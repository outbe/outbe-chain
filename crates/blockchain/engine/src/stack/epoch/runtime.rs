//! Epoch state, DKG lifecycle, routed channels and supervised actor ownership.
use super::super::*;
use super::transport::{ChannelMux, EpochSubchannels};
/// Active authority and the rotation schedule anchored by its last activation.
pub(super) struct EpochState {
    pub(super) current_epoch: Epoch,
    pub(super) validator_set: validators::ValidatorSet,
    pub(super) participants: commonware_utils::ordered::Set<bls12381::PublicKey>,
    pub(super) signing_share: Option<Share>,
    pub(super) polynomial: Sharing<MinSig>,
    pub(super) last_dkg_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub(super) vrf_material_version: u64,
    pub(super) last_dkg_activation_height: u64,
    pub(super) dkg_cycle: u64,
}
/// A frozen target survives failed ceremonies until authenticated activation.
pub(super) struct DkgRotation<E: Clock> {
    pub(super) reshare_in_progress: bool,
    pub(super) frozen_dkg_target: Option<FrozenDkgTarget>,
    pub(super) pending_dkg_activation: Option<PendingDkgActivation>,
    pub(super) dealer_only_dkg_activation: Option<DealerOnlyDkgActivation>,
    pub(super) deferred_startup_pending_epoch: Option<Epoch>,
    pub(super) retry_frozen_dkg: bool,
    pub(super) dkg_mux: ChannelMux<E>,
    pub(super) dkg_result_tx: tokio::sync::mpsc::UnboundedSender<Result<DkgTaskOutcome>>,
    pub(super) dkg_result_rx: tokio::sync::mpsc::UnboundedReceiver<Result<DkgTaskOutcome>>,
    pub(super) dkg_progress_tx: tokio::sync::mpsc::UnboundedSender<dkg_actor::DkgProgress>,
    pub(super) dkg_progress_rx: tokio::sync::mpsc::UnboundedReceiver<dkg_actor::DkgProgress>,
}
/// Keep same-epoch replacement routes separate from a future DKG epoch.
pub(super) struct EpochChannels<E: Clock> {
    pub(super) vote_mux: ChannelMux<E>,
    pub(super) cert_mux: ChannelMux<E>,
    pub(super) res_mux: ChannelMux<E>,
    pub(super) next_epoch_subchannels: Option<EpochSubchannels<E>>,
    pub(super) replacement_epoch_subchannels: Option<EpochSubchannels<E>>,
}
/// These actors survive Simplex restarts and are monitored for fatal exits.
pub(super) struct PersistentActors {
    pub(super) network_handle: commonware_runtime::Handle<()>,
    pub(super) executor_handle_task: commonware_runtime::Handle<Result<()>>,
    pub(super) handler_handle: commonware_runtime::Handle<Result<()>>,
    pub(super) finalization_handle: commonware_runtime::Handle<
        std::result::Result<(), outbe_primitives::error::PrecompileError>,
    >,
    pub(super) peer_manager_handle_task: commonware_runtime::Handle<()>,
    pub(super) marshal_handle: commonware_runtime::Handle<()>,
}
/// Application-level control flow returned by domain operations.
pub(super) enum EventAction {
    /// Finish this event without running another operation on the same height.
    Continue,
    /// The operation completed; the next operation may inspect this height.
    Proceed,
    Outcome(EpochLoopOutcome),
}
pub(super) struct EpochSupervisor<E: Clock> {
    pub(super) state: EpochState,
    pub(super) rotation: DkgRotation<E>,
    pub(super) channels: EpochChannels<E>,
    pub(super) actors: PersistentActors,
    pub(super) args: ConsensusArgs,
    pub(super) node: OutbeFullNode,
    pub(super) bridge: ConsensusExecutionBridge,
    pub(super) signing_key: bls12381::PrivateKey,
    pub(super) key_backend: bls::KeyBackend,
    pub(super) dkg_rotation_params: DkgRotationParams,
    pub(super) dkg_manager: DkgManagerMailbox,
    pub(super) vrf_materials: VrfMaterialProvider<MinSig>,
    pub(super) vrf_safety: VrfSafetyGate,
    pub(super) application_epoch_fence: ApplicationEpochFence,
    pub(super) certificate_scheme_provider: HybridSchemeProvider<MinSig>,
    pub(super) elector_config_provider: HybridElectorConfigProvider<MinSig>,
    pub(super) committee_provider: CommitteeProvider,
    pub(super) peer_manager_mailbox: crate::peer_manager::Mailbox,
    pub(super) bootnode_map: BTreeMap<Vec<u8>, SocketAddr>,
    pub(super) oracle: lookup::Oracle<bls12381::PublicKey>,
    pub(super) recovered_boundary_artifact: Option<DkgBoundaryArtifact>,
    pub(super) reporter_continuity: ReporterContinuity,
    pub(super) genesis_hash: B256,
    pub(super) bt: super::super::startup::BlockTiming,
    pub(super) page_cache: CacheRef,
    pub(super) application: OutbeApplication<E>,
    pub(super) marshal_mailbox: outbe_consensus::marshal_types::MarshalMailbox,
    pub(super) finalization_mailbox: outbe_consensus::finalization::ingress::Mailbox,
    pub(super) finalization_view: outbe_consensus::finalization::state::FinalizationViewHandle,
    pub(super) finalized_parent_cert_store:
        outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore,
    pub(super) finalize_verify_mailbox:
        outbe_consensus::finalization::finalize_verify::FinalizeVerifyMailbox,
    pub(super) application_drain: crate::application_shutdown::ApplicationDrain,
    pub(super) radicle_status: outbe_radicle::integration::RadicleStatusHandle,
    pub(super) radicle_updates:
        tokio::sync::watch::Receiver<outbe_radicle::integration::RadicleStatusSnapshot>,
    pub(super) execution_finalized_height_rx: tokio::sync::mpsc::UnboundedReceiver<u64>,
    pub(super) execution_finalized_height_tx: tokio::sync::mpsc::UnboundedSender<u64>,
    pub(super) consensus_tip_rx:
        tokio::sync::watch::Receiver<Option<crate::marshal_update_reporter::ConsensusTip>>,
}
