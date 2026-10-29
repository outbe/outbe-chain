use crate::endpoint::{
    AnchorError, AnchorSnapshot, AuthorityRecord, EndpointAddress, PeerId, VerifiedEndpoint,
};
use alloy_primitives::{Address, B256};
use outbe_coderegistry::RepoId;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FinalizedBlock {
    pub number: u64,
    pub hash: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedPatchApproval {
    pub block: FinalizedBlock,
    pub approval: outbe_coderegistry::PatchApprovalRecord,
}

/// A receipt read from an exact local canonical finalized state, not a peer certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedPublicationReceipt {
    pub(crate) block: FinalizedBlock,
    pub(crate) receipt: outbe_coderegistry::publication::PublicationReceipt,
}

impl FinalizedPublicationReceipt {
    pub fn block(&self) -> FinalizedBlock {
        self.block
    }
    pub fn receipt(&self) -> &outbe_coderegistry::publication::PublicationReceipt {
        &self.receipt
    }
}

/// Only this node's current finalized state may supply an accepted receipt.
pub trait RepositoryReceiptReader: Send + Sync + 'static {
    fn read_governments_descriptor(
        &self,
    ) -> Result<Option<outbe_primitives::radicle_genesis::GovernmentsDescriptor>, ManagerError>
    {
        Err(ManagerError::Snapshot(
            "genesis descriptor unavailable".into(),
        ))
    }
    fn read_finalized_vote_page(
        &self,
        _: u64,
        _: u32,
    ) -> Result<Option<FinalizedVotePage>, ManagerError> {
        Err(ManagerError::Snapshot(
            "finalized Vote events unavailable".into(),
        ))
    }
    fn read_current_patch_approval(
        &self,
        _: alloy_primitives::U256,
    ) -> Result<Option<FinalizedPatchApproval>, ManagerError> {
        Err(ManagerError::Snapshot(
            "patch approval archive unavailable".into(),
        ))
    }
    fn read_current_publication_progress(
        &self,
        _: &outbe_primitives::radicle_publication::PublicationBody,
    ) -> Result<FinalizedPublicationProgress, ManagerError> {
        Err(ManagerError::Snapshot(
            "publication progress unavailable".into(),
        ))
    }
    fn read_current_publication_receipt(
        &self,
        sequence: u64,
    ) -> Result<Option<FinalizedPublicationReceipt>, ManagerError>;
}

/// Bounded observations of existing Approved Vote events, never permission to sign.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedVotePage {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub block: FinalizedBlock,
    pub next_offset: Option<u32>,
    pub proposals: Vec<alloy_primitives::U256>,
}

pub const MAX_VOTE_PAGE: usize = 32;

/// Delivery observations from one own canonical finalized state, never permission to publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedPublicationProgress {
    pub(crate) block: FinalizedBlock,
    pub(crate) body_hash: B256,
    pub(crate) attesters: Vec<u32>,
    pub(crate) receipt: Option<FinalizedPublicationReceipt>,
}

impl FinalizedPublicationProgress {
    pub fn block(&self) -> FinalizedBlock {
        self.block
    }
    pub fn body_hash(&self) -> B256 {
        self.body_hash
    }
    pub fn attesters(&self) -> &[u32] {
        &self.attesters
    }
    pub fn receipt(&self) -> Option<&FinalizedPublicationReceipt> {
        self.receipt.as_ref()
    }
}

/// Seeded descriptor and current signing set sampled from one canonical finalized state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedBootstrapSigningState {
    pub(crate) authority: FinalizedRepositoryAuthority,
    pub(crate) descriptor: outbe_primitives::radicle_genesis::GovernmentsDescriptor,
}

impl FinalizedBootstrapSigningState {
    pub fn authority(&self) -> &FinalizedRepositoryAuthority {
        &self.authority
    }
    pub fn descriptor(&self) -> &outbe_primitives::radicle_genesis::GovernmentsDescriptor {
        &self.descriptor
    }
}

