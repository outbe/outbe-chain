//! Durable Reth -> CE-MDBX finalization barrier.
//!
//! This module is deliberately outside the executor actor. The actor only owns
//! Marshal ordering; this adapter owns Reth's persistence notification, the
//! DB-only canonical/root check, and the CE tree commit which must all complete
//! before the actor may acknowledge the finalized block.

use std::sync::Arc;

use alloy_consensus::TxReceipt;
use alloy_eips::BlockNumHash;
use alloy_primitives::{B256, U256};
use futures::{future::BoxFuture, stream::BoxStream, FutureExt, StreamExt};
use outbe_compressed_entities::{
    decode_canonical_body_event, CompressedTreeService, DurableFinalizedCheckpoint,
    FinalizedCandidateOutcome, FinalizedMarker, StagedTreeBatch, ACTIVE_COMMITMENT_SCHEME,
};
use outbe_consensus::executor::actor::{FinalizedCeBlock, FinalizedCeCommitter};
use outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS;
use reth_chain_state::PersistedBlockSubscriptions;
use reth_provider::{BlockHashReader, DatabaseProviderFactory, ReceiptProvider};
use reth_storage_api::TryIntoHistoricalStateProvider;

use crate::ce_recovery::{CanonicalCeReplayBlock, CanonicalCeReplaySource};

/// The authoritative compressed-entity root is storage slot 1 at `0xEE0D`.
const CE_ROOT_SLOT: B256 = B256::new(U256::from_limbs([1, 0, 0, 0]).to_be_bytes::<32>());

/// Narrow, behavior-testable view of Reth's durable storage.
pub trait DurableCeState: Send + Sync {
    /// Subscribe before finalized processing starts so no persistence edge is missed.
    fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash>;

    /// Reads only durable canonical data. `None` means this height is not yet on disk.
    fn block_and_root(&self, height: u64) -> eyre::Result<Option<(B256, B256)>>;

    /// Reads and authenticates the canonical receipt history needed to
    /// reconstruct one exact finalized tree batch.
    fn replay_block(&self, height: u64) -> eyre::Result<Option<CanonicalCeReplayBlock>>;
}

/// Production Reth adapter. The block identity is checked through a fresh
/// read-only DB provider before historical state is opened for the same durable
/// hash, preventing an in-memory candidate from satisfying the barrier.
#[derive(Clone, Debug)]
pub struct RethDurableCeState<P> {
    provider: P,
}

