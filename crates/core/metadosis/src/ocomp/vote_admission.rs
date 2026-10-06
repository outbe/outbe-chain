use alloy_primitives::Address;
use outbe_ocomp_protocol::{
    state::{OcompJobRecordV1, OcompJobStatus},
    vote::ResultVoteV1,
    SchemaLimits,
};
use outbe_primitives::{error::PrecompileError, storage::StorageHandle};

use crate::{constants::MAX_ACTIVE_WWDS, schema::MetadosisContract};

use super::{
    live_index::{LIVE_INDEX_HEADER_LEN, LIVE_INDEX_KEY_LEN},
    vote::{
        authorize_historical_result_vote_carrier_signer, preflight_result_vote_calldata,
        resolve_historical_result_vote_member, PinnedCommittee, PinnedVoteBinding,
        ResolvedHistoricalResultVoteMemberV1,
    },
};

/// Read-only admission decision for a full result-vote system carrier.
///
/// Callers may discard a transaction only for [`Self::InvalidCarrier`]. State
/// read failures and persisted invariant violations describe the node or chain
/// view, so they must stop the current admission/build attempt without blaming
/// the transaction or its peer.
#[derive(Debug)]
pub enum ResultVoteCarrierAdmission {
    /// The full vote, historical member and outer signer are valid now.
    Valid { represented_validator: Address },
    /// The window is durably closed. Execution must produce its existing
    /// deadline receipt instead of treating the carrier as invalid.
    DeadlinePassed { represented_validator: Address },
    /// The inclusion height reached the deadline while this window remains
    /// indexed. Execution would reject it until a later begin-block closes it.
    DeadlineDueUnclosed { deadline_height: u64 },
    /// The finalized job is live, but the candidate inclusion height precedes
    /// the begin zone that materializes its response window.
    NotYetOpen { open_height: u64 },
    /// The carrier can never be valid against this committed domain state.
    InvalidCarrier { reason: String },
    /// The node could not read the state needed for a deterministic decision.
    StateUnavailable { source: PrecompileError },
    /// Authenticated committed state violates a Metadosis invariant.
    CorruptCommittedState { source: PrecompileError },
}

/// Verifies one complete `submitLysisResult(bytes)` carrier without writing.
///
/// The full canonical vote is decoded here so that pool and payload
/// construction share the same job, committee, signature, and delegate
/// decision. Consensus execution must still verify and record the vote when
/// the transaction is actually included.
type AdmissionResult<T> = Result<T, ResultVoteCarrierAdmission>;

struct CarrierVote<'vote> {
    vote: &'vote ResultVoteV1,
    outer_signer: Address,
    inclusion_height: u64,
    limits: &'vote SchemaLimits,
}

pub fn verify_result_vote_carrier(
    storage: StorageHandle<'_>,
    calldata: &[u8],
    outer_signer: Address,
    inclusion_height: u64,
    limits: &SchemaLimits,
) -> ResultVoteCarrierAdmission {
    let decision = (|| {
        let vote_bytes = preflight_result_vote_calldata(calldata, limits).map_err(invalid)?;
        let vote = ResultVoteV1::decode_canonical(vote_bytes, limits)
            .map_err(|error| invalid_reason(format!("malformed canonical result vote: {error}")))?;
        let carrier = CarrierVote {
            vote: &vote,
            outer_signer,
            inclusion_height,
            limits,
        };
        verify_carrier_window(storage, &carrier)
    })();
    decision.unwrap_or_else(|admission| admission)
}

fn verify_carrier_window(
    storage: StorageHandle<'_>,
    carrier: &CarrierVote<'_>,
) -> AdmissionResult<ResultVoteCarrierAdmission> {
    let contract = MetadosisContract::new(storage.clone());
    let response = contract
        .response_window_for_job(carrier.vote.job_id)
        .map_err(classify_state_error)?;
    if let Some(response) = response {
        return verify_indexed_window(storage, &contract, carrier, response);
    }
    match live_job_for_finalized_job_id(&contract, carrier.vote.job_id, carrier.limits)
        .map_err(classify_state_error)?
    {
        Some(record) => Ok(verify_unmaterialized_window(storage, carrier, &record)),
        None => verify_closed_window(storage, &contract, carrier),
    }
}

