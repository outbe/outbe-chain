//! Record dispatch and slot duplication checks, independent of envelope framing.
use super::{
    consensus, fields, late_finalize, Bytes, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    ReshareArtifactError, Result, BOUNDARY_TAG, COMMITTEE_PREANNOUNCE_TAG, DEALER_LOG_TAG,
    ENCLAVE_WITNESS_TAG, EXECUTION_SUMMARY_TAG, LATE_FINALIZE_CREDITS_TAG, RADICLE_PUBLICATION_TAG,
    TIMESTAMP_MILLIS_PART_TAG,
};

pub(super) fn decode_into(
    artifacts: &mut OutbeBlockArtifacts,
    tag: u8,
    payload: &[u8],
) -> Result<()> {
    match tag {
        RADICLE_PUBLICATION_TAG => {
            if !artifacts.radicle_publication_enabled {
                return Err(ReshareArtifactError::PublicationFlagRequired.into());
            }
            decode_unique_slot(
                &mut artifacts.radicle_publication,
                ReshareArtifactError::DuplicateRadiclePublication,
                || crate::radicle_publication::PublicationBatch::from_bytes(payload),
            )?;
        }
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
        TIMESTAMP_MILLIS_PART_TAG => decode_timestamp_record(artifacts, payload)?,
        LATE_FINALIZE_CREDITS_TAG => {
            decode_unique_slot(
                &mut artifacts.late_finalize_credits,
                ReshareArtifactError::DuplicateLateFinalizeCredits,
                || late_finalize::decode_record(payload),
            )?;
        }
        ENCLAVE_WITNESS_TAG => {
            decode_unique_slot(
                &mut artifacts.enclave_witness,
                ReshareArtifactError::DuplicateEnclaveWitness,
                || {
                    if payload.is_empty() {
                        return Err(ReshareArtifactError::EmptyEnclaveWitnessPayload.into());
                    }
                    Ok(Bytes::copy_from_slice(payload))
                },
            )?;
        }
        _ => return Err(ReshareArtifactError::UnsupportedTag { tag }.into()),
    }
    Ok(())
}

fn decode_timestamp_record(artifacts: &mut OutbeBlockArtifacts, payload: &[u8]) -> Result<()> {
    if artifacts.timestamp_millis_part != 0 {
        return Err(ReshareArtifactError::DuplicateTimestampMillisPart.into());
    }
    let timestamp_millis_part = fields::decode_timestamp(payload)?;
    // The encoder omits a zero remainder, so a zero record is not canonical.
    if timestamp_millis_part == 0 {
        return Err(ReshareArtifactError::ZeroTimestampMillisPartRecord.into());
    }
    artifacts.timestamp_millis_part = timestamp_millis_part;
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
