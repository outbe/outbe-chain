use super::*;
use futures::channel::mpsc;
use std::sync::Mutex;

struct MemoryState {
    evidence: Mutex<Option<DurableExecutionEvidence>>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<BlockNumHash>>>,
}
impl DurableExecutionState for MemoryState {
    fn persisted_blocks(&self) -> BoxStream<'static, BlockNumHash> {
        self.receiver.lock().unwrap().take().unwrap().boxed()
    }
    fn block_and_root(&self, _: u64) -> eyre::Result<Option<DurableExecutionEvidence>> {
        Ok(*self.evidence.lock().unwrap())
    }
}
fn target() -> FinalizedExecutionBlock {
    FinalizedExecutionBlock {
        height: 2,
        block_hash: B256::repeat_byte(2),
        parent_block_hash: B256::repeat_byte(1),
    }
}
fn evidence(block: FinalizedExecutionBlock) -> DurableExecutionEvidence {
    DurableExecutionEvidence {
        block_hash: block.block_hash,
        parent_block_hash: block.parent_block_hash,
        state_root: B256::repeat_byte(9),
    }
}
fn setup() -> (
    Arc<MemoryState>,
    mpsc::UnboundedSender<BlockNumHash>,
    RethFinalizationBarrier,
) {
    let (tx, rx) = mpsc::unbounded();
    let state = Arc::new(MemoryState {
        evidence: Mutex::new(None),
        receiver: Mutex::new(Some(rx)),
    });
    let barrier = RethFinalizationBarrier::with_wait_policy(
        state.clone(),
        PersistenceWaitPolicy {
            recheck_interval: Duration::from_millis(5),
            deadline: Duration::from_millis(100),
        },
    );
    (state, tx, barrier)
}

#[tokio::test]
async fn notification_without_durable_state_cannot_release_finalization() {
    let (state, tx, barrier) = setup();
    let block = target();
    tx.unbounded_send(BlockNumHash::new(block.height, block.block_hash))
        .unwrap();
    let waiting = barrier.commit_finalized(block);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(15), &mut waiting)
            .await
            .is_err()
    );
    *state.evidence.lock().unwrap() = Some(evidence(block));
    tokio::time::timeout(Duration::from_millis(50), waiting)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn periodic_db_probe_recovers_without_a_notification() {
    let (state, _tx, barrier) = setup();
    let block = target();
    let waiting = barrier.commit_finalized(block);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    *state.evidence.lock().unwrap() = Some(evidence(block));
    tokio::time::timeout(Duration::from_millis(50), waiting)
        .await
        .unwrap()
        .unwrap();
    // The native probe has no second ledger. Repeating the exact delivery is safe.
    barrier.commit_finalized(block).await.unwrap();
}

#[tokio::test]
async fn speculative_same_height_hash_can_be_replaced_before_deadline() {
    let (state, tx, barrier) = setup();
    let block = target();
    let mut prior = evidence(block);
    prior.block_hash = B256::repeat_byte(3);
    *state.evidence.lock().unwrap() = Some(prior);
    tx.unbounded_send(BlockNumHash::new(block.height, prior.block_hash))
        .unwrap();
    let waiting = barrier.commit_finalized(block);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    *state.evidence.lock().unwrap() = Some(evidence(block));
    tx.unbounded_send(BlockNumHash::new(block.height + 1, B256::repeat_byte(4)))
        .unwrap();
    tokio::time::timeout(Duration::from_millis(50), waiting)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn wrong_parent_and_missing_state_fail_without_acknowledgment() {
    let (state, _tx, barrier) = setup();
    let block = target();
    let mut wrong = evidence(block);
    wrong.parent_block_hash = B256::repeat_byte(7);
    *state.evidence.lock().unwrap() = Some(wrong);
    assert!(barrier
        .commit_finalized(block)
        .await
        .unwrap_err()
        .downcast_ref::<FinalizationBarrierError>()
        .is_some_and(|e| matches!(e, FinalizationBarrierError::ParentMismatch { .. })));
    *state.evidence.lock().unwrap() = None;
    assert!(barrier
        .commit_finalized(block)
        .await
        .unwrap_err()
        .downcast_ref::<FinalizationBarrierError>()
        .is_some_and(|e| matches!(
            e,
            FinalizationBarrierError::PersistenceDeadlineExceeded { .. }
        )));
}

#[test]
fn startup_requires_the_exact_durable_recovery_checkpoint() {
    let (state, _tx, barrier) = setup();
    let block = target();
    let checkpoint = ProjectionCheckpoint {
        block_number: block.height,
        block_hash: block.block_hash,
    };
    assert!(barrier.verify_checkpoint(checkpoint).is_err());
    *state.evidence.lock().unwrap() = Some(evidence(block));
    barrier.verify_checkpoint(checkpoint).unwrap();
    assert!(barrier
        .verify_checkpoint(ProjectionCheckpoint {
            block_hash: B256::repeat_byte(8),
            ..checkpoint
        })
        .is_err());
}

#[test]
fn durable_headers_wait_for_executed_state_and_partial_trie_progress() {
    use reth_ethereum::chainspec::ChainSpec;
    use reth_primitives_traits::RecoveredBlock;
    use reth_provider::{test_utils::create_test_provider_factory_with_node_types, BlockWriter};
    use reth_stages_types::{FinishCheckpoint, StageCheckpoint};
    use reth_storage_api::StageCheckpointWriter;

    let factory = create_test_provider_factory_with_node_types::<outbe_node::OutbeNode>(Arc::new(
        ChainSpec::<OutbeHeader>::default(),
    ));
    let writer = factory.provider_rw().unwrap();
    let mut parent_hash = B256::ZERO;
    let blocks: Vec<_> = (0..3)
        .map(|number| {
            let mut block = outbe_primitives::OutbeBlock::default();
            block.header = OutbeHeader::new(alloy_consensus::Header {
                number,
                parent_hash,
                state_root: B256::repeat_byte(number as u8 + 1),
                ..Default::default()
            });
            let block = RecoveredBlock::new_unhashed(block, Vec::new());
            parent_hash = block.hash();
            block
        })
        .collect();
    for block in &blocks {
        writer.insert_block(block).unwrap();
    }
    writer
        .save_stage_checkpoint(StageId::Finish, StageCheckpoint::new(1))
        .unwrap();
    writer.commit().unwrap();
    assert!(durable_execution_evidence(&factory.provider().unwrap(), 2)
        .unwrap()
        .is_none());
    let writer = factory.provider_rw().unwrap();
    writer
        .save_stage_checkpoint(StageId::Finish, StageCheckpoint::new(2))
        .unwrap();
    writer.commit().unwrap();
    let actual = durable_execution_evidence(&factory.provider().unwrap(), 2)
        .unwrap()
        .unwrap();
    assert_eq!(actual.block_hash, blocks[2].hash());
    let writer = factory.provider_rw().unwrap();
    writer
        .save_stage_checkpoint(
            StageId::Finish,
            StageCheckpoint::new(2).with_finish_stage_checkpoint(FinishCheckpoint {
                partial_state_trie: Some(1),
            }),
        )
        .unwrap();
    writer.commit().unwrap();
    assert!(durable_execution_evidence(&factory.provider().unwrap(), 2)
        .unwrap()
        .is_none());
}