fn verify_indexed_window(
    storage: StorageHandle<'_>,
    contract: &MetadosisContract<'_>,
    carrier: &CarrierVote<'_>,
    response: super::index::ResponseDeadlineKey,
) -> AdmissionResult<ResultVoteCarrierAdmission> {
    let CarrierVote {
        vote,
        outer_signer,
        inclusion_height,
        limits,
    } = *carrier;
    let record = contract
        .ocomp_job_record(response.intent_id, limits)
        .map_err(classify_state_error)?
        .ok_or_else(|| corrupt("OCOMP response index points to a missing job"))?;
    let Some(finalized) = record.finalized.as_ref() else {
        return Err(corrupt("OCOMP response-window job is not finalized"));
    };
    if finalized.job_id != response.job_id
        || finalized.deadline_height != response.deadline_height
        || !matches!(
            record.status,
            OcompJobStatus::VotingOpen | OcompJobStatus::Completed
        )
    {
        return Err(corrupt("OCOMP response index/job binding mismatch"));
    }
    let (represented_validator, member, member_count) =
        authorize_vote_for_record(storage, vote, &record, outer_signer)?;
    if inclusion_height < finalized.open_height {
        return Ok(ResultVoteCarrierAdmission::NotYetOpen {
            open_height: finalized.open_height,
        });
    }
    if inclusion_height >= finalized.deadline_height {
        return Ok(ResultVoteCarrierAdmission::DeadlineDueUnclosed {
            deadline_height: finalized.deadline_height,
        });
    }
    if let Err(error) = vote.verify_historical_member(
        &record.intent,
        finalized.job_id,
        member_count,
        member.key_epoch,
        &member.ocomp_public_key_sec1,
        inclusion_height,
        finalized.open_height,
        finalized.deadline_height,
        limits,
    ) {
        return Err(invalid_reason(format!("invalid result vote: {error}")));
    }

    Ok(ResultVoteCarrierAdmission::Valid {
        represented_validator,
    })
}

fn live_job_for_finalized_job_id(
    contract: &MetadosisContract<'_>,
    job_id: alloy_primitives::B256,
    limits: &SchemaLimits,
) -> Result<Option<OcompJobRecordV1>, PrecompileError> {
    let max_scheduler_bytes = LIVE_INDEX_KEY_LEN
        .checked_mul(MAX_ACTIVE_WWDS)
        .and_then(|bytes| LIVE_INDEX_HEADER_LEN.checked_add(bytes))
        .ok_or_else(|| PrecompileError::Fatal("OCOMP live scheduler byte cap overflow".into()))?;
    if contract.ocomp_scheduler.len()? > max_scheduler_bytes {
        return Err(PrecompileError::Fatal(
            "OCOMP live scheduler exceeds the active-job capacity".into(),
        ));
    }
    let mut matched = None;
    for state in contract.live_ocomp_fsm_states(limits)? {
        let intent_id = state
            .projection()
            .live_intent_id
            .ok_or_else(|| PrecompileError::Fatal("OCOMP live scheduler has no IntentId".into()))?;
        let record = contract
            .ocomp_job_record(intent_id, limits)?
            .ok_or_else(|| PrecompileError::Fatal("OCOMP live scheduler job is missing".into()))?;
        let matches_job = record
            .finalized
            .as_ref()
            .is_some_and(|finalized| finalized.job_id == job_id);
        if matches_job {
            if matched.is_some() {
                return Err(PrecompileError::Fatal(
                    "OCOMP live scheduler contains duplicate JobId bindings".into(),
                ));
            }
            matched = Some(record);
        }
    }
    Ok(matched)
}

fn verify_unmaterialized_window(
    storage: StorageHandle<'_>,
    carrier: &CarrierVote<'_>,
    record: &OcompJobRecordV1,
) -> ResultVoteCarrierAdmission {
    let CarrierVote {
        vote,
        outer_signer,
        inclusion_height,
        limits,
    } = *carrier;
    if record.status != OcompJobStatus::AwaitingFinality || record.terminal.is_some() {
        return corrupt("OCOMP live job without a response window is not awaiting finality");
    }
    let Some(finalized) = record.finalized.as_ref() else {
        return invalid_reason("result vote job is not finalized");
    };
    if inclusion_height > finalized.open_height {
        return corrupt("OCOMP finalized job did not materialize its response window on time");
    }
    let (represented_validator, member, member_count) =
        match authorize_vote_for_record(storage, vote, record, outer_signer) {
            Ok(authorized) => authorized,
            Err(admission) => return admission,
        };
    if let Err(error) = vote.verify_historical_member(
        &record.intent,
        finalized.job_id,
        member_count,
        member.key_epoch,
        &member.ocomp_public_key_sec1,
        finalized.open_height,
        finalized.open_height,
        finalized.deadline_height,
        limits,
    ) {
        return invalid_reason(format!("invalid result vote: {error}"));
    }
    if inclusion_height < finalized.open_height {
        ResultVoteCarrierAdmission::NotYetOpen {
            open_height: finalized.open_height,
        }
    } else {
        ResultVoteCarrierAdmission::Valid {
            represented_validator,
        }
    }
}