impl<P> RethDurableCeState<P> {
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P> DurableCeState for RethDurableCeState<P>
where
    P: PersistedBlockSubscriptions + DatabaseProviderFactory + Clone + Send + Sync + 'static,
    P::Provider: BlockHashReader + ReceiptProvider + TryIntoHistoricalStateProvider,
{
    fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash> {
        self.provider.persisted_block_stream().boxed()
    }

    fn block_and_root(&self, height: u64) -> eyre::Result<Option<(B256, B256)>> {
        let durable = self
            .provider
            .database_provider_ro()
            .map_err(|error| eyre::eyre!("failed to open Reth DB-only provider: {error}"))?;
        let Some(block_hash) = durable
            .block_hash(height)
            .map_err(|error| eyre::eyre!("failed to read durable block {height}: {error}"))?
        else {
            return Ok(None);
        };
        // Consume this exact read-only transaction into the historical state
        // view. No in-memory blockchain-tree provider participates.
        let state = durable.try_into_history_at_block(height).map_err(|error| {
            eyre::eyre!(
                "failed to open durable historical state for {height}/{block_hash}: {error}"
            )
        })?;
        let root = state
            .storage(COMPRESSED_ENTITIES_ADDRESS, CE_ROOT_SLOT)
            .map_err(|error| {
                eyre::eyre!("failed to read durable CE root for {height}/{block_hash}: {error}")
            })?
            .map_or(B256::ZERO, |value| B256::from(value.to_be_bytes::<32>()));
        Ok(Some((block_hash, root)))
    }

    fn replay_block(&self, height: u64) -> eyre::Result<Option<CanonicalCeReplayBlock>> {
        CanonicalCeReplaySource::replay_block(self, height)
    }
}

impl<P> CanonicalCeReplaySource for RethDurableCeState<P>
where
    P: PersistedBlockSubscriptions + DatabaseProviderFactory + Clone + Send + Sync + 'static,
    P::Provider: BlockHashReader + ReceiptProvider + TryIntoHistoricalStateProvider,
{
    fn durable_checkpoint(
        &self,
        consensus_finalized_height: u64,
    ) -> eyre::Result<Option<DurableFinalizedCheckpoint>> {
        let Some((block_hash, root)) = self.block_and_root(consensus_finalized_height)? else {
            return Ok(None);
        };
        let (parent_block_hash, parent_root) = if consensus_finalized_height == 0 {
            (B256::ZERO, B256::ZERO)
        } else {
            self.block_and_root(consensus_finalized_height - 1)?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "durable parent checkpoint {} is missing",
                        consensus_finalized_height - 1
                    )
                })?
        };
        Ok(Some(DurableFinalizedCheckpoint {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: consensus_finalized_height,
            block_hash,
            root,
            parent_block_hash,
            parent_root,
            consensus_finalized_height,
        }))
    }

    fn replay_block(&self, height: u64) -> eyre::Result<Option<CanonicalCeReplayBlock>> {
        if height == 0 {
            eyre::bail!("height 0 is the CE replay baseline, not a replay block");
        }
        let Some((hash, new_root)) = self.block_and_root(height)? else {
            return Ok(None);
        };
        let Some((parent_hash, parent_root)) = self.block_and_root(height - 1)? else {
            eyre::bail!(
                "durable canonical parent {} is unavailable while replaying {height}/{hash}",
                height - 1
            );
        };

        // Receipts are read from a fresh DB-only provider. Blockchain-tree or
        // ExEx memory cannot fill a pruned/missing replay row.
        let durable = self
            .provider
            .database_provider_ro()
            .map_err(|error| eyre::eyre!("failed to open DB-only receipt provider: {error}"))?;
        let receipts = durable
            .receipts_by_block(hash.into())
            .map_err(|error| {
                eyre::eyre!("failed to read durable receipts for {height}/{hash}: {error}")
            })?
            .ok_or_else(|| eyre::eyre!("durable receipts missing for {height}/{hash}"))?;
        let mut events = Vec::new();
        for receipt in receipts {
            for log in receipt.logs() {
                if let Some(event) = decode_canonical_body_event(log.address, &log.data)? {
                    events.push(event);
                }
            }
        }

        Ok(Some(CanonicalCeReplayBlock {
            number: height,
            hash,
            parent_hash,
            parent_root,
            new_root,
            events,
        }))
    }
}

/// Narrow tree ownership seam used to test ordering and failure behavior.
pub trait FinalizedCeTree: Send + Sync {
    fn finalized_marker(&self) -> eyre::Result<FinalizedMarker>;

    fn candidate(&self, height: u64, hash: B256) -> eyre::Result<Option<Arc<StagedTreeBatch>>>;

    fn apply_finalized(
        &self,
        height: u64,
        hash: B256,
        authoritative_root: B256,
    ) -> eyre::Result<FinalizedMarker>;

    fn apply_replayed(&self, block: &CanonicalCeReplayBlock) -> eyre::Result<FinalizedMarker>;
}

impl FinalizedCeTree for CompressedTreeService {
    fn finalized_marker(&self) -> eyre::Result<FinalizedMarker> {
        CompressedTreeService::finalized_marker(self).map_err(Into::into)
    }

    fn candidate(&self, height: u64, hash: B256) -> eyre::Result<Option<Arc<StagedTreeBatch>>> {
        CompressedTreeService::candidate(self, height, hash).map_err(Into::into)
    }

    fn apply_finalized(
        &self,
        height: u64,
        hash: B256,
        authoritative_root: B256,
    ) -> eyre::Result<FinalizedMarker> {
        CompressedTreeService::apply_finalized(self, height, hash, authoritative_root)
            .map(FinalizedCandidateOutcome::marker)
            .map_err(Into::into)
    }

