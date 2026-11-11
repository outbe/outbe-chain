use super::{
    binding::LayoutBindingError, split_system_layout, BlockOrder, BlockSystemPlan, SystemTx,
    SystemTxError, SystemTxLayout, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX, MAX_END_BLOCK_SYSTEM_TXS,
    MAX_PREFIX_SYSTEM_TXS, MAX_SYSTEM_TXS_PER_BLOCK,
};
use alloy_consensus::{BlockHeader as _, Transaction as _};
use outbe_primitives::{
    block::{BlockHeightRegime, BlockSystemFlags},
    reshare_artifact::{decode_outbe_block_artifacts, OutbeBlockArtifacts},
    system_tx_context::SystemTxValidationContext,
    OutbeBlockBody, OutbeHeader,
};

pub fn validate_system_tx_set_for_publication(
    order: &BlockOrder,
    layout: &SystemTxLayout<'_>,
    block_number: u64,
    flags: BlockSystemFlags,
) -> Result<(), SystemTxError> {
    let regime = BlockHeightRegime::for_block(block_number);
    if regime.has_system_tx_prefix() && order.consensus.is_empty() {
        return Err(SystemTxError::EmptyBlockOrder { block_number });
    }
    let actual = layout.system_tx_count();
    if actual > usize::from(MAX_SYSTEM_TXS_PER_BLOCK)
        || layout.begin.len() > MAX_PREFIX_SYSTEM_TXS
        || layout.end.len() > MAX_END_BLOCK_SYSTEM_TXS
    {
        return Err(SystemTxError::TooManySystemTxs {
            actual,
            max: MAX_SYSTEM_TXS_PER_BLOCK,
        });
    }
    validate_bootstrap_membership(layout, regime, block_number, flags)?;
    let plan = BlockSystemPlan::for_block(block_number, flags, order);
    let expected_begin = plan.expected_prefix_steps();
    let expected_end = plan.expected_end_block_steps();
    let actual_begin = layout.prefix_steps()?;
    let actual_end = layout.end_block_steps()?;
    if actual_begin != expected_begin || actual_end != expected_end {
        Err(SystemTxError::ActiveSystemTxSetMismatch {
            expected_begin,
            expected_end,
            actual_begin,
            actual_end,
        })
    } else {
        Ok(())
    }
}

fn validate_bootstrap_membership(
    layout: &SystemTxLayout<'_>,
    regime: BlockHeightRegime,
    block_number: u64,
    flags: BlockSystemFlags,
) -> Result<(), SystemTxError> {
    let has_tee_bootstrap = layout.has_consensus_step(CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX);
    if regime.requires_genesis_bootstrap_txs() && !flags.has_boundary_outcome {
        return Err(SystemTxError::V2Block1MissingBoundaryOutcome);
    }
    if regime.requires_genesis_bootstrap_txs() && !has_tee_bootstrap {
        return Err(SystemTxError::V2Block1MissingTeeBootstrap);
    }
    if !regime.requires_genesis_bootstrap_txs() && has_tee_bootstrap {
        return Err(SystemTxError::TeeBootstrapWrongHeight { block_number });
    }
    if regime.requires_genesis_bootstrap_txs() && !layout.user.is_empty() {
        return Err(SystemTxError::V2Block1ContainsUserTransactions {
            actual: layout.user.len(),
        });
    }
    Ok(())
}

/// Decode header artifacts before validating the layout and active membership.
/// Envelope, signer and execution checks belong to the caller.
pub fn validate_system_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
    order: &BlockOrder,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), LayoutBindingError> {
    let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref())
        .map_err(LayoutBindingError::Artifacts)?;
    let layout = split_system_layout(&body.transactions).map_err(LayoutBindingError::Layout)?;
    validate_system_tx_set_for_publication(
        order,
        &layout,
        header.number(),
        BlockSystemFlags::from_artifacts(&artifacts),
    )
    .map_err(LayoutBindingError::Set)?;
    let ctx = SystemTxValidationContext {
        regime: BlockHeightRegime::for_block(header.number()),
        block_number: header.number(),
        parent_hash: header.parent_hash(),
        artifacts: &artifacts,
    };
    for tx in layout.begin.iter().chain(&layout.end) {
        let input = SystemTx::decode((*tx).input()).map_err(LayoutBindingError::Layout)?;
        let step = order.step_for_tx(&input).ok_or_else(|| {
            LayoutBindingError::Set(SystemTxError::UnexpectedSystemTx {
                stage: input.stage(),
                expected: None,
                actual: input.stage_name(),
            })
        })?;
        (step.validate_payload)(input.payload(), &ctx)?;
    }
    Ok((layout, artifacts))
}