/// All fields come from one current local finalized state used for a signing request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedPatchSigningState {
    pub(crate) authority: FinalizedRepositoryAuthority,
    pub(crate) approval: FinalizedPatchApproval,
    pub(crate) checkpoint: FinalizedPublicationReceipt,
    pub(crate) material: FinalizedPublicationReceipt,
    pub(crate) stage: u8,
    pub(crate) anchor: FinalizedBlock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedMaterialSigningState {
    pub(crate) authority: FinalizedRepositoryAuthority,
    pub(crate) checkpoint: FinalizedPublicationReceipt,
    pub(crate) material: FinalizedPublicationReceipt,
    pub(crate) anchor: FinalizedBlock,
}
impl FinalizedMaterialSigningState {
    pub fn authority(&self) -> &FinalizedRepositoryAuthority {
        &self.authority
    }
    pub fn checkpoint(&self) -> &FinalizedPublicationReceipt {
        &self.checkpoint
    }
    pub fn material(&self) -> &FinalizedPublicationReceipt {
        &self.material
    }
    pub fn anchor(&self) -> FinalizedBlock {
        self.anchor
    }
}

impl FinalizedPatchSigningState {
    pub fn authority(&self) -> &FinalizedRepositoryAuthority {
        &self.authority
    }
    pub fn approval(&self) -> &FinalizedPatchApproval {
        &self.approval
    }
    pub fn checkpoint(&self) -> &FinalizedPublicationReceipt {
        &self.checkpoint
    }
    pub fn material(&self) -> &FinalizedPublicationReceipt {
        &self.material
    }
    pub fn stage(&self) -> u8 {
        self.stage
    }
    pub fn anchor(&self) -> FinalizedBlock {
        self.anchor
    }
}

/// Constructed from this node's current canonical finalized state, not peer metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedRepositoryAuthority {
    pub(crate) block: FinalizedBlock,
    pub(crate) chain_id: u64,
    pub(crate) genesis_hash: B256,
    pub(crate) epoch: u64,
    pub(crate) committee: outbe_validatorset::state::CommitteeSnapshot,
}

impl FinalizedRepositoryAuthority {
    pub fn block(&self) -> FinalizedBlock {
        self.block
    }
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }
    pub fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn committee(&self) -> &outbe_validatorset::state::CommitteeSnapshot {
        &self.committee
    }
}

/// Samples this node's current finalized authority; callers cannot choose a historic set.
pub trait RepositoryAuthorityReader: Send + Sync {
    fn read_current_material_signing_state(
        &self,
    ) -> Result<FinalizedMaterialSigningState, ManagerError> {
        Err(ManagerError::Snapshot(
            "material binding state unavailable".into(),
        ))
    }
    fn read_current_bootstrap_signing_state(
        &self,
    ) -> Result<FinalizedBootstrapSigningState, ManagerError> {
        Err(ManagerError::Snapshot(
            "bootstrap signing state unavailable".into(),
        ))
    }

