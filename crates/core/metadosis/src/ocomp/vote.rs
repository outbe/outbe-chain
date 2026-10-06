//! Consensus-state admission for direct validator `ResultVoteV1` records.
//!
//! The public transaction supplies one canonical signed vote. This module:
//!
//! - resolves the finalized job from the bounded response-window index.
//! - verifies the inner OCOMP signature against the pinned historical ValidatorSet.
//! - owns the atomic pinned-ValidatorSet vote transition.
//!
//! It never executes Lysis.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_ocomp_protocol::{
    error::ProtocolError,
    state::{OcompJobRecordV1, OcompJobStatus},
    vote::{OcompQuorumV1, RecordVoteOutcomeV1, ResultVotePrefixV1, ResultVoteV1},
    SchemaLimits,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::{
    aggregate::ValidatedWwdAggregate,
    errors::{
        caller_rejection as reject, result_vote_rejection as vote_reject,
        storage_corruption_message, vote_rejection_code::*,
    },
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent},
    schema::MetadosisContract,
};

use super::{index::ResponseDeadlineKey, schema::remove_response_deadline_key};

mod recording;
mod response_close;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedResultVoteV1 {
    pub outcome: RecordVoteOutcomeV1,
    pub quorum: Option<OcompQuorumV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolvedHistoricalResultVoteMemberV1 {
    pub(super) validator_address: Address,
    validator_index: u16,
    pub(super) key_epoch: u64,
    pub(super) ocomp_public_key_sec1: [u8; 33],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResponseWindowCloseV1 {
    NotDue,
    NoQuorum { intent_id: alloy_primitives::B256 },
    QuorumPreserved { intent_id: alloy_primitives::B256 },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OcompPenaltyMetrics {
    recovery_resolutions: Vec<(Address, &'static str)>,
    misses: Vec<(Address, bool, u64)>,
    punishments: Vec<outbe_validatorset::runtime::DeferredValidatorPunishment>,
}

impl OcompPenaltyMetrics {
    pub(crate) fn record(self) {
        for punishment in self.punishments {
            punishment.record();
        }
        for (validator, outcome) in self.recovery_resolutions {
            outbe_validatorset::metrics::record_ocomp_recovery_resolution(validator, outcome);
            match outcome {
                "jailed" => tracing::warn!(
                    %validator,
                    outcome,
                    "OCOMP recovery window expired below minimum bonded stake"
                ),
                _ => tracing::info!(
                    %validator,
                    outcome,
                    "OCOMP recovery window closed"
                ),
            }
        }
        for (validator, first_in_window, recovery_deadline) in self.misses {
            outbe_validatorset::metrics::record_ocomp_miss(
                validator,
                first_in_window,
                recovery_deadline,
            );
            tracing::warn!(
                %validator,
                first_in_window,
                recovery_deadline,
                "validator missed an OCOMP result vote"
            );
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResponseWindowCloseReport {
    pub(crate) close: ResponseWindowCloseV1,
    pub(crate) metrics: OcompPenaltyMetrics,
}

impl ResponseWindowCloseReport {
    fn not_due() -> Self {
        Self {
            close: ResponseWindowCloseV1::NotDue,
            metrics: OcompPenaltyMetrics::default(),
        }
    }
}

/// Dispatches one normal public EVM transaction containing a canonical signed
/// `ResultVoteV1`. The outer caller is intentionally not protocol authority.
/// Eligibility and fee classification are separate. Only the inner committee
/// signature authorizes this transition.
pub fn dispatch_public_result_vote(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    data: &[u8],
    value: U256,
    is_static: bool,
) -> Result<Bytes> {
    if !value.is_zero() || is_static {
        return Err(vote_reject(CALL_MODE));
    }
    let limits = super::schema::poc_schema_limits();
    let vote_bytes = preflight_result_vote_calldata(data, &limits)?;
    let vote = ResultVoteV1::decode_canonical(vote_bytes, &limits)
        .map_err(|_| vote_reject(MALFORMED_ENCODING))?;
    let inclusion_height = storage.block_number()?;
    MetadosisContract::new(storage)
        .record_ocomp_result_vote(&vote, inclusion_height, scope, &limits)
        .map_err(map_vote_transition_error)?;
    Ok(Bytes::new())
}

pub(super) fn preflight_result_vote_calldata<'a>(
    data: &'a [u8],
    limits: &SchemaLimits,
) -> Result<&'a [u8]> {
    outbe_ocomp_protocol::vote::decode_submit_lysis_result_prefix(data, limits).map_err(
        |error| {
            vote_reject(if matches!(error, ProtocolError::CapacityExceeded { .. }) {
                LIMIT_EXCEEDED
            } else {
                MALFORMED_ENCODING
            })
        },
    )?;
    let payload_len = usize::try_from(U256::from_be_slice(&data[36..68]))
        .map_err(|_| vote_reject(LIMIT_EXCEEDED))?;
    outbe_ocomp_protocol::capacity::result_vote_internal_work(payload_len)
        .map_err(|_| vote_reject(LIMIT_EXCEEDED))?;
    let payload_end = 68 + payload_len;
    Ok(&data[68..payload_end])
}

fn map_vote_transition_error(error: PrecompileError) -> PrecompileError {
    if crate::errors::is_business_failure(&error) {
        return error;
    }
    if is_deadline_passed_result_vote_revert(&error) {
        return error;
    }
    match error {
        PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => vote_reject(PROTOCOL_VOTE),
        other => other,
    }
}

#[must_use]
pub fn is_deadline_passed_result_vote_revert(error: &PrecompileError) -> bool {
    matches!(error, PrecompileError::RevertBytes(data) if is_deadline_passed_result_vote_revert_data(data))
}

#[must_use]
pub fn is_deadline_passed_result_vote_revert_data(data: &[u8]) -> bool {
    data == deadline_passed_result_vote_revert_data().as_ref()
}

#[must_use]
pub fn deadline_passed_result_vote_revert_data() -> Bytes {
    let PrecompileError::RevertBytes(expected) = vote_reject(DEADLINE_PASSED) else {
        unreachable!("vote_reject always returns RevertBytes");
    };
    expected
}

/// Immutable protocol, attempt, and committee identity for a result vote.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct PinnedVoteBinding {
    protocol_bundle_hash: B256,
    attempt: u32,
    committee: PinnedCommittee,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct PinnedCommittee {
    epoch: u64,
    set_hash: B256,
    binding_hash: B256,
}

impl PinnedVoteBinding {
    pub(super) fn from_prefix(prefix: &ResultVotePrefixV1) -> Self {
        Self {
            protocol_bundle_hash: prefix.protocol_bundle_hash,
            attempt: prefix.attempt,
            committee: PinnedCommittee::from_prefix(prefix),
        }
    }

    pub(super) fn from_record(record: &OcompJobRecordV1) -> Self {
        Self {
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            attempt: record.intent.attempt,
            committee: PinnedCommittee {
                epoch: record.intent.result_validator_set_epoch,
                set_hash: record.intent.result_committee_set_hash,
                binding_hash: record.intent.result_ocomp_binding_hash,
            },
        }
    }
}

impl PinnedCommittee {
    pub(super) fn from_prefix(prefix: &ResultVotePrefixV1) -> Self {
        Self {
            epoch: prefix.result_validator_set_epoch,
            set_hash: prefix.result_committee_set_hash,
            binding_hash: prefix.result_ocomp_binding_hash,
        }
    }

    pub(super) fn from_accountability(
        accountability: &outbe_ocomp_protocol::vote::OcompVoteAccountabilityV1,
    ) -> Self {
        Self {
            epoch: accountability.result_validator_set_epoch,
            set_hash: accountability.result_committee_set_hash,
            binding_hash: accountability.result_ocomp_binding_hash,
        }
    }
}

/// Resolve a vote participant from the historical ValidatorSet snapshot for an open or closed
/// window.
/// Membership remains immutable after an attempt opens. Current ValidatorSet status does not
/// affect this lookup.
/// Missing, evicted, or mismatched caller-selected state returns `None`. The resolver never
/// substitutes the current snapshot.
pub fn resolve_historical_result_vote_participant(
    storage: StorageHandle<'_>,
    prefix: &ResultVotePrefixV1,
    limits: &SchemaLimits,
) -> Result<Option<Address>> {
    let contract = MetadosisContract::new(storage.clone());
    let member_count = match contract.response_window_for_job(prefix.job_id)? {
        Some(response) => member_count_for_open_vote(&contract, prefix, response, limits)?,
        None => member_count_for_closed_vote(&contract, prefix, limits)?,
    };
    let Some(member_count) = member_count else {
        return Ok(None);
    };
    let Some(snapshot) = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        storage.clone(),
        prefix.result_validator_set_epoch,
        prefix.result_committee_set_hash,
        prefix.result_ocomp_binding_hash,
    )?
    else {
        return Ok(None);
    };
    if snapshot.member_count != member_count {
        return Ok(None);
    }
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        prefix.result_validator_set_epoch,
        prefix.result_committee_set_hash,
    );
    Ok(resolve_historical_result_vote_member(
        storage,
        snapshot_key,
        member_count,
        prefix.ocomp_key_hash,
        prefix.key_epoch,
    )?
    .map(|member| member.validator_address))
}

fn member_count_for_open_vote(
    contract: &MetadosisContract<'_>,
    prefix: &ResultVotePrefixV1,
    response: ResponseDeadlineKey,
    limits: &SchemaLimits,
) -> Result<Option<u16>> {
    let Some(record) = contract.ocomp_job_record(response.intent_id, limits)? else {
        return Err(storage_corruption_message(
            "OCOMP response index points to a missing job",
        ));
    };
    let Some(finalized) = record.finalized.as_ref() else {
        return Err(storage_corruption_message(
            "OCOMP response-window job is not finalized",
        ));
    };
    if finalized.job_id != response.job_id
        || finalized.deadline_height != response.deadline_height
        || !matches!(
            record.status,
            OcompJobStatus::VotingOpen | OcompJobStatus::Completed
        )
    {
        return Err(storage_corruption_message(
            "OCOMP response index/job binding mismatch",
        ));
    }
    if PinnedVoteBinding::from_prefix(prefix) != PinnedVoteBinding::from_record(&record) {
        return Ok(None);
    }
    Ok(Some(record.intent.result_member_count))
}

fn member_count_for_closed_vote(
    contract: &MetadosisContract<'_>,
    prefix: &ResultVotePrefixV1,
    limits: &SchemaLimits,
) -> Result<Option<u16>> {
    let Some(accountability) = contract.result_vote_accountability(prefix.job_id, limits)? else {
        return Ok(None);
    };
    if accountability.closed_summary.is_none()
        || PinnedCommittee::from_prefix(prefix)
            != PinnedCommittee::from_accountability(&accountability)
    {
        return Ok(None);
    }
    Ok(Some(accountability.member_count))
}

pub(super) fn resolve_historical_result_vote_member(
    storage: StorageHandle<'_>,
    snapshot_key: B256,
    member_count: u16,
    ocomp_key_hash: B256,
    key_epoch: u64,
) -> Result<Option<ResolvedHistoricalResultVoteMemberV1>> {
    let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    let validator_address = validators
        .ocomp_key_hash_to_validator
        .read(&ocomp_key_hash)?;
    if validator_address.is_zero() {
        return Ok(None);
    }

    for validator_index in 0..member_count {
        let member = outbe_validatorset::read_ocomp_snapshot_member_at(
            storage.clone(),
            snapshot_key,
            validator_index,
        )?
        .ok_or_else(|| storage_corruption_message("OCOMP historical snapshot member is missing"))?;
        if member.validator_address != validator_address {
            continue;
        }
        if member.key_epoch != key_epoch
            || keccak256(member.ocomp_public_key_sec1) != ocomp_key_hash
        {
            return Ok(None);
        }
        return Ok(Some(ResolvedHistoricalResultVoteMemberV1 {
            validator_address,
            validator_index,
            key_epoch: member.key_epoch,
            ocomp_public_key_sec1: member.ocomp_public_key_sec1,
        }));
    }
    Ok(None)
}

/// Resolves and authorizes the outer EVM signer of an OCOMP system carrier.
///
/// The represented validator comes exclusively from the exact historical
/// snapshot pinned by the vote. This function accepts the validator's own
/// address only when no OCOMP delegate is configured. Otherwise it accepts only
/// the current reverse-verified OCOMP delegate. Current ACTIVE status is
/// deliberately irrelevant for an already-open historical job.
pub fn resolve_historical_result_vote_carrier_signer(
    storage: StorageHandle<'_>,
    prefix: &ResultVotePrefixV1,
    signer: Address,
    limits: &SchemaLimits,
) -> Result<Option<Address>> {
    let Some(historical_validator) =
        resolve_historical_result_vote_participant(storage.clone(), prefix, limits)?
    else {
        return Ok(None);
    };
    authorize_historical_result_vote_carrier_signer(storage, historical_validator, signer)
}

pub(super) fn authorize_historical_result_vote_carrier_signer(
    storage: StorageHandle<'_>,
    historical_validator: Address,
    signer: Address,
) -> Result<Option<Address>> {
    let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
    let role = outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp;
    let explicit = validators.get_delegate(historical_validator, role)?;
    if signer == historical_validator {
        return Ok(explicit.is_zero().then_some(historical_validator));
    }
    if explicit != signer {
        return Ok(None);
    }
    let reverse = validators
        .validator_by_role_delegate
        .get_nested(&role.id())
        .read(&signer)?;
    Ok((reverse == historical_validator).then_some(historical_validator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_ocomp_protocol::{
        abi::SUBMIT_LYSIS_RESULT_SELECTOR, encode_envelope, registry::ObjectKind, OCB1_HEADER_LEN,
    };

    fn dynamic_bytes_calldata(payload_len: usize) -> Vec<u8> {
        assert!(payload_len >= OCB1_HEADER_LEN + 150);
        let payload = encode_envelope(
            ObjectKind::ResultVoteV1,
            &vec![0_u8; payload_len - OCB1_HEADER_LEN],
            super::super::schema::poc_schema_limits().codec,
        )
        .unwrap();
        let padded_len = (payload_len + 31) & !31;
        let mut data = vec![0_u8; 68 + padded_len];
        data[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
        data[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
        data[36..68].copy_from_slice(&U256::from(payload_len).to_be_bytes::<32>());
        data[68..68 + payload_len].copy_from_slice(&payload);
        data
    }

    #[test]
    fn result_vote_preflight_accepts_cap_minus_one_and_cap_and_rejects_cap_plus_one() {
        let cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_result_vote_bytes,
        )
        .unwrap();
        let limits = super::super::schema::poc_schema_limits();
        for accepted in [cap - 1, cap] {
            let data = dynamic_bytes_calldata(accepted);
            assert_eq!(
                preflight_result_vote_calldata(&data, &limits)
                    .unwrap()
                    .len(),
                accepted
            );
        }

        let rejected = dynamic_bytes_calldata(cap + 1);
        assert!(matches!(
            preflight_result_vote_calldata(&rejected, &limits),
            Err(PrecompileError::RevertBytes(_))
        ));
    }

    #[test]
    fn result_vote_preflight_rejects_nonzero_abi_padding() {
        let mut data = dynamic_bytes_calldata(OCB1_HEADER_LEN + 150);
        *data.last_mut().unwrap() = 1;
        assert!(matches!(
            preflight_result_vote_calldata(&data, &super::super::schema::poc_schema_limits()),
            Err(PrecompileError::RevertBytes(_))
        ));
    }
}
