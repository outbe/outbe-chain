use super::super::{
    index::{insert_response_deadline_key, ResponseDeadlineKey},
    state::{JobFsmCommand, JobFsmState},
};
use crate::{errors::storage_corruption_message, schema::MetadosisContract};
use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    state::{OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus},
    vote::OcompVoteAccountabilityV1,
    SchemaLimits,
};
use outbe_primitives::error::Result;
struct DueVotingJob {
    state: JobFsmState,
    intent_id: B256,
    record: OcompJobRecordV1,
    finalized: OcompFinalizedJobV1,
}
impl MetadosisContract<'_> {
    /// Opens a finalized job once `at_height` reaches its open height. The job
    /// keeps its immutable response deadline.
    ///
    /// A delayed lifecycle tick may catch up before that deadline. At or after
    /// the deadline the caller must retire the still-unopened job as Expired.
    pub(crate) fn open_due_ocomp_voting(
        &mut self,
        at_height: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<bool> {
        (|| {
            let Some(DueVotingJob {
                mut state,
                intent_id,
                mut record,
                finalized,
            }) = self.select_due_voting_job(at_height, schema_limits)?
            else {
                return Ok(false);
            };
            state
                .apply(JobFsmCommand::OpenVoting {
                    at_height,
                    deadline_height: finalized.deadline_height,
                })
                .map_err(|error| storage_corruption_message(error.to_string()))?;
            let accountability = OcompVoteAccountabilityV1::empty(
                finalized.job_id,
                record.intent.result_validator_set_epoch,
                record.intent.result_committee_set_hash,
                record.intent.result_ocomp_binding_hash,
                record.intent.result_member_count,
                record.intent.result_quorum_threshold,
            )
            .map_err(|error| {
                storage_corruption_message(format!("create OCOMP vote slots: {error}"))
            })?;
            let slot = self.ocomp_vote_accountability.get_bytes(&finalized.job_id);
            if !slot.is_empty()? {
                return Err(storage_corruption_message(
                    "OCOMP vote accountability already exists",
                ));
            }
            slot.write(
                &accountability
                    .encode_canonical(schema_limits)
                    .map_err(|error| {
                        storage_corruption_message(format!("encode OCOMP vote slots: {error}"))
                    })?,
            )?;
            let mut response_index = self.read_response_deadline_index()?;
            insert_response_deadline_key(
                &mut response_index,
                ResponseDeadlineKey {
                    deadline_height: finalized.deadline_height,
                    job_id: finalized.job_id,
                    intent_id,
                },
            )?;
            record.status = OcompJobStatus::VotingOpen;
            self.write_ocomp_job_record(intent_id, &record, schema_limits)?;
            self.write_ocomp_state(&state)?;
            self.write_live_scheduler(&state)?;
            self.write_response_deadline_index(&response_index)?;
            Ok(true)
        })()
    }

    fn select_due_voting_job(
        &self,
        at_height: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<Option<DueVotingJob>> {
        let mut selected = None;
        for state in self.live_ocomp_fsm_states(schema_limits)? {
            let intent_id = state.projection().live_intent_id.ok_or_else(|| {
                storage_corruption_message("OCOMP live scheduler has no IntentId")
            })?;
            let record = self
                .ocomp_job_record(intent_id, schema_limits)?
                .ok_or_else(|| storage_corruption_message("OCOMP live scheduler job is missing"))?;
            if record.status != OcompJobStatus::AwaitingFinality {
                continue;
            }
            let Some(finalized) = record.finalized.clone() else {
                continue;
            };
            if at_height < finalized.open_height {
                continue;
            }
            if at_height >= finalized.deadline_height {
                return Err(storage_corruption_message(
                    "OCOMP voting cannot open at or after its deadline",
                ));
            }
            if selected
                .replace(DueVotingJob {
                    state,
                    intent_id,
                    record,
                    finalized,
                })
                .is_some()
            {
                return Err(storage_corruption_message(
                    "multiple OCOMP jobs are due at one bounded open height",
                ));
            }
        }
        Ok(selected)
    }
}
