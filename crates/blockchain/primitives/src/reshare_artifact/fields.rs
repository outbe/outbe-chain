//! Fixed-size record payloads that execution produces. Validation is structural only.
use super::{
    ExecutionSummaryArtifact, PrecompileError, Result, EXECUTION_SUMMARY_LEN,
    TIMESTAMP_MILLIS_PART_LEN, U256,
};

pub(super) fn encode_execution_summary(summary: ExecutionSummaryArtifact) -> Vec<u8> {
    let mut payload = Vec::with_capacity(EXECUTION_SUMMARY_LEN);
    payload.extend_from_slice(&summary.validator_fee_sum.to_be_bytes::<32>());
    payload
}

pub(super) fn encode_timestamp(timestamp_millis_part: u64) -> Vec<u8> {
    // Range validation (< 1000) belongs to the consensus header validator.
    // Preserve structural roundtrips for adversarial header construction.
    let mut payload = Vec::with_capacity(TIMESTAMP_MILLIS_PART_LEN);
    payload.extend_from_slice(&timestamp_millis_part.to_be_bytes());
    payload
}

pub(super) fn decode_execution_summary(payload: &[u8]) -> Result<ExecutionSummaryArtifact> {
    if payload.len() != EXECUTION_SUMMARY_LEN {
        return Err(PrecompileError::revert(format!(
            "invalid execution summary artifact length: {}",
            payload.len()
        )));
    }

    Ok(ExecutionSummaryArtifact {
        validator_fee_sum: U256::from_be_slice(&payload[0..32]),
    })
}

pub(super) fn decode_timestamp(payload: &[u8]) -> Result<u64> {
    if payload.len() != TIMESTAMP_MILLIS_PART_LEN {
        return Err(PrecompileError::revert(format!(
            "timestamp_millis_part payload length: {} (expected {})",
            payload.len(),
            TIMESTAMP_MILLIS_PART_LEN
        )));
    }
    let mut buf = [0u8; TIMESTAMP_MILLIS_PART_LEN];
    buf.copy_from_slice(payload);
    // The consensus header validator owns the range check (`< 1000`).
    // The codec is structural-only.
    Ok(u64::from_be_bytes(buf))
}