    fn apply_replayed(&self, block: &CanonicalCeReplayBlock) -> eyre::Result<FinalizedMarker> {
        crate::ce_recovery::apply_replayed_block(self, block)
    }
}

/// Single serialized finalization coordinator shared by validator/follower
/// executor wiring.
pub struct RethCeFinalizer {
    state: Arc<dyn DurableCeState>,
    persisted: Arc<futures::lock::Mutex<BoxStream<'static, BlockNumHash>>>,
    tree: Arc<dyn FinalizedCeTree>,
    finalization: Arc<futures::lock::Mutex<()>>,
}

impl std::fmt::Debug for RethCeFinalizer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RethCeFinalizer")
            .finish_non_exhaustive()
    }
}

impl RethCeFinalizer {
    /// Captures the persistence subscription eagerly, before any finalized
    /// delivery can enter the executor actor.
    pub fn new(state: Arc<dyn DurableCeState>, tree: Arc<dyn FinalizedCeTree>) -> Self {
        let persisted = state.persisted_blocks();
        Self {
            state,
            persisted: Arc::new(futures::lock::Mutex::new(persisted)),
            tree,
            finalization: Arc::new(futures::lock::Mutex::new(())),
        }
    }

    async fn commit(&self, block: FinalizedCeBlock) -> eyre::Result<()> {
        let _serial = self.finalization.lock().await;
        let current = self.tree.finalized_marker()?;
        if current.height == block.height {
            if current.commitment_scheme_version != ACTIVE_COMMITMENT_SCHEME
                || current.block_hash != block.block_hash
                || current.parent_block_hash != block.parent_block_hash
            {
                eyre::bail!(
                    "redelivered finalized CE block conflicts with the durable marker at {}/{}: {current:?}",
                    block.height,
                    block.block_hash
                );
            }
            let authoritative_root = match self.verify_durable(block)? {
                Some(root) => root,
                None => {
                    self.wait_for_exact_persistence(block).await?;
                    self.verify_durable(block)?.ok_or_else(|| {
                        eyre::eyre!(
                            "Reth emitted persistence for redelivered block {}/{}, but DB-only state is absent",
                            block.height,
                            block.block_hash
                        )
                    })?
                }
            };
            if current.new_root != authoritative_root {
                eyre::bail!(
                    "redelivered finalized CE block root conflicts with the durable marker at {}/{}: Reth={}, marker={}",
                    block.height,
                    block.block_hash,
                    authoritative_root,
                    current.new_root
                );
            }
            let (durable_parent_hash, durable_parent_root) = self
                .state
                .block_and_root(block.height.saturating_sub(1))?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "durable parent is absent while validating redelivered finalized CE block {}/{}",
                        block.height,
                        block.block_hash
                    )
                })?;
            if current.parent_block_hash != durable_parent_hash
                || current.parent_root != durable_parent_root
            {
                eyre::bail!(
                    "redelivered finalized CE block parent identity conflicts with the durable marker at {}/{}: Reth=({durable_parent_hash}, {durable_parent_root}), marker=({}, {})",
                    block.height,
                    block.block_hash,
                    current.parent_block_hash,
                    current.parent_root
                );
            }
            return Ok(());
        }
        if current.height > block.height {
            eyre::bail!(
                "redelivered finalized CE block is behind the durable marker: block={}/{}, marker={current:?}",
                block.height,
                block.block_hash
            );
        }
        let authoritative_root = match self.verify_durable(block)? {
            Some(root) => root,
            None => {
                self.wait_for_exact_persistence(block).await?;
                self.verify_durable(block)?.ok_or_else(|| {
                    eyre::eyre!(
                        "Reth emitted persistence for {}/{}, but DB-only state is absent",
                        block.height,
                        block.block_hash
                    )
                })?
            }
        };

        let marker = if let Some(candidate) = self.tree.candidate(block.height, block.block_hash)? {
            if candidate.parent_block_hash() != block.parent_block_hash {
                eyre::bail!(
                    "finalized CE candidate parent conflict at {}/{}: actor={}, candidate={}",
                    block.height,
                    block.block_hash,
                    block.parent_block_hash,
                    candidate.parent_block_hash()
                );
            }
            if candidate.new_root() != authoritative_root {
                eyre::bail!(
                    "durable EVM/CE candidate root conflict at {}/{}: evm={}, candidate={}",
                    block.height,
                    block.block_hash,
                    authoritative_root,
                    candidate.new_root()
                );
            }
            self.tree
                .apply_finalized(block.height, block.block_hash, authoritative_root)?
        } else {
            // Validator/import execution must not publish before Reth's receipt
            // and state-root checks. Once the block is durable and exact, rebuild
            // the same batch from its canonical receipts instead of trusting a
            // speculative executor artifact.
            let replay = self.state.replay_block(block.height)?.ok_or_else(|| {
                eyre::eyre!(
                    "durable canonical CE replay missing for finalized block {}/{}",
                    block.height,
                    block.block_hash
                )
            })?;
            if replay.number != block.height
                || replay.hash != block.block_hash
                || replay.parent_hash != block.parent_block_hash
                || replay.new_root != authoritative_root
                || replay.parent_hash != current.block_hash
                || replay.parent_root != current.new_root
            {
                eyre::bail!(
                    "durable canonical CE replay identity/root conflict for finalized block {}/{}: current={current:?}, replay={replay:?}, authoritative_root={authoritative_root}",
                    block.height,
                    block.block_hash
                );
            }
            self.tree.apply_replayed(&replay)?
        };
        if marker.commitment_scheme_version != ACTIVE_COMMITMENT_SCHEME
            || marker.height != block.height
            || marker.block_hash != block.block_hash
            || marker.parent_block_hash != block.parent_block_hash
            || marker.parent_root != current.new_root
            || marker.new_root != authoritative_root
        {
            eyre::bail!(
                "CE MDBX returned conflicting finalized marker for {}/{}: {marker:?}",
                block.height,
                block.block_hash
            );
        }
        Ok(())
    }

    fn verify_durable(&self, block: FinalizedCeBlock) -> eyre::Result<Option<B256>> {
        let Some((durable_hash, root)) = self.state.block_and_root(block.height)? else {
            return Ok(None);
        };
        if durable_hash != block.block_hash {
            eyre::bail!(
                "durable canonical conflict at height {}: finalized={}, Reth={}",
                block.height,
                block.block_hash,
                durable_hash
            );
        }
        Ok(Some(root))
    }

    async fn wait_for_exact_persistence(&self, block: FinalizedCeBlock) -> eyre::Result<()> {
        let mut persisted = self.persisted.lock().await;
        while let Some(notification) = persisted.next().await {
            if notification.number < block.height {
                continue;
            }
            if notification.number == block.height && notification.hash == block.block_hash {
                return Ok(());
            }
            if notification.number > block.height && self.verify_durable(block)?.is_some() {
                // This is a watch stream, so a slow receiver may observe a
                // later durable tip. The target is accepted only after a fresh
                // DB-only transaction proves its exact canonical identity.
                return Ok(());
            }
            eyre::bail!(
                "Reth persistence passed finalized CE block {}/{} with {}/{}",
                block.height,
                block.block_hash,
                notification.number,
                notification.hash
            );
        }
        eyre::bail!(
            "Reth persistence stream ended before finalized CE block {}/{}",
            block.height,
            block.block_hash
        )
    }
}