fn authorize_vote_for_record(
    storage: StorageHandle<'_>,
    vote: &ResultVoteV1,
    record: &OcompJobRecordV1,
    outer_signer: Address,
) -> Result<(Address, ResolvedHistoricalResultVoteMemberV1, u16), ResultVoteCarrierAdmission> {
    if PinnedVoteBinding::from_prefix(&vote.prefix()) != PinnedVoteBinding::from_record(record) {
        return Err(invalid_reason(
            "result vote does not match the pinned job binding",
        ));
    }
    let snapshot = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        storage.clone(),
        record.intent.result_validator_set_epoch,
        record.intent.result_committee_set_hash,
        record.intent.result_ocomp_binding_hash,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| corrupt("OCOMP historical snapshot is missing"))?;
    if snapshot.member_count != record.intent.result_member_count {
        return Err(corrupt("OCOMP historical snapshot member count changed"));
    }
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        vote.result_validator_set_epoch,
        vote.result_committee_set_hash,
    );
    let member = resolve_historical_result_vote_member(
        storage.clone(),
        snapshot_key,
        snapshot.member_count,
        vote.ocomp_key_hash,
        vote.key_epoch,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| invalid_reason("result vote member is not in the pinned snapshot"))?;
    let represented_validator = authorize_historical_result_vote_carrier_signer(
        storage,
        member.validator_address,
        outer_signer,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| invalid_reason("result vote carrier signer is not authorized"))?;
    Ok((represented_validator, member, snapshot.member_count))
}

fn verify_closed_window(
    storage: StorageHandle<'_>,
    contract: &MetadosisContract<'_>,
    carrier: &CarrierVote<'_>,
) -> AdmissionResult<ResultVoteCarrierAdmission> {
    let CarrierVote {
        vote,
        outer_signer,
        limits,
        ..
    } = *carrier;
    let accountability = contract
        .result_vote_accountability(vote.job_id, limits)
        .map_err(classify_state_error)?
        .ok_or_else(|| invalid_reason("result vote has no response window"))?;
    if accountability.closed_summary.is_none() {
        return Err(invalid_reason("result vote has no open response window"));
    }
    if PinnedCommittee::from_prefix(&vote.prefix())
        != PinnedCommittee::from_accountability(&accountability)
    {
        return Err(invalid_reason(
            "result vote does not match the closed response window",
        ));
    }
    let snapshot = outbe_validatorset::read_ocomp_snapshot_extension_for_binding(
        storage.clone(),
        vote.result_validator_set_epoch,
        vote.result_committee_set_hash,
        vote.result_ocomp_binding_hash,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| corrupt("OCOMP closed-window historical snapshot is missing"))?;
    if snapshot.member_count != accountability.member_count {
        return Err(corrupt("OCOMP closed-window snapshot member count changed"));
    }
    let snapshot_key = outbe_validatorset::committee_snapshot_key(
        vote.result_validator_set_epoch,
        vote.result_committee_set_hash,
    );
    let member = resolve_historical_result_vote_member(
        storage.clone(),
        snapshot_key,
        snapshot.member_count,
        vote.ocomp_key_hash,
        vote.key_epoch,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| invalid_reason("closed-window vote member is not in the snapshot"))?;
    let represented_validator = authorize_historical_result_vote_carrier_signer(
        storage,
        member.validator_address,
        outer_signer,
    )
    .map_err(classify_state_error)?
    .ok_or_else(|| invalid_reason("closed-window carrier signer is not authorized"))?;
    Ok(ResultVoteCarrierAdmission::DeadlinePassed {
        represented_validator,
    })
}

fn invalid(error: PrecompileError) -> ResultVoteCarrierAdmission {
    invalid_reason(error.to_string())
}

fn invalid_reason(reason: impl Into<String>) -> ResultVoteCarrierAdmission {
    ResultVoteCarrierAdmission::InvalidCarrier {
        reason: reason.into(),
    }
}

fn corrupt(reason: impl Into<String>) -> ResultVoteCarrierAdmission {
    ResultVoteCarrierAdmission::CorruptCommittedState {
        source: PrecompileError::Fatal(reason.into()),
    }
}

