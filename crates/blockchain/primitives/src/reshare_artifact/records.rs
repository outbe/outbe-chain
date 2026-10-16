//! Record dispatch and slot duplication checks, independent of envelope framing.
use super::{
    consensus, fields, late_finalize, Bytes, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    ReshareArtifactError, Result, BOUNDARY_TAG, COMMITTEE_PREANNOUNCE_TAG,
    COMPRESSED_ENTITIES_ROOT_TAG, DEALER_LOG_TAG, EXECUTION_SUMMARY_TAG, LATE_FINALIZE_CREDITS_TAG,
    TIMESTAMP_MILLIS_PART_TAG,
};

pub(super) fn decode_into(
    artifacts: &mut OutbeBlockArtifacts,
    tag: u8,
    payload: &[u8],
) -> Result<()> {
    match tag {
        EXECUTION_SUMMARY_TAG => {
            decode_unique_slot(
                &mut artifacts.execution_summary,
                ReshareArtifactError::DuplicateExecutionSummary,
                || fields::decode_execution_summary(payload),
            )?;
        }
        BOUNDARY_TAG | DEALER_LOG_TAG | COMMITTEE_PREANNOUNCE_TAG => {
            decode_unique_slot(
                &mut artifacts.consensus_header_artifact,
                ReshareArtifactError::DuplicateConsensusHeader,
                || {
                    Ok(match tag {
                        BOUNDARY_TAG => consensus::decode_boundary_record(payload)?,
                        DEALER_LOG_TAG => {
                            ConsensusHeaderArtifact::DealerLog(Bytes::copy_from_slice(payload))
                        }
                        // The outer match limits this arm to the pre-announce tag.
                        _ => consensus::decode_preannounce_record(payload)?,
                    })
                },
            )?;
        }
        TIMESTAMP_MILLIS_PART_TAG => {
            if artifacts.timestamp_millis_part != 0 {
                return Err(ReshareArtifactError::DuplicateTimestampMillisPart.into());
            }
            artifacts.timestamp_millis_part = fields::decode_timestamp(payload)?;
        }
        LATE_FINALIZE_CREDITS_TAG => {
            decode_unique_slot(
                &mut artifacts.late_finalize_credits,
                ReshareArtifactError::DuplicateLateFinalizeCredits,
                || late_finalize::decode_payload(payload),
            )?;
        }
        COMPRESSED_ENTITIES_ROOT_TAG => {
            decode_unique_slot(
                &mut artifacts.compressed_entities_root,
                ReshareArtifactError::DuplicateCompressedEntitiesRoot,
                || fields::decode_root(payload),
            )?;
        }
        _ => return Err(ReshareArtifactError::UnsupportedTag { tag }.into()),
    }
    Ok(())
}

fn decode_unique_slot<T>(
    slot: &mut Option<T>,
    duplicate: ReshareArtifactError,
    decode: impl FnOnce() -> Result<T>,
) -> Result<()> {
    if slot.is_some() {
        return Err(duplicate.into());
    }
    *slot = Some(decode()?);
    Ok(())
}
