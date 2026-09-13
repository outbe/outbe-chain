use super::*;

pub(super) fn executor_test_block(number: u64, seed: u8) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.extra_data = Bytes::from(vec![seed]);
    let block = block.map_header(OutbeHeader::new);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

pub(super) fn ready_projection(
    baseline_hash: B256,
    checkpoint: ProjectionCheckpoint,
) -> (ProjectionReadinessPublisher, ProjectionReadinessHandle) {
    projection_readiness(
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: baseline_hash,
        },
        ProjectionStatus::Ready { checkpoint },
    )
}

pub(super) fn ready_projection_for_block(
    genesis_hash: B256,
    block: &ConsensusBlock,
) -> (ProjectionReadinessPublisher, ProjectionReadinessHandle) {
    ready_projection(
        genesis_hash,
        ProjectionCheckpoint {
            block_number: block.number().saturating_sub(1),
            block_hash: block.parent_hash(),
        },
    )
}
