//! Package the sealed block and its exact executed state into a payload.

use outbe_primitives::{consensus::OUTBE_MAX_BLOCK_SIZE, OutbeBuiltPayload, OutbePrimitives};
use reth_chainspec::EthereumHardforks;
use reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE;
use reth_errors::ConsensusError;
use reth_evm::{
    execute::{BlockBuilderOutcome, BlockExecutionOutput},
    Database,
};
use reth_payload_builder::EthBuiltPayload;
use reth_payload_primitives::{BuiltPayloadExecutedBlock, PayloadBuilderError};
use reth_revm::db::State;
use std::sync::Arc;

use super::{
    discard_failed_payload_candidate, execution::PayloadBuildState, preparation::PayloadContext,
};

pub(super) fn into_payload<DB: Database>(
    outcome: BlockBuilderOutcome<OutbePrimitives>,
    db: &mut State<DB>,
    state: PayloadBuildState,
    context: &PayloadContext<'_>,
    compressed_tree_service: Option<&Arc<outbe_compressed_entities::CompressedTreeService>>,
) -> Result<OutbeBuiltPayload, PayloadBuilderError> {
    let PayloadBuildState {
        total_fees,
        blob_sidecars,
        is_osaka,
        ..
    } = state;
    let BlockBuilderOutcome {
        execution_result,
        hashed_state,
        trie_updates,
        block,
        block_access_list,
    } = outcome;

    let requests = context
        .chain_spec
        .is_prague_active_at_timestamp(context.attributes.inner().timestamp)
        .then_some(execution_result.requests.clone());

    // Capture the full execution result of the block we just built, so the
    // proposer does NOT re-execute and re-root it at finalize time.
    // `builder` only borrowed `&mut db`. After `finish`, the merged post-state
    // bundle is back on our local `db`, and this code takes it here.
    // Reth's launch loop inserts `executed_block()` into the engine tree, so
    // `ExecutorActor`'s finalize-time `new_payload` becomes a cache hit.
    // (Validators already get this through their verify-time `new_payload`.)
    // This is the SAME execution that produced the sealed block below. Thus the
    // cached state matches the sealed block hash exactly, and the proposer and
    // validator do not diverge.
    let recovered_block = Arc::new(block);
    let execution_output = Arc::new(BlockExecutionOutput {
        state: db.take_bundle(),
        result: execution_result,
    });
    let executed_block = BuiltPayloadExecutedBlock::<OutbePrimitives> {
        recovered_block: recovered_block.clone(),
        execution_output,
        hashed_state: Arc::new(hashed_state),
        trie_updates: Arc::new(trie_updates),
    };

    let sealed_block = Arc::new(recovered_block.sealed_block().clone());

    if is_osaka && sealed_block.rlp_length() > MAX_RLP_BLOCK_SIZE {
        discard_failed_payload_candidate(
            compressed_tree_service,
            recovered_block.header().inner.number,
            recovered_block.hash(),
        )?;
        return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
            rlp_length: sealed_block.rlp_length(),
            max_rlp_length: MAX_RLP_BLOCK_SIZE,
        }));
    }

    // Outbe transport cap (always on): the sealed block must fit one
    // consensus P2P message. This check is the final guard in case the per-tx
    // estimate undershot (e.g. system txs / extra_data added after selection).
    if sealed_block.rlp_length() > OUTBE_MAX_BLOCK_SIZE {
        discard_failed_payload_candidate(
            compressed_tree_service,
            recovered_block.header().inner.number,
            recovered_block.hash(),
        )?;
        return Err(PayloadBuilderError::other(ConsensusError::BlockTooLarge {
            rlp_length: sealed_block.rlp_length(),
            max_rlp_length: OUTBE_MAX_BLOCK_SIZE,
        }));
    }

    let inner = EthBuiltPayload::<OutbePrimitives>::new(
        recovered_block,
        total_fees,
        requests,
        block_access_list.map(|bal| alloy_rlp::encode(&bal).into()),
    )
    .with_sidecars(blob_sidecars);
    let payload = OutbeBuiltPayload::new(inner, Some(executed_block));

    Ok(payload)
}