    fn read_current_repository_authority(
        &self,
    ) -> Result<FinalizedRepositoryAuthority, ManagerError>;
    fn read_current_patch_signing_state(
        &self,
        proposal_id: alloy_primitives::U256,
    ) -> Result<FinalizedPatchSigningState, ManagerError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedValidator {
    pub address: Address,
    pub peer: PeerId,
    pub node_id: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedSnapshot {
    pub block: FinalizedBlock,
    pub validators: Vec<FinalizedValidator>,
    pub registry_generation: u64,
    pub repositories: Vec<RepoId>,
}

impl FinalizedSnapshot {
    /// Projects bound validator authorities at this exact finalized block.
    pub fn endpoint_anchor(&self) -> Result<AnchorSnapshot, AnchorError> {
        AnchorSnapshot::new(
            self.block.number,
            self.block.hash,
            self.validators
                .iter()
                .filter_map(|validator| {
                    Some(AuthorityRecord {
                        validator: validator.address,
                        peer: validator.peer,
                        node_id: validator.node_id?,
                    })
                })
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectDisposition {
    Disconnected,
    AlreadyAbsent,
    NotConnected,
    Inbound,
    AddressChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlSession {
    pub node_id: [u8; 32],
    pub address: EndpointAddress,
    pub direction: SessionDirection,
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ManagerPhase {
    #[default]
    Disabled,
    JoiningUnbound,
    Ready,
    RuntimeDegraded,
}

pub use crate::errors::{ManagerError, PhaseError};

/// Maximum repositories seeded and probed in a single reconcile pass.
///
/// Bounds per-pass sidecar round trips (at most 16 `seed` and 16 `available`
/// calls) independent of how many repositories are registered on chain, while
/// rotating through the full registry across passes. Overrun repair ticks are
/// skipped by `PRODUCTION_REPAIR_INTERVAL`'s `MissedTickBehavior::Skip`.
pub const MAX_REPOSITORIES_PER_RECONCILE: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagerStatus {
    pub phase: ManagerPhase,
    pub phase_error: Option<PhaseError>,
    pub last_seen_finalized: Option<FinalizedBlock>,
    /// Finalized block for which desired peer sessions and the current pass's
    /// repository window converged.
    pub last_converged_finalized: Option<FinalizedBlock>,
    pub desired_repository_count: usize,
    pub observed_repository_count: usize,
    pub available_repository_count: usize,
    pub pending_repository_count: usize,
    pub resolved_peer_count: usize,
    pub unresolved_peer_count: usize,
    pub connected_peer_count: usize,
    pub finality_regressions: u64,
    pub finality_conflicts: u64,
    pub provider_failures: u64,
    pub endpoint_failures: u64,
    pub uds_failures: u64,
    pub tcp_status_failures: u64,
    pub signed_peers: Vec<VerifiedEndpoint>,
}

#[derive(Clone, Debug)]
pub struct ManagerConfig {
    pub self_validator: Address,
    pub local_node_id: [u8; 32],
    pub repair_interval: Duration,
    pub retry: RetryPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub base: Duration,
    pub max: Duration,
    pub jitter: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
            jitter: Duration::from_millis(250),
        }
    }
}

impl RetryPolicy {
    pub(crate) fn delay(
        self,
        attempt: u32,
        validator: Address,
        target: Option<FinalizedBlock>,
    ) -> Duration {
        let factor = 1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        let delay = self.base.saturating_mul(factor).min(self.max);
        let jitter_nanos = self.jitter.as_nanos().min(u128::from(u64::MAX)) as u64;
        if jitter_nanos == 0 {
            return delay;
        }
        let mut validator_tail = [0; 8];
        validator_tail.copy_from_slice(&validator.as_slice()[12..]);
        let mut entropy = u64::from_be_bytes(validator_tail) ^ u64::from(attempt);
        if let Some(target) = target {
            let mut hash_head = [0; 8];
            hash_head.copy_from_slice(&target.hash.as_slice()[..8]);
            entropy ^= target.number ^ u64::from_be_bytes(hash_head);
        }
        entropy ^= entropy << 13;
        entropy ^= entropy >> 7;
        entropy ^= entropy << 17;
        delay.saturating_add(Duration::from_nanos(entropy % (jitter_nanos + 1)))
    }
}

pub struct ManagerDependencies {
    pub finality: Arc<dyn FinalizedFeed>,
    pub snapshots: Arc<dyn SnapshotReader>,
    pub endpoints: Arc<dyn EndpointResolver>,
    pub control: Arc<dyn HeartwoodControl>,
    pub repository_status: Arc<dyn RepositoryStatus>,
}

pub trait FinalizedFeed: Send + Sync + 'static {
    fn subscribe(&self) -> Result<mpsc::UnboundedReceiver<FinalizedBlock>, ManagerError>;
    fn sample(&self) -> BoxFuture<'_, Result<Option<FinalizedBlock>, ManagerError>>;
}

pub trait SnapshotReader: Send + Sync + 'static {
    fn read_exact(&self, block: FinalizedBlock) -> Result<FinalizedSnapshot, ManagerError>;
}

pub trait EndpointResolver: Send + Sync + 'static {
    fn refresh<'a>(
        &'a self,
        snapshot: &'a FinalizedSnapshot,
    ) -> BoxFuture<'a, Result<Vec<VerifiedEndpoint>, ManagerError>>;
}

pub trait HeartwoodControl: Send + Sync + 'static {
    fn node_id(&self) -> BoxFuture<'_, Result<[u8; 32], ManagerError>>;
    fn sessions(&self) -> BoxFuture<'_, Result<Vec<ControlSession>, ManagerError>>;
    fn connect<'a>(
        &'a self,
        node_id: [u8; 32],
        addresses: &'a [EndpointAddress],
    ) -> BoxFuture<'a, Result<EndpointAddress, ManagerError>>;
    fn disconnect<'a>(
        &'a self,
        node_id: [u8; 32],
        expected: &'a EndpointAddress,
    ) -> BoxFuture<'a, Result<DisconnectDisposition, ManagerError>>;
    fn seed(&self, repo: RepoId) -> BoxFuture<'_, Result<(), ManagerError>>;
}

pub trait RepositoryStatus: Send + Sync + 'static {
    fn available(&self, repo: RepoId) -> BoxFuture<'_, Result<bool, ManagerError>>;
}
