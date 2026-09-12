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