impl FinalizedCeCommitter for RethCeFinalizer {
    fn commit_finalized(&self, block: FinalizedCeBlock) -> BoxFuture<'static, eyre::Result<()>> {
        let this = Self {
            state: Arc::clone(&self.state),
            persisted: Arc::clone(&self.persisted),
            tree: Arc::clone(&self.tree),
            finalization: Arc::clone(&self.finalization),
        };
        async move { this.commit(block).await }.boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use futures::channel::mpsc;
    use outbe_compressed_entities::{
        CandidateCacheLimits, CeMdbx, Commitment, CompressedTreeService, EntityId36, EntityRef,
        EnvironmentIdentity, ExactParentIdentity, FinalLeafMutation, ACTIVE_COMMITMENT_SCHEME,
        LOCAL_STORAGE_SCHEMA_VERSION,
    };

    use super::*;

    fn hash(last: u8) -> B256 {
        let mut value = [0_u8; 32];
        value[31] = last;
        B256::from(value)
    }

    fn finalized_block() -> FinalizedCeBlock {
        FinalizedCeBlock {
            height: 1,
            block_hash: hash(2),
            parent_block_hash: hash(1),
        }
    }

    fn candidate(root: B256) -> Arc<StagedTreeBatch> {
        Arc::new(
            outbe_compressed_entities::bench_support::staged_batch(
                1,
                hash(2),
                hash(1),
                B256::ZERO,
                root,
                usize::from(root != B256::ZERO),
            )
            .unwrap(),
        )
    }

