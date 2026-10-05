use super::*;

pub(in crate::executor) fn validate_execution_summary_artifact(
    enabled: bool,
    block_number: u64,
    header_summary: Option<ExecutionSummaryArtifact>,
    current_summary: ExecutionSummaryArtifact,
) -> Result<(), BlockExecutionError> {
    if !enabled || block_number == 0 {
        return Ok(());
    }
    let Some(header_summary) = header_summary else {
        return Err(BlockExecutionError::msg(
            "missing execution summary artifact in block extra_data",
        ));
    };
    if header_summary != current_summary {
        return Err(BlockExecutionError::msg(format!(
            "execution summary artifact mismatch: header={header_summary:?}, local={current_summary:?}"
        )));
    }
    Ok(())
}

pub(in crate::executor) fn validate_finish_artifacts(
    executor: &OutbeBlockExecutor<'_, impl Sized>,
    block_number: u64,
    current_summary: ExecutionSummaryArtifact,
) -> Result<outbe_primitives::reshare_artifact::OutbeBlockArtifacts, BlockExecutionError> {
    let block_artifacts = decode_outbe_block_artifacts(executor.final_extra_data().as_ref())
        .map_err(|error| BlockExecutionError::msg(error.to_string()))?;
    if block_number > 0 {
        let seal_output = executor
            .compressed_entities_seal_output
            .as_ref()
            .ok_or_else(|| BlockExecutionError::msg("missing compressed-entities SealOutput"))?;
        validate_compressed_entities_root_after_seal(
            block_artifacts.compressed_entities_root,
            seal_output.new_root,
        )?;
    }
    validate_execution_summary_artifact(
        executor.validate_execution_summary,
        block_number,
        block_artifacts.execution_summary,
        current_summary,
    )?;

    Ok(block_artifacts)
}
