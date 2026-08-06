//! Consensus-state admission for direct validator `ResultVoteV1` records.
//!
//! The public transaction supplies one canonical signed vote. This module
//! resolves the finalized job from the bounded response-window index, verifies
//! the inner OCOMP signature against fork-installed committee state and owns
//! the atomic four-slot/q=3 transition. It never decodes or executes Lysis.

use alloy_primitives::{Address, Bytes, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_ocomp_protocol::{
    abi::{OCOMP_RESULT_VOTE_REJECTED_SELECTOR, SUBMIT_LYSIS_RESULT_SELECTOR},
    state::OcompJobStatus,
    vote::{OcompQuorumV1, RecordVoteOutcomeV1, ResultVoteV1},
    SchemaLimits,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::{
    aggregate::ValidatedWwdAggregate,
    reducer::{reduce_outer_wwd, OcompRetryCause, OuterWwdEvent},
    schema::MetadosisContract,
};

use super::schema::remove_response_deadline_key;

const REJECT_MALFORMED_ENCODING: u16 = 1;
const REJECT_LIMIT_EXCEEDED: u16 = 2;
const REJECT_CALL_MODE: u16 = 3;
const REJECT_PROTOCOL_VOTE: u16 = 4;
pub(crate) const REJECT_LIFECYCLE_INACTIVE: u16 = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedResultVoteV1 {
    pub outcome: RecordVoteOutcomeV1,
    pub quorum: Option<OcompQuorumV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseWindowCloseV1 {
    NotDue,
    NoQuorum { intent_id: alloy_primitives::B256 },
    QuorumPreserved { intent_id: alloy_primitives::B256 },
}

/// Dispatches one normal public EVM transaction containing a canonical signed
/// `ResultVoteV1`. The inner signature authorizes the result vote while the
/// outer OCOMP delegate must resolve to the same installed validator slot.
pub fn dispatch_public_result_vote(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    caller: Address,
    data: &[u8],
    value: U256,
    is_static: bool,
) -> Result<Bytes> {
    if !value.is_zero() || is_static {
        return Err(vote_reject(REJECT_CALL_MODE));
    }
    let limits = super::schema::poc_schema_limits();
    let vote_bytes = preflight_result_vote_calldata(data)?;
    let vote = ResultVoteV1::decode_canonical(vote_bytes, &limits)
        .map_err(|_| vote_reject(REJECT_MALFORMED_ENCODING))?;
    let inclusion_height = storage.block_number()?;
    MetadosisContract::new(storage)
        .record_ocomp_result_vote(&vote, caller, inclusion_height, scope, &limits)
        .map_err(map_vote_transition_error)?;
    Ok(Bytes::new())
}

fn preflight_result_vote_calldata(data: &[u8]) -> Result<&[u8]> {
    let vote_cap = usize::try_from(
        outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_result_vote_bytes,
    )
    .map_err(|_| fatal("OCOMP result-vote cap does not fit usize"))?;
    let padded_cap = vote_cap
        .checked_add(31)
        .map(|value| value & !31)
        .ok_or_else(|| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    let calldata_cap = 68_usize
        .checked_add(padded_cap)
        .ok_or_else(|| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    if data.len() > calldata_cap {
        return Err(vote_reject(REJECT_LIMIT_EXCEEDED));
    }
    if data.len() < 68
        || data.get(..4) != Some(SUBMIT_LYSIS_RESULT_SELECTOR.as_slice())
        || U256::from_be_slice(&data[4..36]) != U256::from(32)
    {
        return Err(vote_reject(REJECT_MALFORMED_ENCODING));
    }
    let payload_len = usize::try_from(U256::from_be_slice(&data[36..68]))
        .map_err(|_| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    if payload_len == 0 || payload_len > vote_cap {
        return Err(vote_reject(REJECT_LIMIT_EXCEEDED));
    }
    outbe_ocomp_protocol::capacity::result_vote_internal_work(payload_len)
        .map_err(|_| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    let padded_len = payload_len
        .checked_add(31)
        .map(|value| value & !31)
        .ok_or_else(|| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    let expected_len = 68_usize
        .checked_add(padded_len)
        .ok_or_else(|| vote_reject(REJECT_LIMIT_EXCEEDED))?;
    if data.len() != expected_len {
        return Err(vote_reject(REJECT_MALFORMED_ENCODING));
    }
    let payload_end = 68 + payload_len;
    if data[payload_end..].iter().any(|byte| *byte != 0) {
        return Err(vote_reject(REJECT_MALFORMED_ENCODING));
    }
    Ok(&data[68..payload_end])
}

fn map_vote_transition_error(error: PrecompileError) -> PrecompileError {
    match error {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
            vote_reject(REJECT_PROTOCOL_VOTE)
        }
        other => other,
    }
}

pub(crate) fn vote_reject(code: u16) -> PrecompileError {
    let mut encoded = Vec::with_capacity(36);
    encoded.extend_from_slice(&OCOMP_RESULT_VOTE_REJECTED_SELECTOR);
    encoded.extend_from_slice(&U256::from(code).to_be_bytes::<32>());
    PrecompileError::RevertBytes(Bytes::from(encoded))
}

impl MetadosisContract<'_> {
    /// Verifies and records one direct result vote at its canonical inclusion
    /// height. The response-window index, job record, vote slots and immutable
    /// quorum transition are committed in one storage checkpoint.
    pub fn record_ocomp_result_vote(
        &mut self,
        vote: &ResultVoteV1,
        caller: Address,
        inclusion_height: u64,
        scope: &ExecutionScope,
        limits: &SchemaLimits,
    ) -> Result<RecordedResultVoteV1> {
        let storage = self.storage.clone();
        let outcome = (|| {
            let response = self
                .response_window_for_job(vote.job_id)?
                .ok_or_else(|| reject("OCOMP result vote has no open response window"))?;
            let record = self
                .ocomp_job_record(response.intent_id, limits)?
                .ok_or_else(|| fatal("OCOMP response index points to a missing job"))?;
            let finalized = record
                .finalized
                .as_ref()
                .ok_or_else(|| fatal("OCOMP response-window job is not finalized"))?;
            if finalized.job_id != response.job_id
                || finalized.deadline_height != response.deadline_height
            {
                return Err(fatal("OCOMP response index/job binding mismatch"));
            }
            if !matches!(
                record.status,
                OcompJobStatus::VotingOpen | OcompJobStatus::Completed | OcompJobStatus::Conflicted
            ) {
                return Err(reject(
                    "OCOMP result vote requires an open or quorum-certified job",
                ));
            }
            let authority = self
                .read_ocomp_activation_authority(limits)?
                .ok_or_else(|| fatal("OCOMP result-vote committee is not installed"))?;
            let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let validator = validators
                .resolve_validator_for_role(
                    caller,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                )?
                .ok_or_else(|| reject("OCOMP result-vote caller is not an active delegate"))?;
            let validator_record = validators
                .get_validator(validator)?
                .ok_or_else(|| fatal("resolved OCOMP validator is missing"))?;
            let expected_identity = outbe_ocomp_protocol::committee::validator_identity_hash(
                vote.validator_index,
                validator,
                &validator_record.consensus_pubkey,
            )
            .map_err(|error| reject(format!("invalid OCOMP validator identity: {error}")))?;
            let member = authority
                .result_committee
                .ordered_members
                .get(usize::from(vote.validator_index))
                .ok_or_else(|| reject("OCOMP vote validator slot is out of range"))?;
            if member.validator_index != vote.validator_index
                || member.validator_identity_hash != expected_identity
            {
                return Err(reject(
                    "OCOMP transaction caller does not match the signed validator slot",
                ));
            }
            vote.verify(
                &record.intent,
                finalized.job_id,
                &authority.result_committee,
                inclusion_height,
                finalized.open_height,
                finalized.deadline_height,
                limits,
            )
            .map_err(|error| reject(format!("invalid OCOMP result vote: {error}")))?;

            let mut accountability = self
                .result_vote_accountability(finalized.job_id, limits)?
                .ok_or_else(|| fatal("OCOMP response-window vote slots are missing"))?;
            if accountability.quorum != finalized.quorum {
                return Err(fatal("OCOMP job/accountability quorum mismatch"));
            }
            let had_quorum = accountability.quorum.is_some();
            let outcome = accountability
                .record_verified_vote(vote, inclusion_height, limits)
                .map_err(|error| reject(format!("invalid OCOMP vote transition: {error}")))?;
            let quorum = accountability.quorum.clone();

            if !had_quorum {
                if let Some(formed) = &quorum {
                    if record.status != OcompJobStatus::VotingOpen {
                        return Err(fatal(
                            "OCOMP quorum formed outside the voting-open transition",
                        ));
                    }
                    let current_time = storage
                        .timestamp()?
                        .try_into()
                        .map_err(|_| fatal("OCOMP block timestamp does not fit u64"))?;
                    let worldwide_day = outbe_common::WorldwideDay::new(record.intent.wwd);
                    let aggregate = ValidatedWwdAggregate::load_and_validate(storage.clone())?;
                    let outer = aggregate.record(worldwide_day).ok_or_else(|| {
                        fatal("OCOMP q-forming vote has no persisted outer WorldwideDay")
                    })?;
                    let completed_transition =
                        reduce_outer_wwd(Some(outer), OuterWwdEvent::OcompCompleted)?;
                    let conflict_transition = reduce_outer_wwd(
                        Some(outer),
                        OuterWwdEvent::OcompRetryScheduled(OcompRetryCause::Conflicted),
                    )?;
                    let apply_context = super::activation::QuorumApplyContext::new(
                        &storage,
                        scope,
                        &completed_transition,
                        &conflict_transition,
                        inclusion_height,
                        current_time,
                        limits,
                    );
                    super::activation::apply_quorum_result(
                        apply_context,
                        self,
                        response.intent_id,
                        &record,
                        &vote.result,
                        formed,
                        &authority,
                    )?;
                    let applied = self
                        .ocomp_job_record(response.intent_id, limits)?
                        .ok_or_else(|| fatal("OCOMP q-forming apply removed the job"))?;
                    if !matches!(
                        applied.status,
                        OcompJobStatus::Completed | OcompJobStatus::Conflicted
                    ) || applied
                        .finalized
                        .as_ref()
                        .and_then(|finalized| finalized.quorum.as_ref())
                        != Some(formed)
                    {
                        return Err(fatal(
                            "OCOMP q-forming apply did not commit terminal quorum state",
                        ));
                    }
                }
            } else if finalized.quorum != quorum {
                return Err(fatal("OCOMP immutable quorum changed"));
            }

            self.write_result_vote_accountability(&accountability, limits)?;
            Ok(RecordedResultVoteV1 { outcome, quorum })
        })();
        outcome
    }

    /// Closes the one due PoC response window and persists the objective
    /// four-slot accountability summary. A timely quorum is never erased;
    /// callers expire only the returned `NoQuorum` live attempt.
    pub(crate) fn close_due_ocomp_response_window(
        &mut self,
        at_height: u64,
        limits: &SchemaLimits,
    ) -> Result<ResponseWindowCloseV1> {
        (|| {
            let mut index = self.read_response_deadline_index()?;
            let Some(key) = index.first().copied() else {
                return Ok(ResponseWindowCloseV1::NotDue);
            };
            if at_height < key.deadline_height {
                return Ok(ResponseWindowCloseV1::NotDue);
            }
            if at_height > key.deadline_height {
                return Err(fatal("OCOMP lifecycle skipped the exact response deadline"));
            }
            let record = self
                .ocomp_job_record(key.intent_id, limits)?
                .ok_or_else(|| fatal("OCOMP response index points to a missing job"))?;
            let finalized = record
                .finalized
                .as_ref()
                .ok_or_else(|| fatal("OCOMP response-window job is not finalized"))?;
            if finalized.job_id != key.job_id || finalized.deadline_height != key.deadline_height {
                return Err(fatal("OCOMP response deadline/job binding mismatch"));
            }

            let mut accountability = self
                .result_vote_accountability(key.job_id, limits)?
                .ok_or_else(|| fatal("OCOMP response-window vote slots are missing"))?;
            if accountability.quorum != finalized.quorum {
                return Err(fatal("OCOMP job/accountability quorum mismatch at close"));
            }
            accountability
                .close(at_height, limits)
                .map_err(|error| fatal(format!("close OCOMP vote accountability: {error}")))?;
            self.write_result_vote_accountability(&accountability, limits)?;
            remove_response_deadline_key(&mut index, key)?;
            self.write_response_deadline_index(&index)?;

            match record.status {
                OcompJobStatus::VotingOpen if finalized.quorum.is_none() => {
                    Ok(ResponseWindowCloseV1::NoQuorum {
                        intent_id: key.intent_id,
                    })
                }
                OcompJobStatus::Completed | OcompJobStatus::Conflicted
                    if finalized.quorum.is_some() =>
                {
                    Ok(ResponseWindowCloseV1::QuorumPreserved {
                        intent_id: key.intent_id,
                    })
                }
                _ => Err(fatal("OCOMP response close found an invalid job status")),
            }
        })()
    }
}

fn reject(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Revert(message.into())
}

fn fatal(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dynamic_bytes_calldata(payload_len: usize) -> Vec<u8> {
        let padded_len = (payload_len + 31) & !31;
        let mut data = vec![0_u8; 68 + padded_len];
        data[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
        data[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
        data[36..68].copy_from_slice(&U256::from(payload_len).to_be_bytes::<32>());
        data
    }

    #[test]
    fn result_vote_preflight_accepts_cap_minus_one_and_cap_and_rejects_cap_plus_one() {
        let cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_result_vote_bytes,
        )
        .unwrap();
        for accepted in [cap - 1, cap] {
            let data = dynamic_bytes_calldata(accepted);
            assert_eq!(
                preflight_result_vote_calldata(&data).unwrap().len(),
                accepted
            );
        }

        let rejected = dynamic_bytes_calldata(cap + 1);
        assert!(matches!(
            preflight_result_vote_calldata(&rejected),
            Err(PrecompileError::RevertBytes(_))
        ));
    }

    #[test]
    fn result_vote_preflight_rejects_nonzero_abi_padding() {
        let mut data = dynamic_bytes_calldata(1);
        *data.last_mut().unwrap() = 1;
        assert!(matches!(
            preflight_result_vote_calldata(&data),
            Err(PrecompileError::RevertBytes(_))
        ));
    }
}