    fn real_tree_service() -> (tempfile::TempDir, Arc<CompressedTreeService>) {
        let directory = tempfile::tempdir().unwrap();
        let db = CeMdbx::open(
            directory.path(),
            EnvironmentIdentity {
                local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                chain_id: 1,
                genesis_hash: hash(1),
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                shard_count: 1,
                tree_format: "ckb-smt-v0.6.1-poseidon-unsharded-control".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: hash(1),
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: B256::ZERO,
            },
        )
        .unwrap();
        let service = CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap();
        (directory, Arc::new(service))
    }

    struct FakeDurableState {
        stream: Mutex<Option<mpsc::UnboundedReceiver<BlockNumHash>>>,
        blocks: Mutex<BTreeMap<u64, (B256, B256)>>,
        replay: Mutex<BTreeMap<u64, CanonicalCeReplayBlock>>,
    }

    impl FakeDurableState {
        fn new() -> (Arc<Self>, mpsc::UnboundedSender<BlockNumHash>) {
            let (tx, rx) = mpsc::unbounded();
            let mut blocks = BTreeMap::new();
            blocks.insert(0, (hash(1), B256::ZERO));
            (
                Arc::new(Self {
                    stream: Mutex::new(Some(rx)),
                    blocks: Mutex::new(blocks),
                    replay: Mutex::new(BTreeMap::new()),
                }),
                tx,
            )
        }

        fn set(&self, height: u64, block_hash: B256, root: B256) {
            self.blocks
                .lock()
                .unwrap()
                .insert(height, (block_hash, root));
        }

        fn set_replay(&self, block: CanonicalCeReplayBlock) {
            self.replay.lock().unwrap().insert(block.number, block);
        }
    }

    impl DurableCeState for FakeDurableState {
        fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash> {
            self.stream.lock().unwrap().take().unwrap().boxed()
        }

        fn block_and_root(&self, height: u64) -> eyre::Result<Option<(B256, B256)>> {
            Ok(self.blocks.lock().unwrap().get(&height).copied())
        }

        fn replay_block(&self, height: u64) -> eyre::Result<Option<CanonicalCeReplayBlock>> {
            Ok(self.replay.lock().unwrap().get(&height).cloned())
        }
    }

    struct FakeTree {
        candidate: Option<Arc<StagedTreeBatch>>,
        marker: FinalizedMarker,
        attempts: Mutex<Vec<(u64, B256, B256)>>,
        apply_error: bool,
    }

    impl FakeTree {
        fn new(candidate: Option<Arc<StagedTreeBatch>>) -> Arc<Self> {
            Arc::new(Self {
                candidate,
                marker: FinalizedMarker {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    height: 0,
                    block_hash: hash(1),
                    parent_block_hash: B256::ZERO,
                    parent_root: B256::ZERO,
                    new_root: B256::ZERO,
                },
                attempts: Mutex::new(Vec::new()),
                apply_error: false,
            })
        }

