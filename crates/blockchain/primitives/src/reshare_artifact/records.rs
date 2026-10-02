//! Record dispatch and slot duplication checks, independent of envelope framing.
use super::{
    consensus, fields, late_finalize, Bytes, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    PrecompileError, Result, BOUNDARY_TAG, COMMITTEE_PREANNOUNCE_TAG, COMPRESSED_ENTITIES_ROOT_TAG,
    DEALER_LOG_TAG, EXECUTION_SUMMARY_TAG, LATE_FINALIZE_CREDITS_TAG, TIMESTAMP_MILLIS_PART_TAG,
};

pub(super) fn decode_into(
    artifacts: &mut OutbeBlockArtifacts,
    tag: u8,
    payload: &[u8],
) -> Result<()> {
    match tag {
        EXECUTION_SUMMARY_TAG => {
            if artifacts.execution_summary.is_some() {
                return Err(PrecompileError::Fatal(
                    "duplicate execution summary artifact".into(),
                ));
            }
            artifacts.execution_summary = Some(fields::decode_execution_summary(payload)?);
        }
        BOUNDARY_TAG | DEALER_LOG_TAG | COMMITTEE_PREANNOUNCE_TAG => {
            if artifacts.consensus_header_artifact.is_some() {
                return Err(PrecompileError::Fatal(
                    "duplicate consensus header artifact".into(),
                ));
            }
            artifacts.consensus_header_artifact = Some(match tag {
                BOUNDARY_TAG => consensus::decode_boundary_record(payload)?,
                DEALER_LOG_TAG => {
                    ConsensusHeaderArtifact::DealerLog(Bytes::copy_from_slice(payload))
                }
                // The outer match limits this arm to the pre-announce tag.
                _ => consensus::decode_preannounce_record(payload)?,
            });
        }
        TIMESTAMP_MILLIS_PART_TAG => {
            if artifacts.timestamp_millis_part != 0 {
                return Err(PrecompileError::Fatal(
                    "duplicate timestamp_millis_part".into(),
                ));
            }
            artifacts.timestamp_millis_part = fields::decode_timestamp(payload)?;
        }
        LATE_FINALIZE_CREDITS_TAG => {
            if artifacts.late_finalize_credits.is_some() {
                return Err(PrecompileError::Fatal(
                    "duplicate late finalize credits artifact".into(),
                ));
            }
            artifacts.late_finalize_credits = Some(late_finalize::decode_payload(payload)?);
        }
        COMPRESSED_ENTITIES_ROOT_TAG => {
            if artifacts.compressed_entities_root.is_some() {
                return Err(PrecompileError::Fatal(
                    "duplicate compressed-entities root artifact".into(),
                ));
            }
            artifacts.compressed_entities_root = Some(fields::decode_root(payload)?);
        }
        _ => {
            return Err(PrecompileError::Fatal(format!(
                "unsupported block artifact tag: {tag}"
            )))
        }
    }
    Ok(())
}