fn classify_state_error(error: PrecompileError) -> ResultVoteCarrierAdmission {
    match error {
        source @ (PrecompileError::Storage(_)
        | PrecompileError::BodyReadUnavailable(_)
        | PrecompileError::BodyReadRequestDeadline
        | PrecompileError::TreeUnavailable(_)) => {
            ResultVoteCarrierAdmission::StateUnavailable { source }
        }
        source => ResultVoteCarrierAdmission::CorruptCommittedState { source },
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256, U256};
    use outbe_ocomp_protocol::abi::encode_submit_lysis_result_calldata;
    use outbe_primitives::{
        block::{BlockContext, BlockRuntimeContext},
        error::PrecompileError,
        storage::{
            readonly::{ReadOnlyBlockContext, ReadOnlyStorageProvider, StorageReader},
            MetadosisMutationPurposeTag, StorageHandle,
        },
    };

    use crate::{
        api::{verify_result_vote_carrier, ResultVoteCarrierAdmission},
        fixture_kernel::ActivationFixture,
        schema::MetadosisContract,
    };

    fn signed_carrier_admission(
        fixture: &mut ActivationFixture,
        signer: Address,
        height: u64,
    ) -> ResultVoteCarrierAdmission {
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();
        StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(storage, &calldata, signer, height, &fixture.limits)
        })
    }

    #[test]
    fn valid_full_vote_identifies_the_historical_validator() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();

        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                14,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::Valid {
                represented_validator
            } if represented_validator == Address::repeat_byte(0xB1)
        ));
    }

    #[test]
    fn invalid_inner_signature_is_permanent_carrier_invalidity() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let mut vote = fixture.signed_result_vote(1);
        vote.signature_rs[0] ^= 1;
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();

        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                14,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::InvalidCarrier { .. }
        ));
    }

    #[test]
    fn unauthorized_outer_signer_is_permanent_carrier_invalidity() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let admission = signed_carrier_admission(&mut fixture, Address::repeat_byte(0xEE), 14);

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::InvalidCarrier { .. }
        ));
    }

    #[test]
    fn pre_open_vote_is_not_peer_invalidity() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();

        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                11,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::NotYetOpen { open_height: 14 }
        ));
    }

    #[test]
    fn pre_open_vote_still_requires_an_authorized_outer_signer() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let admission = signed_carrier_admission(&mut fixture, Address::repeat_byte(0xEE), 11);

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::InvalidCarrier { .. }
        ));
    }

    #[test]
    fn due_open_window_is_distinct_from_a_closed_deadline() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();
        let deadline = StorageHandle::enter(&mut fixture.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(fixture.intent_id, &fixture.limits)
                .unwrap()
                .unwrap()
                .finalized
                .unwrap()
                .deadline_height
        });

        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                deadline,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::DeadlineDueUnclosed {
                deadline_height
            } if deadline_height == deadline
        ));
    }

    struct UnavailableStorage;

    impl StorageReader for UnavailableStorage {
        fn read_storage(&self, _address: Address, _key: B256) -> Result<U256, PrecompileError> {
            Err(PrecompileError::Storage("backend unavailable".into()))
        }
    }

    #[test]
    fn backend_read_failure_is_not_carrier_invalidity() {
        let fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();
        let mut provider = ReadOnlyStorageProvider::new_with_block_context(
            UnavailableStorage,
            ReadOnlyBlockContext {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(17),
                block_number: 14,
                timestamp: 1_010,
            },
        );

        let admission = StorageHandle::enter(&mut provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                14,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::StateUnavailable { .. }
        ));
    }

    #[test]
    fn response_index_pointing_to_missing_job_is_committed_state_corruption() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let key = outbe_ocomp_protocol::intent::intent_storage_key(fixture.intent_id).unwrap();
            MetadosisContract::new(storage)
                .ocomp_job_records
                .get_bytes(&key)
                .clear()
                .unwrap();
        });

        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                14,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::CorruptCommittedState { .. }
        ));
    }

    #[test]
    fn closed_window_preserves_the_deadline_receipt_route() {
        let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
        let vote = fixture.signed_result_vote(1);
        let calldata = encode_submit_lysis_result_calldata(&vote, &fixture.limits).unwrap();
        let deadline = StorageHandle::enter(&mut fixture.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(fixture.intent_id, &fixture.limits)
                .unwrap()
                .unwrap()
                .finalized
                .unwrap()
                .deadline_height
        });
        fixture.seed_ocomp_recovery_stake_for_test();
        fixture.provider.set_block_number(deadline);
        fixture.provider.set_timestamp(U256::from(1_011));
        fixture
            .provider
            .enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let context = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(deadline, 1_011, 1),
                storage,
            );
            crate::commands::run_ocomp_lifecycle_begin_with_scope(&context, &fixture.scope)
                .unwrap();
        });

        let before = fixture.rollback_snapshot();
        let admission = StorageHandle::enter(&mut fixture.provider, |storage| {
            verify_result_vote_carrier(
                storage,
                &calldata,
                Address::repeat_byte(0xB1),
                deadline + 1,
                &fixture.limits,
            )
        });

        assert!(matches!(
            admission,
            ResultVoteCarrierAdmission::DeadlinePassed {
                represented_validator
            } if represented_validator == Address::repeat_byte(0xB1)
        ));
        assert_eq!(fixture.rollback_snapshot(), before);
    }
}