        fn committed(root: B256) -> Arc<Self> {
            Arc::new(Self {
                candidate: None,
                marker: candidate(root).marker(ACTIVE_COMMITMENT_SCHEME),
                attempts: Mutex::new(Vec::new()),
                apply_error: false,
            })
        }

        fn failing(candidate: Arc<StagedTreeBatch>) -> Arc<Self> {
            Arc::new(Self {
                candidate: Some(candidate),
                marker: FinalizedMarker {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    height: 0,
                    block_hash: hash(1),
                    parent_block_hash: B256::ZERO,
                    parent_root: B256::ZERO,
                    new_root: B256::ZERO,
                },
                attempts: Mutex::new(Vec::new()),
                apply_error: true,
            })
        }

        fn attempts(&self) -> usize {
            self.attempts.lock().unwrap().len()
        }
    }

    impl FinalizedCeTree for FakeTree {
        fn finalized_marker(&self) -> eyre::Result<FinalizedMarker> {
            Ok(self.marker)
        }

        fn candidate(&self, height: u64, hash: B256) -> eyre::Result<Option<Arc<StagedTreeBatch>>> {
            Ok(self
                .candidate
                .as_ref()
                .filter(|candidate| {
                    candidate.block_number() == height && candidate.block_hash() == hash
                })
                .cloned())
        }

        fn apply_finalized(
            &self,
            height: u64,
            hash: B256,
            authoritative_root: B256,
        ) -> eyre::Result<FinalizedMarker> {
            self.attempts
                .lock()
                .unwrap()
                .push((height, hash, authoritative_root));
            if self.apply_error {
                eyre::bail!("injected uncertain commit outcome");
            }
            let candidate = self.candidate.as_ref().unwrap();
            Ok(candidate.marker(ACTIVE_COMMITMENT_SCHEME))
        }

