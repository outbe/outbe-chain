//! Fixed-size record payloads that execution produces. Validation is structural only.
use super::{
    CompressedEntitiesRootArtifact, ExecutionSummaryArtifact, PrecompileError, Result, B256,
    COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN, EXECUTION_SUMMARY_LEN, TIMESTAMP_MILLIS_PART_LEN, U256,
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

pub(super) fn encode_root(root: CompressedEntitiesRootArtifact) -> Vec<u8> {
    let mut payload = Vec::with_capacity(COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN);
    payload.extend_from_slice(&root.commitment_scheme_version.to_be_bytes());
    payload.extend_from_slice(root.r_sealed.as_slice());
    payload
}

pub(super) fn decode_execution_summary(payload: &[u8]) -> Result<ExecutionSummaryArtifact> {
    if payload.len() != EXECUTION_SUMMARY_LEN {
        return Err(PrecompileError::Fatal(format!(
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
        return Err(PrecompileError::Fatal(format!(
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

pub(super) fn decode_root(payload: &[u8]) -> Result<CompressedEntitiesRootArtifact> {
    if payload.len() != COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN {
        return Err(PrecompileError::Fatal(format!(
            "compressed-entities root payload length: {} (expected {})",
            payload.len(),
            COMPRESSED_ENTITIES_ROOT_PAYLOAD_LEN
        )));
    }
    let mut scheme = [0_u8; 4];
    scheme.copy_from_slice(&payload[..4]);
    Ok(CompressedEntitiesRootArtifact {
        commitment_scheme_version: u32::from_be_bytes(scheme),
        r_sealed: B256::from_slice(&payload[4..]),
    })
}
