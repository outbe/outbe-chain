use super::super::state::OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS;
use crate::{errors::storage_corruption_message, schema::MetadosisContract};
use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    state::{OcompFinalizedJobV1, OcompJobStatus, RESULT_VOTE_MIN_FINALITY_DEPTH},
    SchemaLimits,
};
use outbe_primitives::error::Result;
struct FinalityWindow {
    open_height: u64,
    deadline_height: u64,
}
impl FinalityWindow {
    fn derive(recorded_height: u64, response_window_blocks: u64) -> Result<Self> {
        let open_height = recorded_height
            .checked_add(RESULT_VOTE_MIN_FINALITY_DEPTH)
            .ok_or_else(|| storage_corruption_message("OCOMP voting open height overflow"))?;
        let deadline_height = open_height
            .checked_add(response_window_blocks)
            .ok_or_else(|| storage_corruption_message("OCOMP response deadline overflow"))?;
        Ok(Self {
            open_height,
            deadline_height,
        })
    }
}
impl MetadosisContract<'_> {
    /// Records the consensus-certified request block and derives the voting
    /// window. The request itself carries no deadline.
    pub fn record_ocomp_finality(
        &mut self,
        intent_id: B256,
        finalized_request_block_hash: B256,
        finalized_request_state_root: B256,
        finality_recorded_height: u64,
        response_window_blocks: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<OcompFinalizedJobV1> {
        (|| {
            let mut record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP finality record has no matching intent")
                })?;
            if record.status != OcompJobStatus::AwaitingFinality || record.terminal.is_some() {
                return Err(storage_corruption_message(
                    "OCOMP finality requires AWAITING_FINALITY",
                ));
            }
            let awaiting_finality_deadline = record
                .intent_height
                .checked_add(OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS)
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP awaiting-finality deadline overflow")
                })?;
            let profile = self
                .read_ocomp_request_profile(schema_limits)?
                .ok_or_else(|| {
                    storage_corruption_message("OCOMP finality has no request profile")
                })?;
            if finality_recorded_height < record.intent_height
                || finality_recorded_height > awaiting_finality_deadline
                || response_window_blocks != profile.capacity_profile.result_deadline_blocks
            {
                return Err(storage_corruption_message(
                    "OCOMP finality/window height is invalid",
                ));
            }
            let FinalityWindow {
                open_height,
                deadline_height,
            } = FinalityWindow::derive(finality_recorded_height, response_window_blocks)?;
            let finalized = OcompFinalizedJobV1 {
                job_id: record
                    .intent
                    .job_id(
                        finalized_request_block_hash,
                        finalized_request_state_root,
                        schema_limits,
                    )
                    .map_err(|error| {
                        storage_corruption_message(format!("derive finalized OCOMP JobId: {error}"))
                    })?,
                finalized_request_block_hash,
                finalized_request_state_root,
                finality_recorded_height,
                open_height,
                deadline_height,
                quorum: None,
            };
            finalized.validate_semantics().map_err(|error| {
                storage_corruption_message(format!("invalid finalized OCOMP job: {error}"))
            })?;
            if let Some(existing) = &record.finalized {
                if existing == &finalized {
                    return Ok(existing.clone());
                }
                return Err(storage_corruption_message("OCOMP finality binding changed"));
            }
            record.finalized = Some(finalized.clone());
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            Ok(finalized)
        })()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_is_anchored_to_recorded_finality_and_rejects_both_overflows() {
        let window = FinalityWindow::derive(100, 12).unwrap();
        assert_eq!(window.open_height, 100 + RESULT_VOTE_MIN_FINALITY_DEPTH);
        assert_eq!(window.deadline_height, window.open_height + 12);
        assert!(FinalityWindow::derive(u64::MAX, 12).is_err());
        assert!(FinalityWindow::derive(100, u64::MAX).is_err());
    }
}