        fn apply_replayed(&self, block: &CanonicalCeReplayBlock) -> eyre::Result<FinalizedMarker> {
            self.attempts
                .lock()
                .unwrap()
                .push((block.number, block.hash, block.new_root));
            Ok(FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: block.number,
                block_hash: block.hash,
                parent_block_hash: block.parent_hash,
                parent_root: block.parent_root,
                new_root: block.new_root,
            })
        }
    }

    fn finalizer(state: Arc<FakeDurableState>, tree: Arc<FakeTree>) -> RethCeFinalizer {
        let state: Arc<dyn DurableCeState> = state;
        let tree: Arc<dyn FinalizedCeTree> = tree;
        RethCeFinalizer::new(state, tree)
    }

    #[tokio::test]
    async fn waits_for_exact_persistence_before_applying_tree() {
        let root = hash(9);
        let (state, persisted_tx) = FakeDurableState::new();
        let tree = FakeTree::new(Some(candidate(root)));
        let finalizer = Arc::new(finalizer(state.clone(), tree.clone()));

        let task = {
            let finalizer = finalizer.clone();
            tokio::spawn(async move { finalizer.commit(finalized_block()).await })
        };
        tokio::task::yield_now().await;
        assert_eq!(tree.attempts(), 0);

        state.set(1, hash(2), root);
        persisted_tx
            .unbounded_send(BlockNumHash::new(1, hash(2)))
            .unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(tree.attempts(), 1);
    }

    #[tokio::test]
    async fn coalesced_later_notification_rechecks_exact_target_in_db() {
        let root = hash(9);
        let (state, persisted_tx) = FakeDurableState::new();
        let tree = FakeTree::new(Some(candidate(root)));
        let finalizer = Arc::new(finalizer(state.clone(), tree.clone()));

        let task = {
            let finalizer = finalizer.clone();
            tokio::spawn(async move { finalizer.commit(finalized_block()).await })
        };
        tokio::task::yield_now().await;
        state.set(1, hash(2), root);
        persisted_tx
            .unbounded_send(BlockNumHash::new(4, hash(4)))
            .unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(tree.attempts(), 1);
    }

    #[tokio::test]
    async fn durable_hash_conflict_fails_before_tree_apply() {
        let root = hash(9);
        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(7), root);
        let tree = FakeTree::new(Some(candidate(root)));
        let error = finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("canonical conflict"));
        assert_eq!(tree.attempts(), 0);
    }

    #[tokio::test]
    async fn durable_root_mismatch_fails_before_tree_apply() {
        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(2), hash(8));
        let tree = FakeTree::new(Some(candidate(hash(9))));
        let error = finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("root conflict"));
        assert_eq!(tree.attempts(), 0);
    }

    #[tokio::test]
    async fn missing_validator_candidate_is_reconstructed_from_durable_receipts() {
        let (state, _tx) = FakeDurableState::new();
        let root = hash(9);
        state.set(1, hash(2), root);
        state.set_replay(CanonicalCeReplayBlock {
            number: 1,
            hash: hash(2),
            parent_hash: hash(1),
            parent_root: B256::ZERO,
            new_root: root,
            events: Vec::new(),
        });
        let tree = FakeTree::new(None);
        finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap();
        assert_eq!(tree.attempts(), 1);
    }

    #[tokio::test]
    async fn durable_replay_applies_a_real_event_through_smt_and_mdbx() {
        let (_directory, service) = real_tree_service();
        let mut id_bytes = [7_u8; 36];
        id_bytes[..4].copy_from_slice(&1_u32.to_be_bytes());
        let entity = EntityRef::Tribute(EntityId36::try_from(id_bytes.as_slice()).unwrap());
        let commitment = Commitment::try_from([3_u8; 32]).unwrap();
        let parent = service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 0,
                block_hash: hash(1),
                root: B256::ZERO,
            })
            .unwrap();
        let expected_root = parent
            .prepare_seal(
                1,
                &[FinalLeafMutation {
                    entity,
                    final_leaf: Some(commitment),
                }],
            )
            .unwrap()
            .new_root();

        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(2), expected_root);
        state.set_replay(CanonicalCeReplayBlock {
            number: 1,
            hash: hash(2),
            parent_hash: hash(1),
            parent_root: B256::ZERO,
            new_root: expected_root,
            events: vec![outbe_compressed_entities::CanonicalBodyEvent {
                entity,
                previous: None,
                next: Some(commitment),
            }],
        });
        let tree: Arc<dyn FinalizedCeTree> = service.clone();
        RethCeFinalizer::new(state, tree)
            .commit(finalized_block())
            .await
            .unwrap();

        assert_eq!(service.finalized_marker().unwrap().new_root, expected_root);
        let reopened = service
            .open_parent(ExactParentIdentity {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                block_number: 1,
                block_hash: hash(2),
                root: expected_root,
            })
            .unwrap();
        assert_eq!(
            reopened.read_leaf_verified(entity, expected_root).unwrap(),
            Some(commitment)
        );
    }

    #[tokio::test]
    async fn redelivery_after_ce_commit_before_ack_is_idempotent_without_candidate() {
        let root = hash(9);
        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(2), root);
        let tree = FakeTree::committed(root);

        finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap();

        assert_eq!(tree.attempts(), 0);
    }

    #[tokio::test]
    async fn redelivery_rejects_a_corrupted_marker_parent_root() {
        let root = hash(9);
        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(2), root);
        let mut marker = candidate(root).marker(ACTIVE_COMMITMENT_SCHEME);
        marker.parent_root = hash(8);
        let tree = Arc::new(FakeTree {
            candidate: None,
            marker,
            attempts: Mutex::new(Vec::new()),
            apply_error: false,
        });

        let error = finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("parent identity conflicts"));
        assert_eq!(tree.attempts(), 0);
    }

    #[tokio::test]
    async fn tree_apply_failure_is_propagated_without_success() {
        let root = hash(9);
        let (state, _tx) = FakeDurableState::new();
        state.set(1, hash(2), root);
        let tree = FakeTree::failing(candidate(root));
        let error = finalizer(state, tree.clone())
            .commit(finalized_block())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("uncertain commit outcome"));
        assert_eq!(tree.attempts(), 1);
    }
}
